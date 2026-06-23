use anyhow::Result;

use crate::cli;

use super::context::{load_repo_context, load_tmux_context};
use super::resolve::resolve_worktree;
use super::window::open_resolved;

pub(super) fn run(args: cli::NameArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let resolved = resolve_worktree(&repo, &args.name)?;

    open_resolved(&repo, &tmux, &resolved, true)?;
    println!("opened {}\t{}", resolved.handle, resolved.path.display());
    Ok(())
}
