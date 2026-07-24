use anyhow::Result;

#[test]
fn discovers_repo_info_from_primary_worktree() -> Result<()> {
    kmux::contract_tests::git::discovers_repo_info_from_primary_worktree()
}

#[test]
fn discovers_repo_info_from_linked_worktree() -> Result<()> {
    kmux::contract_tests::git::discovers_repo_info_from_linked_worktree()
}

#[test]
fn detects_current_branch_and_detached_head() -> Result<()> {
    kmux::contract_tests::git::detects_current_branch_and_detached_head()
}

#[test]
fn creates_branch_from_current_branch_and_reuses_without_moving() -> Result<()> {
    kmux::contract_tests::git::creates_branch_from_current_branch_and_reuses_without_moving()
}

#[test]
fn creates_branch_from_explicit_start_point() -> Result<()> {
    kmux::contract_tests::git::creates_branch_from_explicit_start_point()
}

#[test]
fn returns_merge_base_when_branches_share_history() -> Result<()> {
    kmux::contract_tests::git::returns_merge_base_when_branches_share_history()
}

#[test]
fn returns_none_when_branches_have_no_merge_base() -> Result<()> {
    kmux::contract_tests::git::returns_none_when_branches_have_no_merge_base()
}

#[test]
fn safe_deletion_prefers_configured_upstream_over_local_head() -> Result<()> {
    kmux::contract_tests::git::safe_deletion_prefers_configured_upstream_over_local_head()
}

#[test]
fn adds_and_finds_worktree_by_branch() -> Result<()> {
    kmux::contract_tests::git::adds_and_finds_worktree_by_branch()
}

#[test]
fn remove_worktree_guards_dirty_paths_unless_forced() -> Result<()> {
    kmux::contract_tests::git::remove_worktree_guards_dirty_paths_unless_forced()
}

#[test]
fn discovers_paths_from_primary_worktree() -> Result<()> {
    kmux::contract_tests::git::discovers_paths_from_primary_worktree()
}

#[test]
fn discovers_main_worktree_from_linked_worktree() -> Result<()> {
    kmux::contract_tests::git::discovers_main_worktree_from_linked_worktree()
}

#[test]
fn project_identity_matches_subdirectories_but_not_other_repositories() -> Result<()> {
    kmux::contract_tests::git::project_identity_matches_subdirectories_but_not_other_repositories()
}

#[test]
fn repo_root_resolves_to_canonical_git_worktree_root() -> Result<()> {
    kmux::contract_tests::git::repo_root_resolves_to_canonical_git_worktree_root()
}

#[test]
fn subdirectory_resolves_to_git_worktree_root() -> Result<()> {
    kmux::contract_tests::git::subdirectory_resolves_to_git_worktree_root()
}

#[test]
fn linked_worktree_root_is_distinct_from_main_root() -> Result<()> {
    kmux::contract_tests::git::linked_worktree_root_is_distinct_from_main_root()
}

#[test]
fn non_git_directory_does_not_attach() -> Result<()> {
    kmux::contract_tests::git::non_git_directory_does_not_attach()
}
