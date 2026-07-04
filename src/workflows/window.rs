use anyhow::{Result, bail};

use super::context::{RepoContext, TmuxContext};
use super::files::startup_command;
use super::resolve::ResolvedWorkspace;

/// Create a tmux window for a resolved workspace.
pub(super) fn create_resolved(
    repo: &RepoContext,
    tmux: &TmuxContext,
    resolved: &ResolvedWorkspace,
    focus: bool,
) -> Result<()> {
    let window_name = repo.config.workspace_window_name(&resolved.workspace_slug);
    if tmux
        .tmux
        .window_exists_by_name(&tmux.session_name, &window_name)?
    {
        bail!(
            "tmux window '{}' already exists for workspace '{}'; remove it before creating the workspace",
            window_name,
            resolved.workspace_slug
        );
    }

    let command = startup_command(&repo.config);
    tmux.tmux.create_window_with_command(
        &tmux.session_name,
        &window_name,
        &resolved.path,
        command,
    )?;
    if focus {
        tmux.tmux.select_window(&tmux.session_name, &window_name)?;
    }
    Ok(())
}

/// Ensure a resolved workspace has its expected tmux window.
pub(super) fn restore_resolved(
    repo: &RepoContext,
    tmux: &TmuxContext,
    resolved: &ResolvedWorkspace,
) -> Result<()> {
    let window_name = repo.config.workspace_window_name(&resolved.workspace_slug);
    let expected_windows = tmux
        .tmux
        .list_windows(Some(&tmux.session_name))?
        .iter()
        .filter(|window| window.window_name == window_name)
        .count();

    if expected_windows > 1 {
        bail!(
            "multiple tmux windows are named '{}' for workspace '{}'; remove duplicates before restoring",
            window_name,
            resolved.workspace_slug
        );
    }

    if expected_windows == 1 {
        return Ok(());
    }

    let command = startup_command(&repo.config);
    tmux.tmux.create_window_with_command(
        &tmux.session_name,
        &window_name,
        &resolved.path,
        command,
    )?;
    Ok(())
}
