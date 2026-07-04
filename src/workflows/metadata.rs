use anyhow::Result;

use crate::tmux::{KMUX_WORKSPACE_PATH_OPTION, Tmux, window_target};

use super::resolve::ResolvedWorkspace;

/// Set kmux's recoverable workspace marker on a named tmux window in a session.
pub(super) fn set_workspace_path_marker(
    tmux: &Tmux,
    session_name: &str,
    window_name: &str,
    resolved: &ResolvedWorkspace,
) -> Result<()> {
    let target = window_target(session_name, window_name);
    set_workspace_path_marker_target(tmux, &target, resolved)
}

/// Set kmux's recoverable workspace marker on an arbitrary tmux target.
pub(super) fn set_workspace_path_marker_target(
    tmux: &Tmux,
    target: &str,
    resolved: &ResolvedWorkspace,
) -> Result<()> {
    tmux.set_window_option(
        target,
        KMUX_WORKSPACE_PATH_OPTION,
        &resolved.path.display().to_string(),
    )?;
    Ok(())
}
