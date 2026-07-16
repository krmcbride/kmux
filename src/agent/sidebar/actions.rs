//! Sidebar action intents and side-effect execution.
//!
//! Tmux navigation, selected-target option persistence, and deletion side
//! effects live here so `SidebarApp` can stay focused on UI state
//! transitions. Shared observation-surface fanout policy lives at the agent
//! boundary.

use anyhow::Result;

use super::lifecycle;
use super::model::{SidebarJumpTarget, SidebarRow, SidebarRowIdentity};
use super::selection::{
    PersistedSelectionRollback, PreviousSelectionOption, SELECTED_TARGET_OPTION,
    decode_selected_target, encode_selected_target,
};
use crate::config::StatusIcons;
use crate::state::StateStore;
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
pub(super) struct SidebarDeleteOutcome {
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
        SidebarJumpExecution::Succeeded(Box::new(SidebarJumpOutcome {
            row,
            persistence_warning,
        }))
    }

    /// Delete observations for a selected session and notify dependent sidebar/status surfaces.
    pub(super) fn execute_delete_session(
        &self,
        intent: SidebarDeleteSessionIntent,
    ) -> Result<SidebarDeleteOutcome> {
        self.store.delete_session(&intent.row.selection.key)?;
        crate::agent::refresh_observation_surfaces(&self.store, &self.tmux, &self.status_icons);
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
            SidebarJumpTarget::None => anyhow::bail!(
                "cannot jump to {}: no unambiguous tmux target; check for this workspace open in multiple sessions or stale pane cwd",
                row.primary
            ),
        }
        Ok(())
    }

    fn execute_wake_sidebar(&self, intent: SidebarWakeIntent) {
        let _ = lifecycle::wake_window(&self.tmux, &intent.window_id);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sessions::session_views;
    use crate::agent::sidebar::test_support::{report_state, row_from_view, set_workspace};
    use crate::state::{
        AgentLocationHints, AgentObservationKey, AgentObservationState, AgentSessionKey,
        AgentStatus,
    };
    use crate::tmux::test_support::{TmuxFixture, create_test_session};
    use anyhow::Result;
    use std::fs;
    use std::process::Command;
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
    fn deleting_primary_session_leaves_another_session_for_the_same_workspace() -> Result<()> {
        let temp = tempdir()?;
        let repo = temp.path().join("project-alpha");
        fs::create_dir(&repo)?;
        let git_output = Command::new("git")
            .args(["init", "--initial-branch", "main"])
            .current_dir(&repo)
            .output()?;
        assert!(
            git_output.status.success(),
            "git init failed: {}",
            String::from_utf8_lossy(&git_output.stderr)
        );
        let store = crate::state::test_support::store_with_path(temp.path().join("state"))?;
        for observation in [
            observation_for_session("ses_primary", AgentStatus::Waiting, 200, &repo, "Primary"),
            observation_for_session("ses_secondary", AgentStatus::Done, 100, &repo, "Secondary"),
        ] {
            store.upsert_observation(&observation)?;
        }
        let tmux = Tmux::new();
        let before = session_views(&store, &tmux)?;
        assert_eq!(before.len(), 1);
        assert_eq!(before[0].key.session_id, "ses_primary");
        let primary = row_from_view(&before[0], 200);
        let actions = SidebarActions::new(tmux.clone(), store.clone(), StatusIcons::default());

        actions.execute_delete_session(SidebarDeleteSessionIntent::new(0, primary))?;

        let after = session_views(&store, &tmux)?;
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].key.session_id, "ses_secondary");
        Ok(())
    }

    fn server_row_in_window(session_id: &str, title: &str, window_id: &str) -> SidebarRow {
        let mut report = report_state(AgentStatus::Working, 100, window_id, "%server");
        report.key = session_key("opencode", session_id);
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

    fn observation_for_session(
        session_id: &str,
        status: AgentStatus,
        observed_at: u64,
        directory: &std::path::Path,
        title: &str,
    ) -> AgentObservationState {
        AgentObservationState {
            key: AgentObservationKey {
                session: session_key("opencode", session_id),
                producer_kind: "server".to_owned(),
                producer_instance: "reporter".to_owned(),
            },
            created_at: observed_at,
            status: Some(status),
            status_observed_at: Some(observed_at),
            status_changed_at: Some(observed_at),
            working_elapsed_secs: 0,
            observed_at,
            title: Some(title.to_owned()),
            context: None,
            target: AgentLocationHints {
                directory: Some(directory.display().to_string()),
                ..AgentLocationHints::default()
            },
        }
    }
}
