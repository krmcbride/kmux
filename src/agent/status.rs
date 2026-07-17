//! Status query and presentation for workspace activity rows.
//!
//! Workflows own command orchestration. This module turns shared workspace
//! activity rows into the stable table and JSON status surface.

use std::path::Path;

use serde::Serialize;

use crate::agent::workspace_activity::WorkspaceActivity;
use crate::config::StatusIcons;
use crate::git::Git;
use crate::state::AgentStatus as StoredAgentStatus;

/// Input for filtering and decorating the shared workspace activity surface.
pub struct StatusQuery {
    filters: Vec<String>,
    show_git: bool,
}

impl StatusQuery {
    /// Build a status query from command-boundary input.
    pub fn new(filters: Vec<String>, show_git: bool) -> Self {
        Self { filters, show_git }
    }

    /// Return whether Git status decoration should be included.
    pub fn show_git(&self) -> bool {
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
pub struct StatusEntry {
    agent_kind: String,
    session_id: String,
    workspace_key: String,
    workspace: String,
    git_branch: String,
    status: String,
    icon: String,
    elapsed_secs: u64,
    title: Option<String>,
    context: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_title: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    display_context: Option<String>,
    tmux_pane_id: String,
    workspace_slug: Option<String>,
    git_worktree_path: Option<String>,
    directory: Option<String>,
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

/// Build status entries from the shared global workspace activity model.
pub fn status_entries(
    activities: &[WorkspaceActivity],
    now: u64,
    query: &StatusQuery,
    icons: &StatusIcons,
) -> Vec<StatusEntry> {
    activities
        .iter()
        .filter(|activity| {
            query.filters.is_empty()
                || query
                    .filters
                    .iter()
                    .any(|filter| activity.matches_filter(filter))
        })
        .map(|activity| entry_for_activity(activity, now, query.show_git, icons))
        .collect()
}

// Build a status presentation row from the shared workspace activity aggregate.
fn entry_for_activity(
    activity: &WorkspaceActivity,
    now: u64,
    show_git: bool,
    icons: &StatusIcons,
) -> StatusEntry {
    let git_worktree_path = Some(activity.git_worktree_path().to_owned());
    let directory = activity.directory().map(str::to_owned);
    let branch = activity.git_branch().unwrap_or("-").to_owned();
    let git = if show_git {
        Some(compute_git_info(
            Path::new(activity.git_worktree_path()),
            &branch,
        ))
    } else {
        None
    };
    let title = activity
        .title()
        .map(str::to_owned)
        .or_else(|| activity.tmux_pane_title().map(str::to_owned));
    let context = activity.context().map(str::to_owned);
    let display_title = non_empty_string(&activity.display_title);
    let display_context = non_empty_string(&activity.display_context);

    StatusEntry {
        agent_kind: activity.primary_session_key().agent_kind.clone(),
        session_id: activity.primary_session_key().session_id.clone(),
        workspace_key: activity.workspace_key().to_owned(),
        workspace: activity.primary.clone(),
        git_branch: branch,
        status: activity.status().as_str().to_owned(),
        icon: status_icon(activity.status(), icons).to_owned(),
        elapsed_secs: activity.elapsed_secs(now),
        title,
        context,
        display_title,
        display_context,
        tmux_pane_id: activity.tmux_pane_id().unwrap_or_default().to_owned(),
        workspace_slug: activity.workspace_slug().map(str::to_owned),
        git_worktree_path,
        directory,
        tmux_session_name: activity.tmux_session_name().unwrap_or_default().to_owned(),
        tmux_window_name: activity.tmux_window_name().unwrap_or_default().to_owned(),
        tmux_window_id: activity.tmux_window_id().unwrap_or_default().to_owned(),
        git,
    }
}

fn non_empty_string(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| value.to_owned())
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
pub fn print_table(entries: &[StatusEntry], show_git: bool) {
    let rows = entries
        .iter()
        .map(|entry| DisplayRow {
            workspace: format_workspace(entry),
            status: entry.status.clone(),
            elapsed: compact_elapsed(entry.elapsed_secs),
            git: git_label(&entry.git),
            title: entry
                .display_title
                .clone()
                .or_else(|| entry.title.clone())
                .unwrap_or_else(|| "-".to_owned()),
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sessions::{
        AgentTmuxTarget, ResolvedAgentSession, ResolvedAgentTarget, ResolvedAgentWorkspace,
    };
    use crate::agent::workspace_activity::workspace_activities_from_sessions;
    use crate::state::{AgentSessionKey, AgentStatus};
    use anyhow::Result;

    #[test]
    fn status_entries_filter_by_branch_without_current_repo_scope() -> Result<()> {
        let views = vec![
            session_view("opencode", "ses_feature", "feature/auth", "Feature"),
            session_view("codex", "ses_other", "main", "Other"),
        ];
        let query = StatusQuery::new(vec!["feature/auth".to_owned()], false);
        let activities = workspace_activities_from_sessions(views);

        let entries = status_entries(&activities, 300, &query, &StatusIcons::default());

        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent_kind, "opencode");
        assert_eq!(entries[0].session_id, "ses_feature");
        assert_eq!(entries[0].git_branch, "feature/auth");
        Ok(())
    }

    #[test]
    fn status_entries_without_filters_include_all_activity_rows() {
        let views = vec![
            session_view("opencode", "ses_feature", "feature/auth", "Feature"),
            session_view("codex", "ses_other", "main", "Other"),
        ];
        let query = StatusQuery::new(Vec::new(), false);
        let activities = workspace_activities_from_sessions(views);

        let entries = status_entries(&activities, 300, &query, &StatusIcons::default());

        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].session_id, "ses_feature");
        assert_eq!(entries[1].session_id, "ses_other");
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
        let activities = workspace_activities_from_sessions(views);

        let entries = status_entries(&activities, 300, &query, &StatusIcons::default());
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

    #[test]
    fn status_json_title_preserves_source_title_even_when_display_filters_it() {
        let mut view = session_view("opencode", "ses_feature", "feature/auth", "project");
        view.target.git_repo_name = Some("project".to_owned());
        view.target.tmux_pane_title = Some("project".to_owned());
        view.target.tmux_pane_current_command = None;
        let query = StatusQuery::new(Vec::new(), false);
        let activities = workspace_activities_from_sessions(vec![view]);

        let entries = status_entries(&activities, 300, &query, &StatusIcons::default());

        assert_eq!(entries[0].title.as_deref(), Some("project"));
        assert_eq!(
            entries[0].display_title.as_deref(),
            Some("session ses_feature")
        );
    }

    #[test]
    fn status_json_title_stays_empty_when_only_display_fallback_exists() {
        let mut view = session_view("opencode", "ses_feature", "feature/auth", "Feature");
        view.title = None;
        view.target.tmux_pane_title = None;
        view.target.tmux_pane_current_command = None;
        let query = StatusQuery::new(Vec::new(), false);
        let activities = workspace_activities_from_sessions(vec![view]);

        let entries = status_entries(&activities, 300, &query, &StatusIcons::default());

        assert_eq!(entries[0].title, None);
        assert_eq!(
            entries[0].display_title.as_deref(),
            Some("session ses_feature")
        );
    }

    fn session_view(
        agent_kind: &str,
        session_id: &str,
        git_branch: &str,
        title: &str,
    ) -> ResolvedAgentSession {
        let workspace_path = format!("/repo/{}", git_branch.replace('/', "-"));
        let key = AgentSessionKey {
            agent_kind: agent_kind.to_owned(),
            session_id: session_id.to_owned(),
        };
        ResolvedAgentSession {
            key,
            workspace: ResolvedAgentWorkspace::from_canonical_root(
                workspace_path.clone().into(),
                workspace_path,
            )
            .expect("test workspace should be valid"),
            tmux_target: AgentTmuxTarget::Windows {
                session_name: "project".to_owned(),
                candidates: vec![crate::agent::sessions::AgentTmuxWindowCandidate {
                    window_id: "@1".to_owned(),
                    pane_ids: Vec::new(),
                }],
            },
            created_at: 100,
            status: AgentStatus::Working,
            status_observed_at: 100,
            status_changed_at: 100,
            working_elapsed_secs: 0,
            observed_at: 100,
            title: Some(title.to_owned()),
            context: None,
            target: ResolvedAgentTarget {
                git_branch: Some(git_branch.to_owned()),
                tmux_session_name: Some("project".to_owned()),
                tmux_window_id: Some("@1".to_owned()),
                tmux_window_name: Some(format!("kmux-{}", git_branch.replace('/', "-"))),
                ..ResolvedAgentTarget::default()
            },
        }
    }
}
