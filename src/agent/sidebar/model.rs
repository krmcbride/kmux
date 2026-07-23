//! View model construction for sidebar rows.
//!
//! This module turns shared workspace activity aggregates into display-ready sidebar
//! rows with stable identities, status-derived icons, elapsed-time labels, and
//! compact primary/secondary text for the renderer.

use crate::agent::sessions::AgentTmuxTarget;
use crate::agent::workspace_activity::WorkspaceActivity;
use crate::config::StatusIcons;
use crate::state::{AgentSessionKey, AgentStatus};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Icon set precomputed for rendering sidebar rows.
pub(super) struct SidebarIcons {
    working: String,
    waiting: String,
    done: String,
    sleeping: String,
}

impl SidebarIcons {
    /// Capture configured status icons into the sidebar row model.
    pub(super) fn from_config(status_icons: &StatusIcons) -> Self {
        Self {
            working: status_icons.working().to_owned(),
            waiting: status_icons.waiting().to_owned(),
            done: status_icons.done().to_owned(),
            sleeping: status_icons.sleeping().to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Stable workspace identity for a sidebar row.
pub(super) struct SidebarRowIdentity {
    key: String,
}

impl SidebarRowIdentity {
    /// Return whether this decoded identity is valid for selection state.
    pub(super) fn is_valid(&self) -> bool {
        self.key
            .strip_prefix("workspace:")
            .is_some_and(|key| !key.trim().is_empty())
    }

    fn from_activity(row: &WorkspaceActivity) -> Self {
        Self {
            key: format!("workspace:{}", row.workspace_key()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Non-display workspace data carried with a row for sidebar actions.
pub(super) struct SidebarRowSelection {
    pub(super) workspace_key: String,
    pub(super) member_session_keys: Vec<AgentSessionKey>,
}

impl SidebarRowSelection {
    fn from_activity(row: &WorkspaceActivity) -> Self {
        Self {
            workspace_key: row.workspace_key().to_owned(),
            member_session_keys: row.member_session_keys().to_vec(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Display state derived from agent status and idle age.
pub(super) enum SidebarRowState {
    Working,
    Waiting,
    Done,
    Idle,
}

impl SidebarRowState {
    fn from_status(status: AgentStatus, age_seconds: u64, idle_after_seconds: u64) -> Self {
        match status {
            AgentStatus::Working => Self::Working,
            AgentStatus::Waiting => Self::Waiting,
            AgentStatus::Done if age_seconds >= idle_after_seconds => Self::Idle,
            AgentStatus::Done => Self::Done,
        }
    }

    fn is_working(self) -> bool {
        self == Self::Working
    }

    fn is_idle(self) -> bool {
        self == Self::Idle
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Presentation-ready workspace row rendered by the sidebar TUI.
pub(super) struct SidebarRow {
    pub(super) identity: SidebarRowIdentity,
    pub(super) selection: SidebarRowSelection,
    pub(super) state: SidebarRowState,
    pub(super) icon: String,
    pub(super) primary: String,
    pub(super) secondary: String,
    pub(super) secondary_right: String,
    pub(super) title: String,
    pub(super) elapsed: String,
    pub(super) session_name: String,
    pub(super) window_id: String,
    pub(super) pane_id: Option<String>,
    pub(super) jump_target: AgentTmuxTarget,
}

impl SidebarRow {
    /// Return whether this row is an old completed agent shown in the idle style.
    pub(super) fn is_idle(&self) -> bool {
        self.state.is_idle()
    }

    /// Return whether this row is currently working and should use spinner frames.
    pub(super) fn is_working(&self) -> bool {
        self.state.is_working()
    }
}

impl SidebarRow {
    fn from_activity_with_working_icon(
        row: &WorkspaceActivity,
        now: u64,
        icons: &SidebarIcons,
        working_icon: Option<&str>,
        idle_after_seconds: u64,
    ) -> Self {
        let identity = SidebarRowIdentity::from_activity(row);
        let status = row.status();
        let state =
            SidebarRowState::from_status(status, row.status_age_secs(now), idle_after_seconds);
        let icon = if state.is_idle() {
            icons.sleeping.clone()
        } else {
            match status {
                AgentStatus::Working => working_icon.unwrap_or(&icons.working).to_owned(),
                AgentStatus::Waiting => icons.waiting.clone(),
                AgentStatus::Done => icons.done.clone(),
            }
        };

        let session_name = row.tmux_session_name().unwrap_or_default().to_owned();
        let window_id = row.tmux_window_id().unwrap_or_default().to_owned();
        let pane_id = row.tmux_pane_id().map(str::to_owned);
        let jump_target = row.tmux_target().clone();

        Self {
            identity,
            selection: SidebarRowSelection::from_activity(row),
            state,
            icon,
            primary: row.primary.clone(),
            secondary: row.secondary.clone(),
            secondary_right: row.display_context.clone(),
            title: row.display_title.clone(),
            elapsed: format_elapsed(row.elapsed_secs(now), state),
            session_name,
            window_id,
            pane_id,
            jump_target,
        }
    }
}

/// Build sidebar rows from sorted workspace activity aggregates.
pub(super) fn build_rows_with_working_icon(
    activities: &[WorkspaceActivity],
    now: u64,
    icons: &SidebarIcons,
    working_icon: Option<&str>,
    idle_after_seconds: u64,
) -> Vec<SidebarRow> {
    activities
        .iter()
        .map(|activity| {
            SidebarRow::from_activity_with_working_icon(
                activity,
                now,
                icons,
                working_icon,
                idle_after_seconds,
            )
        })
        .collect()
}

/// Return the row index associated with a tmux window id.
pub(super) fn row_index_by_window(rows: &[SidebarRow], window_id: &str) -> Option<usize> {
    rows.iter().position(|row| row.window_id == window_id)
}

/// Return the row index associated with a stable workspace row identity.
pub(super) fn row_index_by_identity(
    rows: &[SidebarRow],
    identity: &SidebarRowIdentity,
) -> Option<usize> {
    rows.iter().position(|row| &row.identity == identity)
}

fn format_elapsed(seconds: u64, state: SidebarRowState) -> String {
    if state.is_idle() {
        compact_elapsed(seconds)
    } else {
        detailed_elapsed(seconds)
    }
}

fn compact_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        "<1m".to_owned()
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h", seconds / (60 * 60))
    } else {
        format!("{}d", seconds / (60 * 60 * 24))
    }
}

fn detailed_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        "<1m".to_owned()
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        let hours = seconds / (60 * 60);
        let minutes = (seconds % (60 * 60)) / 60;
        format!("{hours}h{minutes}m")
    } else {
        let days = seconds / (60 * 60 * 24);
        let hours = (seconds % (60 * 60 * 24)) / (60 * 60);
        let minutes = (seconds % (60 * 60)) / 60;
        format!("{days}d{hours}h{minutes}m")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sessions::ResolvedAgentSession;
    use crate::agent::sidebar::test_support::{
        TEST_SLEEPING_ICON, report_state, set_session_key, set_workspace, test_icons,
    };
    use crate::agent::workspace_activity::workspace_activities_from_sessions;
    use crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS;

    fn build_rows(
        views: &[ResolvedAgentSession],
        now: u64,
        idle_after_seconds: u64,
    ) -> Vec<SidebarRow> {
        let icons = test_icons();
        let activities = workspace_activities_from_sessions(views.to_vec());
        build_rows_with_working_icon(&activities, now, &icons, None, idle_after_seconds)
    }

    #[test]
    fn non_idle_elapsed_includes_available_lower_units() {
        for state in [
            SidebarRowState::Working,
            SidebarRowState::Waiting,
            SidebarRowState::Done,
        ] {
            assert_eq!(format_elapsed(60 * 60, state), "1h0m");
            assert_eq!(format_elapsed(70 * 60, state), "1h10m");
            assert_eq!(format_elapsed(24 * 60 * 60, state), "1d0h0m");
            assert_eq!(format_elapsed(27 * 60 * 60 + 7 * 60, state), "1d3h7m");
        }
    }

    #[test]
    fn idle_elapsed_uses_compact_largest_unit() {
        assert_eq!(format_elapsed(30 * 60, SidebarRowState::Idle), "30m");
        assert_eq!(format_elapsed(70 * 60, SidebarRowState::Idle), "1h");
        assert_eq!(
            format_elapsed(27 * 60 * 60 + 7 * 60, SidebarRowState::Idle),
            "1d"
        );
    }

    #[test]
    fn row_model_uses_repo_and_branch_labels_and_marks_old_done_idle() {
        let reports = vec![report_state(AgentStatus::Done, 0, "@1", "%1")];
        let rows = build_rows(
            &reports,
            DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS + 1,
            DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
        );

        assert_eq!(rows[0].primary, "kmux");
        assert_eq!(rows[0].secondary, "feature/sidebar");
        assert_eq!(rows[0].title, "Implement sidebar");
        assert_eq!(rows[0].elapsed, "30m");
        assert_eq!(rows[0].icon, TEST_SLEEPING_ICON);
        assert_eq!(rows[0].state, SidebarRowState::Idle);
        assert!(rows[0].is_idle());
    }

    #[test]
    fn row_model_keeps_old_waiting_agent_active() {
        let reports = vec![report_state(AgentStatus::Waiting, 0, "@1", "%1")];
        let rows = build_rows(
            &reports,
            DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS + 1,
            DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
        );

        assert_eq!(rows[0].icon, "?");
        assert_eq!(rows[0].state, SidebarRowState::Waiting);
        assert!(!rows[0].is_idle());
    }

    #[test]
    fn row_model_derives_all_display_states() {
        let reports = [
            report_state(AgentStatus::Working, 0, "@1", "%1"),
            report_state(AgentStatus::Waiting, 0, "@2", "%2"),
            report_state(AgentStatus::Done, 100, "@3", "%3"),
            report_state(AgentStatus::Done, 0, "@4", "%4"),
        ];

        let rows = build_rows(&reports, 300, 300);

        assert!(rows.iter().any(|row| row.state == SidebarRowState::Working));
        assert!(rows.iter().any(|row| row.state == SidebarRowState::Waiting));
        assert!(rows.iter().any(|row| row.state == SidebarRowState::Done));
        assert!(rows.iter().any(|row| row.state == SidebarRowState::Idle));
    }

    #[test]
    fn row_model_uses_configured_idle_threshold() {
        let reports = vec![report_state(AgentStatus::Done, 0, "@1", "%1")];

        let active_rows = build_rows(&reports, 1_799, 1_800);
        let idle_rows = build_rows(&reports, 1_800, 1_800);

        assert_eq!(active_rows[0].state, SidebarRowState::Done);
        assert!(!active_rows[0].is_idle());
        assert_eq!(idle_rows[0].state, SidebarRowState::Idle);
        assert!(idle_rows[0].is_idle());
    }

    #[test]
    fn row_model_prefers_report_title_and_context() {
        let mut report = report_state(AgentStatus::Working, 120, "@1", "%1");
        report.title = Some("Implement richer sidebar".to_owned());
        report.context = Some("163.2K (41%)".to_owned());
        let rows = build_rows(&[report], 300, 1_800);

        assert_eq!(rows[0].title, "Implement richer sidebar");
        assert_eq!(rows[0].secondary_right, "163.2K (41%)");
    }

    #[test]
    fn row_model_omits_secondary_right_when_context_is_absent() {
        let rows = build_rows(
            &[report_state(AgentStatus::Working, 120, "@1", "%1")],
            300,
            1_800,
        );

        assert_eq!(rows[0].secondary_right, "");
    }

    #[test]
    fn row_model_uses_working_frame_only_for_working_rows() {
        let rows = build_rows_with_working_icon(
            &workspace_activities_from_sessions(vec![
                report_state(AgentStatus::Working, 120, "@1", "%1"),
                report_state(AgentStatus::Waiting, 120, "@2", "%2"),
            ]),
            300,
            &test_icons(),
            Some("a"),
            1_800,
        );

        assert_eq!(rows[0].icon, "a");
        assert_eq!(rows[1].icon, "?");
    }

    #[test]
    fn row_model_uses_accumulated_working_elapsed_for_working_rows() {
        let mut report = report_state(AgentStatus::Working, 600, "@1", "%1");
        report.working_elapsed_secs = 20 * 60;

        let rows = build_rows(&[report], 15 * 60, 1_800);

        assert_eq!(rows[0].state, SidebarRowState::Working);
        assert_eq!(rows[0].elapsed, "25m");
    }

    #[test]
    fn row_model_uses_workspace_identity_across_primary_session_changes() {
        let mut first = report_state(AgentStatus::Working, 120, "@1", "%1");
        set_session_key(
            &mut first,
            AgentSessionKey {
                agent_kind: "opencode".to_owned(),
                session_id: "ses_first".to_owned(),
            },
        );
        first.target.tmux_pane_id = None;
        first.title = Some("Implement sidebar rows".to_owned());

        let mut second = report_state(AgentStatus::Waiting, 120, "@1", "%2");
        set_session_key(
            &mut second,
            AgentSessionKey {
                agent_kind: "opencode".to_owned(),
                session_id: "ses_second".to_owned(),
            },
        );
        second.target.tmux_pane_id = None;
        second.title = None;
        second.target.tmux_pane_title = None;
        second.target.tmux_pane_current_command = None;

        let first_rows = build_rows(&[first], 300, 1_800);
        let second_rows = build_rows(&[second], 300, 1_800);

        assert_eq!(first_rows[0].primary, "kmux");
        assert_eq!(second_rows[0].primary, "kmux");
        assert_eq!(first_rows[0].secondary, "feature/sidebar");
        assert_eq!(second_rows[0].secondary, "feature/sidebar");
        assert_eq!(first_rows[0].title, "Implement sidebar rows");
        assert_eq!(second_rows[0].title, "session ses_second");
        assert_eq!(first_rows[0].identity, second_rows[0].identity);
    }

    #[test]
    fn row_selection_carries_workspace_identity_and_all_member_sessions() {
        let report = report_state(AgentStatus::Waiting, 120, "@1", "%1");
        let mut companion = report_state(AgentStatus::Done, 120, "@1", "%2");
        set_session_key(
            &mut companion,
            AgentSessionKey {
                agent_kind: "codex".to_owned(),
                session_id: "ses_secondary".to_owned(),
            },
        );

        let rows = build_rows(&[report, companion], 300, 1_800);

        assert_eq!(
            rows[0].selection.workspace_key,
            "/repo__worktrees/feature-sidebar/@1"
        );
        assert_eq!(rows[0].selection.member_session_keys.len(), 2);
        assert_eq!(rows[0].selection.member_session_keys[0].agent_kind, "codex");
        assert_eq!(
            rows[0].selection.member_session_keys[1].agent_kind,
            "opencode"
        );
    }

    #[test]
    fn row_model_sorts_by_primary_secondary_and_creation_time() {
        let mut kmux_old = report_state(AgentStatus::Working, 100, "@1", "%1");
        set_session_key(
            &mut kmux_old,
            AgentSessionKey {
                agent_kind: "opencode".to_owned(),
                session_id: "ses_kmux_old".to_owned(),
            },
        );
        kmux_old.target.git_repo_name = Some("kmux".to_owned());
        kmux_old.target.git_branch = Some("master".to_owned());
        kmux_old.title = Some("kmux old".to_owned());

        let mut alpha_tools = report_state(AgentStatus::Working, 200, "@2", "%2");
        set_session_key(
            &mut alpha_tools,
            AgentSessionKey {
                agent_kind: "opencode".to_owned(),
                session_id: "ses_alpha_tools".to_owned(),
            },
        );
        alpha_tools.target.git_repo_name = Some("alpha-tools".to_owned());
        alpha_tools.target.git_branch = Some("master".to_owned());
        alpha_tools.title = Some("alpha tools".to_owned());

        let mut kmux_feature = report_state(AgentStatus::Working, 50, "@3", "%3");
        set_session_key(
            &mut kmux_feature,
            AgentSessionKey {
                agent_kind: "opencode".to_owned(),
                session_id: "ses_kmux_feature".to_owned(),
            },
        );
        kmux_feature.target.git_repo_name = Some("kmux".to_owned());
        kmux_feature.target.git_branch = Some("feature/sidebar".to_owned());
        kmux_feature.title = Some("kmux feature".to_owned());

        let mut kmux_new = report_state(AgentStatus::Working, 300, "@4", "%4");
        set_session_key(
            &mut kmux_new,
            AgentSessionKey {
                agent_kind: "opencode".to_owned(),
                session_id: "ses_kmux_new".to_owned(),
            },
        );
        kmux_new.target.git_repo_name = Some("kmux".to_owned());
        kmux_new.target.git_branch = Some("master".to_owned());
        kmux_new.title = Some("kmux new".to_owned());

        let rows = build_rows(&[kmux_new, kmux_feature, alpha_tools, kmux_old], 400, 1_800);

        assert_eq!(
            rows.iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>(),
            ["alpha tools", "kmux feature", "kmux old", "kmux new"]
        );
    }

    #[test]
    fn row_model_falls_back_to_branch_worktree_and_window_without_tmux_session_label() {
        let mut report = report_state(AgentStatus::Working, 120, "@1", "%1");
        report.target.git_repo_name = None;
        report.target.git_repo_path = None;
        report.target.git_branch = None;
        set_workspace(&mut report, "/repo__worktrees/feature-sidebar");

        let rows = build_rows(&[report], 300, 1_800);

        assert_eq!(rows[0].primary, "feature-sidebar");
        assert_eq!(rows[0].secondary, "kmux-feature-sidebar");
    }
}
