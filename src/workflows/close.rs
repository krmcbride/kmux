use anyhow::Result;

use crate::cli;

use super::context::{load_repo_context, load_tmux_context};
use super::resolve::resolve_worktree;

pub(super) fn run(args: cli::NameArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let resolved = resolve_worktree(&repo, &args.name)?;
    let window_name = repo.config.window_name(&resolved.handle);

    tmux.tmux.kill_window(&tmux.session_name, &window_name)?;
    println!("closed {}", resolved.handle);
    Ok(())
}
