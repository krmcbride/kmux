use anyhow::{Context, Result, bail};

use crate::cli;

use super::context::{load_repo_context, load_tmux_context};
use super::resolve::{ResolvedWorktree, resolve_worktree, resolved_from_worktree};
use crate::paths::same_path;

pub(super) fn run(args: cli::RemoveArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let resolved = resolve_remove_target(&repo, args.name.as_deref())?;

    if same_path(&resolved.path, &repo.paths.main_worktree) {
        bail!(
            "cannot remove the main worktree at {}",
            resolved.path.display()
        );
    }
    if resolved.branch.is_none() && !args.keep_branch {
        bail!("worktree branch is unknown; use --keep-branch to remove only the worktree");
    }
    if !args.keep_branch
        && !args.force
        && let Some(branch) = &resolved.branch
        && !repo.git.branch_is_safely_deletable(branch)?
    {
        bail!(
            "branch '{}' is not safely merged; use --force to delete anyway or --keep-branch to remove only the worktree",
            branch
        );
    }

    std::env::set_current_dir(&repo.paths.main_worktree).with_context(|| {
        format!(
            "failed to change directory to {} before removing worktree",
            repo.paths.main_worktree.display()
        )
    })?;
    repo.git.remove_worktree(&resolved.path, args.force)?;
    if !args.keep_branch
        && let Some(branch) = &resolved.branch
    {
        repo.git.delete_local_branch(branch, true)?;
    }

    let window_name = repo.config.window_name(&resolved.handle);
    if tmux
        .tmux
        .window_exists_by_name(&tmux.session_name, &window_name)?
    {
        tmux.tmux.kill_window(&tmux.session_name, &window_name)?;
    }

    println!("removed {}", resolved.handle);
    Ok(())
}

fn resolve_remove_target(
    repo: &super::context::RepoContext,
    name: Option<&str>,
) -> Result<ResolvedWorktree> {
    if let Some(name) = name {
        return resolve_worktree(repo, name);
    }

    if same_path(&repo.paths.current_worktree, &repo.paths.main_worktree) {
        bail!("remove requires a worktree name when run from the main worktree");
    }

    let is_current_kmux_worktree = repo
        .paths
        .current_worktree
        .parent()
        .is_some_and(|parent| same_path(parent, &repo.paths.worktree_base_dir));
    if !is_current_kmux_worktree {
        bail!("current worktree is not kmux-managed; pass a worktree name explicitly");
    }

    let current = repo
        .git
        .worktrees()?
        .into_iter()
        .find(|worktree| same_path(&worktree.path, &repo.paths.current_worktree))
        .ok_or_else(|| {
            anyhow::anyhow!(
                "current worktree {} is not registered with git",
                repo.paths.current_worktree.display()
            )
        })?;

    resolved_from_worktree(current)
}
