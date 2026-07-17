//! Shared workspace activity rows used by sidebar and status views.
//!
//! Agent observations resolve to one primary session per Git workspace. This
//! module owns the UI-neutral labels, timing, primary-session display data, and
//! member-session action data that the sidebar TUI and `kmux status` consume.

use std::path::Path;

use crate::agent::sessions::{AgentTmuxTarget, ResolvedAgentSession, ResolvedAgentTarget};
use crate::state::{AgentSessionKey, AgentStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
/// UI-neutral activity row for one workspace with a primary agent session.
pub struct WorkspaceActivityRow {
    pub workspace_key: String,
    pub session: AgentSessionKey,
    pub member_session_keys: Vec<AgentSessionKey>,
    pub tmux_target: AgentTmuxTarget,
    pub created_at: u64,
    pub status: AgentStatus,
    pub status_age_secs: u64,
    pub elapsed_secs: u64,
    pub primary: String,
    pub secondary: String,
    pub display_title: String,
    pub display_context: String,
    pub title: Option<String>,
    pub context: Option<String>,
    pub workspace_slug: Option<String>,
    pub git_worktree_path: Option<String>,
    pub directory: Option<String>,
    pub target: ResolvedAgentTarget,
}

impl WorkspaceActivityRow {
    fn from_session(view: &ResolvedAgentSession, now: u64) -> Option<Self> {
        let workspace_key = view.workspace_key()?.to_owned();
        let primary = repo_label(view);
        let secondary = branch_label(view, &primary);
        let display_title = view
            .title
            .as_deref()
            .filter(|title| *title != primary && *title != secondary)
            .or_else(|| {
                view.tmux_pane_title()
                    .filter(|title| *title != primary && *title != secondary)
            })
            .or_else(|| view.tmux_pane_current_command())
            .map(str::to_owned)
            .or_else(|| fallback_session_title(view, &primary, &secondary))
            .unwrap_or_default();
        let display_context = view
            .context
            .as_deref()
            .map(str::trim)
            .filter(|context| !context.is_empty())
            .unwrap_or_default()
            .to_owned();

        Some(Self {
            workspace_key,
            session: view.key.clone(),
            member_session_keys: view.member_session_keys.clone(),
            tmux_target: view.tmux_target,
            created_at: view.created_at,
            status: view.status,
            status_age_secs: now.saturating_sub(view.status_changed_at),
            elapsed_secs: view.elapsed_secs(now),
            primary,
            secondary,
            display_title,
            display_context,
            title: view.title.clone(),
            context: view.context.clone(),
            workspace_slug: view
                .kmux_workspace_slug()
                .map(str::to_owned)
                .or_else(|| path_label(view.git_worktree_path())),
            git_worktree_path: view.git_worktree_path().map(str::to_owned),
            directory: view.directory().map(str::to_owned),
            target: view.target.clone(),
        })
    }

    /// Return whether this row matches a user-supplied status filter.
    pub fn matches_filter(&self, filter: &str) -> bool {
        self.session.agent_kind == filter
            || self.session.session_id == filter
            || self.workspace_key == filter
            || self.primary == filter
            || self.secondary == filter
            || self.workspace_slug() == Some(filter)
            || self.git_branch() == Some(filter)
            || self.tmux_window_name() == Some(filter)
            || self.git_worktree_path() == Some(filter)
            || self.directory() == Some(filter)
            || self.title.as_deref() == Some(filter)
            || self.display_title == filter
    }

    pub fn workspace_slug(&self) -> Option<&str> {
        self.workspace_slug.as_deref()
    }

    pub fn git_branch(&self) -> Option<&str> {
        self.target.git_branch.as_deref()
    }

    pub fn git_worktree_path(&self) -> Option<&str> {
        self.git_worktree_path.as_deref()
    }

    pub fn directory(&self) -> Option<&str> {
        self.directory.as_deref()
    }

    pub fn tmux_session_name(&self) -> Option<&str> {
        self.target.tmux_session_name.as_deref()
    }

    pub fn tmux_window_name(&self) -> Option<&str> {
        self.target.tmux_window_name.as_deref()
    }

    pub fn tmux_window_id(&self) -> Option<&str> {
        self.target.tmux_window_id.as_deref()
    }

    pub fn tmux_pane_id(&self) -> Option<&str> {
        self.target.tmux_pane_id.as_deref()
    }
}

/// Build sorted workspace activity rows from resolved agent sessions.
pub fn workspace_activity_rows(
    views: &[ResolvedAgentSession],
    now: u64,
) -> Vec<WorkspaceActivityRow> {
    let mut rows = views
        .iter()
        .filter_map(|view| WorkspaceActivityRow::from_session(view, now))
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        (
            &left.primary,
            &left.secondary,
            left.created_at,
            &left.workspace_key,
            &left.session.agent_kind,
            &left.session.session_id,
        )
            .cmp(&(
                &right.primary,
                &right.secondary,
                right.created_at,
                &right.workspace_key,
                &right.session.agent_kind,
                &right.session.session_id,
            ))
    });
    rows
}

// Primary label should be stable and repo-oriented; fall back through paths,
// tmux window name, and finally session id.
fn repo_label(view: &ResolvedAgentSession) -> String {
    clean_label(view.git_repo_name())
        .or_else(|| path_label(view.git_repo_path()))
        .or_else(|| path_label(view.directory()))
        .or_else(|| path_label(view.git_worktree_path()))
        .or_else(|| clean_label(view.tmux_window_name()))
        .unwrap_or_else(|| view.key.session_id.clone())
}

// Secondary label should add distinguishing context without repeating primary.
fn branch_label(view: &ResolvedAgentSession, primary: &str) -> String {
    clean_label(view.git_branch())
        .or_else(|| distinct_label(view.kmux_workspace_slug(), primary))
        .or_else(|| path_distinct_label(view.directory(), primary))
        .or_else(|| path_distinct_label(view.git_worktree_path(), primary))
        .or_else(|| distinct_label(view.tmux_window_name(), primary))
        .or_else(|| fallback_session_label(view, primary))
        .unwrap_or_default()
}

fn clean_label(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn distinct_label(value: Option<&str>, primary: &str) -> Option<String> {
    clean_label(value).filter(|value| value != primary)
}

fn path_label(value: Option<&str>) -> Option<String> {
    clean_label(value).and_then(|value| {
        Path::new(&value)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
    })
}

fn path_distinct_label(value: Option<&str>, primary: &str) -> Option<String> {
    path_label(value).filter(|value| value != primary)
}

fn fallback_session_label(view: &ResolvedAgentSession, primary: &str) -> Option<String> {
    let label = compact_session_id(&view.key.session_id).to_owned();
    (label != primary).then_some(label)
}

fn fallback_session_title(
    view: &ResolvedAgentSession,
    primary: &str,
    secondary: &str,
) -> Option<String> {
    let label = format!("session {}", compact_session_id(&view.key.session_id));
    (label != primary && label != secondary).then_some(label)
}

fn compact_session_id(session_id: &str) -> &str {
    session_id.get(..12).unwrap_or(session_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sessions::{AgentTmuxTarget, ResolvedAgentWorkspace};

    #[test]
    fn activity_rows_use_repo_branch_title_and_context_labels() {
        let view = session_view();

        let rows = workspace_activity_rows(&[view], 300);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "kmux");
        assert_eq!(rows[0].secondary, "feature/sidebar");
        assert_eq!(rows[0].display_title, "Implement shared rows");
        assert_eq!(rows[0].display_context, "55.2K");
        assert!(rows[0].matches_filter("feature/sidebar"));
        assert!(rows[0].matches_filter("/repo/project-alpha"));
    }

    fn session_view() -> ResolvedAgentSession {
        let key = AgentSessionKey {
            agent_kind: "opencode".to_owned(),
            session_id: "ses_feature_sidebar".to_owned(),
        };
        ResolvedAgentSession {
            member_session_keys: vec![key.clone()],
            key,
            workspace: Some(
                ResolvedAgentWorkspace::from_canonical_root(
                    "/repo/project-alpha".into(),
                    "/repo/project-alpha".to_owned(),
                )
                .expect("test workspace should be valid"),
            ),
            tmux_target: AgentTmuxTarget::Window,
            created_at: 120,
            status: AgentStatus::Working,
            status_observed_at: 120,
            status_changed_at: 120,
            working_elapsed_secs: 0,
            observed_at: 120,
            title: Some("Implement shared rows".to_owned()),
            context: Some("55.2K".to_owned()),
            target: ResolvedAgentTarget {
                git_repo_name: Some("kmux".to_owned()),
                git_repo_path: Some("/repo/kmux".to_owned()),
                git_branch: Some("feature/sidebar".to_owned()),
                kmux_workspace_slug: Some("feature-sidebar".to_owned()),
                git_worktree_path: Some("/repo/project-alpha".to_owned()),
                tmux_session_name: Some("project-alpha".to_owned()),
                tmux_window_id: Some("@1".to_owned()),
                tmux_window_name: Some("kmux-feature-sidebar".to_owned()),
                tmux_pane_id: Some("%1".to_owned()),
                tmux_pane_title: Some("Implement sidebar".to_owned()),
                ..ResolvedAgentTarget::default()
            },
        }
    }
}
