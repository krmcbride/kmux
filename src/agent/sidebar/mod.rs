//! Hidden tmux sidebar capability for monitoring active agent sessions.
//!
//! This module is the thin inbound CLI adapter for sidebar commands. Tmux hook,
//! pane lifecycle, row-query, action, runtime, and rendering concerns stay in
//! focused sibling modules so workspace lifecycle workflows do not own sidebar
//! presentation or tmux pane repair details.

mod actions;
mod app;
mod candidates;
mod commands;
mod lifecycle;
mod model;
mod render;
mod rows;
mod runtime;
mod selection;
mod sizing;
#[cfg(test)]
mod test_support;

use anyhow::Result;

use crate::cli::{SidebarArgs, SidebarCommand};
use crate::tmux::Tmux;

/// Notify live sidebar panes that external agent observation state changed.
pub(super) fn notify_observation_changed(tmux: &Tmux) -> Result<()> {
    lifecycle::notify_observation_changed(tmux)
}

/// Dispatch an explicit sidebar lifecycle command.
pub fn run(args: SidebarArgs) -> Result<()> {
    match args.command {
        SidebarCommand::On => lifecycle::enable(),
        SidebarCommand::Off => lifecycle::disable(),
        SidebarCommand::Toggle => lifecycle::toggle(),
        SidebarCommand::Refresh => lifecycle::refresh(),
        SidebarCommand::Run => lifecycle::run_tui(),
        SidebarCommand::Wake { window_id } => lifecycle::wake(&window_id),
    }
}
