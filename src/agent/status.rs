//! Status query and presentation for resolved agent sessions.
//!
//! Workflows own command orchestration. This module turns already resolved agent
//! sessions into the stable table and JSON status surface.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::Serialize;

use crate::agent::query::{self, WorkspaceMatchMode, WorkspaceTarget};
use crate::agent::sessions::ResolvedAgentSession;
use crate::config::StatusIcons;
use crate::git::{Git, WorktreeInfo};
use crate::paths::RepoPaths;
use crate::state::AgentStatus as StoredAgentStatus;

/// Input for querying the status surface from resolved sessions.
pub(crate) struct StatusQuery {
    filters: Vec<String>,
    show_git: bool,
}

impl StatusQuery {
    /// Build a status query from command-boundary input.
    pub(crate) fn new(filters: Vec<String>, show_git: bool) -> Self {
        Self { filters, show_git }
    }

    /// Return whether Git status decoration should be included.
    pub(crate) fn show_git(&self) -> bool {
        self.show_git
    }
}

#[derive(Debug, Clone, Serialize)]
struct GitInfo {
    has_staged: bool,
    has_unstaged: bool,
    has_unmerged_commits: bool,
}

#[derive(Debug, Serialize)]
pub(crate) struct StatusEntry {
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

// Without filters, prefer views for the current repo when the command is run
// inside a Git repository; otherwise show all known agent sessions.
pub(crate) fn status_entries(
    views: &[ResolvedAgentSession],
    query: &StatusQuery,
    icons: &StatusIcons,
) -> Result<Vec<StatusEntry>> {
    let now = unix_now();
    if !query.filters.is_empty() {
        return Ok(views
            .iter()
            .filter(|view| {
                query
                    .filters
                    .iter()
                    .any(|filter| view_matches_filter(view, filter))
            })
            .map(|view| entry_for_view(view, None, now, query.show_git, icons))
            .collect());
    }

    if let Some(entries) = current_repo_entries(views, now, query.show_git, icons)? {
        return Ok(entries);
    }

    Ok(views
        .iter()
        .map(|view| entry_for_view(view, None, now, query.show_git, icons))
        .collect())
}

// Current-repo scoping uses strict workspace identity matching to avoid pulling
// in stale observations from another worktree that share a branch or slug.
fn current_repo_entries(
    views: &[ResolvedAgentSession],
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
    view: &ResolvedAgentSession,
    worktree: Option<&WorktreeInfo>,
    now: u64,
    show_git: bool,
    icons: &StatusIcons,
) -> StatusEntry {
    let git_worktree_path = worktree
        .map(|worktree| worktree.path.display().to_string())
        .or_else(|| view.git_worktree_path().map(str::to_owned))
        .or_else(|| view.directory().map(str::to_owned));
    let workspace_slug = worktree
        .and_then(|worktree| worktree.path.file_name())
        .map(|name| name.to_string_lossy().into_owned())
        .or_else(|| view.kmux_workspace_slug().map(str::to_owned));
    let branch = worktree
        .and_then(|worktree| worktree.branch.clone())
        .or_else(|| view.git_branch().map(str::to_owned))
        .unwrap_or_else(|| "-".to_owned());
    let workspace_name = workspace_slug
        .clone()
        .or_else(|| view.tmux_window_name().map(str::to_owned))
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
            .or_else(|| view.tmux_pane_title().map(str::to_owned)),
        context: view.context.clone(),
        tmux_pane_id: view.tmux_pane_id().unwrap_or_default().to_owned(),
        workspace_slug,
        git_worktree_path,
        tmux_session_name: view.tmux_session_name().unwrap_or_default().to_owned(),
        tmux_window_name: view.tmux_window_name().unwrap_or_default().to_owned(),
        tmux_window_id: view.tmux_window_id().unwrap_or_default().to_owned(),
        git,
    }
}

fn view_matches_filter(view: &ResolvedAgentSession, filter: &str) -> bool {
    view.key.agent_kind == filter
        || view.key.session_id == filter
        || view.kmux_workspace_slug() == Some(filter)
        || view.git_branch() == Some(filter)
        || view.tmux_window_name() == Some(filter)
        || view.git_worktree_path() == Some(filter)
        || view.directory() == Some(filter)
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

/// Print status entries in the stable human-readable table format.
pub(crate) fn print_table(entries: &[StatusEntry], show_git: bool) {
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
    use crate::agent::sessions::{AgentTmuxTarget, ResolvedAgentSession, ResolvedAgentTarget};
    use crate::state::{AgentSessionKey, AgentStatus};

    #[test]
    fn status_entries_filter_by_branch_without_current_repo_scope() -> Result<()> {
        let views = vec![
            session_view("opencode", "ses_feature", "feature/auth", "Feature"),
            session_view("codex", "ses_other", "main", "Other"),
        ];
        let query = StatusQuery::new(vec!["feature/auth".to_owned()], false);

        let entries = status_entries(&views, &query, &StatusIcons::default())?;

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent_kind, "opencode");
        assert_eq!(entries[0].session_id, "ses_feature");
        assert_eq!(entries[0].git_branch, "feature/auth");
        Ok(())
    }

    #[test]
    fn status_entries_serialize_existing_json_field_names() -> Result<()> {
        let views = vec![session_view(
            "opencode",
            "ses_feature",
            "feature/auth",
            "Feature",
        )];
        let query = StatusQuery::new(vec!["Feature".to_owned()], false);

        let entries = status_entries(&views, &query, &StatusIcons::default())?;
        let json = serde_json::to_value(&entries)?;
        let first = json
            .as_array()
            .and_then(|entries| entries.first())
            .ok_or_else(|| anyhow::anyhow!("expected one serialized status entry"))?;

        assert_eq!(
            first.get("agent_kind").and_then(|value| value.as_str()),
            Some("opencode")
        );
        assert_eq!(
            first.get("session_id").and_then(|value| value.as_str()),
            Some("ses_feature")
        );
        assert_eq!(
            first.get("workspace_slug").and_then(|value| value.as_str()),
            Some("feature-auth")
        );
        assert_eq!(
            first.get("git_branch").and_then(|value| value.as_str()),
            Some("feature/auth")
        );
        Ok(())
    }

    fn session_view(
        agent_kind: &str,
        session_id: &str,
        git_branch: &str,
        title: &str,
    ) -> ResolvedAgentSession {
        ResolvedAgentSession {
            key: AgentSessionKey {
                agent_kind: agent_kind.to_owned(),
                session_id: session_id.to_owned(),
            },
            workspace: None,
            workspace_key: Some(format!("/repo/{session_id}")),
            tmux_target: AgentTmuxTarget::Window,
            created_at: 100,
            status: AgentStatus::Working,
            status_observed_at: 100,
            status_changed_at: 100,
            working_elapsed_secs: 0,
            observed_at: 100,
            title: Some(title.to_owned()),
            context: None,
            metadata: Default::default(),
            target: ResolvedAgentTarget {
                kmux_workspace_slug: Some(git_branch.replace('/', "-")),
                git_worktree_path: Some(format!("/repo/{session_id}")),
                git_branch: Some(git_branch.to_owned()),
                tmux_session_name: Some("project".to_owned()),
                tmux_window_id: Some("@1".to_owned()),
                tmux_window_name: Some(format!("kmux-{}", git_branch.replace('/', "-"))),
                ..ResolvedAgentTarget::default()
            },
        }
    }
}
