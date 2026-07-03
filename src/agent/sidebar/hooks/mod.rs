//! Sidebar hook events, command execution, and diagnostics.

mod diagnostics;
mod runner;
mod selection;

use anyhow::Result;

use crate::agent::sidebar::model::SidebarRow;
use crate::config::SidebarSelectionHookConfig;
use crate::state::StateStore;

pub(super) fn run_selection_hooks(
    hooks: &[SidebarSelectionHookConfig],
    store: &StateStore,
    tmux_instance: &str,
    row: &SidebarRow,
) -> Result<()> {
    selection::run_selection_hooks(hooks, store, tmux_instance, row)
}
