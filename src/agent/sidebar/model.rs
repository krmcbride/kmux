use crate::state::{AgentState, AgentStatus};

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidebarRow {
    pub(super) status: AgentStatus,
    pub(super) icon: String,
    pub(super) primary: String,
    pub(super) secondary: String,
    pub(super) secondary_right: String,
    pub(super) title: String,
    pub(super) elapsed: String,
    pub(super) is_idle: bool,
    pub(super) session_name: String,
    pub(super) window_id: String,
    pub(super) pane_id: String,
}

impl SidebarRow {
    #[cfg(test)]
    pub(super) fn from_agent(agent: &AgentState, now: u64, sleeping_icon: &str) -> Self {
        Self::from_agent_with_working_icon(
            agent,
            now,
            sleeping_icon,
            None,
            crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
        )
    }

    fn from_agent_with_working_icon(
        agent: &AgentState,
        now: u64,
        sleeping_icon: &str,
        working_icon: Option<&str>,
        idle_after_seconds: u64,
    ) -> Self {
        let primary = agent
            .worktree_handle
            .as_deref()
            .or(agent.branch.as_deref())
            .unwrap_or(&agent.window_name)
            .to_owned();
        let secondary = secondary_label(agent, &primary);
        let title = agent
            .agent_title
            .as_deref()
            .filter(|title| *title != primary && *title != secondary)
            .or_else(|| {
                agent
                    .pane_title
                    .as_deref()
                    .filter(|title| *title != primary && *title != secondary)
            })
            .or(agent.pane_current_command.as_deref())
            .unwrap_or_default()
            .to_owned();
        let secondary_right = agent
            .context_usage
            .as_deref()
            .map(str::trim)
            .filter(|context| !context.is_empty())
            .unwrap_or_default()
            .to_owned();
        let age = now.saturating_sub(agent.status_changed_at);
        let is_idle = agent.status == AgentStatus::Done && age >= idle_after_seconds;
        let icon = if is_idle {
            sleeping_icon.to_owned()
        } else if agent.status == AgentStatus::Working {
            working_icon.unwrap_or(&agent.icon).to_owned()
        } else {
            agent.icon.clone()
        };

        Self {
            status: agent.status,
            icon,
            primary,
            secondary,
            secondary_right,
            title,
            elapsed: compact_elapsed(age),
            is_idle,
            session_name: agent.session_name.clone(),
            window_id: agent.window_id.clone(),
            pane_id: agent.pane_key.pane_id.clone(),
        }
    }
}

fn secondary_label(agent: &AgentState, primary: &str) -> String {
    match agent.branch.as_deref().filter(|branch| *branch != primary) {
        Some(branch) => format!("{} / {branch}", agent.session_name),
        None => agent.session_name.clone(),
    }
}

#[cfg(test)]
pub(super) fn build_rows(
    agents: &[AgentState],
    now: u64,
    sleeping_icon: &str,
    idle_after_seconds: u64,
) -> Vec<SidebarRow> {
    build_rows_with_working_icon(agents, now, sleeping_icon, None, idle_after_seconds)
}

pub(super) fn build_rows_with_working_icon(
    agents: &[AgentState],
    now: u64,
    sleeping_icon: &str,
    working_icon: Option<&str>,
    idle_after_seconds: u64,
) -> Vec<SidebarRow> {
    agents
        .iter()
        .map(|agent| {
            SidebarRow::from_agent_with_working_icon(
                agent,
                now,
                sleeping_icon,
                working_icon,
                idle_after_seconds,
            )
        })
        .collect()
}

pub(super) fn row_index_by_window(rows: &[SidebarRow], window_id: &str) -> Option<usize> {
    rows.iter().position(|row| row.window_id == window_id)
}

pub(super) fn row_index_by_pane(rows: &[SidebarRow], pane_id: &str) -> Option<usize> {
    rows.iter().position(|row| row.pane_id == pane_id)
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

#[cfg(test)]
pub(super) const TEST_SLEEPING_ICON: &str = "z";

#[cfg(test)]
pub(super) fn agent_state(
    status: AgentStatus,
    status_changed_at: u64,
    window_id: &str,
    pane_id: &str,
) -> AgentState {
    use crate::state::PaneKey;

    AgentState {
        pane_key: PaneKey::new_tmux("test", pane_id),
        status,
        icon: "?".to_owned(),
        status_changed_at,
        observed_at: status_changed_at,
        agent_title: None,
        context_usage: None,
        pane_title: Some("Implement sidebar".to_owned()),
        pane_current_command: Some("nvim".to_owned()),
        worktree_handle: Some("feature-sidebar".to_owned()),
        worktree_path: Some("/repo__worktrees/feature-sidebar".to_owned()),
        branch: Some("feature/sidebar".to_owned()),
        session_name: "project".to_owned(),
        window_name: "kmux-feature-sidebar".to_owned(),
        window_id: window_id.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS;

    #[test]
    fn row_model_prefers_worktree_and_marks_old_done_idle() {
        let agents = vec![agent_state(AgentStatus::Done, 0, "@1", "%1")];
        let rows = build_rows(
            &agents,
            DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS + 1,
            TEST_SLEEPING_ICON,
            DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
        );

        assert_eq!(rows[0].primary, "feature-sidebar");
        assert_eq!(rows[0].secondary, "project / feature/sidebar");
        assert_eq!(rows[0].title, "Implement sidebar");
        assert_eq!(rows[0].elapsed, "30m");
        assert_eq!(rows[0].icon, TEST_SLEEPING_ICON);
        assert!(rows[0].is_idle);
    }

    #[test]
    fn row_model_keeps_old_waiting_agent_active() {
        let agents = vec![agent_state(AgentStatus::Waiting, 0, "@1", "%1")];
        let rows = build_rows(
            &agents,
            DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS + 1,
            TEST_SLEEPING_ICON,
            DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
        );

        assert_eq!(rows[0].icon, "?");
        assert!(!rows[0].is_idle);
    }

    #[test]
    fn row_model_uses_configured_idle_threshold() {
        let agents = vec![agent_state(AgentStatus::Done, 0, "@1", "%1")];

        let active_rows = build_rows(&agents, 1_799, TEST_SLEEPING_ICON, 1_800);
        let idle_rows = build_rows(&agents, 1_800, TEST_SLEEPING_ICON, 1_800);

        assert!(!active_rows[0].is_idle);
        assert!(idle_rows[0].is_idle);
    }

    #[test]
    fn row_model_prefers_agent_title_and_context_usage() {
        let mut agent = agent_state(AgentStatus::Working, 120, "@1", "%1");
        agent.agent_title = Some("Implement richer sidebar".to_owned());
        agent.context_usage = Some("163.2K (41%)".to_owned());
        let rows = build_rows(&[agent], 300, TEST_SLEEPING_ICON, 1_800);

        assert_eq!(rows[0].title, "Implement richer sidebar");
        assert_eq!(rows[0].secondary_right, "163.2K (41%)");
    }

    #[test]
    fn row_model_omits_secondary_right_when_context_usage_is_absent() {
        let rows = build_rows(
            &[agent_state(AgentStatus::Working, 120, "@1", "%1")],
            300,
            TEST_SLEEPING_ICON,
            1_800,
        );

        assert_eq!(rows[0].secondary_right, "");
    }

    #[test]
    fn row_model_uses_working_frame_only_for_working_rows() {
        let rows = build_rows_with_working_icon(
            &[
                agent_state(AgentStatus::Working, 120, "@1", "%1"),
                agent_state(AgentStatus::Waiting, 120, "@2", "%2"),
            ],
            300,
            TEST_SLEEPING_ICON,
            Some("a"),
            1_800,
        );

        assert_eq!(rows[0].icon, "a");
        assert_eq!(rows[1].icon, "?");
    }
}
