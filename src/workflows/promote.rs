//! In-place retention changes preserve Git state and running presentation paths.

use anyhow::{Context, Result, bail};

use crate::cli;
use crate::state::workspace::WorkspaceStateStore;
use crate::workspace::WorkspaceRecord;

use super::context::load_repo_context;
use super::project_session;
use super::resolve::{load_workspace_state, resolve_workspace};

/// Promote an owned ephemeral workspace without relocating or changing its checkout.
pub(super) fn run(args: cli::PromoteArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let resolution = project_session::resolve(&repo.paths)?;
    let workspace = resolve_workspace(
        &repo,
        args.target
            .as_deref()
            .unwrap_or(&repo.paths.current_worktree.to_string_lossy()),
    )?;
    if !workspace.is_live() {
        bail!("workspace is unavailable; promotion requires its original live registration");
    }
    let mut policy = workspace.policy().clone();
    policy.promote(args.name.as_deref())?;
    let promoted = WorkspaceRecord::from_policy(policy.clone(), workspace.git().cloned())?;
    let (mut state, _) = load_workspace_state(&repo)?;
    state.upsert_policy(policy)?;
    // Check both the original window and the proposed name before saving retention.
    let window = resolution.prepare_presentation_update(&repo.config, &workspace, &promoted)?;
    WorkspaceStateStore::new(&repo.paths.git_common_dir).save(&state)?;
    if let Some(window) = window {
        resolution
            .rename_prepared_window(&window, &super::window::presentation_name(&repo.config, &promoted))
            .context("workspace is persistent, but its window could not be renamed; restore will reconcile the name")?;
    }
    println!(
        "promoted {}\t{}",
        promoted.policy().label(),
        promoted.path().display()
    );
    Ok(())
}
