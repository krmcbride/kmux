use anyhow::{Context, Result, bail};

use crate::cli;
use crate::slug::workspace_slug_from_branch;

use super::context::load_repo_context;
use super::files::{apply_file_operations, run_post_create};
use super::launch::resolve_add;
use super::parent::{record_parent, validate_no_cycle};
use super::project_session;
use super::resolve::{
    find_kmux_workspace_by_name, find_kmux_workspace_by_slug, resolved_from_kmux_worktree,
};
use super::window::{create_shell, select_created, start_launcher};
use crate::state::workspace::WorkspaceStateStore;
use crate::workspace::WorkspaceRecord;

/// Create a new branch workspace, tmux window, and parent metadata link.
pub(super) fn run(args: cli::AddArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let launcher = resolve_add(&repo.config, &args)?;
    let tmux = project_session::resolve(&repo.paths)?.require("kmux add")?;
    if !args.background && !tmux.is_ambient {
        bail!(
            "kmux add cannot focus resolved tmux session '{}' because the caller is not attached to it; pass --background",
            tmux.session_name
        );
    }
    let target = AddTarget::resolve(&repo, &args)?;
    let workspace_slug = workspace_slug_from_branch(&target.branch)?;
    let worktree_path = repo.paths.workspace_path(&workspace_slug);
    let window_name = repo.config.workspace_window_name(&workspace_slug);

    if let Some(existing) = find_kmux_workspace_by_name(&repo, &target.branch)? {
        let resolved = resolved_from_kmux_worktree(&repo, existing)?;
        bail_existing_workspace(&target.branch, resolved)?;
    }

    if let Some(existing) = find_kmux_workspace_by_slug(&repo, &workspace_slug)? {
        let resolved = resolved_from_kmux_worktree(&repo, existing)?;
        bail_existing_workspace(&target.branch, resolved)?;
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
        .window_exists_by_name_by_id(&tmux.session_id, &window_name)?
    {
        bail!(
            "tmux window '{}' already exists for workspace '{}'; remove it before creating the workspace",
            window_name,
            workspace_slug
        );
    }
    if worktree_path.exists() {
        bail!(
            "workspace path {} already exists for '{}'",
            worktree_path.display(),
            workspace_slug
        );
    }
    if repo.git.local_branch_exists(&target.branch)? {
        bail!(
            "branch '{}' already exists; kmux add creates new branch workspaces only",
            target.branch
        );
    }
    if target.branch == target.parent {
        bail!(
            "workspace branch '{}' cannot be its own parent",
            target.branch
        );
    }
    if !repo.git.local_branch_exists(&target.parent)? {
        bail!("parent branch '{}' does not exist locally", target.parent);
    }
    let state_store = WorkspaceStateStore::new(&repo.paths.git_common_dir);
    let state = state_store.load()?;
    validate_no_cycle(&state, &target.branch, &target.parent)?;
    repo.git
        .merge_base(&target.start_point, &target.parent)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "branches '{}' and '{}' have no merge base",
                target.branch,
                target.parent
            )
        })?;

    repo.git.ensure_available_worktree_path(&worktree_path)?;
    repo.git
        .ensure_local_branch(&target.branch, Some(&target.start_point))?;
    repo.git.add_worktree(&worktree_path, &target.branch)?;
    apply_file_operations(&repo.config, &repo.paths.main_worktree, &worktree_path)?;
    run_post_create(
        &repo.config,
        &repo.paths.main_worktree,
        &worktree_path,
        &workspace_slug,
    )?;

    let resolved = WorkspaceRecord::from_created_kmux_workspace(
        workspace_slug,
        worktree_path,
        target.branch.clone(),
    )?;
    let window = create_shell(&repo, &tmux, &resolved)?;
    record_parent(&repo, &target.branch, &target.parent)?;
    if let Some(launcher) = &launcher {
        start_launcher(&tmux, &window, launcher, resolved.path()).with_context(|| {
            format!(
                "launcher {:?} handoff failed; its process may already be running if spawn acknowledgment timed out; workspace files, parent metadata, and its shell window remain available; inspect the window before manual recovery",
                launcher.name()
            )
        })?;
    }
    if !args.background {
        select_created(&tmux, &window)?;
    }
    println!(
        "created {}\t{}",
        resolved.workspace_slug(),
        resolved.path().display()
    );
    Ok(())
}

// Existing kmux worktrees are create-only conflicts, even when the tmux window
// is gone. `kmux restore` owns tmux reconciliation for those cases.
fn bail_existing_workspace(expected_branch: &str, resolved: WorkspaceRecord) -> Result<()> {
    if resolved.branch() != Some(expected_branch) {
        bail!(
            "workspace slug '{}' already exists at {} for branch '{}', not '{}'",
            resolved.workspace_slug(),
            resolved.path().display(),
            resolved.branch().unwrap_or("<unknown>"),
            expected_branch
        );
    }
    bail!(
        "workspace for '{}' already exists at {}; use 'kmux restore' to restore tmux windows",
        expected_branch,
        resolved.path().display()
    );
}

struct AddTarget {
    branch: String,
    start_point: String,
    parent: String,
}

impl AddTarget {
    // Resolve remote-tracking input into the local branch name to create while
    // preserving the remote ref as the start point.
    fn resolve(repo: &super::context::RepoContext, args: &cli::AddArgs) -> Result<Self> {
        let parent = args
            .parent
            .as_ref()
            .cloned()
            .map(Ok)
            .unwrap_or_else(|| repo.git.require_current_branch())?;

        if let Some(remote) = repo.git.known_remote_branch(&args.branch)? {
            return Ok(Self {
                branch: remote.branch,
                start_point: remote.ref_name,
                parent,
            });
        }

        Ok(Self {
            branch: args.branch.clone(),
            start_point: parent.clone(),
            parent,
        })
    }
}
