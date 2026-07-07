use anyhow::{Context, Result, bail};

use crate::cli;
use crate::state::workspace::WorkspaceStateStore;

use super::context::{load_repo_context, load_tmux_context};
use super::resolve::{resolve_current_kmux_workspace, resolve_workspace};
use crate::paths::same_path;
use crate::workspace::WorkspaceRecord;

/// Remove a kmux workspace, its worktree, local branch, tmux window, and owned parent link.
pub(super) fn run(args: cli::RemoveArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let resolved = resolve_remove_target(&repo, args.name.as_deref())?;

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

    std::env::set_current_dir(&repo.paths.main_worktree).with_context(|| {
        format!(
            "failed to change directory to {} before removing worktree",
            repo.paths.main_worktree.display()
        )
    })?;
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

    let window_name = repo.config.workspace_window_name(resolved.workspace_slug());
    if tmux
        .tmux
        .window_exists_by_name(&tmux.session_name, &window_name)?
    {
        tmux.tmux.kill_window(&tmux.session_name, &window_name)?;
    }

    println!("removed {}", resolved.workspace_slug());
    Ok(())
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
