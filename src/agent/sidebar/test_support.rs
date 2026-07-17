use crate::agent::sessions::{
    AgentTmuxTarget, AgentTmuxWindowCandidate, ResolvedAgentSession, ResolvedAgentTarget,
    ResolvedAgentWorkspace,
};
use crate::agent::sidebar::model::{SidebarIcons, SidebarRow, build_rows_with_working_icon};
use crate::agent::workspace_activity::workspace_activity_rows;
use crate::config::{DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS, StatusIcons};
use crate::state::{AgentSessionKey, AgentStatus, StateStore};

/// Sleeping icon used by sidebar tests to assert idle-row rendering.
pub(super) const TEST_SLEEPING_ICON: &str = "z";

/// Build deterministic sidebar icons for tests.
pub(super) fn test_icons() -> SidebarIcons {
    SidebarIcons::from_config(&StatusIcons {
        working: Some("?".to_owned()),
        waiting: Some("?".to_owned()),
        done: Some("?".to_owned()),
        sleeping: Some(TEST_SLEEPING_ICON.to_owned()),
        ..StatusIcons::default()
    })
}

/// Build the first sidebar row generated from a single agent session view.
pub(super) fn row_from_view(view: &ResolvedAgentSession, now: u64) -> SidebarRow {
    let icons = test_icons();
    let activities = workspace_activity_rows(std::slice::from_ref(view), now);
    build_rows_with_working_icon(
        &activities,
        &icons,
        None,
        DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
    )
    .remove(0)
}

/// Build an agent session view with stable tmux, repo, workspace, and pane metadata.
pub(super) fn report_state(
    status: AgentStatus,
    status_changed_at: u64,
    window_id: &str,
    pane_id: &str,
) -> ResolvedAgentSession {
    let key = AgentSessionKey {
        agent_kind: "opencode".to_owned(),
        session_id: format!("ses_{pane_id}"),
    };
    ResolvedAgentSession {
        member_session_keys: vec![key.clone()],
        key,
        workspace: Some(resolved_workspace(format!(
            "/repo__worktrees/feature-sidebar/{window_id}"
        ))),
        tmux_target: AgentTmuxTarget::Windows {
            session_name: "project".to_owned(),
            candidates: vec![AgentTmuxWindowCandidate {
                window_id: window_id.to_owned(),
                pane_ids: vec![pane_id.to_owned()],
            }],
        },
        created_at: status_changed_at,
        status,
        status_observed_at: status_changed_at,
        status_changed_at,
        working_elapsed_secs: 0,
        observed_at: status_changed_at,
        title: None,
        context: None,
        target: ResolvedAgentTarget {
            tmux_instance: Some("test".to_owned()),
            tmux_pane_id: Some(pane_id.to_owned()),
            tmux_window_id: Some(window_id.to_owned()),
            tmux_session_name: Some("project".to_owned()),
            tmux_window_name: Some("kmux-feature-sidebar".to_owned()),
            tmux_pane_title: Some("Implement sidebar".to_owned()),
            tmux_pane_current_command: Some("nvim".to_owned()),
            tmux_pane_current_path: None,
            git_repo_name: Some("kmux".to_owned()),
            git_repo_path: Some("/repo".to_owned()),
            kmux_workspace_slug: Some("feature-sidebar".to_owned()),
            git_worktree_path: Some("/repo__worktrees/feature-sidebar".to_owned()),
            git_branch: Some("feature/sidebar".to_owned()),
            directory: None,
        },
    }
}

/// Replace the primary and sole member session key on a test session view.
pub(super) fn set_session_key(view: &mut ResolvedAgentSession, key: AgentSessionKey) {
    view.member_session_keys = vec![key.clone()];
    view.key = key;
}

/// Replace the resolved workspace identity on a test session view.
pub(super) fn set_workspace(view: &mut ResolvedAgentSession, path: impl ToString) {
    view.workspace = Some(resolved_workspace(path));
}

fn resolved_workspace(path: impl ToString) -> ResolvedAgentWorkspace {
    let path = path.to_string();
    ResolvedAgentWorkspace::from_canonical_root(path.clone().into(), path)
        .expect("test workspace should be valid")
}

/// Build the standard sidebar test agent state.
pub(super) fn agent_state(
    status: AgentStatus,
    status_changed_at: u64,
    window_id: &str,
    pane_id: &str,
) -> ResolvedAgentSession {
    report_state(status, status_changed_at, window_id, pane_id)
}

/// Create an isolated empty state store for sidebar unit tests.
pub(super) fn empty_state_store() -> StateStore {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    crate::state::test_support::store_with_path(std::env::temp_dir().join(format!(
        "kmux-sidebar-test-empty-{}-{nanos}",
        std::process::id()
    )))
    .expect("test state store should be created")
}
