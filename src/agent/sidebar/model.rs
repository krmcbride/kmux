use crate::config::StatusIcons;
use crate::state::{AgentReportState, AgentStatus};

#[cfg(test)]
use crate::state::{AgentReportKey, AgentTargetHints};

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
    source: String,
    instance: String,
    id: String,
}

impl SidebarRowIdentity {
    fn from_report(report: &AgentReportState) -> Self {
        Self {
            source: report.key.source.clone(),
            instance: report.key.instance.clone(),
            id: report.key.id.clone(),
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
    pub(super) fn from_report(report: &AgentReportState, now: u64) -> Self {
        let icons = test_icons();
        Self::from_report_with_working_icon(
            report,
            now,
            &icons,
            None,
            crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
        )
    }

    #[cfg(test)]
    pub(super) fn from_agent(report: &AgentReportState, now: u64, _sleeping_icon: &str) -> Self {
        Self::from_report(report, now)
    }

    fn from_report_with_working_icon(
        report: &AgentReportState,
        now: u64,
        icons: &SidebarIcons,
        working_icon: Option<&str>,
        idle_after_seconds: u64,
    ) -> Self {
        let target = &report.target;
        let primary = target
            .worktree_handle
            .as_deref()
            .or(target.branch.as_deref())
            .or(target.window_name.as_deref())
            .unwrap_or(&report.key.id)
            .to_owned();
        let secondary = secondary_label(report, &primary);
        let title = report
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
            .or_else(|| fallback_session_title(report, &primary, &secondary))
            .unwrap_or_default();
        let secondary_right = report
            .context
            .as_deref()
            .map(str::trim)
            .filter(|context| !context.is_empty())
            .unwrap_or_default()
            .to_owned();
        let age = now.saturating_sub(report.status_changed_at);
        let state = SidebarRowState::from_status(report.status, age, idle_after_seconds);
        let icon = if state.is_idle() {
            icons.sleeping.clone()
        } else {
            match report.status {
                AgentStatus::Working => working_icon.unwrap_or(&icons.working).to_owned(),
                AgentStatus::Waiting => icons.waiting.clone(),
                AgentStatus::Done => icons.done.clone(),
            }
        };

        Self {
            identity: SidebarRowIdentity::from_report(report),
            status: report.status,
            state,
            icon,
            primary,
            secondary,
            secondary_right,
            title,
            elapsed: compact_elapsed(age),
            session_name: target.session_name.clone().unwrap_or_default(),
            window_id: target.window_id.clone().unwrap_or_default(),
            pane_id: target.pane_id.clone(),
        }
    }
}

fn secondary_label(report: &AgentReportState, primary: &str) -> String {
    let session = report.target.session_name.as_deref().unwrap_or_default();
    match report
        .target
        .branch
        .as_deref()
        .filter(|branch| *branch != primary)
    {
        Some(branch) if !session.is_empty() => format!("{session} / {branch}"),
        Some(branch) => branch.to_owned(),
        None => session.to_owned(),
    }
}

#[cfg(test)]
pub(super) fn build_rows(
    reports: &[AgentReportState],
    now: u64,
    idle_after_seconds: u64,
) -> Vec<SidebarRow> {
    let icons = test_icons();
    build_rows_with_working_icon(reports, now, &icons, None, idle_after_seconds)
}

pub(super) fn build_rows_with_working_icon(
    reports: &[AgentReportState],
    now: u64,
    icons: &SidebarIcons,
    working_icon: Option<&str>,
    idle_after_seconds: u64,
) -> Vec<SidebarRow> {
    reports
        .iter()
        .map(|report| {
            SidebarRow::from_report_with_working_icon(
                report,
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
    report: &AgentReportState,
    primary: &str,
    secondary: &str,
) -> Option<String> {
    let session_id = report.session_id.as_deref().or_else(|| {
        report
            .target
            .pane_id
            .is_none()
            .then_some(report.key.id.as_str())
    })?;
    let label = format!("session {}", compact_session_id(session_id));
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
) -> AgentReportState {
    AgentReportState {
        key: AgentReportKey::tmux_pane("test", pane_id),
        session_id: None,
        status,
        status_changed_at,
        observed_at: status_changed_at,
        title: None,
        context: None,
        target: AgentTargetHints {
            tmux_instance: Some("test".to_owned()),
            pane_id: Some(pane_id.to_owned()),
            window_id: Some(window_id.to_owned()),
            session_name: Some("project".to_owned()),
            window_name: Some("kmux-feature-sidebar".to_owned()),
            pane_title: Some("Implement sidebar".to_owned()),
            pane_current_command: Some("nvim".to_owned()),
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
) -> AgentReportState {
    report_state(status, status_changed_at, window_id, pane_id)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS;

    #[test]
    fn row_model_prefers_worktree_and_marks_old_done_idle() {
        let reports = vec![report_state(AgentStatus::Done, 0, "@1", "%1")];
        let rows = build_rows(
            &reports,
            DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS + 1,
            DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
        );

        assert_eq!(rows[0].primary, "feature-sidebar");
        assert_eq!(rows[0].secondary, "project / feature/sidebar");
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
    fn row_model_keeps_multiple_sessions_for_one_window_distinguishable() {
        let mut first = report_state(AgentStatus::Working, 120, "@1", "%1");
        first.key = AgentReportKey::new("opencode-server", "server", "ses_first");
        first.session_id = Some("ses_first".to_owned());
        first.target.pane_id = None;
        first.title = Some("Implement sidebar rows".to_owned());

        let mut second = report_state(AgentStatus::Waiting, 120, "@1", "%2");
        second.key = AgentReportKey::new("opencode-server", "server", "ses_second");
        second.session_id = Some("ses_second".to_owned());
        second.target.pane_id = None;
        second.title = None;
        second.target.pane_title = None;
        second.target.pane_current_command = None;

        let rows = build_rows(&[first, second], 300, 1_800);

        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].primary, "feature-sidebar");
        assert_eq!(rows[1].primary, "feature-sidebar");
        assert_eq!(rows[0].title, "Implement sidebar rows");
        assert_eq!(rows[1].title, "session ses_second");
        assert_ne!(rows[0].identity, rows[1].identity);
    }
}
