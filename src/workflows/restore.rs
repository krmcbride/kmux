use anyhow::{Context, Result, bail};

use super::context::{load_repo_context, load_tmux_context};
use super::launch::resolve_default;
use super::resolve::strict_kmux_workspaces;
use super::window::{RestoreWindow, restore_shell, start_launcher};

/// Recreate or repair tmux windows for existing strict kmux Git worktrees only.
pub(super) fn run() -> Result<()> {
    let repo = load_repo_context()?;
    // Restore intentionally ignores any one-shot launcher used by `add`: only
    // the current configured default applies to newly recreated windows.
    let launcher = resolve_default(&repo.config);
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
        if let RestoreWindow::Created(window) = restore_shell(&repo, &tmux, &workspace)?
            && let Some(launcher) = &launcher
        {
            start_launcher(&tmux, &window, launcher, workspace.path()).with_context(|| {
                format!(
                    "default launcher {:?} handoff failed while restoring workspace {:?}; its process may already be running if spawn acknowledgment timed out; its shell window remains available; inspect the window before retrying; restore stopped",
                    launcher.name(),
                    workspace.workspace_slug()
                )
            })?;
        }
        println!(
            "restored {}\t{}",
            workspace.workspace_slug(),
            workspace.path().display()
        );
    }

    Ok(())
}
