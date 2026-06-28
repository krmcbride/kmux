use std::collections::HashMap;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, anyhow};
use serde::Serialize;

use crate::agent::sessions::{AgentSessionView, session_views};
use crate::cli;
use crate::config::{Config, StatusIcons};
use crate::git::{Git, WorktreeInfo};
use crate::paths::{RepoPaths, infer_repo_metadata_from_paths, path_basename, same_path};
use crate::state::{
    AgentLocationHints, AgentObservationKey, AgentObservationState, AgentSessionKey,
    AgentStatus as StoredAgentStatus, StateStore, next_observation_timing, now_unix_seconds,
};
use crate::tmux::Tmux;

const KMUX_STATUS_OPTION: &str = "@kmux_status";

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
    worktree: String,
    branch: String,
    status: String,
    icon: String,
    elapsed_secs: u64,
    title: Option<String>,
    context: Option<String>,
    pane_id: String,
    worktree_handle: Option<String>,
    worktree_path: Option<String>,
    session_name: String,
    window_name: String,
    window_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    git: Option<GitInfo>,
}

struct DisplayRow {
    worktree: String,
    status: String,
    elapsed: String,
    git: String,
    title: String,
}

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
        let _ = refresh_window_statuses(&store, &tmux, &config.status_icons);
        return Ok(());
    }
    if args.delete {
        store.delete_observation(&key)?;
        let _ = refresh_window_statuses(&store, &tmux, &config.status_icons);
        return Ok(());
    }

    let now = now_unix_seconds();
    let previous = store.get_observation(&key)?;
    let status = args.status.map(stored_status);
    let status_supplied = status.is_some();
    let timing = next_observation_timing(previous.as_ref(), status, now);
    let mut state = previous.unwrap_or_else(|| AgentObservationState {
        key: key.clone(),
        status: None,
        status_observed_at: None,
        status_changed_at: None,
        working_elapsed_secs: 0,
        observed_at: now,
        title: None,
        context: None,
        target: AgentLocationHints::default(),
    });
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
    let _ = refresh_window_statuses(&store, &tmux, &config.status_icons);
    Ok(())
}

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
    apply_optional(&mut target.pane_id, &args.pane_id);
    apply_optional(&mut target.window_id, &args.window_id);
    apply_optional(&mut target.session_name, &args.session_name);
    apply_optional(&mut target.window_name, &args.window_name);
    apply_optional(&mut target.repo_name, &args.repo_name);
    apply_optional(&mut target.repo_path, &args.repo_path);
    apply_optional(&mut target.worktree_handle, &args.worktree_handle);
    apply_optional(&mut target.worktree_path, &args.worktree_path);
    apply_optional(&mut target.branch, &args.branch);
    apply_optional(&mut target.directory, &args.directory);
}

fn apply_optional(target: &mut Option<String>, value: &Option<String>) {
    if let Some(value) = clean_optional_ref(value.as_ref()) {
        *target = Some(value);
    }
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

fn enrich_missing_repo_metadata(target: &mut AgentLocationHints) {
    let metadata = infer_repo_metadata_from_paths(&[
        target.directory.as_deref(),
        target.worktree_path.as_deref(),
    ]);
    if target.repo_path.is_none() {
        target.repo_path = metadata.repo_path.clone();
    }
    if target.repo_name.is_none() {
        target.repo_name = target
            .repo_path
            .as_deref()
            .and_then(path_basename)
            .or(metadata.repo_name);
    }
    if target.branch.is_none() {
        target.branch = metadata.branch;
    }
}

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
        for view in views
            .iter()
            .filter(|view| view_matches_worktree(view, worktree))
        {
            entries.push(entry_for_view(view, Some(worktree), now, show_git, icons));
        }
    }
    Ok(Some(entries))
}

fn entry_for_view(
    view: &AgentSessionView,
    worktree: Option<&WorktreeInfo>,
    now: u64,
    show_git: bool,
    icons: &StatusIcons,
) -> StatusEntry {
    let worktree_path = worktree
        .map(|worktree| worktree.path.display().to_string())
        .or_else(|| view.target.worktree_path.clone())
        .or_else(|| view.target.directory.clone());
    let handle = worktree
        .and_then(|worktree| worktree.path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .or_else(|| view.target.worktree_handle.clone());
    let branch = worktree
        .and_then(|worktree| worktree.branch.clone())
        .or_else(|| view.target.branch.clone())
        .unwrap_or_else(|| "-".to_owned());
    let worktree_name = handle
        .clone()
        .or_else(|| view.target.window_name.clone())
        .unwrap_or_else(|| view.key.session_id.clone());
    let git = if show_git {
        worktree_path
            .as_deref()
            .map(Path::new)
            .map(|path| compute_git_info(path, &branch))
    } else {
        None
    };

    StatusEntry {
        agent_kind: view.key.agent_kind.clone(),
        session_id: view.key.session_id.clone(),
        worktree: worktree_name,
        branch,
        status: view.status.as_str().to_owned(),
        icon: status_icon(view.status, icons).to_owned(),
        elapsed_secs: view.elapsed_secs(now),
        title: view
            .title
            .clone()
            .or_else(|| view.target.pane_title.clone()),
        context: view.context.clone(),
        pane_id: view.target.pane_id.clone().unwrap_or_default(),
        worktree_handle: handle,
        worktree_path,
        session_name: view.target.session_name.clone().unwrap_or_default(),
        window_name: view.target.window_name.clone().unwrap_or_default(),
        window_id: view.target.window_id.clone().unwrap_or_default(),
        git,
    }
}

fn view_matches_filter(view: &AgentSessionView, filter: &str) -> bool {
    view.key.agent_kind == filter
        || view.key.session_id == filter
        || view.target.worktree_handle.as_deref() == Some(filter)
        || view.target.branch.as_deref() == Some(filter)
        || view.target.window_name.as_deref() == Some(filter)
        || view.target.worktree_path.as_deref() == Some(filter)
        || view.target.directory.as_deref() == Some(filter)
        || view.title.as_deref() == Some(filter)
}

fn view_matches_worktree(view: &AgentSessionView, worktree: &WorktreeInfo) -> bool {
    let report_path = view
        .target
        .worktree_path
        .as_deref()
        .or(view.target.directory.as_deref())
        .map(Path::new);
    if let Some(report_path) = report_path
        && same_path(report_path, &worktree.path)
    {
        return true;
    }

    let handle = worktree.path.file_name().map(|name| name.to_string_lossy());
    let branch_matches = view.target.branch.as_deref() == worktree.branch.as_deref();
    let handle_matches = handle
        .as_deref()
        .is_some_and(|handle| view.target.worktree_handle.as_deref() == Some(handle));

    report_path.is_none() && branch_matches && handle_matches
}

fn status_icon(status: StoredAgentStatus, icons: &StatusIcons) -> &str {
    match status {
        StoredAgentStatus::Working => icons.working(),
        StoredAgentStatus::Waiting => icons.waiting(),
        StoredAgentStatus::Done => icons.done(),
    }
}

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

pub fn refresh_window_statuses(store: &StateStore, tmux: &Tmux, icons: &StatusIcons) -> Result<()> {
    let views = session_views(store, tmux)?;
    let mut by_window = HashMap::<String, StoredAgentStatus>::new();
    for view in views {
        let Some(window_id) = view.target.window_id else {
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
            worktree: format_worktree(entry),
            status: entry.status.clone(),
            elapsed: compact_elapsed(entry.elapsed_secs),
            git: git_label(&entry.git),
            title: entry.title.clone().unwrap_or_else(|| "-".to_owned()),
        })
        .collect::<Vec<_>>();
    let headers = if show_git {
        vec!["WORKTREE", "STATUS", "ELAPSED", "GIT", "TITLE"]
    } else {
        vec!["WORKTREE", "STATUS", "ELAPSED", "TITLE"]
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

fn format_worktree(entry: &StatusEntry) -> String {
    if entry.branch != "-" && entry.branch != entry.worktree {
        format!("{} ({})", entry.worktree, entry.branch)
    } else {
        entry.worktree.clone()
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
            &row.worktree,
            &row.status,
            &row.elapsed,
            &row.git,
            &row.title,
        ]
    } else {
        vec![&row.worktree, &row.status, &row.elapsed, &row.title]
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
