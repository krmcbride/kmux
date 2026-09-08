//! Create the ephemeral default or the explicit persistent branch preset.

use anyhow::{Context, Result, bail};

use crate::cli;
use crate::git::Git;
use crate::paths::EphemeralAllocation;
use crate::slug::workspace_slug_from_branch;
use crate::state::workspace::WorkspaceStateStore;
use crate::workspace::{Retention, WorkspacePolicy, WorkspaceRecord, validate_label};

use super::context::{RepoContext, load_repo_context};
use super::files::{apply_file_operations, run_post_create};
use super::launch::resolve_create;
use super::project_session::{self, TmuxContext};
use super::resolve::{
    find_kmux_workspace_by_name, find_kmux_workspace_by_slug, load_workspace_state,
    resolve_workspace, resolved_from_kmux_worktree,
};
use super::set_parent::{record_parent, validate_no_cycle};
use super::window::{create_shell, select_created, start_launcher};

/// Create a worktree, persist its explicit policy, and then run configured setup and presentation.
pub(super) fn run(args: cli::CreateArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let launcher = resolve_create(&repo.config, &args)?;
    let tmux = project_session::resolve(&repo.paths)?.require("kmux workspace create")?;
    if !args.background && !tmux.is_ambient {
        bail!(
            "kmux workspace create cannot focus resolved tmux session '{}' because the caller is not attached to it; pass --background",
            tmux.session_name
        );
    }
    let created = if let Some(branch) = args.branch.as_deref() {
        create_persistent(&repo, &tmux, &args, branch)?
    } else {
        create_ephemeral(&repo, &tmux, &args)?
    };
    let workspace = &created.workspace;
    apply_file_operations(&repo.config, &repo.paths.main_worktree, workspace.path())?;
    run_post_create(
        &repo.config,
        &repo.paths.main_worktree,
        workspace.path(),
        workspace.workspace_slug(),
    )?;
    let window = create_shell(&repo, &tmux, workspace)?;
    if let Some((branch, parent)) = created.branch_parent {
        record_parent(&repo, &branch, &parent)?;
    }
    if let Some(launcher) = &launcher {
        start_launcher(&tmux, &window, launcher, workspace.path()).with_context(|| {
            format!("launcher {:?} handoff failed; its process may already be running if spawn acknowledgment timed out; workspace files, parent metadata, and its shell window remain available; inspect the window before manual recovery", launcher.name())
        })?;
    }
    if !args.background {
        select_created(&tmux, &window)?;
    }
    let retention = if workspace.policy().retention() == Some(Retention::Ephemeral) {
        "ephemeral "
    } else {
        ""
    };
    println!(
        "created {retention}{}\t{}",
        workspace.policy().label(),
        workspace.path().display()
    );
    Ok(())
}

struct CreatedWorkspace {
    workspace: WorkspaceRecord,
    branch_parent: Option<(String, String)>,
}

// The default starts from the caller's checkout, including a detached source.
// Reserve the directory exclusively, and preserve it once Git creation begins.
fn create_ephemeral(
    repo: &RepoContext,
    tmux: &TmuxContext,
    args: &cli::CreateArgs,
) -> Result<CreatedWorkspace> {
    let reference = args
        .from
        .as_deref()
        .or(args.parent.as_deref())
        .unwrap_or("HEAD");
    let anchor = Git::new(&repo.paths.current_worktree).resolve_commit(reference)?;
    if let Some(name) = &args.name {
        validate_label(name)?;
    }
    let (mut state, _) = load_workspace_state(repo)?;
    let allocation =
        EphemeralAllocation::reserve(&repo.config.worktree_root()?, &repo.paths.main_worktree)?;
    let label = args
        .name
        .clone()
        .unwrap_or_else(|| allocation.id().to_owned());
    let planned = WorkspacePolicy::owned(
        format!("ws-{}", allocation.id()),
        allocation.path().to_path_buf(),
        label.clone(),
        Retention::Ephemeral,
        Some(anchor.clone()),
        None,
    )?;
    // Validate label/path collisions without persisting speculative ownership.
    state.clone().upsert_policy(planned.clone())?;
    ensure_window_available(repo, tmux, &planned.presentation_slug())?;
    let path = allocation.keep();
    repo.git.add_detached_worktree(&path, &anchor)?;
    let mut policy = WorkspacePolicy::owned(
        repo.git.claim_worktree(&path)?,
        path.clone(),
        label,
        Retention::Ephemeral,
        Some(anchor),
        None,
    )?;
    policy.set_allocation_directory(
        path.parent()
            .context("allocation path has no parent")?
            .to_path_buf(),
    );
    state.upsert_policy(policy)?;
    WorkspaceStateStore::new(&repo.paths.git_common_dir).save(&state)?;
    Ok(CreatedWorkspace {
        workspace: resolve_workspace(repo, &path.to_string_lossy())?,
        branch_parent: None,
    })
}

// Keep branch, remote-tracking, parent, and sibling-layout compatibility explicit.
fn create_persistent(
    repo: &RepoContext,
    tmux: &TmuxContext,
    args: &cli::CreateArgs,
    branch: &str,
) -> Result<CreatedWorkspace> {
    let target = PersistentTarget::resolve(repo, args, branch)?;
    let label = workspace_slug_from_branch(&target.branch)?;
    let path = repo.paths.workspace_path(&label);
    if let Some(existing) = find_kmux_workspace_by_name(repo, &target.branch)? {
        bail_existing_workspace(&target.branch, resolved_from_kmux_worktree(repo, existing)?)?;
    }
    if let Some(existing) = find_kmux_workspace_by_slug(repo, &label)? {
        bail_existing_workspace(&target.branch, resolved_from_kmux_worktree(repo, existing)?)?;
    }
    if let Some(existing) = repo.git.find_worktree_by_branch(&target.branch)? {
        bail!(
            "branch '{}' is already checked out outside kmux at {}",
            target.branch,
            existing.path.display()
        );
    }
    ensure_window_available(repo, tmux, &label)?;
    if path.exists() {
        bail!(
            "workspace path {} already exists for '{}'",
            path.display(),
            label
        );
    }
    if repo.git.local_branch_exists(&target.branch)? {
        bail!(
            "branch '{}' already exists; kmux workspace create creates new branch workspaces only",
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
    let (mut state, _) = load_workspace_state(repo)?;
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
    let anchor = repo.git.resolve_commit(&target.start_point)?;
    // The persistent preset must not collide with a closed workspace's label.
    let planned = WorkspacePolicy::owned(
        "ws-pending-persistent".to_owned(),
        path.clone(),
        label.clone(),
        Retention::Persistent,
        Some(anchor.clone()),
        Some(target.branch.clone()),
    )?;
    state.clone().upsert_policy(planned)?;
    repo.git.ensure_available_worktree_path(&path)?;
    repo.git
        .ensure_local_branch(&target.branch, Some(&target.start_point))?;
    repo.git.add_worktree(&path, &target.branch)?;
    state.upsert_policy(WorkspacePolicy::owned(
        repo.git.claim_worktree(&path)?,
        path.clone(),
        label,
        Retention::Persistent,
        Some(anchor),
        Some(target.branch.clone()),
    )?)?;
    WorkspaceStateStore::new(&repo.paths.git_common_dir).save(&state)?;
    Ok(CreatedWorkspace {
        workspace: resolve_workspace(repo, &path.to_string_lossy())?,
        branch_parent: Some((target.branch, target.parent)),
    })
}

fn ensure_window_available(repo: &RepoContext, tmux: &TmuxContext, slug: &str) -> Result<()> {
    let name = repo.config.workspace_window_name(slug);
    if tmux
        .tmux
        .window_exists_by_name_by_id(&tmux.session_id, &name)?
    {
        bail!(
            "tmux window '{}' already exists for workspace '{}'; remove it before creating the workspace",
            name,
            slug
        );
    }
    Ok(())
}

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
        "workspace for '{}' already exists at {}; use 'kmux workspace restore' to restore tmux windows",
        expected_branch,
        resolved.path().display()
    );
}

struct PersistentTarget {
    branch: String,
    start_point: String,
    parent: String,
}

impl PersistentTarget {
    fn resolve(repo: &RepoContext, args: &cli::CreateArgs, branch: &str) -> Result<Self> {
        let parent =
            args.parent.clone().map(Ok).unwrap_or_else(|| {
                Git::new(&repo.paths.current_worktree).require_current_branch()
            })?;
        if let Some(remote) = repo.git.known_remote_branch(branch)? {
            return Ok(Self {
                branch: remote.branch,
                start_point: remote.ref_name,
                parent,
            });
        }
        Ok(Self {
            branch: branch.to_owned(),
            start_point: parent.clone(),
            parent,
        })
    }
}
