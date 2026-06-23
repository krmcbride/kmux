use anyhow::{Result, bail};

use crate::cli;
use crate::slug::derive_handle;

use super::context::{load_repo_context, load_tmux_context};
use super::files::{apply_file_operations, run_post_create};
use super::resolve::{
    ResolvedWorktree, find_kmux_worktree_by_handle, find_kmux_worktree_by_name,
    resolved_from_worktree,
};
use super::window::open_resolved;

pub(super) fn run(args: cli::AddArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let base = args.base.as_deref().or(repo.config.base_branch.as_deref());
    let handle = derive_handle(&args.branch, args.name.as_deref())?;
    let worktree_path = repo.paths.handle_path(&handle);

    if let Some(existing) = find_kmux_worktree_by_name(&repo, &args.branch)? {
        if !args.open_if_exists {
            bail!(
                "worktree for '{}' already exists at {}",
                args.branch,
                existing.path.display()
            );
        }
        let resolved = resolved_from_worktree(existing)?;
        open_resolved(&repo, &tmux, &resolved, !args.background)?;
        println!("opened {}\t{}", resolved.handle, resolved.path.display());
        return Ok(());
    }

    if let Some(existing) = find_kmux_worktree_by_handle(&repo, &handle)? {
        if !args.open_if_exists {
            bail!(
                "worktree handle '{}' already exists at {}",
                handle,
                existing.path.display()
            );
        }
        let resolved = resolved_from_worktree(existing)?;
        open_resolved(&repo, &tmux, &resolved, !args.background)?;
        println!("opened {}\t{}", resolved.handle, resolved.path.display());
        return Ok(());
    }

    if let Some(existing) = repo.git.find_worktree_by_branch(&args.branch)? {
        bail!(
            "branch '{}' is already checked out outside kmux at {}",
            args.branch,
            existing.path.display()
        );
    }

    repo.git.ensure_available_worktree_path(&worktree_path)?;
    repo.git.ensure_local_branch(&args.branch, base)?;
    repo.git.add_worktree(&worktree_path, &args.branch)?;
    apply_file_operations(&repo.config, &repo.paths.main_worktree, &worktree_path)?;
    run_post_create(
        &repo.config,
        &repo.paths.main_worktree,
        &worktree_path,
        &handle,
    )?;

    let resolved = ResolvedWorktree {
        handle,
        path: worktree_path,
        branch: Some(args.branch),
    };
    open_resolved(&repo, &tmux, &resolved, !args.background)?;
    println!("created {}\t{}", resolved.handle, resolved.path.display());
    Ok(())
}
