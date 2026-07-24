//! Unsupported entry points for process-backed adapter contract targets.
//!
//! This module exists only when the non-default
//! `internal-adapter-contract-tests` feature is enabled. It keeps contract
//! harness access out of kmux's supported default library API.

/// Git CLI adapter contracts.
pub mod git {
    pub use crate::git::contract_tests::{
        adds_and_finds_worktree_by_branch,
        creates_branch_from_current_branch_and_reuses_without_moving,
        creates_branch_from_explicit_start_point, detects_current_branch_and_detached_head,
        discovers_repo_info_from_linked_worktree, discovers_repo_info_from_primary_worktree,
        remove_worktree_guards_dirty_paths_unless_forced,
        returns_merge_base_when_branches_share_history,
        returns_none_when_branches_have_no_merge_base,
        safe_deletion_prefers_configured_upstream_over_local_head,
    };
    pub use crate::paths::contract_tests::{
        discovers_main_worktree_from_linked_worktree, discovers_paths_from_primary_worktree,
        project_identity_matches_subdirectories_but_not_other_repositories,
    };

    pub fn repo_root_resolves_to_canonical_git_worktree_root() -> anyhow::Result<()> {
        crate::agent::contract_repo_root_resolves_to_canonical_git_worktree_root()
    }

    pub fn subdirectory_resolves_to_git_worktree_root() -> anyhow::Result<()> {
        crate::agent::contract_subdirectory_resolves_to_git_worktree_root()
    }

    pub fn linked_worktree_root_is_distinct_from_main_root() -> anyhow::Result<()> {
        crate::agent::contract_linked_worktree_root_is_distinct_from_main_root()
    }

    pub fn non_git_directory_does_not_attach() -> anyhow::Result<()> {
        crate::agent::contract_non_git_directory_does_not_attach()
    }
}

/// Tmux CLI adapter contracts.
pub mod tmux {
    pub use crate::tmux::contract_tests::{
        creates_selects_lists_and_kills_windows_on_isolated_socket,
        lightweight_pane_listing_treats_missing_server_as_empty,
        literal_command_runs_inside_shell_and_window_survives_exit,
        physical_window_id_disambiguates_duplicate_names,
        project_session_window_commands_use_opaque_session_ids,
    };
}

/// Launcher child-process contracts.
pub mod launcher {
    pub use crate::launcher::contract_tests::spawn_failure_acknowledgment_and_diagnostics_are_sanitized;
    #[cfg(unix)]
    pub use crate::launcher::contract_tests::{
        absent_and_empty_input_remain_distinct,
        acknowledgment_delivery_failure_keeps_waiting_and_leaves_spawn_state_unknown,
        concurrent_requests_do_not_collide_or_cross_acknowledgments,
        relative_executable_paths_resolve_from_launcher_cwd,
        request_round_trip_preserves_exact_argv_and_cleans_up,
    };
}

/// Sidebar action orchestration against a private tmux server.
pub mod sidebar {
    pub fn selection_options_round_trip_restore_and_cleanup() -> anyhow::Result<()> {
        crate::agent::contract_selection_options_round_trip_restore_and_cleanup()
    }

    pub fn stale_jump_candidate_falls_back_and_focuses_content_pane() -> anyhow::Result<()> {
        crate::agent::contract_stale_jump_candidate_falls_back_and_focuses_content_pane()
    }

    pub fn failed_detached_jump_restores_previous_selection() -> anyhow::Result<()> {
        crate::agent::contract_failed_detached_jump_restores_previous_selection()
    }

    pub fn delete_refreshes_badge_and_sidebar_surfaces_on_private_server() -> anyhow::Result<()> {
        crate::agent::contract_delete_refreshes_badge_and_sidebar_surfaces_on_private_server()
    }
}
