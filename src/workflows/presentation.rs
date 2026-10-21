//! Open and close tmux presentations without changing worktree lifecycle policy.

use anyhow::{Context, Result, bail};

use crate::cli;
use crate::state::workspace::WorkspaceStateStore;
use crate::workspace::WorkspaceRecord;

use super::context::{RepoContext, load_repo_context};
use super::launch::resolve_launcher;
use super::project_session;
use super::resolve::{load_workspace_state, resolve_workspace};
use super::window::{RestoreWindow, restore_shell, select_created, start_launcher};

/// Open the current or selected registered worktree and remember its presentation.
pub(super) fn open(args: cli::OpenArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let launcher = resolve_launcher(
        &repo.config,
        args.launcher.as_deref(),
        args.launcher_input.as_deref(),
    )?;
    let tmux = project_session::resolve(&repo.paths)?.require("kmux workspace open")?;
    if !args.background && !tmux.is_ambient {
        bail!(
            "kmux workspace open cannot focus resolved tmux session '{}' because the caller is not attached to it; pass --background",
            tmux.session_name
        );
    }
    let workspace = resolve_target(&repo, args.target.as_deref())?;
    if !workspace.is_live() {
        bail!(
            "workspace '{}' is unavailable; its Git registration or checkout is missing",
            workspace.policy().label()
        );
    }
    // Validate collisions before recording intent, while keeping it durable
    // before shell creation or launcher handoff can partially succeed.
    super::window::find_existing(&tmux.tmux, &tmux.session_id, &repo.config, &workspace)?;
    remember(&repo, &workspace, true)?;
    match restore_shell(&repo, &tmux, &workspace)? {
        RestoreWindow::Existing(window_id) => {
            if !args.background {
                tmux.tmux
                    .select_window_id_in_session(&tmux.session_id, &window_id)?;
            }
        }
        RestoreWindow::Created(window) => {
            if let Some(launcher) = &launcher {
                start_launcher(&tmux, &window, launcher, workspace.path()).context("launcher handoff failed; its process may already be running and the shell window remains available; inspect it before retrying")?;
            }
            if !args.background {
                select_created(&tmux, &window)?;
            }
        }
    }
    println!(
        "opened {}\t{}",
        workspace.policy().label(),
        workspace.path().display()
    );
    Ok(())
}

/// Close only the managed presentation; an external checkout remains restorable.
pub(super) fn close(args: cli::CloseArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let resolution = project_session::resolve(&repo.paths)?;
    let workspace = resolve_target(&repo, args.target.as_deref())?;
    resolution.close_presentation(&repo.config, &workspace)?;
    remember(&repo, &workspace, false)?;
    println!("closed {}", workspace.policy().label());
    Ok(())
}

fn resolve_target(repo: &RepoContext, target: Option<&str>) -> Result<WorkspaceRecord> {
    resolve_workspace(
        repo,
        target.unwrap_or(&repo.paths.current_worktree.to_string_lossy()),
    )
}

fn remember(repo: &RepoContext, workspace: &WorkspaceRecord, presented: bool) -> Result<()> {
    let (mut state, _) = load_workspace_state(repo)?;
    let mut policy = workspace.policy().clone();
    policy.set_presentation(presented);
    state.upsert_policy(policy)?;
    WorkspaceStateStore::new(&repo.paths.git_common_dir).save(&state)
}
