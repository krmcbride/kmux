//! Sidebar action intents and side-effect execution.
//!
//! Tmux navigation, selected-target option persistence, hook execution, deletion,
//! badge refresh, and sidebar wake fanout live here so `SidebarApp` can stay
//! focused on UI state transitions.

use anyhow::Result;

use super::hooks::{SelectionHookInput, run_selection_hooks};
use super::lifecycle;
use super::model::{SidebarJumpTarget, SidebarRow, SidebarRowIdentity};
use super::selection::{
    PersistedSelectionRollback, PreviousSelectionOption, SELECTED_TARGET_OPTION,
    decode_selected_target, encode_selected_target,
};
use crate::agent::status_badges::refresh_window_statuses;
use crate::config::{SidebarSelectionHookConfig, StatusIcons};
use crate::state::{AgentObservationState, AgentSessionKey, StateStore};
use crate::tmux::Tmux;

/// Intent to disable the sidebar after the current TUI process exits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct SidebarDisableIntent;

/// Intent to switch tmux focus to a selected sidebar row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidebarJumpIntent {
    pub(super) row: SidebarRow,
}

/// Intent to delete persisted observations for a selected sidebar row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidebarDeleteSessionIntent {
    pub(super) index: usize,
    pub(super) row: SidebarRow,
}

/// Intent to wake the hidden sidebar process for a tmux window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidebarWakeIntent {
    pub(super) window_id: String,
}

/// Intent to run configured selection hooks for a sidebar row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct SidebarHookIntent {
    pub(super) row: SidebarRow,
}

/// Result of a successful jump side-effect execution.
#[derive(Debug)]
pub(super) struct SidebarJumpOutcome {
    pub(super) row: SidebarRow,
    pub(super) persistence_warning: Option<String>,
    pub(super) hook_error: Option<anyhow::Error>,
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
pub(super) struct SidebarDeleteOutcome {
    pub(super) index: usize,
    pub(super) row: SidebarRow,
}

#[derive(Debug, Clone)]
/// Concrete executor for sidebar actions that touch tmux, state, hooks, or notifications.
pub(super) struct SidebarActions {
    tmux: Tmux,
    store: StateStore,
    status_icons: StatusIcons,
    selection_hooks: Vec<SidebarSelectionHookConfig>,
}

impl SidebarJumpIntent {
    /// Build a jump intent from the currently selected row.
    pub(super) fn new(row: SidebarRow) -> Self {
        Self { row }
    }
}

impl SidebarDeleteSessionIntent {
    /// Build a delete intent from the selected index and row.
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

impl SidebarHookIntent {
    fn new(row: SidebarRow) -> Self {
        Self { row }
    }
}

impl SidebarActions {
    /// Create an executor for sidebar actions.
    pub(super) fn new(
        tmux: Tmux,
        store: StateStore,
        status_icons: StatusIcons,
        selection_hooks: Vec<SidebarSelectionHookConfig>,
    ) -> Self {
        Self {
            tmux,
            store,
            status_icons,
            selection_hooks,
        }
    }

    /// Return the current tmux context when the sidebar is running inside tmux.
    pub(super) fn current_context(&self) -> Option<crate::tmux::TmuxContext> {
        self.tmux.current_context().ok().flatten()
    }

    /// Return the persisted selected row identity for a host window, if present and valid.
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

    /// Execute tmux navigation, selection persistence, wake, and hook effects for a jump.
    pub(super) fn execute_jump(&self, intent: SidebarJumpIntent) -> SidebarJumpExecution {
        let row = intent.row;
        let mut persistence_warning = None;
        let rollback = match self.persist_selection_before_jump(&row) {
            Ok(rollback) => rollback,
            Err(error) => {
                persistence_warning = Some(format!("selection state failed: {error}"));
                None
            }
        };

        if let Err(error) = self.select_row_target(&row) {
            let rollback_error =
                rollback.and_then(|rollback| self.restore_persisted_selection(rollback).err());
            return SidebarJumpExecution::Failed(SidebarJumpFailure {
                error,
                rollback_error,
            });
        }

        self.clear_other_persisted_selections(&row.window_id);
        if let Some(intent) = SidebarWakeIntent::for_row(&row) {
            self.execute_wake_sidebar(intent);
        }
        let hook_error = self
            .execute_selection_hooks(SidebarHookIntent::new(row.clone()))
            .err();
        SidebarJumpExecution::Succeeded(Box::new(SidebarJumpOutcome {
            row,
            persistence_warning,
            hook_error,
        }))
    }

    /// Delete observations for a selected session and notify dependent sidebar/status surfaces.
    pub(super) fn execute_delete_session(
        &self,
        intent: SidebarDeleteSessionIntent,
    ) -> Result<SidebarDeleteOutcome> {
        self.store.delete_session(&intent.row.selection.key)?;
        let _ = refresh_window_statuses(&self.store, &self.tmux, &self.status_icons);
        let _ = super::notify_observation_changed(&self.tmux);
        Ok(SidebarDeleteOutcome {
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
        let encoded = encode_selected_target(&attempted)?;
        self.tmux
            .set_window_option(&row.window_id, SELECTED_TARGET_OPTION, encoded.as_str())?;

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
        if current.as_deref().and_then(decode_selected_target) != Some(rollback.attempted) {
            return Ok(());
        }

        match rollback.previous {
            PreviousSelectionOption::Value(value) => self.tmux.set_window_option(
                &rollback.window_id,
                SELECTED_TARGET_OPTION,
                value.as_str(),
            ),
            PreviousSelectionOption::Unset => self
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
        for window in windows {
            if window.window_id != selected_window_id {
                let _ = self
                    .tmux
                    .unset_window_option(&window.window_id, SELECTED_TARGET_OPTION);
            }
        }
    }

    /// Replace hook config in tests without exposing mutable executor internals.
    #[cfg(test)]
    pub(super) fn set_selection_hooks_for_test(
        &mut self,
        selection_hooks: Vec<SidebarSelectionHookConfig>,
    ) {
        self.selection_hooks = selection_hooks;
    }

    fn select_row_target(&self, row: &SidebarRow) -> Result<()> {
        match &row.jump_target {
            SidebarJumpTarget::Window {
                session_name,
                window_id,
                pane_id,
            } => {
                self.tmux.select_window_id(window_id)?;
                self.tmux.switch_client_to_session(session_name)?;
                if let Some(pane_id) = pane_id {
                    self.tmux.select_pane(pane_id)?;
                }
            }
            SidebarJumpTarget::Session { session_name } => {
                self.tmux.switch_client_to_session(session_name)?;
            }
            SidebarJumpTarget::None => {}
        }
        Ok(())
    }

    fn execute_wake_sidebar(&self, intent: SidebarWakeIntent) {
        let _ = lifecycle::wake_window(&self.tmux, &intent.window_id);
    }

    fn execute_selection_hooks(&self, intent: SidebarHookIntent) -> Result<()> {
        if self.selection_hooks.is_empty() {
            return Ok(());
        }

        let selected = self.selection_hook_input(&intent.row)?;
        run_selection_hooks(&self.selection_hooks, &selected)
    }

    fn selection_hook_input(&self, row: &SidebarRow) -> Result<SelectionHookInput> {
        let selection = &row.selection;
        let observations = self.selected_observations(&selection.key)?;
        Ok(SelectionHookInput::new(
            selection.key.clone(),
            selection.status,
            selection.title.clone(),
            selection.context.clone(),
            selection.metadata.clone(),
            selection.target.clone(),
            observations,
        ))
    }

    fn selected_observations(
        &self,
        session: &AgentSessionKey,
    ) -> Result<Vec<AgentObservationState>> {
        let tmux_instance = self.tmux.instance_id();
        Ok(self
            .store
            .list_observations()?
            .into_iter()
            // Preserve hook scoping: selected session, current tmux instance,
            // plus unscoped legacy observations from older producers.
            .filter(|observation| {
                observation.key.session == *session
                    && observation
                        .target
                        .tmux_instance
                        .as_deref()
                        .is_none_or(|target_instance| target_instance == tmux_instance)
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sidebar::test_support::{report_state, row_from_view};
    use crate::state::{AgentObservationKey, AgentObservationState, AgentSessionKey, AgentStatus};
    use crate::tmux::test_support::{TmuxFixture, create_test_session};
    use anyhow::Result;
    use std::fs;
    use tempfile::{TempDir, tempdir};

    struct SidebarActionFixture {
        fixture: TmuxFixture,
        _temp: TempDir,
        window_id: String,
    }

    impl SidebarActionFixture {
        fn new() -> Result<Option<Self>> {
            let Some(fixture) = TmuxFixture::new()? else {
                return Ok(None);
            };
            let temp = tempdir()?;
            create_test_session(&fixture.tmux, "project", temp.path())?;
            let pane_id = fixture.tmux.create_window_with_command(
                "project",
                "feature-sidebar",
                temp.path(),
                None,
            )?;
            let window_id = fixture
                .tmux
                .list_pane_snapshots()?
                .into_iter()
                .find(|pane| pane.pane_id == pane_id)
                .map(|pane| pane.window_id)
                .ok_or_else(|| anyhow::anyhow!("expected created pane in tmux snapshot"))?;

            Ok(Some(Self {
                fixture,
                _temp: temp,
                window_id,
            }))
        }

        fn actions(&self) -> SidebarActions {
            SidebarActions::new(
                self.fixture.tmux.clone(),
                crate::agent::sidebar::test_support::empty_state_store(),
                StatusIcons::default(),
                Vec::new(),
            )
        }

        fn set_selected_row(&self, row: &SidebarRow) -> Result<()> {
            let encoded = encode_selected_target(&row.identity)?;
            self.set_raw_selected_target(&encoded)
        }

        fn set_raw_selected_target(&self, value: &str) -> Result<()> {
            self.fixture
                .tmux
                .set_window_option(&self.window_id, SELECTED_TARGET_OPTION, value)
        }

        fn raw_selected_target(&self) -> Result<Option<String>> {
            self.fixture
                .tmux
                .show_window_option(&self.window_id, SELECTED_TARGET_OPTION)
        }

        fn selected_target(&self) -> Result<Option<SidebarRowIdentity>> {
            Ok(self
                .raw_selected_target()?
                .as_deref()
                .and_then(decode_selected_target))
        }
    }

    #[test]
    fn rollback_does_not_overwrite_newer_persisted_selection() -> Result<()> {
        let Some(fixture) = SidebarActionFixture::new()? else {
            return Ok(());
        };
        let previous = server_row_in_window("ses_previous", "Previous", &fixture.window_id);
        fixture.set_selected_row(&previous)?;
        let row = server_row_in_window("ses_attempted", "Attempted", &fixture.window_id);
        let actions = fixture.actions();
        let rollback = actions
            .persist_selection_before_jump(&row)?
            .ok_or_else(|| anyhow::anyhow!("expected persisted selection rollback"))?;
        let newer = server_row_in_window("ses_newer", "Newer", &fixture.window_id);
        fixture.set_selected_row(&newer)?;

        actions.restore_persisted_selection(rollback)?;

        assert_eq!(fixture.selected_target()?, Some(newer.identity));
        Ok(())
    }

    #[test]
    fn successful_selection_clears_other_window_persisted_targets() -> Result<()> {
        let Some(fixture) = SidebarActionFixture::new()? else {
            return Ok(());
        };
        let other_window_id = fixture
            .fixture
            .tmux
            .list_windows(None)?
            .into_iter()
            .map(|window| window.window_id)
            .find(|window_id| window_id != &fixture.window_id)
            .ok_or_else(|| anyhow::anyhow!("expected a second fixture window"))?;
        let selected = server_row_in_window("ses_selected", "Selected", &fixture.window_id);
        let stale = server_row_in_window("ses_stale", "Stale", &other_window_id);
        let selected_encoded = encode_selected_target(&selected.identity)?;
        let stale_encoded = encode_selected_target(&stale.identity)?;
        fixture.fixture.tmux.set_window_option(
            &fixture.window_id,
            SELECTED_TARGET_OPTION,
            selected_encoded.as_str(),
        )?;
        fixture.fixture.tmux.set_window_option(
            &other_window_id,
            SELECTED_TARGET_OPTION,
            stale_encoded.as_str(),
        )?;
        let actions = fixture.actions();

        actions.clear_other_persisted_selections(&fixture.window_id);

        assert_eq!(
            fixture.raw_selected_target()?.as_deref(),
            Some(selected_encoded.as_str())
        );
        assert_eq!(
            fixture
                .fixture
                .tmux
                .show_window_option(&other_window_id, SELECTED_TARGET_OPTION)?,
            None
        );
        Ok(())
    }

    #[test]
    fn selection_hooks_receive_selected_observations_for_current_tmux_instance() -> Result<()> {
        let dir = tempdir()?;
        let payload_path = dir.path().join("payload.json");
        let store = crate::agent::sidebar::test_support::empty_state_store();
        let mut row = server_row_in_window("ses_selected", "Selected", "@1");
        row.selection.target.tmux_instance = Some("default".to_owned());
        let mut selected_server = observation_for_row(&row, "server", "default-server");
        selected_server.target.tmux_instance = Some("default".to_owned());
        let mut selected_legacy = observation_for_row(&row, "tui", "legacy-pane");
        selected_legacy.target.tmux_instance = None;
        let mut selected_other_instance = observation_for_row(&row, "server", "other-instance");
        selected_other_instance.target.tmux_instance = Some("other".to_owned());
        let mut other_session = observation_for_row(
            &server_row_in_window("ses_other", "Other", "@1"),
            "server",
            "other-session",
        );
        other_session.target.tmux_instance = Some("default".to_owned());
        for observation in [
            selected_server,
            selected_legacy,
            selected_other_instance,
            other_session,
        ] {
            store.upsert_observation(&observation)?;
        }
        let command = format!("cat > '{}'", payload_path.display());
        let actions = SidebarActions::new(
            Tmux::new(),
            store,
            StatusIcons::default(),
            vec![hook_config(&command, Some("opencode"), None, Some(1000))],
        );

        actions.execute_selection_hooks(SidebarHookIntent::new(row))?;

        let payload: serde_json::Value = serde_json::from_str(&fs::read_to_string(payload_path)?)?;
        let producers = payload["observations"]
            .as_array()
            .expect("observations should be an array")
            .iter()
            .map(|observation| {
                observation["producer_instance"]
                    .as_str()
                    .expect("producer instance should be a string")
                    .to_owned()
            })
            .collect::<Vec<_>>();
        assert_eq!(producers, vec!["default-server", "legacy-pane"]);
        Ok(())
    }

    #[test]
    fn producer_kind_hooks_ignore_observations_outside_current_tmux_instance() -> Result<()> {
        let dir = tempdir()?;
        let marker_path = dir.path().join("marker");
        let store = crate::agent::sidebar::test_support::empty_state_store();
        let mut row = server_row_in_window("ses_selected", "Selected", "@1");
        row.selection.target.tmux_instance = Some("default".to_owned());
        let mut out_of_scope_server = observation_for_row(&row, "server", "other-instance");
        out_of_scope_server.target.tmux_instance = Some("other".to_owned());
        let mut selected_legacy_tui = observation_for_row(&row, "tui", "legacy-pane");
        selected_legacy_tui.target.tmux_instance = None;
        for observation in [out_of_scope_server, selected_legacy_tui] {
            store.upsert_observation(&observation)?;
        }
        let command = format!("touch '{}'", marker_path.display());
        let actions = SidebarActions::new(
            Tmux::new(),
            store,
            StatusIcons::default(),
            vec![hook_config(
                &command,
                Some("opencode"),
                Some("server"),
                Some(1000),
            )],
        );

        actions.execute_selection_hooks(SidebarHookIntent::new(row))?;

        assert!(!marker_path.exists());
        Ok(())
    }

    fn server_row_in_window(session_id: &str, title: &str, window_id: &str) -> SidebarRow {
        let mut report = report_state(AgentStatus::Working, 100, window_id, "%server");
        report.key = session_key("opencode", session_id);
        report.workspace_key = Some(format!("/repo/{window_id}/{session_id}"));
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

    fn hook_config(
        command: &str,
        agent_kind: Option<&str>,
        producer_kind: Option<&str>,
        timeout_ms: Option<u64>,
    ) -> SidebarSelectionHookConfig {
        SidebarSelectionHookConfig {
            command: command.to_owned(),
            agent_kind: agent_kind.map(str::to_owned),
            producer_kind: producer_kind.map(str::to_owned),
            timeout_ms,
        }
    }

    fn observation_for_row(
        row: &SidebarRow,
        producer_kind: &str,
        producer_instance: &str,
    ) -> AgentObservationState {
        AgentObservationState {
            key: AgentObservationKey {
                session: row.selection.key.clone(),
                producer_kind: producer_kind.to_owned(),
                producer_instance: producer_instance.to_owned(),
            },
            created_at: 100,
            status: Some(row.selection.status),
            status_observed_at: Some(100),
            status_changed_at: Some(100),
            working_elapsed_secs: 0,
            observed_at: 100,
            title: row.selection.title.clone(),
            context: row.selection.context.clone(),
            metadata: row.selection.metadata.clone(),
            metadata_cleared: Default::default(),
            target: row.selection.target.clone(),
        }
    }
}
