//! Sidebar hook events, command execution, and diagnostics.

mod diagnostics;
mod runner;
mod selection;

use anyhow::Result;

use crate::config::SidebarSelectionHookConfig;

pub(super) use selection::SelectionHookInput;

pub(super) fn run_selection_hooks(
    hooks: &[SidebarSelectionHookConfig],
    selected: &SelectionHookInput,
) -> Result<()> {
    selection::run_selection_hooks(hooks, selected)
}
