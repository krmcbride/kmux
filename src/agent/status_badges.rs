//! Tmux status-option refresh for windows with active agent sessions.

use std::collections::HashMap;

use anyhow::Result;

use crate::agent::sessions::session_views;
use crate::config::StatusIcons;
use crate::state::{AgentStatus, StateStore};
use crate::tmux::Tmux;

const KMUX_STATUS_OPTION: &str = "@kmux_status";

/// Refresh each tmux window's kmux status option from the highest-priority agent in it.
pub(crate) fn refresh_window_statuses(
    store: &StateStore,
    tmux: &Tmux,
    icons: &StatusIcons,
) -> Result<()> {
    let views = session_views(store, tmux)?;
    let mut by_window = HashMap::<String, AgentStatus>::new();
    for view in views {
        if !view.is_window_tmux_target() {
            continue;
        }
        let Some(window_id) = view.tmux_window_id() else {
            continue;
        };
        record_window_status(&mut by_window, window_id, view.status);
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
    by_window
        .entry(window_id.to_owned())
        .and_modify(|current| {
            if status_rank(status) > status_rank(*current) {
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

// Higher ranks win when multiple agents report different states in one window.
fn status_rank(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Waiting => 3,
        AgentStatus::Working => 2,
        AgentStatus::Done => 1,
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
