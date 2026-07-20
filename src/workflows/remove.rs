use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cli;
use crate::state::workspace::WorkspaceStateStore;

use super::context::load_repo_context;
use super::project_session;
use super::resolve::{resolve_current_kmux_workspace, resolve_workspace};
use crate::paths::same_path;
use crate::workspace::WorkspaceRecord;

/// Remove a kmux workspace, its worktree, local branch, tmux window, and owned parent link.
pub(super) fn run(args: cli::RemoveArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let resolved = resolve_remove_target(&repo, args.name.as_deref())?;
    let tmux_resolution = project_session::resolve(&repo.paths)?;

    if same_path(resolved.path(), &repo.paths.main_worktree) {
        bail!(
            "cannot remove the main worktree at {}",
            resolved.path().display()
        );
    }
    let branch = resolved.branch().ok_or_else(|| {
        anyhow::anyhow!(
            "workspace '{}' has no known git branch and cannot be removed by kmux",
            resolved.workspace_slug()
        )
    })?;
    if !args.force && !repo.git.branch_is_safely_deletable(branch)? {
        bail!(
            "branch '{}' is not safely merged; use --force to delete the workspace anyway",
            branch
        );
    }
    let state_store = WorkspaceStateStore::new(&repo.paths.git_common_dir);
    let mut state = state_store.load()?;
    // Removing a parent branch is metadata-only for descendants: warn about
    // dangling child links instead of silently reparenting or deleting them.
    let remaining_children = state.children_of(branch);

    leave_worktree_before_removal(&repo.paths.main_worktree)?;
    // Refresh live tmux evidence at the last responsible moment. The held
    // project lifecycle lock prevents another kmux lifecycle command from
    // changing project windows between this check and Git removal.
    let window_name = repo.config.workspace_window_name(resolved.workspace_slug());
    let window_id = tmux_resolution.prepare_workspace_removal(resolved.path(), &window_name)?;
    repo.git.remove_worktree(resolved.path(), args.force)?;
    repo.git.delete_local_branch(branch, true)?;
    if state.remove_parent(branch) {
        state_store.save(&state)?;
    }
    if !remaining_children.is_empty() {
        eprintln!(
            "warning: parent links still reference removed branch '{}': {}",
            branch,
            remaining_children.join(", ")
        );
    }

    if let Some(window_id) = window_id {
        tmux_resolution.kill_prepared_window(&window_id)?;
    }

    println!("removed {}", resolved.workspace_slug());
    Ok(())
}

// Leave the worktree before deleting it so the process cwd remains valid and
// later Git and tmux subprocesses inherit the existing main-worktree directory.
fn leave_worktree_before_removal(main_worktree: &Path) -> Result<()> {
    std::env::set_current_dir(main_worktree).with_context(|| {
        format!(
            "failed to change directory to {} before removing worktree",
            main_worktree.display()
        )
    })
}

// Support both explicit removal by name and short-form removal from inside a kmux worktree.
fn resolve_remove_target(
    repo: &super::context::RepoContext,
    name: Option<&str>,
) -> Result<WorkspaceRecord> {
    if let Some(name) = name {
        return resolve_workspace(repo, name);
    }

    resolve_current_kmux_workspace(repo, "remove")
}
