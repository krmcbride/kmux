use anyhow::{Result, bail};

use super::context::{load_repo_context, load_tmux_context};
use super::resolve::strict_kmux_workspaces;
use super::window::restore_resolved;

/// Recreate or repair tmux windows for existing strict kmux Git worktrees only.
pub(super) fn run() -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let workspaces = strict_kmux_workspaces(&repo)?;

    if workspaces.is_empty() {
        println!("restored 0 workspaces");
        return Ok(());
    }

    for workspace in workspaces {
        if workspace.branch().is_none() {
            bail!(
                "workspace '{}' has no known git branch and cannot be restored by kmux",
                workspace.workspace_slug()
            );
        }
        restore_resolved(&repo, &tmux, &workspace)?;
        println!(
            "restored {}\t{}",
            workspace.workspace_slug(),
            workspace.path().display()
        );
    }

    Ok(())
}
