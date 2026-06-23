use anyhow::Result;

use crate::tmux::{Tmux, kmux_worktree_option, window_target};

use super::resolve::ResolvedWorktree;

pub(super) fn set_worktree_metadata(
    tmux: &Tmux,
    session_name: &str,
    window_name: &str,
    resolved: &ResolvedWorktree,
) -> Result<()> {
    let target = window_target(session_name, window_name);
    tmux.set_window_option(
        &target,
        &kmux_worktree_option(&resolved.handle, "handle")?,
        &resolved.handle,
    )?;
    tmux.set_window_option(
        &target,
        &kmux_worktree_option(&resolved.handle, "path")?,
        &resolved.path.display().to_string(),
    )?;
    if let Some(branch) = &resolved.branch {
        tmux.set_window_option(
            &target,
            &kmux_worktree_option(&resolved.handle, "branch")?,
            branch,
        )?;
    }
    Ok(())
}

pub(super) fn clear_worktree_metadata(
    tmux: &Tmux,
    session_name: &str,
    window_name: &str,
    resolved: &ResolvedWorktree,
) -> Result<()> {
    let target = window_target(session_name, window_name);
    for field in ["handle", "path", "branch"] {
        tmux.unset_window_option(&target, &kmux_worktree_option(&resolved.handle, field)?)?;
    }
    Ok(())
}
