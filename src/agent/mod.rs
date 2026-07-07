//! Agent-facing presentation and observation workflows.
//!
//! This module turns persisted agent observations and live tmux state into user
//! surfaces such as status output, workspace badges, and the sidebar UI.

mod workspace;

pub(crate) mod observations;
pub mod query;
pub mod sessions;
pub mod sidebar;
pub mod status;
pub(crate) mod status_badges;

use anyhow::Result;

use crate::tmux::Tmux;

/// Notify live agent presentation surfaces that observation state changed.
pub(crate) fn notify_observation_changed(tmux: &Tmux) -> Result<()> {
    sidebar::notify_observation_changed(tmux)
}
