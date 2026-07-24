//! Sidebar action intents and side-effect execution.
//!
//! Tmux navigation, selected-target option persistence, and deletion side
//! effects live here so `SidebarApp` can stay focused on UI state
//! transitions. Shared observation-surface fanout policy lives at the agent
//! boundary.

use anyhow::Result;

use super::lifecycle;
use super::model::{SidebarRow, SidebarRowIdentity};
use super::selection::{
    PersistedSelectionRollback, PreviousSelectionOption, SELECTED_TARGET_OPTION,
    decode_selected_target, encode_selected_target,
};
use crate::agent::sessions::{
    AgentTmuxTarget, AgentTmuxUnavailableReason, AgentTmuxWindowCandidate,
};
use crate::config::StatusIcons;
use crate::state::{AgentSessionKey, StateStore};
use crate::tmux::{Tmux, TmuxPane};

/// Intent to disable the sidebar after the current TUI process exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SidebarDisableIntent;

/// Intent to switch tmux focus to a selected sidebar row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidebarJumpIntent {
    pub(super) row: SidebarRow,
}

/// Intent to delete persisted observations represented by one workspace row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidebarDeleteWorkspaceRowIntent {
    pub(super) index: usize,
    pub(super) row: SidebarRow,
}

/// Intent to wake the hidden sidebar process for a tmux window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidebarWakeIntent {
    pub(super) window_id: String,
}

/// Result of a successful jump side-effect execution.
#[derive(Debug)]
pub(super) struct SidebarJumpOutcome {
    pub(super) row: SidebarRow,
    pub(super) persistence_warning: Option<String>,
}

/// Result of a failed jump side-effect execution.
#[derive(Debug)]
pub(super) struct SidebarJumpFailure {
    pub(super) error: anyhow::Error,
    pub(super) rollback_error: Option<anyhow::Error>,
}

/// Jump execution result separated from app state updates.
#[derive(Debug)]
pub(super) enum SidebarJumpExecution {
    Succeeded(Box<SidebarJumpOutcome>),
    Failed(SidebarJumpFailure),
}

/// Result of a successful deletion side-effect execution.
#[derive(Debug)]
pub(super) struct SidebarDeleteWorkspaceRowOutcome {
    pub(super) index: usize,
    pub(super) row: SidebarRow,
}

/// Concrete executor for sidebar actions that touch tmux, state, or notifications.
#[derive(Debug, Clone)]
pub(super) struct SidebarActions {
    tmux: Tmux,
    store: StateStore,
    status_icons: StatusIcons,
}

#[derive(Debug)]
struct SidebarJumpDestination {
    session_name: String,
    window_id: String,
    pane_ids: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PersistedSelectionRestore<'a> {
    Set(&'a str),
    Unset,
}

impl SidebarJumpIntent {
    /// Build a jump intent from the currently selected row.
    pub(super) fn new(row: SidebarRow) -> Self {
        Self { row }
    }
}

impl SidebarDeleteWorkspaceRowIntent {
    /// Build a workspace-row delete intent from the selected index and row.
    pub(super) fn new(index: usize, row: SidebarRow) -> Self {
        Self { index, row }
    }
}

impl SidebarWakeIntent {
    fn for_row(row: &SidebarRow) -> Option<Self> {
        (!row.window_id.trim().is_empty()).then(|| Self {
            window_id: row.window_id.clone(),
        })
    }
}

impl SidebarActions {
    /// Create an executor for sidebar actions.
    pub(super) fn new(tmux: Tmux, store: StateStore, status_icons: StatusIcons) -> Self {
        Self {
            tmux,
            store,
            status_icons,
        }
    }

    /// Return the current tmux context when the sidebar is running inside tmux.
    pub(super) fn current_context(&self) -> Option<crate::tmux::TmuxContext> {
        self.tmux.current_context().ok().flatten()
    }

    /// Return the persisted selected workspace row identity for a sidebar window.
    pub(super) fn persisted_selection_identity(
        &self,
        window_id: &str,
    ) -> Option<SidebarRowIdentity> {
        let value = self
            .tmux
            .show_window_option(window_id, SELECTED_TARGET_OPTION)
            .ok()
            .flatten()?;
        decode_selected_target(&value)
    }

    /// Return whether the selected-target option exists, even if the value is stale or invalid.
    pub(super) fn selection_option_exists(&self, window_id: &str) -> bool {
        self.tmux
            .show_window_option(window_id, SELECTED_TARGET_OPTION)
            .ok()
            .flatten()
            .is_some()
    }

    /// Persist the selected workspace row for a sidebar tmux window.
    pub(super) fn persist_selection_identity(
        &self,
        window_id: &str,
        identity: &SidebarRowIdentity,
    ) -> Result<()> {
        if window_id.trim().is_empty() {
            return Ok(());
        }

        let encoded = encode_selected_target(identity)?;
        self.tmux
            .set_window_option(window_id, SELECTED_TARGET_OPTION, encoded.as_str())
    }

    /// Execute tmux navigation, selection persistence, and wake effects for a jump.
    pub(super) fn execute_jump(&self, intent: SidebarJumpIntent) -> SidebarJumpExecution {
        let mut row = intent.row;
        let destination = match self.resolve_jump_destination(&row) {
            Ok(destination) => destination,
            Err(error) => {
                return SidebarJumpExecution::Failed(SidebarJumpFailure {
                    error,
                    rollback_error: None,
                });
            }
        };
        row.session_name.clone_from(&destination.session_name);
        row.window_id.clone_from(&destination.window_id);
        row.pane_id = None;
        let mut persistence_warning = None;
        let rollback = match self.persist_selection_before_jump(&row) {
            Ok(rollback) => rollback,
            Err(error) => {
                persistence_warning = Some(format!("selection state failed: {error}"));
                None
            }
        };

        row.pane_id = match self.select_row_target(&destination) {
            Ok(pane_id) => pane_id,
            Err(error) => {
                let rollback_error =
                    rollback.and_then(|rollback| self.restore_persisted_selection(rollback).err());
                return SidebarJumpExecution::Failed(SidebarJumpFailure {
                    error,
                    rollback_error,
                });
            }
        };

        self.clear_other_persisted_selections(&row.window_id);
        if let Some(intent) = SidebarWakeIntent::for_row(&row) {
            self.execute_wake_sidebar(intent);
        }
        SidebarJumpExecution::Succeeded(Box::new(SidebarJumpOutcome {
            row,
            persistence_warning,
        }))
    }

    /// Delete observations represented by a workspace row and notify dependent surfaces.
    pub(super) fn execute_delete_workspace_row(
        &self,
        intent: SidebarDeleteWorkspaceRowIntent,
    ) -> Result<SidebarDeleteWorkspaceRowOutcome> {
        delete_captured_member_sessions(&intent.row, |sessions| {
            self.store.delete_sessions(sessions)
        })?;
        crate::agent::refresh_observation_surfaces(&self.store, &self.tmux, &self.status_icons);
        Ok(SidebarDeleteWorkspaceRowOutcome {
            index: intent.index,
            row: intent.row,
        })
    }

    fn persist_selection_before_jump(
        &self,
        row: &SidebarRow,
    ) -> Result<Option<PersistedSelectionRollback>> {
        if row.window_id.trim().is_empty() {
            return Ok(None);
        }

        let previous = PreviousSelectionOption::from(
            self.tmux
                .show_window_option(&row.window_id, SELECTED_TARGET_OPTION)?,
        );
        let attempted = row.identity.clone();
        self.persist_selection_identity(&row.window_id, &attempted)?;

        Ok(Some(PersistedSelectionRollback {
            window_id: row.window_id.clone(),
            attempted,
            previous,
        }))
    }

    fn restore_persisted_selection(&self, rollback: PersistedSelectionRollback) -> Result<()> {
        let current = self
            .tmux
            .show_window_option(&rollback.window_id, SELECTED_TARGET_OPTION)?;
        let Some(restore) = persisted_selection_restore_decision(current.as_deref(), &rollback)
        else {
            return Ok(());
        };

        match restore {
            PersistedSelectionRestore::Set(value) => {
                self.tmux
                    .set_window_option(&rollback.window_id, SELECTED_TARGET_OPTION, value)
            }
            PersistedSelectionRestore::Unset => self
                .tmux
                .unset_window_option(&rollback.window_id, SELECTED_TARGET_OPTION),
        }
    }

    fn clear_other_persisted_selections(&self, selected_window_id: &str) {
        if selected_window_id.trim().is_empty() {
            return;
        }

        let Ok(windows) = self.tmux.list_windows(None) else {
            return;
        };
        let window_ids = windows.iter().map(|window| window.window_id.as_str());
        for window_id in other_window_cleanup_targets(selected_window_id, window_ids) {
            let _ = self
                .tmux
                .unset_window_option(window_id, SELECTED_TARGET_OPTION);
        }
    }

    fn resolve_jump_destination(&self, row: &SidebarRow) -> Result<SidebarJumpDestination> {
        // Reconciliation has already applied workspace, session, and preference policy. Enter
        // takes the first still-live candidate in that order instead of recalculating policy from
        // the user's current tmux context, which might belong to an unrelated scratch window.
        let (session_name, candidates) = jump_target_candidates(row)?;
        let live_windows = self
            .tmux
            .list_windows(Some(session_name))
            .map_err(|error| {
                missing_target_error(row, Some(format!("tmux lookup failed: {error}")))
            })?;
        let live_window_ids = live_windows
            .into_iter()
            .map(|window| window.window_id)
            .collect::<std::collections::BTreeSet<_>>();
        jump_destination_from_live_window_ids(row, session_name, candidates, &live_window_ids)
    }

    fn select_row_target(&self, destination: &SidebarJumpDestination) -> Result<Option<String>> {
        self.tmux
            .select_window_id_in_session(&destination.session_name, &destination.window_id)?;
        self.tmux
            .switch_client_to_session(&destination.session_name)?;
        Ok(self.focus_first_available_pane(&destination.window_id, &destination.pane_ids))
    }

    /// Pane focus is optional after the exact destination window is selected.
    fn focus_first_available_pane(
        &self,
        destination_window_id: &str,
        pane_ids: &[String],
    ) -> Option<String> {
        let live_panes = self.tmux.list_panes().ok()?;
        focus_first_available_pane_with(destination_window_id, pane_ids, &live_panes, |pane_id| {
            self.tmux.select_pane(pane_id)
        })
    }

    fn execute_wake_sidebar(&self, intent: SidebarWakeIntent) {
        let _ = lifecycle::wake_window(&self.tmux, &intent.window_id);
    }
}

fn jump_target_candidates(row: &SidebarRow) -> Result<(&str, &[AgentTmuxWindowCandidate])> {
    match &row.jump_target {
        AgentTmuxTarget::Windows {
            session_name,
            candidates,
        } => Ok((session_name, candidates)),
        AgentTmuxTarget::Unavailable(reason) => match reason {
            AgentTmuxUnavailableReason::Missing => Err(missing_target_error(row, None)),
            AgentTmuxUnavailableReason::CrossSession { session_names } => anyhow::bail!(
                "cannot jump to {}: matching windows span tmux sessions: {}",
                row.primary,
                session_names.join(", ")
            ),
        },
    }
}

fn jump_destination_from_live_window_ids(
    row: &SidebarRow,
    session_name: &str,
    candidates: &[AgentTmuxWindowCandidate],
    live_window_ids: &std::collections::BTreeSet<String>,
) -> Result<SidebarJumpDestination> {
    let candidate = candidates
        .iter()
        .find(|candidate| live_window_ids.contains(&candidate.window_id))
        .ok_or_else(|| missing_target_error(row, None))?;
    Ok(SidebarJumpDestination::from_candidate(
        session_name,
        candidate,
    ))
}

fn focus_first_available_pane_with<F>(
    destination_window_id: &str,
    pane_ids: &[String],
    live_panes: &[TmuxPane],
    mut focus: F,
) -> Option<String>
where
    F: FnMut(&str) -> Result<()>,
{
    let eligible_pane_ids = live_panes
        .iter()
        .filter(|pane| {
            pane.identity.window_id == destination_window_id
                && pane.kmux_role.as_deref() != Some("sidebar")
        })
        .map(|pane| pane.identity.pane_id.as_str())
        .collect::<std::collections::BTreeSet<_>>();
    pane_ids.iter().find_map(|pane_id| {
        (eligible_pane_ids.contains(pane_id.as_str()) && focus(pane_id).is_ok())
            .then(|| pane_id.clone())
    })
}

fn persisted_selection_restore_decision<'a>(
    current: Option<&str>,
    rollback: &'a PersistedSelectionRollback,
) -> Option<PersistedSelectionRestore<'a>> {
    let current_identity = current.and_then(decode_selected_target);
    if current_identity.as_ref() != Some(&rollback.attempted) {
        return None;
    }

    match &rollback.previous {
        PreviousSelectionOption::Value(value) => Some(PersistedSelectionRestore::Set(value)),
        PreviousSelectionOption::Unset => Some(PersistedSelectionRestore::Unset),
    }
}

fn other_window_cleanup_targets<'a>(
    selected_window_id: &str,
    window_ids: impl IntoIterator<Item = &'a str>,
) -> Vec<&'a str> {
    if selected_window_id.trim().is_empty() {
        return Vec::new();
    }

    window_ids
        .into_iter()
        .filter(|window_id| *window_id != selected_window_id)
        .collect()
}

fn delete_captured_member_sessions<F>(row: &SidebarRow, delete_sessions: F) -> Result<()>
where
    F: FnOnce(&[AgentSessionKey]) -> Result<()>,
{
    delete_sessions(&row.selection.member_session_keys)
}

fn missing_target_error(row: &SidebarRow, detail: Option<String>) -> anyhow::Error {
    let detail = detail
        .map(|detail| format!(" ({detail})"))
        .unwrap_or_default();
    anyhow::anyhow!(
        "cannot jump to {}: no live tmux window matches workspace {}; run `kmux workspace restore` if this is a managed workspace{detail}",
        row.primary,
        row.selection.workspace_key
    )
}

impl SidebarJumpDestination {
    fn from_candidate(session_name: &str, candidate: &AgentTmuxWindowCandidate) -> Self {
        Self {
            session_name: session_name.to_owned(),
            window_id: candidate.window_id.clone(),
            pane_ids: candidate.pane_ids.clone(),
        }
    }
}

#[cfg(feature = "internal-adapter-contract-tests")]
pub mod contract_tests {
    use std::thread;
    use std::time::{Duration, Instant};

    use anyhow::{Context, Result, bail};
    use tempfile::TempDir;

    use crate::agent::sessions::{AgentTmuxTarget, AgentTmuxWindowCandidate};
    use crate::agent::sidebar::model::contract_row;
    use crate::config::StatusIcons;
    use crate::state::{
        AgentLocationHints, AgentObservationKey, AgentObservationState, AgentSessionKey,
        AgentStatus,
    };
    use crate::tmux::contract_support::{TmuxFixture, create_test_session};

    use super::*;

    struct SidebarContractFixture {
        tmux_fixture: TmuxFixture,
        temp: TempDir,
        session_id: String,
        initial_pane_id: String,
        initial_window_id: String,
        store: StateStore,
    }

    impl SidebarContractFixture {
        fn new() -> Result<Self> {
            let tmux_fixture = TmuxFixture::new()?;
            let temp = TempDir::new()?;
            let initial_pane_id = create_test_session(&tmux_fixture.tmux, "project", temp.path())?;
            let initial = tmux_fixture
                .tmux
                .list_panes()?
                .into_iter()
                .find(|pane| pane.identity.pane_id == initial_pane_id)
                .context("expected initial sidebar contract pane")?;
            let store = crate::state::contract_store_with_path(temp.path().join("state"))?;
            Ok(Self {
                session_id: initial.identity.session_id,
                initial_window_id: initial.identity.window_id,
                initial_pane_id,
                tmux_fixture,
                temp,
                store,
            })
        }

        fn actions(&self) -> SidebarActions {
            SidebarActions::new(
                self.tmux_fixture.tmux.clone(),
                self.store.clone(),
                StatusIcons::default(),
            )
        }

        fn create_window(&self, name: &str) -> Result<(String, String)> {
            let pane_id = self.tmux_fixture.tmux.create_window_by_id(
                &self.session_id,
                name,
                self.temp.path(),
            )?;
            let window_id = self
                .tmux_fixture
                .tmux
                .list_panes()?
                .into_iter()
                .find(|pane| pane.identity.pane_id == pane_id)
                .map(|pane| pane.identity.window_id)
                .context("expected created sidebar contract window")?;
            Ok((pane_id, window_id))
        }
    }

    fn session_key(session_id: &str) -> AgentSessionKey {
        AgentSessionKey {
            agent_kind: "example-agent".to_owned(),
            session_id: session_id.to_owned(),
        }
    }

    fn row(
        workspace_key: &str,
        logical_session_id: &str,
        window_id: &str,
        pane_ids: Vec<String>,
    ) -> SidebarRow {
        contract_row(
            workspace_key,
            session_key(logical_session_id),
            "project",
            window_id,
            pane_ids,
        )
    }

    fn wait_for_path(path: &std::path::Path, require_content: bool) -> Result<()> {
        let started = Instant::now();
        let timeout = Duration::from_secs(10);
        loop {
            let ready = path
                .metadata()
                .is_ok_and(|metadata| !require_content || metadata.len() > 0);
            if ready {
                return Ok(());
            }
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                bail!(
                    "timed out after {elapsed:?} waiting for contract path {}",
                    path.display()
                );
            }
            thread::sleep(Duration::from_millis(25).min(timeout - elapsed));
        }
    }

    pub fn selection_options_round_trip_restore_and_cleanup() -> Result<()> {
        let fixture = SidebarContractFixture::new()?;
        let actions = fixture.actions();
        let (_, target_window_id) = fixture.create_window("target")?;
        let (_, other_window_id) = fixture.create_window("other")?;
        let previous = row(
            "/repo/project-alpha",
            "ses_previous",
            &target_window_id,
            vec![],
        );
        let attempted = row(
            "/repo/project-alpha__worktrees/feature-sidebar",
            "ses_attempted",
            &target_window_id,
            vec![],
        );
        let other = row("/repo/project-beta", "ses_other", &other_window_id, vec![]);

        actions.persist_selection_identity(&target_window_id, &previous.identity)?;
        actions.persist_selection_identity(&other_window_id, &other.identity)?;
        assert_eq!(
            actions.persisted_selection_identity(&target_window_id),
            Some(previous.identity.clone())
        );

        let rollback = actions
            .persist_selection_before_jump(&attempted)?
            .context("expected persisted selection rollback")?;
        assert_eq!(
            actions.persisted_selection_identity(&target_window_id),
            Some(attempted.identity)
        );
        actions.restore_persisted_selection(rollback)?;
        assert_eq!(
            actions.persisted_selection_identity(&target_window_id),
            Some(previous.identity)
        );

        actions.clear_other_persisted_selections(&target_window_id);
        assert!(actions.selection_option_exists(&target_window_id));
        assert!(!actions.selection_option_exists(&other_window_id));
        Ok(())
    }

    pub fn stale_jump_candidate_falls_back_and_focuses_content_pane() -> Result<()> {
        let fixture = SidebarContractFixture::new()?;
        let actions = fixture.actions();
        let (content_pane_id, target_window_id) = fixture.create_window("target")?;
        create_test_session(
            &fixture.tmux_fixture.tmux,
            "project-copy",
            fixture.temp.path(),
        )?;
        fixture.tmux_fixture.tmux.stdout([
            "link-window",
            "-s",
            &target_window_id,
            "-t",
            "project-copy:",
        ])?;
        let sidebar_pane_id =
            fixture
                .tmux_fixture
                .tmux
                .split_window_left(&target_window_id, 10, "/bin/sh")?;
        fixture
            .tmux_fixture
            .tmux
            .set_pane_option(&sidebar_pane_id, "@kmux_role", "sidebar")?;
        let mut selected = row(
            "/repo/project-alpha",
            "ses_selected",
            &target_window_id,
            vec![sidebar_pane_id, content_pane_id.clone()],
        );
        let AgentTmuxTarget::Windows { candidates, .. } = &mut selected.jump_target else {
            bail!("expected window candidates");
        };
        candidates.insert(
            0,
            AgentTmuxWindowCandidate {
                window_id: "@999999".to_owned(),
                pane_ids: vec!["%999999".to_owned()],
            },
        );

        let destination = actions.resolve_jump_destination(&selected)?;
        assert_eq!(destination.window_id, target_window_id);
        assert_eq!(
            actions
                .focus_first_available_pane(&destination.window_id, &destination.pane_ids)
                .as_deref(),
            Some(content_pane_id.as_str())
        );
        let linked_rows = fixture
            .tmux_fixture
            .tmux
            .list_windows(None)?
            .into_iter()
            .filter(|window| window.window_id == target_window_id)
            .count();
        assert_eq!(linked_rows, 2);
        Ok(())
    }

    pub fn failed_detached_jump_restores_previous_selection() -> Result<()> {
        let fixture = SidebarContractFixture::new()?;
        let actions = fixture.actions();
        let (pane_id, target_window_id) = fixture.create_window("target")?;
        let previous = row(
            "/repo/project-alpha",
            "ses_previous",
            &target_window_id,
            vec![],
        );
        let attempted = row(
            "/repo/project-alpha__worktrees/feature-sidebar",
            "ses_attempted",
            &target_window_id,
            vec![pane_id],
        );
        actions.persist_selection_identity(&target_window_id, &previous.identity)?;

        let execution = actions.execute_jump(SidebarJumpIntent::new(attempted));

        let SidebarJumpExecution::Failed(failure) = execution else {
            bail!("detached jump unexpectedly succeeded");
        };
        let failure_message = failure.error.to_string();
        assert!(
            failure_message.contains("no current client")
                || failure_message.contains("no clients")
                || failure_message.contains("not connected to a client"),
            "jump did not reach client switching: {failure_message}"
        );
        assert_eq!(
            actions.persisted_selection_identity(&target_window_id),
            Some(previous.identity)
        );
        Ok(())
    }

    pub fn delete_refreshes_badge_and_sidebar_surfaces_on_private_server() -> Result<()> {
        let fixture = SidebarContractFixture::new()?;
        let actions = fixture.actions();
        let ready = fixture.temp.path().join("sidebar-ready");
        let capture = fixture.temp.path().join("sidebar-key");
        let capture_command = format!(
            "sh -c 'stty raw -echo; : > \"$1\"; dd bs=1 count=1 of=\"$2\" 2>/dev/null' sh {} {}",
            crate::agent::sidebar::commands::shell_quote(&ready.display().to_string()),
            crate::agent::sidebar::commands::shell_quote(&capture.display().to_string()),
        );
        fixture
            .tmux_fixture
            .tmux
            .respawn_pane(&fixture.initial_pane_id, &capture_command)?;
        wait_for_path(&ready, false)?;
        fixture.tmux_fixture.tmux.set_pane_option(
            &fixture.initial_pane_id,
            "@kmux_role",
            "sidebar",
        )?;
        fixture.tmux_fixture.tmux.set_window_option(
            &fixture.initial_window_id,
            "@kmux_status",
            "sentinel",
        )?;
        let key = session_key("ses_delete");
        let observation = AgentObservationState {
            key: AgentObservationKey {
                session: key,
                reporter_kind: "example-reporter".to_owned(),
                reporter_instance: "instance-1".to_owned(),
            },
            created_at: 100,
            status: Some(AgentStatus::Working),
            status_observed_at: Some(100),
            status_changed_at: Some(100),
            working_elapsed_secs: 0,
            observed_at: 100,
            title: Some("Example task".to_owned()),
            context: None,
            target: AgentLocationHints {
                tmux_instance: Some(fixture.tmux_fixture.tmux.instance_id()),
                directory: Some(fixture.temp.path().display().to_string()),
                ..AgentLocationHints::default()
            },
        };
        let observation_key = observation.key.clone();
        fixture
            .store
            .mutate_observation(&observation_key, |_| Ok(Some(observation)))?;
        let selected = row(
            "/repo/project-alpha",
            "ses_delete",
            &fixture.initial_window_id,
            vec![fixture.initial_pane_id.clone()],
        );

        actions.execute_delete_workspace_row(SidebarDeleteWorkspaceRowIntent::new(0, selected))?;

        assert!(fixture.store.list_observations()?.is_empty());
        assert_eq!(
            fixture
                .tmux_fixture
                .tmux
                .show_window_option(&fixture.initial_window_id, "@kmux_status")?,
            None
        );
        wait_for_path(&capture, true)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sidebar::test_support::{
        report_state, row_from_view, set_session_key, set_workspace,
    };
    use crate::state::{AgentSessionKey, AgentStatus};
    use crate::tmux::{TmuxPaneIdentity, TmuxPanePlacement};
    use anyhow::Result;

    #[test]
    fn rollback_does_not_overwrite_newer_persisted_selection() -> Result<()> {
        let previous = server_row_in_window("ses_previous", "Previous", "@1");
        let attempted = server_row_in_window("ses_attempted", "Attempted", "@1");
        let newer = server_row_in_window("ses_newer", "Newer", "@1");
        let rollback = PersistedSelectionRollback {
            window_id: "@1".to_owned(),
            attempted: attempted.identity,
            previous: PreviousSelectionOption::Value(encode_selected_target(&previous.identity)?),
        };
        let newer_value = encode_selected_target(&newer.identity)?;

        assert_eq!(
            persisted_selection_restore_decision(Some(&newer_value), &rollback),
            None
        );
        Ok(())
    }

    #[test]
    fn rollback_restores_previous_value_or_unsets_when_attempt_is_still_current() -> Result<()> {
        let previous = server_row_in_window("ses_previous", "Previous", "@1");
        let attempted = server_row_in_window("ses_attempted", "Attempted", "@1");
        let previous_value = encode_selected_target(&previous.identity)?;
        let attempted_value = encode_selected_target(&attempted.identity)?;
        let restore_previous = PersistedSelectionRollback {
            window_id: "@1".to_owned(),
            attempted: attempted.identity.clone(),
            previous: PreviousSelectionOption::Value(previous_value.clone()),
        };
        let restore_unset = PersistedSelectionRollback {
            window_id: "@1".to_owned(),
            attempted: attempted.identity,
            previous: PreviousSelectionOption::Unset,
        };

        assert_eq!(
            persisted_selection_restore_decision(Some(&attempted_value), &restore_previous),
            Some(PersistedSelectionRestore::Set(&previous_value))
        );
        assert_eq!(
            persisted_selection_restore_decision(Some(&attempted_value), &restore_unset),
            Some(PersistedSelectionRestore::Unset)
        );
        Ok(())
    }

    #[test]
    fn successful_selection_only_cleans_other_window_persisted_targets() {
        assert_eq!(
            other_window_cleanup_targets("@2", ["@1", "@2", "@3"]),
            ["@1", "@3"]
        );
        assert!(other_window_cleanup_targets("", ["@1", "@2"]).is_empty());
    }

    #[test]
    fn jump_resolution_skips_a_candidate_window_that_disappeared() -> Result<()> {
        let mut row = server_row_in_window("ses_selected", "Selected", "@2");
        let AgentTmuxTarget::Windows { candidates, .. } = &mut row.jump_target else {
            anyhow::bail!("expected matching window candidates");
        };
        candidates.insert(
            0,
            AgentTmuxWindowCandidate {
                window_id: "@999999".to_owned(),
                pane_ids: vec!["%999999".to_owned()],
            },
        );
        let live_window_ids = std::collections::BTreeSet::from(["@2".to_owned()]);
        let (session_name, candidates) = jump_target_candidates(&row)?;

        let destination = jump_destination_from_live_window_ids(
            &row,
            session_name,
            candidates,
            &live_window_ids,
        )?;

        assert_eq!(destination.session_name, "project");
        assert_eq!(destination.window_id, "@2");
        Ok(())
    }

    #[test]
    fn stale_candidate_error_includes_restore_guidance() {
        let mut row = server_row_in_window("ses_stale", "Stale", "@999999");
        row.jump_target = AgentTmuxTarget::Windows {
            session_name: "project".to_owned(),
            candidates: vec![AgentTmuxWindowCandidate {
                window_id: "@999999".to_owned(),
                pane_ids: vec!["%999999".to_owned()],
            }],
        };
        let (session_name, candidates) =
            jump_target_candidates(&row).expect("window target should be valid");

        let error = jump_destination_from_live_window_ids(
            &row,
            session_name,
            candidates,
            &std::collections::BTreeSet::new(),
        )
        .expect_err("stale candidates should fail");

        assert!(error.to_string().contains("kmux workspace restore"));
    }

    #[test]
    fn unavailable_jump_fails_before_live_window_selection() {
        let mut unavailable = server_row_in_window("ses_missing", "Missing", "@1");
        unavailable.jump_target = AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::Missing);

        let error = jump_target_candidates(&unavailable).expect_err("missing target should fail");

        assert!(error.to_string().contains("kmux workspace restore"));
    }

    #[test]
    fn cross_session_jump_error_names_conflicting_sessions() {
        let mut row = server_row_in_window("ses_ambiguous", "Ambiguous", "@1");
        row.jump_target = AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::CrossSession {
            session_names: vec!["project-alpha".to_owned(), "project-beta".to_owned()],
        });

        let error = jump_target_candidates(&row).expect_err("ambiguous target should fail");

        assert!(error.to_string().contains("project-alpha, project-beta"));
    }

    #[test]
    fn sidebar_only_panes_are_not_eligible_for_focus() {
        let live_panes = [pane("@1", "%1", Some("sidebar"))];
        let mut attempts = Vec::new();

        let selected_pane =
            focus_first_available_pane_with("@1", &["%1".to_owned()], &live_panes, |pane_id| {
                attempts.push(pane_id.to_owned());
                Ok(())
            });

        assert_eq!(selected_pane, None);
        assert!(attempts.is_empty());
    }

    #[test]
    fn pane_focus_skips_ineligible_and_failed_candidates_before_next_success() {
        let live_panes = [
            pane("@1", "%sidebar", Some("sidebar")),
            pane("@1", "%first", None),
            pane("@1", "%second", None),
            pane("@2", "%other-window", None),
        ];
        let candidates = [
            "%stale".to_owned(),
            "%sidebar".to_owned(),
            "%other-window".to_owned(),
            "%first".to_owned(),
            "%second".to_owned(),
        ];
        let mut attempts = Vec::new();

        let selected_pane =
            focus_first_available_pane_with("@1", &candidates, &live_panes, |pane_id| {
                attempts.push(pane_id.to_owned());
                if pane_id == "%first" {
                    anyhow::bail!("pane disappeared before focus")
                }
                Ok(())
            });

        assert_eq!(selected_pane.as_deref(), Some("%second"));
        assert_eq!(attempts, ["%first", "%second"]);
    }

    #[test]
    fn deleting_workspace_row_uses_all_captured_members_but_not_later_arrivals() -> Result<()> {
        let mut row = server_row_in_window("ses_primary", "Primary", "@1");
        row.selection.member_session_keys = vec![
            session_key("codex", "ses_companion"),
            session_key("opencode", "ses_primary"),
            session_key("opencode", "ses_secondary"),
        ];
        let arrived_after_snapshot = session_key("codex", "ses_arrived_later");
        let mut deleted = Vec::new();

        delete_captured_member_sessions(&row, |sessions| {
            deleted.extend_from_slice(sessions);
            Ok(())
        })?;

        assert_eq!(deleted, row.selection.member_session_keys);
        assert!(!deleted.contains(&arrived_after_snapshot));
        Ok(())
    }

    #[test]
    fn captured_member_deletion_propagates_store_failure() {
        let row = server_row_in_window("ses_selected", "Selected", "@1");

        let error = delete_captured_member_sessions(&row, |_| anyhow::bail!("store unavailable"))
            .expect_err("store failure should stop deletion execution");

        assert_eq!(error.to_string(), "store unavailable");
    }

    fn server_row_in_window(session_id: &str, title: &str, window_id: &str) -> SidebarRow {
        let mut report = report_state(AgentStatus::Working, 100, window_id, "%server");
        set_session_key(&mut report, session_key("opencode", session_id));
        set_workspace(&mut report, format!("/repo/{window_id}/{session_id}"));
        report.title = Some(title.to_owned());
        report.target.tmux_pane_id = None;
        row_from_view(&report, 100)
    }

    fn session_key(agent_kind: &str, session_id: &str) -> AgentSessionKey {
        AgentSessionKey {
            agent_kind: agent_kind.to_owned(),
            session_id: session_id.to_owned(),
        }
    }

    fn pane(window_id: &str, pane_id: &str, kmux_role: Option<&str>) -> TmuxPane {
        TmuxPane {
            identity: TmuxPaneIdentity {
                session_id: "$1".to_owned(),
                window_id: window_id.to_owned(),
                pane_id: pane_id.to_owned(),
            },
            placement: TmuxPanePlacement {
                session_name: "project".to_owned(),
                window_name: "workspace".to_owned(),
                window_index: "1".to_owned(),
                pane_index: "0".to_owned(),
                current_path: Some("/repo/project".to_owned()),
            },
            kmux_role: kmux_role.map(str::to_owned),
        }
    }
}
