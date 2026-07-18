//! Stateful controller for the sidebar terminal UI.
//!
//! `SidebarApp` owns selection, focus-following behavior, refreshed rows, spinner
//! state, and transient errors. Rendering, terminal event plumbing, row queries,
//! and side-effect execution live in sibling modules.

use ratatui::widgets::ListState;

use super::actions::{
    SidebarActions, SidebarDeleteWorkspaceRowIntent, SidebarDeleteWorkspaceRowOutcome,
    SidebarDisableIntent, SidebarJumpExecution, SidebarJumpIntent, SidebarJumpOutcome,
};
use super::rows::{SidebarRefreshRowsIntent, SidebarRowsQuery, SidebarRowsSnapshot};
use crate::agent::sessions::AgentTmuxTarget;
use crate::agent::sidebar::model::{SidebarRow, SidebarRowIdentity};
use crate::agent::sidebar::selection::{self, SelectionMode};

#[cfg(test)]
use crate::tmux::Tmux;
use crate::tmux::TmuxPaneVisibility;

use crate::telemetry;

/// Mutable state for the sidebar terminal UI.
pub(super) struct SidebarApp {
    rows_query: SidebarRowsQuery,
    actions: SidebarActions,
    working_frames: Vec<String>,
    spinner_frame: usize,
    rows: Vec<SidebarRow>,
    list_state: ListState,
    sidebar_pane_id: Option<String>,
    sidebar_session_name: Option<String>,
    sidebar_window_id: Option<String>,
    selection_mode: SelectionMode,
    selected_identity: Option<SidebarRowIdentity>,
    sidebar_has_focus: bool,
    window_visible: bool,
    last_error: Option<String>,
    should_quit: bool,
    disable_requested: bool,
}

impl SidebarApp {
    /// Create sidebar UI state around injected row and action services.
    pub(super) fn new(
        rows_query: SidebarRowsQuery,
        actions: SidebarActions,
        working_frames: Vec<String>,
        sidebar_session_name: Option<String>,
        sidebar_window_id: Option<String>,
        sidebar_pane_id: Option<String>,
    ) -> Self {
        Self {
            rows_query,
            actions,
            working_frames,
            spinner_frame: 0,
            rows: Vec::new(),
            list_state: ListState::default(),
            sidebar_pane_id,
            sidebar_session_name,
            sidebar_window_id,
            selection_mode: SelectionMode::FollowSidebarContext,
            selected_identity: None,
            sidebar_has_focus: false,
            window_visible: true,
            last_error: None,
            should_quit: false,
            disable_requested: false,
        }
    }

    /// Refresh rows from current agent/tmux state.
    pub(super) fn refresh_rows(&mut self) -> bool {
        let sidebar_pane_id = self.sidebar_pane_id.clone();
        let working_icon = self.working_icon().map(str::to_owned);
        let visibility = self.rows_query.visibility(sidebar_pane_id.as_deref());
        let intent = SidebarRefreshRowsIntent {
            working_icon: working_icon.as_deref(),
        };
        let result = telemetry::timed_result_event!(
            "sidebar.refresh",
            {
                rows = self.rows.len(),
                visible = self.window_visible,
                focused = self.sidebar_has_focus,
            },
            || {
                self.apply_refresh_visibility(visibility);
                let snapshot = self.rows_query.load(intent, visibility)?;
                let activity_count = snapshot.activity_count;
                self.apply_rows_snapshot(snapshot);
                Ok::<usize, anyhow::Error>(activity_count)
            },
            ok |activity_count| { activities = *activity_count, },
        );

        if let Err(error) = result {
            self.last_error = Some(error.to_string());
        }
        true
    }

    /// Request that the TUI exit and that the sidebar be disabled afterward.
    pub(super) fn request_disable(&mut self) -> SidebarDisableIntent {
        self.disable_requested = true;
        self.should_quit = true;
        SidebarDisableIntent
    }

    /// Move manual selection to the next row.
    pub(super) fn next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let next = self
            .list_state
            .selected()
            .map_or(0, |selected| (selected + 1).min(self.rows.len() - 1));
        self.select_index_manual(next);
    }

    /// Move manual selection to the previous row.
    pub(super) fn previous(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let selected = self.list_state.selected().unwrap_or(0);
        self.select_index_manual(selected.saturating_sub(1));
    }

    /// Move manual selection to the first row.
    pub(super) fn select_first(&mut self) {
        if !self.rows.is_empty() {
            self.select_index_manual(0);
        }
    }

    /// Move manual selection to the last row.
    pub(super) fn select_last(&mut self) {
        if !self.rows.is_empty() {
            self.select_index_manual(self.rows.len() - 1);
        }
    }

    /// Switch to the selected workspace's exact matching window, then try to
    /// focus its matching pane.
    pub(super) fn jump_to_selected(&mut self) {
        let Some(intent) = self.selected_jump_intent() else {
            return;
        };
        match self.actions.execute_jump(intent) {
            SidebarJumpExecution::Succeeded(outcome) => {
                self.apply_successful_jump_outcome(*outcome);
            }
            SidebarJumpExecution::Failed(failure) => {
                let rollback_error = failure
                    .rollback_error
                    .map(|error| format!("; selection restore failed: {error}"))
                    .unwrap_or_default();
                let _ = self.refresh_rows();
                self.last_error = Some(format!("jump failed: {}{rollback_error}", failure.error));
            }
        }
    }

    /// Delete all observations represented by the selected workspace row.
    pub(super) fn delete_selected_workspace_row(&mut self) {
        let Some(intent) = self.selected_delete_workspace_row_intent() else {
            return;
        };

        match self.actions.execute_delete_workspace_row(intent) {
            Ok(outcome) => self.apply_delete_workspace_row_outcome(outcome),
            Err(error) => self.last_error = Some(format!("delete failed: {error}")),
        }
    }

    /// Return whether the TUI event loop should exit.
    pub(super) fn should_quit(&self) -> bool {
        self.should_quit
    }

    /// Return whether exit should also disable the sidebar globally.
    pub(super) fn disable_requested(&self) -> bool {
        self.disable_requested
    }

    /// Return the rows currently rendered by the sidebar.
    pub(super) fn rows(&self) -> &[SidebarRow] {
        &self.rows
    }

    /// Return the last refresh or jump error, if one should be displayed.
    pub(super) fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    /// Return the row index for the sidebar pane's current tmux context.
    pub(super) fn active_index(&self) -> Option<usize> {
        if self.selection_mode == SelectionMode::FollowSidebarContext {
            self.active_row_index()
        } else {
            selection::sidebar_context_index(
                &self.rows,
                self.sidebar_window_id.as_deref(),
                self.sidebar_session_name.as_deref(),
            )
        }
    }

    /// Return the cursor row index when the sidebar pane has focus.
    pub(super) fn cursor_index(&self) -> Option<usize> {
        self.sidebar_has_focus
            .then(|| self.list_state.selected())
            .flatten()
    }

    /// Return mutable ratatui list state for rendering.
    pub(super) fn list_state_mut(&mut self) -> &mut ListState {
        &mut self.list_state
    }

    /// Return whether the sidebar's tmux window is visible to an attached client.
    pub(super) fn window_visible(&self) -> bool {
        self.window_visible
    }

    /// Return whether working rows should animate spinner frames right now.
    pub(super) fn should_animate_spinner(&self) -> bool {
        self.window_visible
            && !self.working_frames.is_empty()
            && self.rows.iter().any(SidebarRow::is_working)
    }

    /// Advance the spinner and update working row icons.
    pub(super) fn tick_spinner(&mut self) {
        if !self.should_animate_spinner() {
            return;
        }

        self.advance_spinner_frame();
        let Some(icon) = self.working_icon().map(str::to_owned) else {
            return;
        };
        for row in &mut self.rows {
            if row.is_working() {
                row.icon.clone_from(&icon);
            }
        }
    }

    fn apply_successful_jump_outcome(&mut self, outcome: SidebarJumpOutcome) {
        if let Some(row) = self
            .rows
            .iter_mut()
            .find(|row| row.identity == outcome.row.identity)
        {
            row.clone_from(&outcome.row);
        }
        self.reset_after_successful_jump(&outcome.row);
        self.last_error = outcome.persistence_warning;
    }

    fn apply_rows_snapshot(&mut self, snapshot: SidebarRowsSnapshot) {
        self.apply_refresh_visibility(snapshot.visibility);
        self.rows = snapshot.rows;
        self.last_error = None;
        self.sync_selection();
    }

    fn apply_refresh_visibility(&mut self, visibility: TmuxPaneVisibility) {
        self.window_visible = visibility.window_visible;
        self.update_selection_mode_for_focus(visibility.pane_has_focus);
    }

    fn selected_jump_intent(&self) -> Option<SidebarJumpIntent> {
        self.selected_row().cloned().map(SidebarJumpIntent::new)
    }

    fn selected_delete_workspace_row_intent(&self) -> Option<SidebarDeleteWorkspaceRowIntent> {
        let index = self.list_state.selected()?;
        let row = self.rows.get(index).cloned()?;
        Some(SidebarDeleteWorkspaceRowIntent::new(index, row))
    }

    fn apply_delete_workspace_row_outcome(&mut self, outcome: SidebarDeleteWorkspaceRowOutcome) {
        self.rows
            .retain(|candidate| candidate.identity != outcome.row.identity);
        self.last_error = None;
        if self.rows.is_empty() {
            self.list_state.select(None);
            self.selected_identity = None;
        } else {
            self.select_index_manual(outcome.index.min(self.rows.len() - 1));
        }
    }

    fn sync_selection(&mut self) {
        if self.rows.is_empty() {
            self.list_state.select(None);
            return;
        }

        let selected = match self.selection_mode {
            SelectionMode::FollowSidebarContext => self.active_row_index(),
            SelectionMode::Manual => Some(
                selection::manual_selection_index(
                    &self.rows,
                    self.selected_identity.as_ref(),
                    self.list_state.selected(),
                )
                .unwrap_or(0),
            ),
        };
        match selected {
            Some(index) => {
                self.select_index_internal(index);
                self.seed_sidebar_selection_option(index);
            }
            None => self.list_state.select(None),
        }
    }

    fn active_row_index(&self) -> Option<usize> {
        self.persisted_sidebar_context_identity_index().or_else(|| {
            selection::sidebar_context_index(
                &self.rows,
                self.sidebar_window_id.as_deref(),
                self.sidebar_session_name.as_deref(),
            )
        })
    }

    fn selected_row(&self) -> Option<&SidebarRow> {
        self.list_state
            .selected()
            .and_then(|index| self.rows.get(index))
    }

    fn persisted_sidebar_context_identity_index(&self) -> Option<usize> {
        let sidebar_window_id = self.sidebar_window_id.as_deref()?;
        let identity = self
            .actions
            .persisted_selection_identity(sidebar_window_id)?;
        selection::persisted_sidebar_context_identity_index(
            &self.rows,
            self.sidebar_window_id.as_deref(),
            self.sidebar_session_name.as_deref(),
            &identity,
        )
    }

    fn seed_sidebar_selection_option(&self, index: usize) {
        if self.selection_mode != SelectionMode::FollowSidebarContext {
            return;
        }
        let Some(sidebar_window_id) = self.sidebar_window_id.as_deref() else {
            return;
        };
        if self.actions.selection_option_exists(sidebar_window_id) {
            return;
        }
        if let Some(row) = self.rows.get(index) {
            let _ = self
                .actions
                .persist_selection_identity(sidebar_window_id, &row.identity);
        }
    }

    fn reset_after_successful_jump(&mut self, row: &SidebarRow) {
        self.selection_mode = SelectionMode::FollowSidebarContext;
        self.sidebar_has_focus = false;
        if matches!(row.jump_target, AgentTmuxTarget::Windows { .. })
            && self
                .sidebar_window_id
                .as_deref()
                .is_some_and(|sidebar_window_id| sidebar_window_id != row.window_id)
        {
            self.window_visible = false;
        }
        self.sync_selection();
    }

    fn select_index_manual(&mut self, index: usize) {
        self.selection_mode = SelectionMode::Manual;
        self.sidebar_has_focus = true;
        self.select_index_internal(index);
    }

    fn select_index_internal(&mut self, index: usize) {
        let index = index.min(self.rows.len().saturating_sub(1));
        self.list_state.select(Some(index));
        if let Some(row) = self.rows.get(index) {
            self.selected_identity = Some(row.identity.clone());
        }
    }

    fn update_selection_mode_for_focus(&mut self, sidebar_has_focus: bool) {
        self.sidebar_has_focus = sidebar_has_focus;
        if !sidebar_has_focus {
            self.selection_mode = SelectionMode::FollowSidebarContext;
        }
    }

    fn working_icon(&self) -> Option<&str> {
        if self.working_frames.is_empty() {
            return None;
        }
        self.working_frames
            .get(self.spinner_frame % self.working_frames.len())
            .map(String::as_str)
    }

    fn advance_spinner_frame(&mut self) {
        if !self.working_frames.is_empty() {
            self.spinner_frame = self.spinner_frame.wrapping_add(1);
        }
    }
}

#[cfg(test)]
impl SidebarApp {
    /// Build a sidebar app with injected rows for unit tests.
    pub(super) fn test(sidebar_window_id: Option<&str>, rows: Vec<SidebarRow>) -> Self {
        Self::test_with_tmux(Tmux::new(), None, sidebar_window_id, rows)
    }

    pub(super) fn test_with_tmux(
        tmux: Tmux,
        sidebar_session_name: Option<&str>,
        sidebar_window_id: Option<&str>,
        rows: Vec<SidebarRow>,
    ) -> Self {
        let store = crate::agent::sidebar::test_support::empty_state_store();
        Self::test_with_store(tmux, store, sidebar_session_name, sidebar_window_id, rows)
    }

    pub(super) fn set_last_error_for_test(&mut self, error: impl Into<String>) {
        self.last_error = Some(error.into());
    }

    fn test_with_store(
        tmux: Tmux,
        store: crate::state::StateStore,
        sidebar_session_name: Option<&str>,
        sidebar_window_id: Option<&str>,
        rows: Vec<SidebarRow>,
    ) -> Self {
        let rows_query = SidebarRowsQuery::new(
            store.clone(),
            tmux.clone(),
            crate::agent::sidebar::test_support::test_icons(),
            crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
        );
        let actions = SidebarActions::new(tmux, store, crate::config::StatusIcons::default());
        let mut app = Self {
            rows_query,
            actions,
            working_frames: Vec::new(),
            spinner_frame: 0,
            rows,
            list_state: ListState::default(),
            sidebar_pane_id: None,
            sidebar_session_name: sidebar_session_name.map(str::to_owned),
            sidebar_window_id: sidebar_window_id.map(str::to_owned),
            selection_mode: SelectionMode::FollowSidebarContext,
            selected_identity: None,
            sidebar_has_focus: false,
            window_visible: true,
            last_error: None,
            should_quit: false,
            disable_requested: false,
        };
        app.sync_selection();
        app
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sessions::AgentTmuxTarget;
    use crate::agent::sidebar::selection::{
        SELECTED_TARGET_OPTION, decode_selected_target, encode_selected_target,
    };
    use crate::agent::sidebar::test_support::{
        TEST_SLEEPING_ICON, agent_state, report_state, row_from_view, set_workspace,
    };
    use crate::config::{DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS, StatusIcons};
    use crate::state::{AgentSessionKey, AgentStatus};
    use crate::tmux::test_support::{TmuxFixture, create_test_session};
    use anyhow::Result;
    use std::fs;
    use tempfile::{TempDir, tempdir};

    struct SidebarTmuxFixture {
        fixture: TmuxFixture,
        _temp: TempDir,
        window_id: String,
    }

    impl SidebarTmuxFixture {
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

        fn tmux(&self) -> Tmux {
            self.fixture.tmux.clone()
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

    fn selected_index(app: &SidebarApp) -> Option<usize> {
        app.list_state.selected()
    }

    fn rows_snapshot(visibility: TmuxPaneVisibility, rows: Vec<SidebarRow>) -> SidebarRowsSnapshot {
        SidebarRowsSnapshot {
            visibility,
            rows,
            activity_count: 0,
        }
    }

    #[test]
    fn sidebar_app_cycles_working_frames() {
        let mut app = SidebarApp::test(None, Vec::new());
        app.working_frames = vec!["a".to_owned(), "b".to_owned()];

        assert_eq!(app.working_icon(), Some("a"));
        app.advance_spinner_frame();
        assert_eq!(app.working_icon(), Some("b"));
        app.advance_spinner_frame();
        assert_eq!(app.working_icon(), Some("a"));
    }

    #[test]
    fn spinner_tick_updates_only_active_working_rows() {
        let rows = vec![
            row_from_view(&agent_state(AgentStatus::Working, 100, "@1", "%1"), 100),
            row_from_view(&agent_state(AgentStatus::Waiting, 100, "@2", "%2"), 100),
            row_from_view(
                &agent_state(AgentStatus::Done, 0, "@3", "%3"),
                DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS + 1,
            ),
        ];
        let mut app = SidebarApp::test(None, rows);
        app.working_frames = vec!["a".to_owned(), "b".to_owned()];

        assert!(app.should_animate_spinner());

        app.tick_spinner();

        assert_eq!(app.rows()[0].icon, "b");
        assert_eq!(app.rows()[1].icon, "?");
        assert_eq!(app.rows()[2].icon, TEST_SLEEPING_ICON);
    }

    #[test]
    fn spinner_tick_is_noop_without_frames_or_working_rows() {
        let rows = vec![row_from_view(
            &agent_state(AgentStatus::Waiting, 100, "@1", "%1"),
            100,
        )];
        let mut app = SidebarApp::test(None, rows);

        assert!(!app.should_animate_spinner());
        app.tick_spinner();
        assert_eq!(app.rows()[0].icon, "?");

        app.working_frames = vec!["a".to_owned(), "b".to_owned()];
        assert!(!app.should_animate_spinner());
        app.tick_spinner();
        assert_eq!(app.rows()[0].icon, "?");
    }

    #[test]
    fn spinner_tick_is_noop_when_window_is_hidden() {
        let rows = vec![row_from_view(
            &agent_state(AgentStatus::Working, 100, "@1", "%1"),
            100,
        )];
        let mut app = SidebarApp::test(None, rows);
        app.working_frames = vec!["a".to_owned(), "b".to_owned()];
        app.window_visible = false;

        assert!(!app.should_animate_spinner());

        app.tick_spinner();

        assert_eq!(app.rows()[0].icon, "?");
    }

    #[test]
    fn request_disable_returns_disable_intent_and_marks_app_for_exit() {
        let mut app = SidebarApp::test(None, Vec::new());

        let intent = app.request_disable();

        assert_eq!(intent, SidebarDisableIntent);
        assert!(app.should_quit());
        assert!(app.disable_requested());
    }

    #[test]
    fn selected_row_intents_capture_current_row_without_executing_side_effects() {
        let rows = vec![server_row("ses_a", "First"), server_row("ses_b", "Second")];
        let mut app = SidebarApp::test(Some("@1"), rows);
        app.next();

        let jump = app.selected_jump_intent().expect("selected jump intent");
        let delete = app
            .selected_delete_workspace_row_intent()
            .expect("selected delete intent");

        assert_eq!(jump.row.title, "Second");
        assert_eq!(delete.index, 1);
        assert_eq!(delete.row.selection.workspace_key, "/repo/@1/ses_b");
        assert_eq!(
            delete.row.selection.member_session_keys,
            vec![session_key("opencode", "ses_b")]
        );
        assert_eq!(app.rows().len(), 2);
    }

    #[test]
    fn applying_rows_snapshot_syncs_focus_visibility_and_selection() {
        let rows = vec![
            row_from_view(&agent_state(AgentStatus::Working, 100, "@1", "%1"), 100),
            row_from_view(&agent_state(AgentStatus::Waiting, 100, "@2", "%2"), 100),
        ];
        let mut app = SidebarApp::test(Some("@1"), rows);
        app.next();
        let refreshed_rows = vec![
            row_from_view(&agent_state(AgentStatus::Working, 200, "@1", "%10"), 200),
            row_from_view(&agent_state(AgentStatus::Waiting, 200, "@2", "%20"), 200),
        ];

        app.apply_rows_snapshot(rows_snapshot(
            TmuxPaneVisibility {
                pane_has_focus: true,
                window_visible: false,
            },
            refreshed_rows,
        ));

        assert!(!app.window_visible());
        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(selected_index(&app), Some(1));
    }

    #[test]
    fn refresh_failure_still_updates_visibility_and_focus_state() -> Result<()> {
        let Some(fixture) = TmuxFixture::new()? else {
            return Ok(());
        };
        let temp = tempdir()?;
        create_test_session(&fixture.tmux, "project", temp.path())?;
        let pane = fixture
            .tmux
            .list_pane_snapshots()?
            .into_iter()
            .find(|pane| pane.session_name == "project")
            .ok_or_else(|| anyhow::anyhow!("expected test sidebar pane"))?;

        let state_base = temp.path().join("state");
        let store = crate::state::test_support::store_with_path(&state_base)?;
        let observations_dir = state_base.join("agent-observations");
        fs::remove_dir(&observations_dir)?;
        fs::write(&observations_dir, "not a directory")?;
        let rows_query = SidebarRowsQuery::new(
            store.clone(),
            fixture.tmux.clone(),
            crate::agent::sidebar::test_support::test_icons(),
            DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
        );
        let actions = SidebarActions::new(fixture.tmux.clone(), store, StatusIcons::default());
        let mut app = SidebarApp::new(
            rows_query,
            actions,
            Vec::new(),
            None,
            Some(pane.window_id),
            Some(pane.pane_id),
        );
        app.window_visible = true;
        app.sidebar_has_focus = true;
        app.selection_mode = SelectionMode::Manual;

        app.refresh_rows();

        assert!(!app.window_visible());
        assert_eq!(app.selection_mode, SelectionMode::FollowSidebarContext);
        assert!(
            app.last_error()
                .is_some_and(|error| error.contains("failed to read state directory"))
        );
        Ok(())
    }

    #[test]
    fn hidden_idle_refresh_rebuilds_rows() {
        let rows = vec![
            row_from_view(
                &agent_state(AgentStatus::Done, 0, "@1", "%1"),
                DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS + 1,
            ),
            row_from_view(&agent_state(AgentStatus::Waiting, 100, "@2", "%2"), 100),
        ];
        let mut app = SidebarApp::test(Some("@1"), rows);
        app.next();
        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(selected_index(&app), Some(1));

        app.apply_rows_snapshot(rows_snapshot(
            TmuxPaneVisibility {
                pane_has_focus: false,
                window_visible: false,
            },
            Vec::new(),
        ));

        assert!(!app.window_visible());
        assert!(app.rows().is_empty());
    }

    #[test]
    fn hidden_missing_sidebar_context_refresh_rebuilds_rows() {
        let rows = vec![row_from_view(
            &agent_state(AgentStatus::Working, 100, "@other", "%1"),
            100,
        )];
        let mut app = SidebarApp::test(Some("@missing"), rows);

        app.apply_rows_snapshot(rows_snapshot(
            TmuxPaneVisibility {
                pane_has_focus: false,
                window_visible: false,
            },
            Vec::new(),
        ));

        assert!(!app.window_visible());
        assert!(app.rows().is_empty());
    }

    #[test]
    fn hidden_non_idle_model_refresh_rebuilds_rows() {
        let rows = vec![row_from_view(
            &agent_state(AgentStatus::Working, 100, "@1", "%1"),
            100,
        )];
        let mut app = SidebarApp::test(Some("@1"), rows);

        app.apply_rows_snapshot(rows_snapshot(
            TmuxPaneVisibility {
                pane_has_focus: false,
                window_visible: false,
            },
            Vec::new(),
        ));

        assert!(!app.window_visible());
        assert!(app.rows().is_empty());
    }

    #[test]
    fn selection_follows_sidebar_window_then_manual_navigation_takes_over() {
        let rows = vec![
            row_from_view(&agent_state(AgentStatus::Working, 100, "@1", "%1"), 100),
            row_from_view(&agent_state(AgentStatus::Waiting, 100, "@2", "%2"), 100),
        ];
        let mut app = SidebarApp::test(Some("@2"), rows);

        assert_eq!(selected_index(&app), Some(1));
        assert_eq!(app.active_index(), Some(1));
        assert_eq!(app.cursor_index(), None);

        app.previous();

        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(selected_index(&app), Some(0));
        assert_eq!(app.active_index(), Some(1));
        assert_eq!(app.cursor_index(), Some(0));
    }

    #[test]
    fn selection_follows_matching_candidate_window() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let row = server_row_in_window("ses_project_alpha", "Project alpha", &fixture.window_id);

        let app = SidebarApp::test_with_tmux(
            fixture.tmux(),
            Some("project"),
            Some(&fixture.window_id),
            vec![row.clone()],
        );

        assert_eq!(selected_index(&app), Some(0));
        assert_eq!(app.active_index(), Some(0));
        assert_eq!(app.cursor_index(), None);
        assert_eq!(fixture.selected_target()?, Some(row.identity));
        Ok(())
    }

    #[test]
    fn selection_clears_when_followed_sidebar_window_has_no_agent_row() {
        let rows = vec![row_from_view(
            &agent_state(AgentStatus::Working, 100, "@1", "%1"),
            100,
        )];
        let mut app = SidebarApp::test(Some("@missing"), rows);

        assert_eq!(app.selection_mode, SelectionMode::FollowSidebarContext);
        assert_eq!(selected_index(&app), None);

        app.next();

        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(selected_index(&app), Some(0));
    }

    #[test]
    fn manual_selection_survives_empty_refresh_and_pane_id_change() {
        let rows = vec![
            row_from_view(&agent_state(AgentStatus::Working, 100, "@1", "%1"), 100),
            row_from_view(&agent_state(AgentStatus::Waiting, 100, "@2", "%2"), 100),
        ];
        let mut app = SidebarApp::test(Some("@1"), rows);

        app.next();
        assert_eq!(selected_index(&app), Some(1));

        app.rows = Vec::new();
        app.sync_selection();
        app.rows = vec![
            row_from_view(&agent_state(AgentStatus::Working, 200, "@1", "%10"), 200),
            row_from_view(&agent_state(AgentStatus::Waiting, 200, "@2", "%20"), 200),
        ];
        app.sync_selection();

        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(selected_index(&app), Some(1));
    }

    #[test]
    fn manual_selection_tracks_workspace_row_when_windows_match() {
        let rows = vec![server_row("ses_a", "First"), server_row("ses_b", "Second")];
        let mut app = SidebarApp::test(Some("@1"), rows);

        assert_eq!(app.active_index(), Some(0));
        app.next();
        assert_eq!(selected_index(&app), Some(1));
        assert_eq!(app.cursor_index(), Some(1));

        app.rows = vec![
            server_row("ses_a", "First refreshed"),
            server_row("ses_b", "Second refreshed"),
        ];
        app.sync_selection();

        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(app.active_index(), Some(0));
        assert_eq!(selected_index(&app), Some(1));
        assert_eq!(app.rows()[1].title, "Second refreshed");
    }

    #[test]
    fn manual_selection_returns_to_sidebar_context_when_sidebar_loses_focus() {
        let rows = vec![
            row_from_view(&agent_state(AgentStatus::Working, 100, "@1", "%1"), 100),
            row_from_view(&agent_state(AgentStatus::Waiting, 100, "@2", "%2"), 100),
        ];
        let mut app = SidebarApp::test(Some("@1"), rows);

        app.next();
        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(selected_index(&app), Some(1));

        app.update_selection_mode_for_focus(false);
        app.sync_selection();

        assert_eq!(app.selection_mode, SelectionMode::FollowSidebarContext);
        assert_eq!(selected_index(&app), Some(0));
        assert_eq!(app.active_index(), Some(0));
        assert_eq!(app.cursor_index(), None);
    }

    #[test]
    fn same_window_manual_selection_without_persisted_target_returns_to_first_sidebar_row() {
        let rows = vec![server_row("ses_a", "First"), server_row("ses_b", "Second")];
        let mut app = SidebarApp::test(Some("@1"), rows);

        app.next();
        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(selected_index(&app), Some(1));

        app.update_selection_mode_for_focus(false);
        app.sync_selection();

        assert_eq!(app.selection_mode, SelectionMode::FollowSidebarContext);
        assert_eq!(selected_index(&app), Some(0));
        assert_eq!(app.active_index(), Some(0));
        assert_eq!(app.cursor_index(), None);
    }

    #[test]
    fn successful_cross_window_jump_marks_source_sidebar_hidden() {
        let rows = vec![
            row_from_view(&agent_state(AgentStatus::Working, 100, "@1", "%1"), 100),
            row_from_view(&agent_state(AgentStatus::Waiting, 100, "@2", "%2"), 100),
        ];
        let mut app = SidebarApp::test(Some("@1"), rows);
        app.next();
        let target = app.rows()[1].clone();

        app.reset_after_successful_jump(&target);

        assert!(!app.window_visible());
        assert_eq!(app.selection_mode, SelectionMode::FollowSidebarContext);
        assert_eq!(app.active_index(), Some(0));
        assert_eq!(app.cursor_index(), None);
    }

    #[test]
    fn successful_same_window_jump_keeps_source_sidebar_visible() {
        let rows = vec![row_from_view(
            &agent_state(AgentStatus::Working, 100, "@1", "%1"),
            100,
        )];
        let mut app = SidebarApp::test(Some("@1"), rows);
        app.sidebar_has_focus = true;
        let target = app.rows()[0].clone();

        app.reset_after_successful_jump(&target);

        assert!(app.window_visible());
        assert_eq!(app.selection_mode, SelectionMode::FollowSidebarContext);
        assert_eq!(app.active_index(), Some(0));
        assert_eq!(app.cursor_index(), None);
    }

    #[test]
    fn successful_fallback_updates_stored_row_to_actual_destination() {
        let mut original = server_row_in_window("ses_project_alpha", "Project alpha", "@1");
        let AgentTmuxTarget::Windows { candidates, .. } = &mut original.jump_target else {
            assert!(matches!(
                &original.jump_target,
                AgentTmuxTarget::Windows { .. }
            ));
            return;
        };
        candidates.push(crate::agent::sessions::AgentTmuxWindowCandidate {
            window_id: "@2".to_owned(),
            pane_ids: vec!["%2".to_owned()],
        });
        let mut resolved = original.clone();
        resolved.window_id = "@2".to_owned();
        resolved.pane_id = Some("%2".to_owned());
        let mut app = SidebarApp::test(Some("@2"), vec![original]);

        app.apply_successful_jump_outcome(SidebarJumpOutcome {
            row: resolved,
            persistence_warning: None,
        });

        assert_eq!(app.rows()[0].window_id, "@2");
        assert_eq!(app.rows()[0].pane_id.as_deref(), Some("%2"));
        assert_eq!(selected_index(&app), Some(0));
        assert_eq!(app.active_index(), Some(0));
    }

    #[test]
    fn successful_same_window_jump_keeps_logical_session_highlight_sticky() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let rows = vec![
            server_row_in_window("ses_a", "First", &fixture.window_id),
            server_row_in_window("ses_b", "Second", &fixture.window_id),
        ];
        let mut app = SidebarApp::test_with_tmux(
            fixture.tmux(),
            Some("project"),
            Some(&fixture.window_id),
            rows,
        );
        app.next();
        let target = app.rows()[1].clone();

        fixture.set_selected_row(&target)?;
        app.reset_after_successful_jump(&target);

        assert!(app.window_visible());
        assert_eq!(app.selection_mode, SelectionMode::FollowSidebarContext);
        assert_eq!(selected_index(&app), Some(1));
        assert_eq!(app.active_index(), Some(1));
        assert_eq!(app.cursor_index(), None);
        Ok(())
    }

    #[test]
    fn persisted_selection_from_another_sidebar_process_selects_matching_sidebar_row() -> Result<()>
    {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let rows = vec![
            server_row_in_window("ses_a", "First", &fixture.window_id),
            server_row_in_window("ses_b", "Second", &fixture.window_id),
        ];
        fixture.set_selected_row(&rows[1])?;

        let destination = SidebarApp::test_with_tmux(
            fixture.tmux(),
            Some("project"),
            Some(&fixture.window_id),
            rows,
        );

        assert_eq!(selected_index(&destination), Some(1));
        assert_eq!(destination.active_index(), Some(1));
        assert_eq!(destination.cursor_index(), None);
        Ok(())
    }

    #[test]
    fn stale_persisted_selection_falls_back_to_first_sidebar_window_row() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        fixture
            .set_raw_selected_target(r#"{"version":2,"target":{"key":"directory:/missing"}}"#)?;

        let app = SidebarApp::test_with_tmux(
            fixture.tmux(),
            Some("project"),
            Some(&fixture.window_id),
            vec![
                server_row_in_window("ses_a", "First", &fixture.window_id),
                server_row_in_window("ses_b", "Second", &fixture.window_id),
            ],
        );

        assert_eq!(selected_index(&app), Some(0));
        assert_eq!(app.active_index(), Some(0));
        Ok(())
    }

    #[test]
    fn malformed_persisted_selection_falls_back_to_first_sidebar_window_row() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        fixture.set_raw_selected_target("not json")?;

        let app = SidebarApp::test_with_tmux(
            fixture.tmux(),
            Some("project"),
            Some(&fixture.window_id),
            vec![
                server_row_in_window("ses_a", "First", &fixture.window_id),
                server_row_in_window("ses_b", "Second", &fixture.window_id),
            ],
        );

        assert_eq!(selected_index(&app), Some(0));
        assert_eq!(app.active_index(), Some(0));
        Ok(())
    }

    #[test]
    fn persisted_selection_for_row_in_another_window_is_ignored() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let other_window_row = server_row_in_window("ses_a", "Other window", "@2");
        fixture.set_selected_row(&other_window_row)?;

        let rows = vec![
            other_window_row,
            server_row_in_window("ses_b", "Sidebar window", &fixture.window_id),
        ];
        let app = SidebarApp::test_with_tmux(
            fixture.tmux(),
            Some("project"),
            Some(&fixture.window_id),
            rows,
        );

        assert_eq!(selected_index(&app), Some(1));
        assert_eq!(app.active_index(), Some(1));
        Ok(())
    }

    #[test]
    fn jump_failure_restores_previous_persisted_selection() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let previous = server_row_in_window("ses_previous", "Previous", &fixture.window_id);
        fixture.set_selected_row(&previous)?;
        let row = row_with_missing_jump_session(server_row_in_window(
            "ses_attempted",
            "Attempted",
            &fixture.window_id,
        ));
        let mut app = SidebarApp::test_with_tmux(
            fixture.tmux(),
            Some("project"),
            Some(&fixture.window_id),
            vec![row],
        );

        app.jump_to_selected();

        assert_eq!(fixture.selected_target()?, Some(previous.identity));
        assert!(
            app.last_error()
                .is_some_and(|error| error.contains("jump failed"))
        );
        Ok(())
    }

    #[test]
    fn jump_failure_restores_malformed_previous_selection_option() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        fixture.set_raw_selected_target("not json")?;
        let row = row_with_missing_jump_session(server_row_in_window(
            "ses_attempted",
            "Attempted",
            &fixture.window_id,
        ));
        let mut app = SidebarApp::test_with_tmux(
            fixture.tmux(),
            Some("project"),
            Some(&fixture.window_id),
            vec![row],
        );

        app.jump_to_selected();

        assert_eq!(fixture.raw_selected_target()?.as_deref(), Some("not json"));
        assert!(
            app.last_error()
                .is_some_and(|error| error.contains("jump failed"))
        );
        Ok(())
    }

    #[test]
    fn jump_failure_preserves_seeded_initial_selection() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let row = row_with_missing_jump_session(server_row_in_window(
            "ses_attempted",
            "Attempted",
            &fixture.window_id,
        ));
        let row_identity = row.identity.clone();
        let mut app = SidebarApp::test_with_tmux(
            fixture.tmux(),
            Some("project"),
            Some(&fixture.window_id),
            vec![row],
        );

        app.jump_to_selected();

        assert_eq!(fixture.selected_target()?, Some(row_identity));
        assert!(
            app.last_error()
                .is_some_and(|error| error.contains("jump failed"))
        );
        Ok(())
    }

    #[test]
    fn jump_failure_is_reported_without_panicking_or_quitting() {
        let rows = vec![row_from_view(
            &agent_state(AgentStatus::Waiting, 100, "not-a-window", "%missing"),
            100,
        )];
        let mut app = SidebarApp::test(Some("not-a-window"), rows);

        app.jump_to_selected();

        assert!(!app.should_quit());
        assert!(
            app.last_error()
                .is_some_and(|error| error.contains("jump failed"))
        );
    }

    #[test]
    fn no_jump_target_reports_error() -> Result<()> {
        let dir = tempdir()?;
        let rows = vec![no_jump_row(
            "ses_no_jump",
            "No jump",
            dir.path().to_string_lossy().as_ref(),
        )];
        let mut app = SidebarApp::test(None, rows);
        app.select_index_manual(0);

        app.jump_to_selected();

        assert!(!app.should_quit());
        assert!(
            app.last_error()
                .is_some_and(|error| error.contains("no live tmux window"))
        );
        Ok(())
    }

    #[test]
    fn delete_selected_workspace_row_removes_it_without_quitting_sidebar() {
        let rows = vec![server_row("ses_a", "First"), server_row("ses_b", "Second")];
        let mut app = SidebarApp::test(Some("@1"), rows);
        app.next();

        app.delete_selected_workspace_row();

        assert!(!app.should_quit());
        assert_eq!(app.rows().len(), 1);
        assert_eq!(app.rows()[0].title, "First");
        assert_eq!(selected_index(&app), Some(0));
        assert_eq!(app.last_error(), None);
    }

    #[test]
    fn delete_failure_keeps_workspace_row_and_reports_error() -> Result<()> {
        let temp = tempdir()?;
        let state_path = temp.path().join("state");
        let store = crate::state::test_support::store_with_path(&state_path)?;
        let observations_path = state_path.join("agent-observations");
        fs::remove_dir(&observations_path)?;
        fs::write(&observations_path, "not a directory")?;
        let rows = vec![server_row("ses_a", "First")];
        let mut app = SidebarApp::test_with_store(Tmux::new(), store, None, Some("@1"), rows);

        app.delete_selected_workspace_row();

        assert!(!app.should_quit());
        assert_eq!(app.rows().len(), 1);
        assert_eq!(app.rows()[0].title, "First");
        assert_eq!(selected_index(&app), Some(0));
        assert!(
            app.last_error()
                .is_some_and(|error| error.starts_with("delete failed:"))
        );
        Ok(())
    }

    fn server_row(session_id: &str, title: &str) -> SidebarRow {
        server_row_in_window(session_id, title, "@1")
    }

    fn server_row_in_window(session_id: &str, title: &str, window_id: &str) -> SidebarRow {
        let mut report = report_state(AgentStatus::Working, 100, window_id, "%server");
        crate::agent::sidebar::test_support::set_session_key(
            &mut report,
            session_key("opencode", session_id),
        );
        set_workspace(&mut report, format!("/repo/{window_id}/{session_id}"));
        report.title = Some(title.to_owned());
        report.target.tmux_pane_id = None;
        row_from_view(&report, 100)
    }

    fn no_jump_row(session_id: &str, title: &str, directory: &str) -> SidebarRow {
        let mut report = report_state(AgentStatus::Working, 100, "", "");
        crate::agent::sidebar::test_support::set_session_key(
            &mut report,
            session_key("opencode", session_id),
        );
        set_workspace(&mut report, directory);
        report.tmux_target = AgentTmuxTarget::Unavailable(
            crate::agent::sessions::AgentTmuxUnavailableReason::Missing,
        );
        report.title = Some(title.to_owned());
        report.target.tmux_pane_id = None;
        report.target.tmux_window_id = None;
        report.target.tmux_session_name = None;
        report.target.tmux_window_name = None;
        report.target.tmux_pane_title = None;
        report.target.tmux_pane_current_command = None;
        report.target.directory = Some(directory.to_owned());
        row_from_view(&report, 100)
    }

    fn row_with_missing_jump_session(mut row: SidebarRow) -> SidebarRow {
        row.session_name = "missing-session".to_owned();
        row.jump_target = AgentTmuxTarget::Windows {
            session_name: "missing-session".to_owned(),
            candidates: vec![crate::agent::sessions::AgentTmuxWindowCandidate {
                window_id: row.window_id.clone(),
                pane_ids: row.pane_id.iter().cloned().collect(),
            }],
        };
        row
    }

    fn session_key(agent_kind: &str, session_id: &str) -> AgentSessionKey {
        AgentSessionKey {
            agent_kind: agent_kind.to_owned(),
            session_id: session_id.to_owned(),
        }
    }
}
