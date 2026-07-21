//! Shell-hosted workspace-window orchestration.
//!
//! A kmux window is created detached without a tmux start command, so tmux starts
//! its configured shell as the pane's long-lived process. An optional launcher is
//! then handed to that shell as the controlled hidden command
//! `kmux _launch <capability>`. The hidden ingress reads the real argv from private
//! transient storage, starts the launcher as the shell's foreground job with the
//! pane TTY, acknowledges spawn to the original create/restore process, and waits for
//! the launcher. When ingress exits, the original pane shell naturally resumes.
//!
//! This module owns workflow ordering around that mechanism: duplicate checks,
//! detached shell creation, optional launcher handoff, and later focus. Tmux
//! command syntax remains in `tmux`, while request transport and process lifetime
//! remain in `launcher`.

use std::path::Path;

use anyhow::{Result, bail};

use super::context::RepoContext;
use super::project_session::TmuxContext;
use crate::launcher::{PendingLaunch, ResolvedLauncher};
use crate::workspace::WorkspaceRecord;

/// A newly-created detached shell window that can receive one hidden ingress.
pub(super) struct CreatedWindow {
    window_name: String,
    pane_id: String,
}

/// Whether restore found an existing window or created a missing shell window.
pub(super) enum RestoreWindow {
    Existing,
    Created(CreatedWindow),
}

/// Create a detached shell window for a resolved workspace.
pub(super) fn create_shell(
    repo: &RepoContext,
    tmux: &TmuxContext,
    resolved: &WorkspaceRecord,
) -> Result<CreatedWindow> {
    let window_name = repo.config.workspace_window_name(resolved.workspace_slug());
    if tmux
        .tmux
        .window_exists_by_name_by_id(&tmux.session_id, &window_name)?
    {
        bail!(
            "tmux window '{}' already exists for workspace '{}'; remove it before creating the workspace",
            window_name,
            resolved.workspace_slug()
        );
    }

    let pane_id = tmux
        .tmux
        .create_window_by_id(&tmux.session_id, &window_name, resolved.path())?;
    Ok(CreatedWindow {
        window_name,
        pane_id,
    })
}

/// Return an existing expected window unchanged or create its missing shell window.
pub(super) fn restore_shell(
    repo: &RepoContext,
    tmux: &TmuxContext,
    resolved: &WorkspaceRecord,
) -> Result<RestoreWindow> {
    let window_name = repo.config.workspace_window_name(resolved.workspace_slug());
    let expected_windows = tmux
        .tmux
        .list_windows_by_id(&tmux.session_id)?
        .iter()
        .filter(|window| window.window_name == window_name)
        .count();

    if expected_windows > 1 {
        bail!(
            "multiple tmux windows are named '{}' for workspace '{}'; remove duplicates before restoring",
            window_name,
            resolved.workspace_slug()
        );
    }
    if expected_windows == 1 {
        return Ok(RestoreWindow::Existing);
    }

    create_shell(repo, tmux, resolved).map(RestoreWindow::Created)
}

/// Materialize, deliver, and await one launcher's spawn acknowledgment.
pub(super) fn start_launcher(
    tmux: &TmuxContext,
    window: &CreatedWindow,
    launcher: &ResolvedLauncher,
    cwd: &Path,
) -> Result<()> {
    let pending = PendingLaunch::create(launcher, cwd)?;
    let ingress_command = pending.ingress_command()?;
    tmux.tmux
        .send_literal_command(&window.pane_id, &ingress_command)?;
    pending.wait_for_spawn()
}

/// Select a newly-created window only after its optional launcher handoff.
pub(super) fn select_created(tmux: &TmuxContext, window: &CreatedWindow) -> Result<()> {
    tmux.tmux
        .select_window_by_id(&tmux.session_id, &window.window_name)
}
