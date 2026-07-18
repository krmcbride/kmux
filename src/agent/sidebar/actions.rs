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
        self.store
            .delete_sessions(&intent.row.selection.member_session_keys)?;
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

    fn resolve_jump_destination(&self, row: &SidebarRow) -> Result<SidebarJumpDestination> {
        // Reconciliation has already applied workspace, session, and preference policy. Enter
        // takes the first still-live candidate in that order instead of recalculating policy from
        // the user's current tmux context, which might belong to an unrelated scratch window.
        let (session_name, candidates) = match &row.jump_target {
            AgentTmuxTarget::Windows {
                session_name,
                candidates,
            } => (session_name, candidates),
            AgentTmuxTarget::Unavailable(reason) => match reason {
                AgentTmuxUnavailableReason::Missing => return Err(missing_target_error(row, None)),
                AgentTmuxUnavailableReason::CrossSession { session_names } => anyhow::bail!(
                    "cannot jump to {}: matching windows span tmux sessions: {}",
                    row.primary,
                    session_names.join(", ")
                ),
            },
        };
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
        let candidate = candidates
            .iter()
            .find(|candidate| live_window_ids.contains(&candidate.window_id))
            .ok_or_else(|| missing_target_error(row, None))?;
        Ok(SidebarJumpDestination::from_candidate(
            session_name,
            candidate,
        ))
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
        let live_pane_ids = self
            .tmux
            .list_panes()
            .ok()?
            .into_iter()
            .filter(|pane| {
                pane.window_id == destination_window_id
                    && pane.kmux_role.as_deref() != Some("sidebar")
            })
            .map(|pane| pane.pane_id)
            .collect::<std::collections::BTreeSet<_>>();
        for pane_id in pane_ids {
            if live_pane_ids.contains(pane_id) && self.tmux.select_pane(pane_id).is_ok() {
                return Some(pane_id.clone());
            }
        }
        None
    }

    fn execute_wake_sidebar(&self, intent: SidebarWakeIntent) {
        let _ = lifecycle::wake_window(&self.tmux, &intent.window_id);
    }
}

fn missing_target_error(row: &SidebarRow, detail: Option<String>) -> anyhow::Error {
    let detail = detail
        .map(|detail| format!(" ({detail})"))
        .unwrap_or_default();
    anyhow::anyhow!(
        "cannot jump to {}: no live tmux window matches workspace {}; run `kmux restore` if this is a managed workspace{detail}",
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sidebar::test_support::{
        SidebarTmuxFixture, report_state, row_from_activity, row_from_view, set_session_key,
        set_workspace,
    };
    use crate::agent::workspace_activity::workspace_activities;
    use crate::git::test_support::GitRepoFixture;
    use crate::state::test_support::{StateStoreFixture, observation_state};
    use crate::state::{AgentObservationState, AgentSessionKey, AgentStatus};
    use anyhow::Result;

    impl SidebarTmuxFixture {
        fn actions(&self) -> Result<SidebarActions> {
            Ok(SidebarActions::new(
                self.fixture.tmux.clone(),
                self.state_store()?,
                StatusIcons::default(),
            ))
        }
    }

    #[test]
    fn rollback_does_not_overwrite_newer_persisted_selection() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let previous = server_row_in_window("ses_previous", "Previous", &fixture.window_id);
        fixture.set_selected_row(&previous)?;
        let row = server_row_in_window("ses_attempted", "Attempted", &fixture.window_id);
        let actions = fixture.actions()?;
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
        let Some(fixture) = SidebarTmuxFixture::new()? else {
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
        let actions = fixture.actions()?;

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
    fn jump_resolution_skips_a_candidate_window_that_disappeared() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let mut row = server_row_in_window("ses_selected", "Selected", &fixture.window_id);
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

        let destination = fixture.actions()?.resolve_jump_destination(&row)?;

        assert_eq!(destination.session_name, "project");
        assert_eq!(destination.window_id, fixture.window_id);
        Ok(())
    }

    #[test]
    fn stale_candidate_error_includes_restore_guidance() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let mut row = server_row_in_window("ses_stale", "Stale", "@999999");
        row.jump_target = AgentTmuxTarget::Windows {
            session_name: "project".to_owned(),
            candidates: vec![AgentTmuxWindowCandidate {
                window_id: "@999999".to_owned(),
                pane_ids: vec!["%999999".to_owned()],
            }],
        };

        let error = fixture
            .actions()?
            .resolve_jump_destination(&row)
            .expect_err("stale candidates should fail");

        assert!(error.to_string().contains("kmux restore"));
        Ok(())
    }

    #[test]
    fn unavailable_jump_preserves_existing_selection_option() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let previous = server_row_in_window("ses_previous", "Previous", &fixture.window_id);
        fixture.set_selected_row(&previous)?;
        let mut unavailable = server_row_in_window("ses_missing", "Missing", &fixture.window_id);
        unavailable.jump_target = AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::Missing);

        let result = fixture
            .actions()?
            .execute_jump(SidebarJumpIntent::new(unavailable));

        let failure = match result {
            SidebarJumpExecution::Failed(failure) => failure,
            SidebarJumpExecution::Succeeded(_) => anyhow::bail!("missing target should fail"),
        };
        assert!(failure.error.to_string().contains("kmux restore"));
        assert_eq!(fixture.selected_target()?, Some(previous.identity));
        Ok(())
    }

    #[test]
    fn cross_session_jump_error_names_conflicting_sessions() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let mut row = server_row_in_window("ses_ambiguous", "Ambiguous", &fixture.window_id);
        row.jump_target = AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::CrossSession {
            session_names: vec!["project-alpha".to_owned(), "project-beta".to_owned()],
        });

        let error = fixture
            .actions()?
            .resolve_jump_destination(&row)
            .expect_err("ambiguous target should fail");

        assert!(error.to_string().contains("project-alpha, project-beta"));
        Ok(())
    }

    #[test]
    fn stale_matching_pane_allows_sidebar_only_window_to_remain_selected() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let target_pane = fixture
            .fixture
            .tmux
            .list_pane_snapshots()?
            .into_iter()
            .find(|pane| pane.window_id == fixture.window_id)
            .ok_or_else(|| anyhow::anyhow!("expected target window pane"))?;
        fixture
            .fixture
            .tmux
            .set_pane_option(&target_pane.pane_id, "@kmux_role", "sidebar")?;
        fixture
            .fixture
            .tmux
            .select_window_id_in_session("project", &fixture.window_id)?;

        let selected_pane = fixture
            .actions()?
            .focus_first_available_pane(&fixture.window_id, &[target_pane.pane_id]);

        let active_window = fixture
            .fixture
            .tmux
            .list_windows(Some("project"))?
            .into_iter()
            .find(|window| window.active)
            .map(|window| window.window_id);
        assert_eq!(active_window.as_deref(), Some(fixture.window_id.as_str()));
        assert_eq!(selected_pane, None);
        assert_eq!(
            fixture
                .fixture
                .tmux
                .list_panes()?
                .into_iter()
                .find(|pane| pane.window_id == fixture.window_id)
                .and_then(|pane| pane.kmux_role)
                .as_deref(),
            Some("sidebar")
        );
        Ok(())
    }

    #[test]
    fn pane_focus_skips_stale_candidate_and_selects_next_live_pane() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let content_pane_id = fixture
            .fixture
            .tmux
            .list_pane_snapshots()?
            .into_iter()
            .find(|pane| pane.window_id == fixture.window_id)
            .map(|pane| pane.pane_id)
            .ok_or_else(|| anyhow::anyhow!("expected content pane"))?;
        let sidebar_pane_id =
            fixture
                .fixture
                .tmux
                .split_window_left(&fixture.window_id, 20, "sleep 60")?;
        fixture
            .fixture
            .tmux
            .set_pane_option(&sidebar_pane_id, "@kmux_role", "sidebar")?;
        fixture.fixture.tmux.select_pane(&sidebar_pane_id)?;

        let selected_pane = fixture.actions()?.focus_first_available_pane(
            &fixture.window_id,
            &["%999999".to_owned(), content_pane_id.clone()],
        );

        assert_eq!(selected_pane.as_deref(), Some(content_pane_id.as_str()));
        assert!(
            fixture
                .fixture
                .tmux
                .list_pane_snapshots()?
                .into_iter()
                .any(|pane| pane.pane_id == content_pane_id && pane.pane_active)
        );
        Ok(())
    }

    #[test]
    fn deleting_workspace_row_removes_all_members_and_allows_recreation() -> Result<()> {
        let selected_fixture = GitRepoFixture::new()?;
        let unrelated_fixture = GitRepoFixture::new()?;
        let selected_repo = selected_fixture.path();
        let unrelated_repo = unrelated_fixture.path();
        let state = StateStoreFixture::new()?;
        let store = state.store().clone();
        let mut primary_report = observation_for_session(
            "opencode",
            "ses_primary",
            AgentStatus::Working,
            150,
            selected_repo,
            "Primary report",
        );
        primary_report.key.reporter_kind = "reporter-a".to_owned();
        primary_report.key.reporter_instance = "instance-1".to_owned();
        let observations = [
            observation_for_session(
                "opencode",
                "ses_primary",
                AgentStatus::Waiting,
                200,
                selected_repo,
                "Primary",
            ),
            primary_report,
            observation_for_session(
                "opencode",
                "ses_secondary",
                AgentStatus::Done,
                100,
                selected_repo,
                "Secondary",
            ),
            observation_for_session(
                "codex",
                "ses_companion",
                AgentStatus::Working,
                175,
                selected_repo,
                "Companion",
            ),
            observation_for_session(
                "opencode",
                "ses_unrelated",
                AgentStatus::Working,
                125,
                unrelated_repo,
                "Unrelated",
            ),
        ];
        for observation in &observations {
            store.upsert_observation(observation)?;
        }
        let tmux = Tmux::new();
        let before = workspace_activities(&store, &tmux)?;
        assert_eq!(before.len(), 2);
        let selected = before
            .iter()
            .find(|activity| activity.workspace_key() == selected_repo.to_string_lossy().as_ref())
            .ok_or_else(|| anyhow::anyhow!("expected selected workspace"))?;
        assert_eq!(selected.primary_session_key().session_id, "ses_primary");
        assert_eq!(
            selected.member_session_keys(),
            [
                session_key("codex", "ses_companion"),
                session_key("opencode", "ses_primary"),
                session_key("opencode", "ses_secondary"),
            ]
        );
        let selected_row = row_from_activity(selected, 200);
        let actions = SidebarActions::new(tmux.clone(), store.clone(), StatusIcons::default());

        actions
            .execute_delete_workspace_row(SidebarDeleteWorkspaceRowIntent::new(0, selected_row))?;

        let remaining = store.list_observations()?;
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].key.session.session_id, "ses_unrelated");
        let after = workspace_activities(&store, &tmux)?;
        assert_eq!(after.len(), 1);
        assert_eq!(after[0].primary_session_key().session_id, "ses_unrelated");

        store.upsert_observation(&observations[0])?;
        let recreated = workspace_activities(&store, &tmux)?;
        assert_eq!(recreated.len(), 2);
        assert!(recreated.iter().any(|activity| {
            activity.workspace_key() == selected_repo.to_string_lossy().as_ref()
                && activity.member_session_keys() == [session_key("opencode", "ses_primary")]
        }));
        Ok(())
    }

    #[test]
    fn deleting_workspace_row_uses_captured_member_snapshot() -> Result<()> {
        let repo = GitRepoFixture::new()?;
        let state = StateStoreFixture::new()?;
        let store = state.store().clone();
        let captured = observation_for_session(
            "opencode",
            "ses_captured",
            AgentStatus::Waiting,
            200,
            repo.path(),
            "Captured",
        );
        store.upsert_observation(&captured)?;
        let tmux = Tmux::new();
        let before = workspace_activities(&store, &tmux)?;
        assert_eq!(before.len(), 1);
        let captured_row = row_from_activity(&before[0], 200);

        let arrived_after_snapshot = observation_for_session(
            "codex",
            "ses_arrived_later",
            AgentStatus::Working,
            250,
            repo.path(),
            "Arrived later",
        );
        store.upsert_observation(&arrived_after_snapshot)?;
        let actions = SidebarActions::new(tmux.clone(), store.clone(), StatusIcons::default());

        actions
            .execute_delete_workspace_row(SidebarDeleteWorkspaceRowIntent::new(0, captured_row))?;

        assert_eq!(store.list_observations()?, vec![arrived_after_snapshot]);
        let after = workspace_activities(&store, &tmux)?;
        assert_eq!(after.len(), 1);
        assert_eq!(
            after[0].primary_session_key().session_id,
            "ses_arrived_later"
        );
        Ok(())
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

    fn observation_for_session(
        agent_kind: &str,
        session_id: &str,
        status: AgentStatus,
        observed_at: u64,
        directory: &std::path::Path,
        title: &str,
    ) -> AgentObservationState {
        let mut observation = observation_state();
        observation.key.session = session_key(agent_kind, session_id);
        observation.created_at = observed_at;
        observation.status = Some(status);
        observation.status_observed_at = Some(observed_at);
        observation.status_changed_at = Some(observed_at);
        observation.observed_at = observed_at;
        observation.title = Some(title.to_owned());
        observation.context = None;
        observation.target.directory = Some(directory.display().to_string());
        observation
    }
}
