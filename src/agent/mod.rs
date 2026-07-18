//! Agent-facing presentation and observation workflows.
//!
//! This module turns persisted agent observations and live tmux state into user
//! surfaces such as status output, workspace badges, and the sidebar UI.

mod sessions;
mod status_badges;
#[cfg(test)]
mod test_support;
mod workspace;

pub mod observations;
pub mod query;
pub mod sidebar;
pub mod status;
pub mod workspace_activity;

use crate::config::StatusIcons;
use crate::state::StateStore;
use crate::tmux::Tmux;

/// Refresh presentation surfaces after persisted observation state changes.
///
/// This is an explicit, synchronous stopgap for what may eventually become an
/// evented observation-applied flow. For now every successful observation
/// mutation should refresh badges and wake sidebars, and those presentation
/// updates stay best-effort so UI refresh failures do not roll back persisted
/// agent state.
pub fn refresh_observation_surfaces(store: &StateStore, tmux: &Tmux, icons: &StatusIcons) {
    let _ = status_badges::refresh_window_statuses(store, tmux, icons);
    let _ = sidebar::notify_observation_changed(tmux);
}
