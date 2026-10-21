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
use crate::config::Config;
use crate::git::Git;
use crate::launcher::{PendingLaunch, ResolvedLauncher};
use crate::paths::same_path;
use crate::tmux::{Tmux, TmuxWindow};
use crate::workspace::WorkspaceRecord;

const WORKSPACE_ID_OPTION: &str = "@kmux_workspace_id";

/// A newly-created detached shell window that can receive one hidden ingress.
pub(super) struct CreatedWindow {
    window_id: String,
    pane_id: String,
}

/// Whether restore found an existing window or created a missing shell window.
pub(super) enum RestoreWindow {
    Existing(String),
    Created(CreatedWindow),
}

/// Create a detached shell window for a resolved workspace.
pub(super) fn create_shell(
    repo: &RepoContext,
    tmux: &TmuxContext,
    resolved: &WorkspaceRecord,
) -> Result<CreatedWindow> {
    let window_name = presentation_name(&repo.config, resolved);
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
    let window_id = tmux.tmux.pane_context(&pane_id)?.window_id;
    tmux.tmux
        .set_window_option(&window_id, WORKSPACE_ID_OPTION, resolved.policy().id())?;
    Ok(CreatedWindow { window_id, pane_id })
}

/// Return an existing expected window unchanged or create its missing shell window.
pub(super) fn restore_shell(
    repo: &RepoContext,
    tmux: &TmuxContext,
    resolved: &WorkspaceRecord,
) -> Result<RestoreWindow> {
    if let Some(window) = find_existing(&tmux.tmux, &tmux.session_id, &repo.config, resolved)? {
        let name = presentation_name(&repo.config, resolved);
        if window.window_name != name {
            tmux.tmux.rename_window(&window.window_id, &name)?;
        }
        return Ok(RestoreWindow::Existing(window.window_id));
    }

    create_shell(repo, tmux, resolved).map(RestoreWindow::Created)
}

/// Resolve presentation by stable ID, admitting legacy names only with path evidence.
pub(super) fn find_existing(
    tmux: &Tmux,
    session_id: &str,
    config: &Config,
    workspace: &WorkspaceRecord,
) -> Result<Option<TmuxWindow>> {
    let name = presentation_name(config, workspace);
    let windows = tmux.list_windows_by_id(session_id)?;
    let named = windows
        .iter()
        .filter(|window| window.window_name == name)
        .collect::<Vec<_>>();
    if named.len() > 1 {
        bail!(
            "multiple tmux windows are named '{}' for workspace '{}'; close duplicate windows before continuing",
            name,
            workspace.workspace_slug()
        );
    }
    let mut bound = Vec::new();
    for window in &windows {
        if tmux
            .show_window_option(&window.window_id, WORKSPACE_ID_OPTION)?
            .as_deref()
            == Some(workspace.policy().id())
        {
            bound.push(window);
        }
    }
    if bound.len() > 1 {
        bail!(
            "multiple tmux windows are bound to workspace '{}'; close duplicates first",
            workspace.policy().id()
        );
    }
    let candidate = match (bound.first(), named.first()) {
        (Some(bound), Some(named)) if bound.window_id != named.window_id => {
            bail!("tmux window name '{}' conflicts with another window", name)
        }
        (Some(bound), _) => Some(*bound),
        (_, Some(named)) => Some(*named),
        _ => None,
    };
    let Some(window) = candidate else {
        return Ok(None);
    };
    let panes = tmux.list_panes()?;
    if panes.iter().any(|pane| {
        pane.identity.window_id == window.window_id && pane.identity.session_id != session_id
    }) {
        bail!(
            "workspace presentation is linked into another tmux session; unlink it before continuing"
        );
    }
    if bound.is_empty() {
        if tmux
            .show_window_option(&window.window_id, WORKSPACE_ID_OPTION)?
            .is_some()
        {
            bail!("tmux window '{}' belongs to another workspace", name);
        }
        let content = panes
            .iter()
            .filter(|pane| {
                pane.identity.window_id == window.window_id
                    && pane.kmux_role.as_deref() != Some("sidebar")
            })
            .collect::<Vec<_>>();
        if content.is_empty()
            || !content.iter().all(|pane| {
                pane.placement
                    .current_path
                    .as_deref()
                    .and_then(|path| Git::new(path).worktree_root().ok())
                    .is_some_and(|root| same_path(&root, workspace.path()))
            })
        {
            bail!(
                "tmux window '{}' has no verified presentation for workspace '{}'; rename or close the conflicting window",
                name,
                workspace.workspace_slug()
            );
        }
        tmux.set_window_option(
            &window.window_id,
            WORKSPACE_ID_OPTION,
            workspace.policy().id(),
        )?;
    }
    Ok(Some(window.clone()))
}

/// Compose configured naming with explicit ephemeral retention.
pub(super) fn presentation_name(config: &Config, workspace: &WorkspaceRecord) -> String {
    config.workspace_window_name(&workspace.policy().presentation_slug())
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
        .select_window_id_in_session(&tmux.session_id, &window.window_id)
}
