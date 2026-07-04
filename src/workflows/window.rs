use std::path::Path;

use anyhow::{Result, bail};

use super::context::{RepoContext, TmuxContext};
use super::files::startup_command;
use super::metadata::{set_workspace_path_marker, set_workspace_path_marker_target};
use super::resolve::ResolvedWorkspace;
use crate::paths::same_path;
use crate::tmux::TmuxWindow;

/// Create a tmux window for a resolved workspace and attach kmux metadata options.
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
    set_workspace_path_marker(&tmux.tmux, &tmux.session_name, &window_name, resolved)?;
    if focus {
        tmux.tmux.select_window(&tmux.session_name, &window_name)?;
    }
    Ok(())
}

/// Ensure a resolved workspace has exactly one matching tmux window with fresh metadata.
pub(super) fn restore_resolved(
    repo: &RepoContext,
    tmux: &TmuxContext,
    resolved: &ResolvedWorkspace,
) -> Result<()> {
    let window_name = repo.config.workspace_window_name(&resolved.workspace_slug);
    let windows = tmux.tmux.list_windows(Some(&tmux.session_name))?;
    let expected_windows = windows
        .iter()
        .filter(|window| window.window_name == window_name)
        .collect::<Vec<_>>();

    if expected_windows.len() > 1 {
        bail!(
            "multiple tmux windows are named '{}' for workspace '{}'; remove duplicates before restoring",
            window_name,
            resolved.workspace_slug
        );
    }

    let matches = windows
        .iter()
        .filter(|window| window_matches_workspace(window, resolved))
        .collect::<Vec<_>>();

    if let Some(expected_window) = expected_windows.first() {
        if matches
            .iter()
            .any(|window| window.window_id != expected_window.window_id)
        {
            bail!(
                "multiple tmux windows match workspace '{}'; remove duplicates before restoring",
                resolved.workspace_slug
            );
        }
        set_workspace_path_marker_target(&tmux.tmux, &expected_window.window_id, resolved)?;
        return Ok(());
    }

    match matches.as_slice() {
        [] => {
            let command = startup_command(&repo.config);
            tmux.tmux.create_window_with_command(
                &tmux.session_name,
                &window_name,
                &resolved.path,
                command,
            )?;
            set_workspace_path_marker(&tmux.tmux, &tmux.session_name, &window_name, resolved)?;
        }
        [window] => {
            tmux.tmux.rename_window(&window.window_id, &window_name)?;
            set_workspace_path_marker_target(&tmux.tmux, &window.window_id, resolved)?;
        }
        _ => bail!(
            "multiple tmux windows match workspace '{}'; remove duplicates before restoring",
            resolved.workspace_slug
        ),
    }

    Ok(())
}

// Restore accepts the live path marker as a repair hint. Git worktree state and
// the configured window name remain the source of truth for workspace identity.
fn window_matches_workspace(window: &TmuxWindow, resolved: &ResolvedWorkspace) -> bool {
    window
        .kmux_workspace_path
        .as_deref()
        .is_some_and(|path| same_path(Path::new(path), &resolved.path))
}
