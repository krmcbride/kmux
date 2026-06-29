use anyhow::{Result, bail};

use super::context::{RepoContext, TmuxContext};
use super::files::startup_command;
use super::metadata::set_workspace_metadata;
use super::resolve::ResolvedWorkspace;

pub(super) fn open_or_create_resolved(
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
        set_workspace_metadata(&tmux.tmux, &tmux.session_name, &window_name, resolved)?;
        if focus {
            tmux.tmux.select_window(&tmux.session_name, &window_name)?;
        }
        return Ok(());
    }

    let command = startup_command(&repo.config);
    tmux.tmux.create_window_with_command(
        &tmux.session_name,
        &window_name,
        &resolved.path,
        command,
    )?;
    set_workspace_metadata(&tmux.tmux, &tmux.session_name, &window_name, resolved)?;
    if focus {
        tmux.tmux.select_window(&tmux.session_name, &window_name)?;
    }
    Ok(())
}

pub(super) fn select_existing_resolved(
    repo: &RepoContext,
    tmux: &TmuxContext,
    resolved: &ResolvedWorkspace,
) -> Result<()> {
    let window_name = repo.config.workspace_window_name(&resolved.workspace_slug);
    if !tmux
        .tmux
        .window_exists_by_name(&tmux.session_name, &window_name)?
    {
        bail!(
            "tmux window '{}' does not exist for workspace '{}'; use 'kmux add -o {}' to repair",
            window_name,
            resolved.workspace_slug,
            resolved
                .branch
                .as_deref()
                .unwrap_or(&resolved.workspace_slug)
        );
    }

    set_workspace_metadata(&tmux.tmux, &tmux.session_name, &window_name, resolved)?;
    tmux.tmux.select_window(&tmux.session_name, &window_name)?;
    Ok(())
}
