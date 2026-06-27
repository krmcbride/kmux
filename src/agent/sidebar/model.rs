use std::path::Path;

use crate::agent::sessions::AgentSessionView;
use crate::config::StatusIcons;
#[cfg(test)]
use crate::state::AgentLocationHints;
use crate::state::AgentStatus;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidebarIcons {
    working: String,
    waiting: String,
    done: String,
    sleeping: String,
}

impl SidebarIcons {
    pub(super) fn from_config(status_icons: &StatusIcons) -> Self {
        Self {
            working: status_icons.working().to_owned(),
            waiting: status_icons.waiting().to_owned(),
            done: status_icons.done().to_owned(),
            sleeping: status_icons.sleeping().to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidebarRowIdentity {
    agent_kind: String,
    session_id: String,
}

impl SidebarRowIdentity {
    fn from_view(view: &AgentSessionView) -> Self {
        Self {
            agent_kind: view.key.agent_kind.clone(),
            session_id: view.key.session_id.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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

    pub(super) fn is_working(self) -> bool {
        self == Self::Working
    }

    pub(super) fn is_idle(self) -> bool {
        self == Self::Idle
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidebarRow {
    pub(super) identity: SidebarRowIdentity,
    pub(super) status: AgentStatus,
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
}

impl SidebarRow {
    pub(super) fn is_idle(&self) -> bool {
        self.state.is_idle()
    }

    pub(super) fn is_working(&self) -> bool {
        self.state.is_working()
    }
}

impl SidebarRow {
    #[cfg(test)]
    pub(super) fn from_view(view: &AgentSessionView, now: u64) -> Self {
        let icons = test_icons();
        Self::from_view_with_working_icon(
            view,
            now,
            &icons,
            None,
            crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
        )
    }

    fn from_view_with_working_icon(
        view: &AgentSessionView,
        now: u64,
        icons: &SidebarIcons,
        working_icon: Option<&str>,
        idle_after_seconds: u64,
    ) -> Self {
        let target = &view.target;
        let primary = repo_label(view);
        let secondary = branch_label(view, &primary);
        let title = view
            .title
            .as_deref()
            .filter(|title| *title != primary && *title != secondary)
            .or_else(|| {
                target
                    .pane_title
                    .as_deref()
                    .filter(|title| *title != primary && *title != secondary)
            })
            .or(target.pane_current_command.as_deref())
            .map(str::to_owned)
            .or_else(|| fallback_session_title(view, &primary, &secondary))
            .unwrap_or_default();
        let secondary_right = view
            .context
            .as_deref()
            .map(str::trim)
            .filter(|context| !context.is_empty())
            .unwrap_or_default()
            .to_owned();
        let age = now.saturating_sub(view.status_changed_at);
        let state = SidebarRowState::from_status(view.status, age, idle_after_seconds);
        let elapsed = view.elapsed_secs(now);
        let icon = if state.is_idle() {
            icons.sleeping.clone()
        } else {
            match view.status {
                AgentStatus::Working => working_icon.unwrap_or(&icons.working).to_owned(),
                AgentStatus::Waiting => icons.waiting.clone(),
                AgentStatus::Done => icons.done.clone(),
            }
        };

        Self {
            identity: SidebarRowIdentity::from_view(view),
            status: view.status,
            state,
            icon,
            primary,
            secondary,
            secondary_right,
            title,
            elapsed: compact_elapsed(elapsed),
            session_name: target.session_name.clone().unwrap_or_default(),
            window_id: target.window_id.clone().unwrap_or_default(),
            pane_id: target.pane_id.clone(),
        }
    }
}

fn repo_label(view: &AgentSessionView) -> String {
    clean_label(view.target.repo_name.as_deref())
        .or_else(|| path_label(view.target.repo_path.as_deref()))
        .or_else(|| path_label(view.target.directory.as_deref()))
        .or_else(|| path_label(view.target.worktree_path.as_deref()))
        .or_else(|| clean_label(view.target.window_name.as_deref()))
        .unwrap_or_else(|| view.key.session_id.clone())
}

fn branch_label(view: &AgentSessionView, primary: &str) -> String {
    clean_label(view.target.branch.as_deref())
        .or_else(|| distinct_label(view.target.worktree_handle.as_deref(), primary))
        .or_else(|| path_distinct_label(view.target.directory.as_deref(), primary))
        .or_else(|| path_distinct_label(view.target.worktree_path.as_deref(), primary))
        .or_else(|| distinct_label(view.target.window_name.as_deref(), primary))
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

fn fallback_session_label(view: &AgentSessionView, primary: &str) -> Option<String> {
    let label = compact_session_id(&view.key.session_id).to_owned();
    (label != primary).then_some(label)
}

#[cfg(test)]
pub(super) fn build_rows(
    views: &[AgentSessionView],
    now: u64,
    idle_after_seconds: u64,
) -> Vec<SidebarRow> {
    let icons = test_icons();
    build_rows_with_working_icon(views, now, &icons, None, idle_after_seconds)
}

pub(super) fn build_rows_with_working_icon(
    views: &[AgentSessionView],
    now: u64,
    icons: &SidebarIcons,
    working_icon: Option<&str>,
    idle_after_seconds: u64,
) -> Vec<SidebarRow> {
    views
        .iter()
        .map(|view| {
            SidebarRow::from_view_with_working_icon(
                view,
                now,
                icons,
                working_icon,
                idle_after_seconds,
            )
        })
        .collect()
}

pub(super) fn row_index_by_window(rows: &[SidebarRow], window_id: &str) -> Option<usize> {
    rows.iter().position(|row| row.window_id == window_id)
}

pub(super) fn row_index_by_identity(
    rows: &[SidebarRow],
    identity: &SidebarRowIdentity,
) -> Option<usize> {
    rows.iter().position(|row| &row.identity == identity)
}

pub(super) fn row_index_by_pane(rows: &[SidebarRow], pane_id: &str) -> Option<usize> {
    rows.iter()
        .position(|row| row.pane_id.as_deref() == Some(pane_id))
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

fn fallback_session_title(
    view: &AgentSessionView,
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
pub(super) const TEST_SLEEPING_ICON: &str = "z";

#[cfg(test)]
pub(super) fn test_icons() -> SidebarIcons {
    SidebarIcons {
        working: "?".to_owned(),
        waiting: "?".to_owned(),
        done: "?".to_owned(),
        sleeping: TEST_SLEEPING_ICON.to_owned(),
    }
}

#[cfg(test)]
pub(super) fn report_state(
    status: AgentStatus,
    status_changed_at: u64,
    window_id: &str,
    pane_id: &str,
) -> AgentSessionView {
    AgentSessionView {
        key: crate::state::AgentSessionKey {
            agent_kind: "opencode".to_owned(),
            session_id: format!("ses_{pane_id}"),
        },
        status,
        status_changed_at,
        working_elapsed_secs: 0,
        observed_at: status_changed_at,
        title: None,
        context: None,
        target: AgentLocationHints {
            tmux_instance: Some("test".to_owned()),
            pane_id: Some(pane_id.to_owned()),
            window_id: Some(window_id.to_owned()),
            session_name: Some("project".to_owned()),
            window_name: Some("kmux-feature-sidebar".to_owned()),
            pane_title: Some("Implement sidebar".to_owned()),
            pane_current_command: Some("nvim".to_owned()),
            pane_current_path: None,
            repo_name: Some("kmux".to_owned()),
            repo_path: Some("/repo".to_owned()),
            worktree_handle: Some("feature-sidebar".to_owned()),
            worktree_path: Some("/repo__worktrees/feature-sidebar".to_owned()),
            branch: Some("feature/sidebar".to_owned()),
            directory: None,
        },
    }
}

#[cfg(test)]
pub(super) fn agent_state(
    status: AgentStatus,
    status_changed_at: u64,
    window_id: &str,
    pane_id: &str,
) -> AgentSessionView {
    report_state(status, status_changed_at, window_id, pane_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS;

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

        assert_eq!(rows[0].state, SidebarRowState::Working);
        assert_eq!(rows[1].state, SidebarRowState::Waiting);
        assert_eq!(rows[2].state, SidebarRowState::Done);
        assert_eq!(rows[3].state, SidebarRowState::Idle);
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
            &[
                report_state(AgentStatus::Working, 120, "@1", "%1"),
                report_state(AgentStatus::Waiting, 120, "@2", "%2"),
            ],
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
    fn row_model_keeps_multiple_sessions_for_one_window_distinguishable() {
        let mut first = report_state(AgentStatus::Working, 120, "@1", "%1");
        first.key = crate::state::AgentSessionKey {
            agent_kind: "opencode".to_owned(),
            session_id: "ses_first".to_owned(),
        };
        first.target.pane_id = None;
        first.title = Some("Implement sidebar rows".to_owned());

        let mut second = report_state(AgentStatus::Waiting, 120, "@1", "%2");
        second.key = crate::state::AgentSessionKey {
            agent_kind: "opencode".to_owned(),
            session_id: "ses_second".to_owned(),
        };
        second.target.pane_id = None;
        second.title = None;
        second.target.pane_title = None;
        second.target.pane_current_command = None;

        let rows = build_rows(&[first, second], 300, 1_800);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].primary, "kmux");
        assert_eq!(rows[1].primary, "kmux");
        assert_eq!(rows[0].secondary, "feature/sidebar");
        assert_eq!(rows[1].secondary, "feature/sidebar");
        assert_eq!(rows[0].title, "Implement sidebar rows");
        assert_eq!(rows[1].title, "session ses_second");
        assert_ne!(rows[0].identity, rows[1].identity);
    }

    #[test]
    fn row_model_falls_back_to_branch_worktree_and_window_without_tmux_session_label() {
        let mut report = report_state(AgentStatus::Working, 120, "@1", "%1");
        report.target.repo_name = None;
        report.target.repo_path = None;
        report.target.branch = None;

        let rows = build_rows(&[report], 300, 1_800);

        assert_eq!(rows[0].primary, "feature-sidebar");
        assert_eq!(rows[0].secondary, "kmux-feature-sidebar");
    }
}
