//! Agent status command workflows and tmux status option updates.
//!
//! This module handles both sides of the status surface: external producers call
//! `set-agent-status` to persist observations, while users call `status` to view
//! reconciled sessions, optional Git context, and per-window tmux status badges.

use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;

use crate::agent::query::{self, WorkspaceMatchMode, WorkspaceTarget};
use crate::agent::sessions::{AgentSessionView, AgentTmuxTarget, session_views};
use crate::agent::sidebar;
use crate::cli;
use crate::config::{Config, StatusIcons};
use crate::git::{Git, WorktreeInfo};
use crate::paths::{RepoPaths, infer_repo_metadata_from_paths, path_basename};
use crate::state::{
    AgentLocationHints, AgentObservationKey, AgentObservationState, AgentSessionKey,
    AgentStatus as StoredAgentStatus, StateStore, next_observation_timing, now_unix_seconds,
};
use crate::tmux::Tmux;

const KMUX_STATUS_OPTION: &str = "@kmux_status";

/// Print active agent sessions, optionally scoped to the current repo and enriched with Git state.
pub fn run(args: cli::StatusArgs) -> Result<()> {
    let store = StateStore::new()?;
    let tmux = Tmux::from_env();
    let config = Config::load()?;
    let views = session_views(&store, &tmux)?;
    let entries = status_entries(&views, &args, &config.status_icons)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&entries)?);
    } else if entries.is_empty() {
        println!("No active agents");
    } else {
        print_table(&entries, args.git);
    }
    Ok(())
}

/// Record or delete one agent status observation from an external producer.
pub fn set_agent_status(args: cli::SetAgentStatusArgs) -> Result<()> {
    if std::env::var_os("KMUX_DISABLE_SET_AGENT_STATUS").is_some() {
        return Ok(());
    }

    let config = Config::load()?;
    let tmux = Tmux::from_env();
    let store = StateStore::new()?;
    let key = observation_key(&args)?;

    if args.delete_session {
        store.delete_session(&key.session)?;
        notify_observation_changed(&store, &tmux, &config.status_icons);
        return Ok(());
    }
    if args.delete {
        store.delete_observation(&key)?;
        notify_observation_changed(&store, &tmux, &config.status_icons);
        return Ok(());
    }

    let now = now_unix_seconds();
    let previous = store.get_observation(&key)?;
    let status = args.status.map(stored_status);
    let status_supplied = status.is_some();
    let timing = next_observation_timing(previous.as_ref(), status, now);
    let mut state = previous.unwrap_or_else(|| AgentObservationState {
        key: key.clone(),
        created_at: now,
        status: None,
        status_observed_at: None,
        status_changed_at: None,
        working_elapsed_secs: 0,
        observed_at: now,
        title: None,
        context: None,
        target: AgentLocationHints::default(),
    });
    if state.created_at == 0 {
        state.created_at = state.effective_created_at();
    }
    state.key = key;
    if status_supplied {
        state.status = status;
        state.status_observed_at = Some(now);
    }
    state.status_changed_at = timing.status_changed_at;
    state.working_elapsed_secs = timing.working_elapsed_secs;
    state.observed_at = now;
    if let Some(title) = clean_optional_ref(args.title.as_ref()) {
        state.title = Some(title);
    }
    if let Some(context) = clean_optional_ref(args.context.as_ref()) {
        state.context = Some(context);
    }
    apply_location_args(&mut state.target, &args);
    enrich_missing_repo_metadata(&mut state.target);

    store.upsert_observation(&state)?;
    notify_observation_changed(&store, &tmux, &config.status_icons);
    Ok(())
}

fn notify_observation_changed(store: &StateStore, tmux: &Tmux, icons: &StatusIcons) {
    let _ = refresh_window_statuses(store, tmux, icons);
    let _ = sidebar::notify_observation_changed(tmux);
}

/// Refresh each tmux window's kmux status option from the highest-priority agent in it.
pub fn refresh_window_statuses(store: &StateStore, tmux: &Tmux, icons: &StatusIcons) -> Result<()> {
    let views = session_views(store, tmux)?;
    let mut by_window = HashMap::<String, StoredAgentStatus>::new();
    for view in views {
        if view.tmux_target != AgentTmuxTarget::Window {
            continue;
        }
        let Some(window_id) = view.target.tmux_window_id else {
            continue;
        };
        by_window
            .entry(window_id)
            .and_modify(|status| {
                if status_rank(view.status) > status_rank(*status) {
                    *status = view.status;
                }
            })
            .or_insert(view.status);
    }

    for window in tmux.list_windows(None)? {
        if let Some(status) = by_window.get(&window.window_id).copied() {
            tmux.set_window_option(
                &window.window_id,
                KMUX_STATUS_OPTION,
                status_icon(status, icons),
            )?;
        } else {
            tmux.unset_window_option(&window.window_id, KMUX_STATUS_OPTION)?;
        }
    }
    Ok(())
}

#[derive(Debug, Clone, Serialize)]
struct GitInfo {
    has_staged: bool,
    has_unstaged: bool,
    has_unmerged_commits: bool,
}

#[derive(Debug, Serialize)]
struct StatusEntry {
    agent_kind: String,
    session_id: String,
    workspace: String,
    git_branch: String,
    status: String,
    icon: String,
    elapsed_secs: u64,
    title: Option<String>,
    context: Option<String>,
    tmux_pane_id: String,
    workspace_slug: Option<String>,
    git_worktree_path: Option<String>,
    tmux_session_name: String,
    tmux_window_name: String,
    tmux_window_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    git: Option<GitInfo>,
}

struct DisplayRow {
    workspace: String,
    status: String,
    elapsed: String,
    git: String,
    title: String,
}

// The observation key identifies both the logical agent session and the producer
// that reported it, so TUI and server observations can coexist for one session.
fn observation_key(args: &cli::SetAgentStatusArgs) -> Result<AgentObservationKey> {
    Ok(AgentObservationKey {
        session: AgentSessionKey {
            agent_kind: clean_required(&args.agent_kind, "--agent-kind")?,
            session_id: clean_required(&args.session_id, "--session-id")?,
        },
        producer_kind: clean_required(&args.producer_kind, "--producer-kind")?,
        producer_instance: clean_required(&args.producer_instance, "--producer-instance")?,
    })
}

fn stored_status(status: cli::AgentStatus) -> StoredAgentStatus {
    match status {
        cli::AgentStatus::Working => StoredAgentStatus::Working,
        cli::AgentStatus::Waiting => StoredAgentStatus::Waiting,
        cli::AgentStatus::Done => StoredAgentStatus::Done,
    }
}

fn apply_location_args(target: &mut AgentLocationHints, args: &cli::SetAgentStatusArgs) {
    apply_optional(&mut target.tmux_instance, &args.tmux_instance);
    apply_optional(&mut target.tmux_pane_id, &args.tmux_pane_id);
    apply_optional(&mut target.tmux_window_id, &args.tmux_window_id);
    apply_agent_workspace_id(target, args);
    apply_optional(&mut target.git_repo_name, &args.git_repo_name);
    apply_optional(&mut target.git_repo_path, &args.git_repo_path);
    apply_optional(&mut target.git_worktree_path, &args.git_worktree_path);
    apply_optional(&mut target.git_branch, &args.git_branch);
    apply_reported_directory(target, args);
}

fn apply_agent_workspace_id(target: &mut AgentLocationHints, args: &cli::SetAgentStatusArgs) {
    if let Some(value) = clean_optional_ref(args.agent_workspace_id.as_ref()) {
        target.agent_workspace_id = Some(value);
    } else if args.clear_agent_workspace_id {
        target.agent_workspace_id = None;
    }
}

// Metadata-only updates should not erase existing fields with empty strings.
fn apply_optional(target: &mut Option<String>, value: &Option<String>) {
    if let Some(value) = clean_optional_ref(value.as_ref()) {
        *target = Some(value);
    }
}

fn apply_reported_directory(target: &mut AgentLocationHints, args: &cli::SetAgentStatusArgs) {
    target.directory = clean_optional_ref(args.directory.as_ref());
}

fn clean_required(value: &str, label: &str) -> Result<String> {
    clean_str(value)
        .map(str::to_owned)
        .ok_or_else(|| anyhow!("{label} cannot be empty"))
}

fn clean_optional_ref(value: Option<&String>) -> Option<String> {
    value.and_then(|value| clean_str(value).map(str::to_owned))
}

fn clean_str(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

// Fill missing repo fields opportunistically from path hints so older or sparse
// producers still show useful repo/branch labels.
fn enrich_missing_repo_metadata(target: &mut AgentLocationHints) {
    let metadata = infer_repo_metadata_from_paths(&[
        target.directory.as_deref(),
        target.git_worktree_path.as_deref(),
    ]);
    if target.git_repo_path.is_none() {
        target.git_repo_path = metadata.repo_path.clone();
    }
    if target.git_repo_name.is_none() {
        target.git_repo_name = target
            .git_repo_path
            .as_deref()
            .and_then(path_basename)
            .or(metadata.repo_name);
    }
    if target.git_branch.is_none() {
        target.git_branch = metadata.branch;
    }
}

// Without filters, prefer views for the current repo when the command is run
// inside a Git repository; otherwise show all known agent sessions.
fn status_entries(
    views: &[AgentSessionView],
    args: &cli::StatusArgs,
    icons: &StatusIcons,
) -> Result<Vec<StatusEntry>> {
    let now = unix_now();
    if !args.filters.is_empty() {
        return Ok(views
            .iter()
            .filter(|view| {
                args.filters
                    .iter()
                    .any(|filter| view_matches_filter(view, filter))
            })
            .map(|view| entry_for_view(view, None, now, args.git, icons))
            .collect());
    }

    if let Some(entries) = current_repo_entries(views, now, args.git, icons)? {
        return Ok(entries);
    }

    Ok(views
        .iter()
        .map(|view| entry_for_view(view, None, now, args.git, icons))
        .collect())
}

// Current-repo scoping uses strict workspace identity matching to avoid pulling
// in stale observations from another worktree that share a branch or slug.
fn current_repo_entries(
    views: &[AgentSessionView],
    now: u64,
    show_git: bool,
    icons: &StatusIcons,
) -> Result<Option<Vec<StatusEntry>>> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let Ok(paths) = RepoPaths::discover(&cwd) else {
        return Ok(None);
    };
    let git = Git::new(&paths.main_worktree);
    let worktrees = git.worktrees()?;

    let mut entries = Vec::new();
    for worktree in &worktrees {
        let target = workspace_target(worktree);
        for view in views.iter().filter(|view| {
            query::view_matches_workspace(view, &target, WorkspaceMatchMode::Identity)
        }) {
            entries.push(entry_for_view(view, Some(worktree), now, show_git, icons));
        }
    }
    Ok(Some(entries))
}

// Build a presentation row from the resolved session view, falling back through
// worktree, explicit target, and tmux metadata as needed.
fn entry_for_view(
    view: &AgentSessionView,
    worktree: Option<&WorktreeInfo>,
    now: u64,
    show_git: bool,
    icons: &StatusIcons,
) -> StatusEntry {
    let git_worktree_path = worktree
        .map(|worktree| worktree.path.display().to_string())
        .or_else(|| view.target.git_worktree_path.clone())
        .or_else(|| view.target.directory.clone());
    let workspace_slug = worktree
        .and_then(|worktree| worktree.path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .or_else(|| view.target.kmux_workspace_slug.clone());
    let branch = worktree
        .and_then(|worktree| worktree.branch.clone())
        .or_else(|| view.target.git_branch.clone())
        .unwrap_or_else(|| "-".to_owned());
    let workspace_name = workspace_slug
        .clone()
        .or_else(|| view.target.tmux_window_name.clone())
        .unwrap_or_else(|| view.key.session_id.clone());
    let git = if show_git {
        git_worktree_path
            .as_deref()
            .map(Path::new)
            .map(|path| compute_git_info(path, &branch))
    } else {
        None
    };

    StatusEntry {
        agent_kind: view.key.agent_kind.clone(),
        session_id: view.key.session_id.clone(),
        workspace: workspace_name,
        git_branch: branch,
        status: view.status.as_str().to_owned(),
        icon: status_icon(view.status, icons).to_owned(),
        elapsed_secs: view.elapsed_secs(now),
        title: view
            .title
            .clone()
            .or_else(|| view.target.tmux_pane_title.clone()),
        context: view.context.clone(),
        tmux_pane_id: view.target.tmux_pane_id.clone().unwrap_or_default(),
        workspace_slug,
        git_worktree_path,
        tmux_session_name: view.target.tmux_session_name.clone().unwrap_or_default(),
        tmux_window_name: view.target.tmux_window_name.clone().unwrap_or_default(),
        tmux_window_id: view.target.tmux_window_id.clone().unwrap_or_default(),
        git,
    }
}

fn view_matches_filter(view: &AgentSessionView, filter: &str) -> bool {
    view.key.agent_kind == filter
        || view.key.session_id == filter
        || view.target.kmux_workspace_slug.as_deref() == Some(filter)
        || view.target.git_branch.as_deref() == Some(filter)
        || view.target.tmux_window_name.as_deref() == Some(filter)
        || view.target.git_worktree_path.as_deref() == Some(filter)
        || view.target.directory.as_deref() == Some(filter)
        || view.title.as_deref() == Some(filter)
}

fn workspace_target(worktree: &WorktreeInfo) -> WorkspaceTarget<'_> {
    WorkspaceTarget::new(&worktree.path)
}

fn status_icon(status: StoredAgentStatus, icons: &StatusIcons) -> &str {
    match status {
        StoredAgentStatus::Working => icons.working(),
        StoredAgentStatus::Waiting => icons.waiting(),
        StoredAgentStatus::Done => icons.done(),
    }
}

// Git status is display-only here; failures become false so a missing worktree
// or transient Git error does not hide agent status.
fn compute_git_info(path: &Path, branch: &str) -> GitInfo {
    let git = Git::new(path);
    GitInfo {
        has_staged: git.has_staged_changes().unwrap_or(false),
        has_unstaged: git.has_unstaged_changes().unwrap_or(false),
        has_unmerged_commits: if branch == "-" {
            false
        } else {
            git.branch_is_safely_deletable(branch)
                .map(|safe| !safe)
                .unwrap_or(false)
        },
    }
}

// Higher ranks win when multiple agents report different states in one window.
fn status_rank(status: StoredAgentStatus) -> u8 {
    match status {
        StoredAgentStatus::Waiting => 3,
        StoredAgentStatus::Working => 2,
        StoredAgentStatus::Done => 1,
    }
}

fn print_table(entries: &[StatusEntry], show_git: bool) {
    let rows = entries
        .iter()
        .map(|entry| DisplayRow {
            workspace: format_workspace(entry),
            status: entry.status.clone(),
            elapsed: compact_elapsed(entry.elapsed_secs),
            git: git_label(&entry.git),
            title: entry.title.clone().unwrap_or_else(|| "-".to_owned()),
        })
        .collect::<Vec<_>>();
    let headers = if show_git {
        vec!["WORKSPACE", "STATUS", "ELAPSED", "GIT", "TITLE"]
    } else {
        vec!["WORKSPACE", "STATUS", "ELAPSED", "TITLE"]
    };
    let mut widths = headers
        .iter()
        .map(|header| header.len())
        .collect::<Vec<_>>();

    for row in &rows {
        let values = row_values(row, show_git);
        for (index, value) in values.iter().enumerate() {
            widths[index] = widths[index].max(value.chars().count());
        }
    }

    println!("{}", format_row(&headers, &widths));
    for row in &rows {
        println!("{}", format_row(&row_values(row, show_git), &widths));
    }
}

fn format_workspace(entry: &StatusEntry) -> String {
    if entry.git_branch != "-" && entry.git_branch != entry.workspace {
        format!("{} ({})", entry.workspace, entry.git_branch)
    } else {
        entry.workspace.clone()
    }
}

fn git_label(git: &Option<GitInfo>) -> String {
    let Some(git) = git else {
        return "-".to_owned();
    };
    let mut parts = Vec::new();
    if git.has_staged {
        parts.push("staged");
    }
    if git.has_unstaged {
        parts.push("unstaged");
    }
    if git.has_unmerged_commits {
        parts.push("unmerged");
    }
    if parts.is_empty() {
        "clean".to_owned()
    } else {
        parts.join(",")
    }
}

fn row_values(row: &DisplayRow, show_git: bool) -> Vec<&str> {
    if show_git {
        vec![
            &row.workspace,
            &row.status,
            &row.elapsed,
            &row.git,
            &row.title,
        ]
    } else {
        vec![&row.workspace, &row.status, &row.elapsed, &row.title]
    }
}

fn format_row(values: &[&str], widths: &[usize]) -> String {
    values
        .iter()
        .enumerate()
        .map(|(index, value)| format!("{value:<width$}", width = widths[index]))
        .collect::<Vec<_>>()
        .join("  ")
        .trim_end()
        .to_owned()
}

fn compact_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        "<1m".to_owned()
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        let hours = seconds / (60 * 60);
        let minutes = (seconds % (60 * 60)) / 60;
        if minutes == 0 {
            format!("{hours}h")
        } else {
            format!("{hours}h {minutes}m")
        }
    } else {
        format!("{}d", seconds / (60 * 60 * 24))
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn apply_location_args_records_agent_workspace_id() {
        let mut target = AgentLocationHints::default();
        let mut args = set_status_args();
        args.agent_workspace_id = Some("  wrk_01KTEST  ".to_owned());

        apply_location_args(&mut target, &args);

        assert_eq!(target.agent_workspace_id.as_deref(), Some("wrk_01KTEST"));
    }

    #[test]
    fn apply_location_args_ignores_empty_agent_workspace_id() {
        let mut target = AgentLocationHints {
            agent_workspace_id: Some("wrk_existing".to_owned()),
            ..AgentLocationHints::default()
        };
        let mut args = set_status_args();
        args.agent_workspace_id = Some("   ".to_owned());

        apply_location_args(&mut target, &args);

        assert_eq!(target.agent_workspace_id.as_deref(), Some("wrk_existing"));
    }

    #[test]
    fn apply_location_args_clears_agent_workspace_id() {
        let mut target = AgentLocationHints {
            agent_workspace_id: Some("wrk_existing".to_owned()),
            ..AgentLocationHints::default()
        };
        let mut args = set_status_args();
        args.clear_agent_workspace_id = true;

        apply_location_args(&mut target, &args);

        assert_eq!(target.agent_workspace_id, None);
    }

    #[test]
    fn apply_location_args_replaces_directory_each_update() {
        let mut target = AgentLocationHints {
            directory: Some("/repo/old".to_owned()),
            ..AgentLocationHints::default()
        };
        let args = set_status_args();

        apply_location_args(&mut target, &args);

        assert_eq!(target.directory, None);
    }

    fn set_status_args() -> cli::SetAgentStatusArgs {
        cli::SetAgentStatusArgs {
            status: Some(cli::AgentStatus::Working),
            agent_kind: "opencode".to_owned(),
            session_id: "ses_root".to_owned(),
            producer_kind: "tui".to_owned(),
            producer_instance: "default/%1".to_owned(),
            delete: false,
            delete_session: false,
            title: None,
            context: None,
            tmux_instance: None,
            tmux_pane_id: None,
            tmux_window_id: None,
            agent_workspace_id: None,
            clear_agent_workspace_id: false,
            git_repo_name: None,
            git_repo_path: None,
            git_worktree_path: None,
            git_branch: None,
            directory: None,
        }
    }
}
