//! Agent-facing presentation and observation workflows.
//!
//! This module turns persisted agent observations and live tmux state into user
//! surfaces such as status output, workspace badges, and the sidebar UI.

mod sessions;
mod status_badges;
#[cfg(test)]
mod test_support;
mod workspace;

#[cfg(feature = "internal-adapter-contract-tests")]
pub(super) fn contract_repo_root_resolves_to_canonical_git_worktree_root() -> anyhow::Result<()> {
    workspace::contract_tests::repo_root_resolves_to_canonical_git_worktree_root()
}

#[cfg(feature = "internal-adapter-contract-tests")]
pub(super) fn contract_subdirectory_resolves_to_git_worktree_root() -> anyhow::Result<()> {
    workspace::contract_tests::subdirectory_resolves_to_git_worktree_root()
}

#[cfg(feature = "internal-adapter-contract-tests")]
pub(super) fn contract_linked_worktree_root_is_distinct_from_main_root() -> anyhow::Result<()> {
    workspace::contract_tests::linked_worktree_root_is_distinct_from_main_root()
}

#[cfg(feature = "internal-adapter-contract-tests")]
pub(super) fn contract_non_git_directory_does_not_attach() -> anyhow::Result<()> {
    workspace::contract_tests::non_git_directory_does_not_attach()
}

#[cfg(feature = "internal-adapter-contract-tests")]
pub(super) fn contract_selection_options_round_trip_restore_and_cleanup() -> anyhow::Result<()> {
    sidebar::contract_selection_options_round_trip_restore_and_cleanup()
}

#[cfg(feature = "internal-adapter-contract-tests")]
pub(super) fn contract_stale_jump_candidate_falls_back_and_focuses_content_pane()
-> anyhow::Result<()> {
    sidebar::contract_stale_jump_candidate_falls_back_and_focuses_content_pane()
}

#[cfg(feature = "internal-adapter-contract-tests")]
pub(super) fn contract_failed_detached_jump_restores_previous_selection() -> anyhow::Result<()> {
    sidebar::contract_failed_detached_jump_restores_previous_selection()
}

#[cfg(feature = "internal-adapter-contract-tests")]
pub(super) fn contract_delete_refreshes_badge_and_sidebar_surfaces_on_private_server()
-> anyhow::Result<()> {
    sidebar::contract_delete_refreshes_badge_and_sidebar_surfaces_on_private_server()
}

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
