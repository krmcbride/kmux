//! Agent-facing presentation and observation workflows.
//!
//! This module turns persisted agent observations and live tmux state into user
//! surfaces such as status output, workspace badges, and the sidebar UI.

mod workspace;

pub mod query;
pub mod sessions;
pub mod sidebar;
pub mod status;
