use anyhow::Result;

use crate::tmux::{
    KMUX_WORKTREE_BRANCH_OPTION, KMUX_WORKTREE_HANDLE_OPTION, KMUX_WORKTREE_PATH_OPTION, Tmux,
    kmux_worktree_option, window_target,
};

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
    tmux.set_window_option(&target, KMUX_WORKTREE_HANDLE_OPTION, &resolved.handle)?;
    tmux.set_window_option(
        &target,
        &kmux_worktree_option(&resolved.handle, "path")?,
        &resolved.path.display().to_string(),
    )?;
    tmux.set_window_option(
        &target,
        KMUX_WORKTREE_PATH_OPTION,
        &resolved.path.display().to_string(),
    )?;
    if let Some(branch) = &resolved.branch {
        tmux.set_window_option(
            &target,
            &kmux_worktree_option(&resolved.handle, "branch")?,
            branch,
        )?;
        tmux.set_window_option(&target, KMUX_WORKTREE_BRANCH_OPTION, branch)?;
    } else {
        tmux.unset_window_option(&target, KMUX_WORKTREE_BRANCH_OPTION)?;
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
    for option in [
        KMUX_WORKTREE_HANDLE_OPTION,
        KMUX_WORKTREE_PATH_OPTION,
        KMUX_WORKTREE_BRANCH_OPTION,
    ] {
        tmux.unset_window_option(&target, option)?;
    }
    Ok(())
}
