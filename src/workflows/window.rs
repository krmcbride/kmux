use anyhow::Result;

use super::context::{RepoContext, TmuxContext};
use super::files::startup_command;
use super::metadata::set_worktree_metadata;
use super::resolve::ResolvedWorktree;

pub(super) fn open_resolved(
    repo: &RepoContext,
    tmux: &TmuxContext,
    resolved: &ResolvedWorktree,
    focus: bool,
) -> Result<()> {
    let window_name = repo.config.window_name(&resolved.handle);
    if tmux
        .tmux
        .window_exists_by_name(&tmux.session_name, &window_name)?
    {
        set_worktree_metadata(&tmux.tmux, &tmux.session_name, &window_name, resolved)?;
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
    set_worktree_metadata(&tmux.tmux, &tmux.session_name, &window_name, resolved)?;
    if focus {
        tmux.tmux.select_window(&tmux.session_name, &window_name)?;
    }
    Ok(())
}
