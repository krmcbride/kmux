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
    let target = AddTarget::resolve(&repo, &args)?;
    let handle = derive_handle(&target.branch, args.name.as_deref())?;
    let worktree_path = repo.paths.handle_path(&handle);

    if let Some(existing) = find_kmux_worktree_by_name(&repo, &target.branch)? {
        if !args.open_if_exists {
            bail!(
                "worktree for '{}' already exists at {}",
                target.branch,
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

    if let Some(existing) = repo.git.find_worktree_by_branch(&target.branch)? {
        bail!(
            "branch '{}' is already checked out outside kmux at {}",
            target.branch,
            existing.path.display()
        );
    }

    repo.git.ensure_available_worktree_path(&worktree_path)?;
    repo.git
        .ensure_local_branch(&target.branch, target.base.as_deref())?;
    repo.git.add_worktree(&worktree_path, &target.branch)?;
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
        branch: Some(target.branch),
    };
    open_resolved(&repo, &tmux, &resolved, !args.background)?;
    println!("created {}\t{}", resolved.handle, resolved.path.display());
    Ok(())
}

struct AddTarget {
    branch: String,
    base: Option<String>,
}

impl AddTarget {
    fn resolve(repo: &super::context::RepoContext, args: &cli::AddArgs) -> Result<Self> {
        if let Some(remote) = repo.git.known_remote_branch(&args.branch)? {
            if args.base.is_some() {
                bail!(
                    "cannot use --base with remote branch '{}'; the remote branch is already the base",
                    args.branch
                );
            }

            return Ok(Self {
                branch: remote.branch,
                base: Some(remote.ref_name),
            });
        }

        Ok(Self {
            branch: args.branch.clone(),
            base: args
                .base
                .as_ref()
                .or(repo.config.base_branch.as_ref())
                .cloned(),
        })
    }
}
