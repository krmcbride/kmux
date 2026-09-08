use anyhow::{Context, Result};

use super::context::load_repo_context;
use super::launch::resolve_default;
use super::project_session;
use super::resolve::restorable_workspaces;
use super::window::{RestoreWindow, restore_shell, start_launcher};

/// Restore all live external worktrees and remembered workspace presentations.
pub(super) fn run() -> Result<()> {
    let repo = load_repo_context()?;
    // Restore intentionally ignores any one-shot launcher used by `create`: only
    // the current configured default applies to newly recreated windows.
    let launcher = resolve_default(&repo.config);
    let tmux = project_session::resolve(&repo.paths)?.require("kmux workspace restore")?;
    let workspaces = restorable_workspaces(&repo)?;

    if workspaces.is_empty() {
        println!("restored 0 workspaces");
        return Ok(());
    }

    for workspace in workspaces {
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
