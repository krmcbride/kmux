use std::path::Path;

use crate::agent::sessions::AgentSessionView;
use crate::config::StatusIcons;
use crate::state::{AgentSessionKey, AgentStatus};

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
    pub(super) fn session_key(&self) -> AgentSessionKey {
        AgentSessionKey {
            agent_kind: self.agent_kind.clone(),
            session_id: self.session_id.clone(),
        }
    }

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

    fn is_working(self) -> bool {
        self == Self::Working
    }

    fn is_idle(self) -> bool {
        self == Self::Idle
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidebarRow {
    pub(super) identity: SidebarRowIdentity,
    created_at: u64,
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
                    .tmux_pane_title
                    .as_deref()
                    .filter(|title| *title != primary && *title != secondary)
            })
            .or(target.tmux_pane_current_command.as_deref())
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
            created_at: view.created_at,
            state,
            icon,
            primary,
            secondary,
            secondary_right,
            title,
            elapsed: compact_elapsed(elapsed),
            session_name: target.tmux_session_name.clone().unwrap_or_default(),
            window_id: target.tmux_window_id.clone().unwrap_or_default(),
            pane_id: target.tmux_pane_id.clone(),
        }
    }
}

pub(super) fn build_rows_with_working_icon(
    views: &[AgentSessionView],
    now: u64,
    icons: &SidebarIcons,
    working_icon: Option<&str>,
    idle_after_seconds: u64,
) -> Vec<SidebarRow> {
    let mut rows = views
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
        .collect::<Vec<_>>();
    rows.sort_by(|left, right| {
        (
            &left.primary,
            &left.secondary,
            left.created_at,
            &left.identity.agent_kind,
            &left.identity.session_id,
        )
            .cmp(&(
                &right.primary,
                &right.secondary,
                right.created_at,
                &right.identity.agent_kind,
                &right.identity.session_id,
            ))
    });
    rows
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

fn repo_label(view: &AgentSessionView) -> String {
    clean_label(view.target.git_repo_name.as_deref())
        .or_else(|| path_label(view.target.git_repo_path.as_deref()))
        .or_else(|| path_label(view.target.directory.as_deref()))
        .or_else(|| path_label(view.target.git_worktree_path.as_deref()))
        .or_else(|| clean_label(view.target.tmux_window_name.as_deref()))
        .unwrap_or_else(|| view.key.session_id.clone())
}

fn branch_label(view: &AgentSessionView, primary: &str) -> String {
    clean_label(view.target.git_branch.as_deref())
        .or_else(|| distinct_label(view.target.kmux_worktree_handle.as_deref(), primary))
        .or_else(|| path_distinct_label(view.target.directory.as_deref(), primary))
        .or_else(|| path_distinct_label(view.target.git_worktree_path.as_deref(), primary))
        .or_else(|| distinct_label(view.target.tmux_window_name.as_deref(), primary))
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
mod tests {
    use super::*;
    use crate::agent::sidebar::test_support::{TEST_SLEEPING_ICON, report_state, test_icons};
    use crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS;

    fn build_rows(
        views: &[AgentSessionView],
        now: u64,
        idle_after_seconds: u64,
    ) -> Vec<SidebarRow> {
        let icons = test_icons();
        build_rows_with_working_icon(views, now, &icons, None, idle_after_seconds)
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
        first.target.tmux_pane_id = None;
        first.title = Some("Implement sidebar rows".to_owned());

        let mut second = report_state(AgentStatus::Waiting, 120, "@1", "%2");
        second.key = crate::state::AgentSessionKey {
            agent_kind: "opencode".to_owned(),
            session_id: "ses_second".to_owned(),
        };
        second.target.tmux_pane_id = None;
        second.title = None;
        second.target.tmux_pane_title = None;
        second.target.tmux_pane_current_command = None;

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
    fn row_model_sorts_by_primary_secondary_and_creation_time() {
        let mut kmux_old = report_state(AgentStatus::Working, 100, "@1", "%1");
        kmux_old.key.session_id = "ses_kmux_old".to_owned();
        kmux_old.target.git_repo_name = Some("kmux".to_owned());
        kmux_old.target.git_branch = Some("master".to_owned());
        kmux_old.title = Some("kmux old".to_owned());

        let mut dotfiles = report_state(AgentStatus::Working, 200, "@2", "%2");
        dotfiles.key.session_id = "ses_dotfiles".to_owned();
        dotfiles.target.git_repo_name = Some(".dotfiles".to_owned());
        dotfiles.target.git_branch = Some("master".to_owned());
        dotfiles.title = Some("dotfiles".to_owned());

        let mut kmux_feature = report_state(AgentStatus::Working, 50, "@3", "%3");
        kmux_feature.key.session_id = "ses_kmux_feature".to_owned();
        kmux_feature.target.git_repo_name = Some("kmux".to_owned());
        kmux_feature.target.git_branch = Some("feature/sidebar".to_owned());
        kmux_feature.title = Some("kmux feature".to_owned());

        let mut kmux_new = report_state(AgentStatus::Working, 300, "@4", "%4");
        kmux_new.key.session_id = "ses_kmux_new".to_owned();
        kmux_new.target.git_repo_name = Some("kmux".to_owned());
        kmux_new.target.git_branch = Some("master".to_owned());
        kmux_new.title = Some("kmux new".to_owned());

        let rows = build_rows(&[kmux_new, kmux_feature, dotfiles, kmux_old], 400, 1_800);

        assert_eq!(
            rows.iter()
                .map(|row| row.title.as_str())
                .collect::<Vec<_>>(),
            ["dotfiles", "kmux feature", "kmux old", "kmux new"]
        );
    }

    #[test]
    fn row_model_falls_back_to_branch_worktree_and_window_without_tmux_session_label() {
        let mut report = report_state(AgentStatus::Working, 120, "@1", "%1");
        report.target.git_repo_name = None;
        report.target.git_repo_path = None;
        report.target.git_branch = None;

        let rows = build_rows(&[report], 300, 1_800);

        assert_eq!(rows[0].primary, "feature-sidebar");
        assert_eq!(rows[0].secondary, "kmux-feature-sidebar");
    }
}
