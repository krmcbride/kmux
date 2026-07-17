//! Agent-status badges shown with tmux window titles.
//!
//! This module writes the configured working, waiting, or done icon to each
//! window's `@kmux_status` user option and unsets it when the window has no
//! matching activity. To display the badge, the user's `window-status-format`
//! and `window-status-current-format` must include `#{@kmux_status}`; that tmux
//! configuration owns the badge's placement, spacing, and styling.

use std::collections::HashMap;

use anyhow::Result;

use crate::agent::sessions::activity_status_priority;
use crate::agent::workspace_activity::workspace_activities;
use crate::config::StatusIcons;
use crate::state::{AgentStatus, StateStore};
use crate::tmux::Tmux;

const KMUX_STATUS_OPTION: &str = "@kmux_status";

/// Refresh each tmux window from the highest-priority workspace activity targeting it.
pub fn refresh_window_statuses(store: &StateStore, tmux: &Tmux, icons: &StatusIcons) -> Result<()> {
    let activities = workspace_activities(store, tmux)?;
    let mut by_window = HashMap::<String, AgentStatus>::new();
    for activity in activities {
        if !activity.has_window_tmux_target() {
            continue;
        }
        let Some(window_id) = activity.tmux_window_id() else {
            continue;
        };
        record_window_status(&mut by_window, window_id, activity.status());
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

fn record_window_status(
    by_window: &mut HashMap<String, AgentStatus>,
    window_id: &str,
    status: AgentStatus,
) {
    // A tmux window can contain panes from more than one Git workspace, so more
    // than one workspace activity may target the same window. Because the window
    // has one badge, choose its status with the shared primary-session priority.
    by_window
        .entry(window_id.to_owned())
        .and_modify(|current| {
            if activity_status_priority(status) > activity_status_priority(*current) {
                *current = status;
            }
        })
        .or_insert(status);
}

fn status_icon(status: AgentStatus, icons: &StatusIcons) -> &str {
    match status {
        AgentStatus::Working => icons.working(),
        AgentStatus::Waiting => icons.waiting(),
        AgentStatus::Done => icons.done(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_status_prefers_waiting_over_working_and_done() {
        let mut by_window = HashMap::new();

        record_window_status(&mut by_window, "@1", AgentStatus::Done);
        record_window_status(&mut by_window, "@1", AgentStatus::Working);
        record_window_status(&mut by_window, "@1", AgentStatus::Waiting);
        record_window_status(&mut by_window, "@1", AgentStatus::Done);

        assert_eq!(by_window.get("@1").copied(), Some(AgentStatus::Waiting));
    }
}
