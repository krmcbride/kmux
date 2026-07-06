//! Hidden tmux sidebar UI for monitoring active agent sessions.
//!
//! Sidebar lifecycle and rendering stay here so presentation, tmux pane repair,
//! and keyboard navigation do not leak into workspace lifecycle workflows.

mod app;
mod candidates;
mod commands;
mod hooks;
mod lifecycle;
mod model;
mod render;
mod runtime;
mod selection;
#[cfg(test)]
mod test_support;

use anyhow::Result;

use crate::cli::{SidebarArgs, SidebarCommand};
use crate::tmux::Tmux;

/// Notify live sidebar panes that external agent observation state changed.
pub(super) fn notify_observation_changed(tmux: &Tmux) -> Result<()> {
    lifecycle::notify_observation_changed(tmux)
}

/// Dispatch sidebar lifecycle commands or toggle the sidebar when no subcommand is provided.
pub fn run(args: SidebarArgs) -> Result<()> {
    match args.command {
        Some(SidebarCommand::On) => lifecycle::enable(),
        Some(SidebarCommand::Off) => lifecycle::disable(),
        Some(SidebarCommand::Refresh) => lifecycle::refresh(),
        Some(SidebarCommand::Run) => lifecycle::run_tui(),
        Some(SidebarCommand::Wake { window_id }) => lifecycle::wake(&window_id),
        None => lifecycle::toggle(),
    }
}
