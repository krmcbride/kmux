use anyhow::{Result, bail};

use crate::cli;
use crate::slug::workspace_slug_from_branch;

use super::context::{load_repo_context, load_tmux_context};
use super::files::{apply_file_operations, run_post_create};
use super::resolve::{
    ResolvedWorkspace, find_kmux_workspace_by_name, find_kmux_workspace_by_slug,
    resolved_from_kmux_worktree,
};
use super::window::open_or_create_resolved;

pub(super) fn run(args: cli::AddArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let target = AddTarget::resolve(&repo, &args)?;
    let workspace_slug = workspace_slug_from_branch(&target.branch)?;
    let worktree_path = repo.paths.workspace_path(&workspace_slug);
    let window_name = repo.config.workspace_window_name(&workspace_slug);

    if let Some(existing) = find_kmux_workspace_by_name(&repo, &target.branch)? {
        let resolved = resolved_from_kmux_worktree(&repo, existing)?;
        open_existing_workspace(&repo, &tmux, &args, &target.branch, resolved)?;
        return Ok(());
    }

    if let Some(existing) = find_kmux_workspace_by_slug(&repo, &workspace_slug)? {
        let resolved = resolved_from_kmux_worktree(&repo, existing)?;
        open_existing_workspace(&repo, &tmux, &args, &target.branch, resolved)?;
        return Ok(());
    }

    if let Some(existing) = repo.git.find_worktree_by_branch(&target.branch)? {
        bail!(
            "branch '{}' is already checked out outside kmux at {}",
            target.branch,
            existing.path.display()
        );
    }
    if tmux
        .tmux
        .window_exists_by_name(&tmux.session_name, &window_name)?
    {
        if args.open_if_exists {
            bail!(
                "tmux window '{}' exists, but kmux workspace '{}' has no matching worktree at {}; remove the window or restore the worktree before repairing",
                window_name,
                workspace_slug,
                worktree_path.display()
            );
        }
        bail!(
            "tmux window '{}' already exists for workspace '{}'; use -o to repair an existing workspace",
            window_name,
            workspace_slug
        );
    }
    if !args.open_if_exists && worktree_path.exists() {
        bail!(
            "workspace path {} already exists for '{}'; use -o to repair an existing workspace",
            worktree_path.display(),
            workspace_slug
        );
    }
    if !args.open_if_exists && repo.git.local_branch_exists(&target.branch)? {
        bail!(
            "branch '{}' already exists; use -o to create or open the corresponding workspace",
            target.branch
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
        &workspace_slug,
    )?;

    let resolved = ResolvedWorkspace {
        workspace_slug,
        path: worktree_path,
        branch: Some(target.branch),
    };
    open_or_create_resolved(&repo, &tmux, &resolved, !args.background)?;
    println!(
        "created {}\t{}",
        resolved.workspace_slug,
        resolved.path.display()
    );
    Ok(())
}

fn open_existing_workspace(
    repo: &super::context::RepoContext,
    tmux: &super::context::TmuxContext,
    args: &cli::AddArgs,
    expected_branch: &str,
    resolved: ResolvedWorkspace,
) -> Result<()> {
    if resolved.branch.as_deref() != Some(expected_branch) {
        bail!(
            "workspace slug '{}' already exists at {} for branch '{}', not '{}'",
            resolved.workspace_slug,
            resolved.path.display(),
            resolved.branch.as_deref().unwrap_or("<unknown>"),
            expected_branch
        );
    }
    if !args.open_if_exists {
        bail!(
            "workspace for '{}' already exists at {}; use -o to open or repair it",
            expected_branch,
            resolved.path.display()
        );
    }

    open_or_create_resolved(repo, tmux, &resolved, !args.background)?;
    println!(
        "opened {}\t{}",
        resolved.workspace_slug,
        resolved.path.display()
    );
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
