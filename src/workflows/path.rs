use anyhow::Result;

use crate::cli;

use super::context::load_repo_context;
use super::resolve::resolve_worktree;

pub(super) fn run(args: cli::NameArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let resolved = resolve_worktree(&repo, &args.name)?;

    println!("{}", resolved.path.display());
    Ok(())
}
