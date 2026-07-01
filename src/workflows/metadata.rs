use anyhow::Result;

use crate::tmux::{
    KMUX_WORKSPACE_BRANCH_OPTION, KMUX_WORKSPACE_PATH_OPTION, KMUX_WORKSPACE_SLUG_OPTION, Tmux,
    window_target,
};

use super::resolve::ResolvedWorkspace;

/// Set kmux workspace metadata on a named tmux window in a session.
pub(super) fn set_workspace_metadata(
    tmux: &Tmux,
    session_name: &str,
    window_name: &str,
    resolved: &ResolvedWorkspace,
) -> Result<()> {
    let target = window_target(session_name, window_name);
    set_workspace_metadata_target(tmux, &target, resolved)
}

/// Set kmux workspace metadata on an arbitrary tmux target.
pub(super) fn set_workspace_metadata_target(
    tmux: &Tmux,
    target: &str,
    resolved: &ResolvedWorkspace,
) -> Result<()> {
    tmux.set_window_option(target, KMUX_WORKSPACE_SLUG_OPTION, &resolved.workspace_slug)?;
    tmux.set_window_option(
        target,
        KMUX_WORKSPACE_PATH_OPTION,
        &resolved.path.display().to_string(),
    )?;
    if let Some(branch) = &resolved.branch {
        tmux.set_window_option(target, KMUX_WORKSPACE_BRANCH_OPTION, branch)?;
    } else {
        tmux.unset_window_option(target, KMUX_WORKSPACE_BRANCH_OPTION)?;
    }
    Ok(())
}
