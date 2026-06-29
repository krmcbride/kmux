use anyhow::{Result, bail};

use crate::cli;

use super::context::{load_repo_context, load_tmux_context};
use super::resolve::resolve_workspace;
use super::window::select_existing_resolved;

pub(super) fn run(args: cli::WorkspaceNameArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let resolved = resolve_workspace(&repo, &args.name)?;
    if resolved.branch.is_none() {
        bail!(
            "workspace '{}' has no known git branch and cannot be opened by kmux",
            resolved.workspace_slug
        );
    }

    select_existing_resolved(&repo, &tmux, &resolved)?;
    println!(
        "opened {}\t{}",
        resolved.workspace_slug,
        resolved.path.display()
    );
    Ok(())
}
