//! Create the ephemeral default or the explicit persistent branch preset.

use anyhow::{Context, Result, bail};

use crate::cli;
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
use super::set_parent::{ResolvedSource, lineage_at, resolve_source};
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
    let workspace = if let Some(branch) = args.branch.as_deref() {
        create_persistent(&repo, &tmux, &args, branch)?
    } else {
        create_ephemeral(&repo, &tmux, &args)?
    };
    apply_file_operations(&repo.config, &repo.paths.main_worktree, workspace.path())?;
    run_post_create(
        &repo.config,
        &repo.paths.main_worktree,
        workspace.path(),
        workspace.workspace_slug(),
    )?;
    let window = create_shell(&repo, &tmux, &workspace)?;
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

// The default starts from the caller's checkout, including a detached source.
// Reserve the directory exclusively, and preserve it once Git creation begins.
fn create_ephemeral(
    repo: &RepoContext,
    tmux: &TmuxContext,
    args: &cli::CreateArgs,
) -> Result<WorkspaceRecord> {
    let source = resolve_source(
        repo,
        args.from.as_deref().or(args.parent.as_deref()),
        args.from.is_some(),
    )?;
    let anchor = source.commit.clone();
    let lineage = lineage_at(repo, &source, &anchor)?;
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
    let mut planned = WorkspacePolicy::owned(
        format!("ws-{}", allocation.id()),
        allocation.path().to_path_buf(),
        label.clone(),
        Retention::Ephemeral,
        Some(anchor.clone()),
        None,
    )?;
    planned.set_lineage(lineage.clone());
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
    policy.set_lineage(lineage);
    policy.set_allocation_directory(
        path.parent()
            .context("allocation path has no parent")?
            .to_path_buf(),
    );
    state.upsert_policy(policy)?;
    WorkspaceStateStore::new(&repo.paths.git_common_dir).save(&state)?;
    resolve_workspace(repo, &path.to_string_lossy())
}

// Keep branch, remote-tracking, parent, and sibling-layout compatibility explicit.
fn create_persistent(
    repo: &RepoContext,
    tmux: &TmuxContext,
    args: &cli::CreateArgs,
    branch: &str,
) -> Result<WorkspaceRecord> {
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
    let (mut state, _) = load_workspace_state(repo)?;
    let anchor = repo.git.resolve_commit(&target.start_point)?;
    let lineage = lineage_at(repo, &target.source, &anchor)?;
    // The persistent preset must not collide with a closed workspace's label.
    let mut planned = WorkspacePolicy::owned(
        "ws-pending-persistent".to_owned(),
        path.clone(),
        label.clone(),
        Retention::Persistent,
        Some(anchor.clone()),
        Some(target.branch.clone()),
    )?;
    planned.set_lineage(lineage.clone());
    state.clone().upsert_policy(planned)?;
    repo.git.ensure_available_worktree_path(&path)?;
    repo.git
        .ensure_local_branch(&target.branch, Some(&target.start_point))?;
    repo.git.add_worktree(&path, &target.branch)?;
    let mut policy = WorkspacePolicy::owned(
        repo.git.claim_worktree(&path)?,
        path.clone(),
        label,
        Retention::Persistent,
        Some(anchor),
        Some(target.branch),
    )?;
    policy.set_lineage(lineage);
    state.upsert_policy(policy)?;
    WorkspaceStateStore::new(&repo.paths.git_common_dir).save(&state)?;
    resolve_workspace(repo, &path.to_string_lossy())
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
    source: ResolvedSource,
}

impl PersistentTarget {
    fn resolve(repo: &RepoContext, args: &cli::CreateArgs, branch: &str) -> Result<Self> {
        let source = resolve_source(repo, args.parent.as_deref(), false)?;
        if let Some(remote) = repo.git.known_remote_branch(branch)? {
            return Ok(Self {
                branch: remote.branch,
                start_point: remote.ref_name,
                source,
            });
        }
        Ok(Self {
            branch: branch.to_owned(),
            start_point: source.commit.clone(),
            source,
        })
    }
}
