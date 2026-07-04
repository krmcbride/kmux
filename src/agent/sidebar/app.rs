//! Stateful controller for the sidebar terminal UI.
//!
//! `SidebarApp` owns selection, focus-following behavior, row refreshes, jump
//! actions, deletion, spinner state, and transient errors. Rendering and terminal
//! event plumbing live in sibling modules.

use anyhow::Result;
use ratatui::widgets::ListState;

use crate::agent::sessions::session_views;
use crate::agent::sidebar::hooks::run_selection_hooks;
use crate::agent::sidebar::model::{
    SidebarIcons, SidebarRow, SidebarRowIdentity, build_rows_with_working_icon,
};
use crate::agent::sidebar::selection::{
    self, PersistedSelectionRollback, PreviousSelectionOption, SELECTED_TARGET_OPTION,
    SelectionMode, decode_selected_target, encode_selected_target,
};
use crate::agent::status::refresh_window_statuses;
use crate::config::{SidebarSelectionHookConfig, StatusIcons};
use crate::state::StateStore;
use crate::tmux::{Tmux, TmuxPaneVisibility};

/// Mutable state for the sidebar terminal UI.
pub(super) struct SidebarApp {
    tmux: Tmux,
    store: StateStore,
    status_icons: StatusIcons,
    icons: SidebarIcons,
    working_frames: Vec<String>,
    idle_after_seconds: u64,
    selection_hooks: Vec<SidebarSelectionHookConfig>,
    spinner_frame: usize,
    rows: Vec<SidebarRow>,
    list_state: ListState,
    sidebar_pane_id: Option<String>,
    host_window_id: Option<String>,
    selection_mode: SelectionMode,
    selected_identity: Option<SidebarRowIdentity>,
    selected_pane_id: Option<String>,
    selected_window_id: Option<String>,
    sidebar_has_focus: bool,
    window_visible: bool,
    last_error: Option<String>,
    should_quit: bool,
    disable_requested: bool,
}

impl SidebarApp {
    /// Create sidebar UI state and capture the host tmux window/pane identity.
    pub(super) fn new(
        tmux: Tmux,
        store: StateStore,
        status_icons: StatusIcons,
        icons: SidebarIcons,
        working_frames: Vec<String>,
        idle_after_seconds: u64,
        selection_hooks: Vec<SidebarSelectionHookConfig>,
    ) -> Self {
        let context = tmux.current_context().ok().flatten();
        let host_window_id = context.as_ref().map(|context| context.window_id.clone());
        let sidebar_pane_id = context.map(|context| context.pane_id);
        Self {
            tmux,
            store,
            status_icons,
            icons,
            working_frames,
            idle_after_seconds,
            selection_hooks,
            spinner_frame: 0,
            rows: Vec::new(),
            list_state: ListState::default(),
            sidebar_pane_id,
            host_window_id,
            selection_mode: SelectionMode::FollowHost,
            selected_identity: None,
            selected_pane_id: None,
            selected_window_id: None,
            sidebar_has_focus: false,
            window_visible: true,
            last_error: None,
            should_quit: false,
            disable_requested: false,
        }
    }

    /// Refresh rows from current agent/tmux state when visibility policy allows it.
    pub(super) fn refresh_rows(&mut self) -> bool {
        let visibility = self.sidebar_visibility();
        self.refresh_rows_for_visibility(visibility)
    }

    /// Request that the TUI exit and that the sidebar be disabled afterward.
    pub(super) fn request_disable(&mut self) {
        self.disable_requested = true;
        self.should_quit = true;
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

    /// Switch tmux focus to the selected agent row's pane or window.
    pub(super) fn jump_to_selected(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        let mut persistence_warning = None;
        let rollback = match self.persist_selection_before_jump(&row) {
            Ok(rollback) => rollback,
            Err(error) => {
                persistence_warning = Some(format!("selection state failed: {error}"));
                None
            }
        };
        if let Err(error) = self.select_row_target(&row) {
            let rollback_error = rollback
                .and_then(|rollback| self.restore_persisted_selection(rollback).err())
                .map(|error| format!("; selection restore failed: {error}"))
                .unwrap_or_default();
            let _ = self.refresh_rows();
            self.last_error = Some(format!("jump failed: {error}{rollback_error}"));
        } else {
            let hook_result = self.run_selection_hooks_for_row(&row);
            self.reset_after_successful_jump(&row);
            if let Err(error) = hook_result {
                self.last_error = Some(format!("hook failed: {error}"));
            } else if let Some(warning) = persistence_warning {
                self.last_error = Some(warning);
            } else {
                self.last_error = None;
            }
        }
    }

    /// Delete all observations for the selected agent session and update the model.
    pub(super) fn delete_selected_session(&mut self) {
        let Some(index) = self.list_state.selected() else {
            return;
        };
        let Some(row) = self.rows.get(index).cloned() else {
            return;
        };

        if let Err(error) = self.store.delete_session(&row.selection.key) {
            self.last_error = Some(format!("delete failed: {error}"));
            return;
        }
        let _ = refresh_window_statuses(&self.store, &self.tmux, &self.status_icons);

        self.rows
            .retain(|candidate| candidate.identity != row.identity);
        self.last_error = None;
        if self.rows.is_empty() {
            self.list_state.select(None);
            self.selected_identity = None;
            self.selected_pane_id = None;
            self.selected_window_id = None;
        } else {
            self.select_index_manual(index.min(self.rows.len() - 1));
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

    /// Return the row index for the host window the sidebar is attached to.
    pub(super) fn active_index(&self) -> Option<usize> {
        if self.selection_mode == SelectionMode::FollowHost {
            selection::remembered_host_identity_index(
                &self.rows,
                self.host_window_id.as_deref(),
                self.selected_identity.as_ref(),
            )
            .or_else(|| selection::host_window_index(&self.rows, self.host_window_id.as_deref()))
        } else {
            selection::host_window_index(&self.rows, self.host_window_id.as_deref())
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

    // Hidden sidebars keep lightweight state fresh but avoid full model refreshes
    // while invisible unless focus/visibility changes require it.
    fn refresh_rows_for_visibility(&mut self, visibility: TmuxPaneVisibility) -> bool {
        self.window_visible = visibility.window_visible;
        self.update_selection_mode_for_focus(visibility.pane_has_focus);
        if !self.should_refresh_model(visibility) {
            self.sync_selection();
            return false;
        }

        match session_views(&self.store, &self.tmux) {
            Ok(views) => {
                let working_icon = self.working_icon().map(str::to_owned);
                self.rows = build_rows_with_working_icon(
                    &views,
                    crate::state::now_unix_seconds(),
                    &self.icons,
                    working_icon.as_deref(),
                    self.idle_after_seconds,
                );
                self.last_error = None;
                self.sync_selection();
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
            }
        }
        true
    }

    fn should_refresh_model(&self, visibility: TmuxPaneVisibility) -> bool {
        visibility.window_visible || self.host_row().is_some_and(|row| !row.is_idle())
    }

    fn host_row(&self) -> Option<&SidebarRow> {
        let host_window_id = self.host_window_id.as_deref()?;
        self.rows.iter().find(|row| row.window_id == host_window_id)
    }

    fn sync_selection(&mut self) {
        if self.rows.is_empty() {
            self.list_state.select(None);
            return;
        }

        let selected = match self.selection_mode {
            SelectionMode::FollowHost => self
                .persisted_host_identity_index()
                .or_else(|| {
                    selection::remembered_host_identity_index(
                        &self.rows,
                        self.host_window_id.as_deref(),
                        self.selected_identity.as_ref(),
                    )
                })
                .or_else(|| {
                    selection::host_window_index(&self.rows, self.host_window_id.as_deref())
                }),
            SelectionMode::Manual => Some(
                selection::manual_selection_index(
                    &self.rows,
                    self.selected_identity.as_ref(),
                    self.selected_pane_id.as_deref(),
                    self.selected_window_id.as_deref(),
                    self.list_state.selected(),
                )
                .unwrap_or(0),
            ),
        };
        match selected {
            Some(index) => self.select_index_internal(index),
            None => self.list_state.select(None),
        }
    }

    fn select_row_target(&self, row: &SidebarRow) -> Result<()> {
        self.tmux.select_window_id(&row.window_id)?;
        self.tmux.switch_client_to_session(&row.session_name)?;
        if let Some(pane_id) = &row.pane_id {
            self.tmux.select_pane(pane_id)?;
        }
        Ok(())
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

    fn run_selection_hooks_for_row(&self, row: &SidebarRow) -> Result<()> {
        run_selection_hooks(
            &self.selection_hooks,
            &self.store,
            &self.tmux.instance_id(),
            row,
        )
    }

    fn selected_row(&self) -> Option<&SidebarRow> {
        self.list_state
            .selected()
            .and_then(|index| self.rows.get(index))
    }

    fn persisted_host_identity_index(&self) -> Option<usize> {
        let host_window_id = self.host_window_id.as_deref()?;
        let value = self
            .tmux
            .show_window_option(host_window_id, SELECTED_TARGET_OPTION)
            .ok()
            .flatten()?;
        let identity = decode_selected_target(&value)?;
        selection::persisted_host_identity_index(
            &self.rows,
            self.host_window_id.as_deref(),
            &identity,
        )
    }

    fn reset_after_successful_jump(&mut self, row: &SidebarRow) {
        self.selection_mode = SelectionMode::FollowHost;
        self.sidebar_has_focus = false;
        if self
            .host_window_id
            .as_deref()
            .is_some_and(|host_window_id| host_window_id != row.window_id)
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
            self.selected_pane_id = row.pane_id.clone();
            self.selected_window_id = Some(row.window_id.clone());
        }
    }

    fn sidebar_visibility(&self) -> TmuxPaneVisibility {
        let Some(pane_id) = self.sidebar_pane_id.as_deref() else {
            return TmuxPaneVisibility {
                pane_has_focus: false,
                window_visible: true,
            };
        };
        self.tmux
            .pane_visibility(pane_id)
            .unwrap_or(TmuxPaneVisibility {
                pane_has_focus: false,
                window_visible: true,
            })
    }

    fn update_selection_mode_for_focus(&mut self, sidebar_has_focus: bool) {
        self.sidebar_has_focus = sidebar_has_focus;
        if !sidebar_has_focus {
            self.selection_mode = SelectionMode::FollowHost;
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
    pub(super) fn test(host_window_id: Option<&str>, rows: Vec<SidebarRow>) -> Self {
        Self::test_with_tmux(Tmux::new(), host_window_id, rows)
    }

    pub(super) fn test_with_tmux(
        tmux: Tmux,
        host_window_id: Option<&str>,
        rows: Vec<SidebarRow>,
    ) -> Self {
        let mut app = Self {
            tmux,
            store: crate::agent::sidebar::test_support::empty_state_store(),
            status_icons: StatusIcons::default(),
            icons: crate::agent::sidebar::test_support::test_icons(),
            working_frames: Vec::new(),
            idle_after_seconds: crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
            selection_hooks: Vec::new(),
            spinner_frame: 0,
            rows,
            list_state: ListState::default(),
            sidebar_pane_id: None,
            host_window_id: host_window_id.map(str::to_owned),
            selection_mode: SelectionMode::FollowHost,
            selected_identity: None,
            selected_pane_id: None,
            selected_window_id: None,
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
    use crate::agent::sidebar::test_support::{
        TEST_SLEEPING_ICON, agent_state, report_state, row_from_view,
    };
    use crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS;
    use crate::state::{AgentSessionKey, AgentStatus};
    use crate::tmux::test_support::{TmuxFixture, create_test_session};
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
    fn hidden_idle_refresh_skips_model_rebuild() {
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

        let refreshed = app.refresh_rows_for_visibility(TmuxPaneVisibility {
            pane_has_focus: false,
            window_visible: false,
        });

        assert!(!refreshed);
        assert!(!app.window_visible());
        assert_eq!(app.selection_mode, SelectionMode::FollowHost);
        assert_eq!(selected_index(&app), Some(0));
        assert_eq!(app.rows().len(), 2);
        assert_eq!(app.rows()[0].primary, "kmux");
    }

    #[test]
    fn hidden_missing_host_refresh_skips_model_rebuild() {
        let rows = vec![row_from_view(
            &agent_state(AgentStatus::Working, 100, "@other", "%1"),
            100,
        )];
        let mut app = SidebarApp::test(Some("@missing"), rows);

        let refreshed = app.refresh_rows_for_visibility(TmuxPaneVisibility {
            pane_has_focus: false,
            window_visible: false,
        });

        assert!(!refreshed);
        assert!(!app.window_visible());
        assert_eq!(app.rows().len(), 1);
        assert_eq!(app.rows()[0].window_id, "@other");
    }

    #[test]
    fn hidden_non_idle_model_refresh_rebuilds_rows() {
        let rows = vec![row_from_view(
            &agent_state(AgentStatus::Working, 100, "@1", "%1"),
            100,
        )];
        let mut app = SidebarApp::test(Some("@1"), rows);

        let refreshed = app.refresh_rows_for_visibility(TmuxPaneVisibility {
            pane_has_focus: false,
            window_visible: false,
        });

        assert!(refreshed);
        assert!(!app.window_visible());
        assert!(app.rows().is_empty());
    }

    #[test]
    fn selection_follows_host_window_then_manual_navigation_takes_over() {
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
    fn selection_clears_when_followed_host_window_has_no_agent_row() {
        let rows = vec![row_from_view(
            &agent_state(AgentStatus::Working, 100, "@1", "%1"),
            100,
        )];
        let mut app = SidebarApp::test(Some("@missing"), rows);

        assert_eq!(app.selection_mode, SelectionMode::FollowHost);
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
        assert_eq!(app.selected_window_id.as_deref(), Some("@2"));
        assert_eq!(app.selected_pane_id.as_deref(), Some("%20"));
    }

    #[test]
    fn manual_selection_tracks_exact_non_pane_row_when_windows_match() {
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
    fn manual_selection_returns_to_host_when_sidebar_loses_focus() {
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

        assert_eq!(app.selection_mode, SelectionMode::FollowHost);
        assert_eq!(selected_index(&app), Some(0));
        assert_eq!(app.active_index(), Some(0));
        assert_eq!(app.cursor_index(), None);
        assert_eq!(app.selected_window_id.as_deref(), Some("@1"));
        assert_eq!(app.selected_pane_id.as_deref(), Some("%1"));
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
        assert_eq!(app.selection_mode, SelectionMode::FollowHost);
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
        assert_eq!(app.selection_mode, SelectionMode::FollowHost);
        assert_eq!(app.active_index(), Some(0));
        assert_eq!(app.cursor_index(), None);
    }

    #[test]
    fn successful_same_window_jump_keeps_logical_session_highlight_sticky() {
        let rows = vec![server_row("ses_a", "First"), server_row("ses_b", "Second")];
        let mut app = SidebarApp::test(Some("@1"), rows);
        app.next();
        let target = app.rows()[1].clone();

        app.reset_after_successful_jump(&target);

        assert!(app.window_visible());
        assert_eq!(app.selection_mode, SelectionMode::FollowHost);
        assert_eq!(selected_index(&app), Some(1));
        assert_eq!(app.active_index(), Some(1));
        assert_eq!(app.cursor_index(), None);
    }

    #[test]
    fn persisted_selection_from_another_sidebar_process_selects_matching_host_row() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let rows = vec![
            server_row_in_window("ses_a", "First", &fixture.window_id),
            server_row_in_window("ses_b", "Second", &fixture.window_id),
        ];
        let source = SidebarApp::test_with_tmux(fixture.tmux(), Some("@other"), rows.clone());
        source.persist_selection_before_jump(&rows[1])?;

        let destination =
            SidebarApp::test_with_tmux(fixture.tmux(), Some(&fixture.window_id), rows);

        assert_eq!(selected_index(&destination), Some(1));
        assert_eq!(destination.active_index(), Some(1));
        assert_eq!(destination.cursor_index(), None);
        Ok(())
    }

    #[test]
    fn stale_persisted_selection_falls_back_to_first_host_window_row() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        fixture
            .set_raw_selected_target(r#"{"version":2,"target":{"key":"directory:/missing"}}"#)?;

        let app = SidebarApp::test_with_tmux(
            fixture.tmux(),
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
    fn malformed_persisted_selection_falls_back_to_first_host_window_row() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        fixture.set_raw_selected_target("not json")?;

        let app = SidebarApp::test_with_tmux(
            fixture.tmux(),
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
            server_row_in_window("ses_b", "Host window", &fixture.window_id),
        ];
        let app = SidebarApp::test_with_tmux(fixture.tmux(), Some(&fixture.window_id), rows);

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
        let mut row = server_row_in_window("ses_attempted", "Attempted", &fixture.window_id);
        row.session_name = "missing-session".to_owned();
        let mut app =
            SidebarApp::test_with_tmux(fixture.tmux(), Some(&fixture.window_id), vec![row]);

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
        let mut row = server_row_in_window("ses_attempted", "Attempted", &fixture.window_id);
        row.session_name = "missing-session".to_owned();
        let mut app =
            SidebarApp::test_with_tmux(fixture.tmux(), Some(&fixture.window_id), vec![row]);

        app.jump_to_selected();

        assert_eq!(fixture.raw_selected_target()?.as_deref(), Some("not json"));
        assert!(
            app.last_error()
                .is_some_and(|error| error.contains("jump failed"))
        );
        Ok(())
    }

    #[test]
    fn jump_failure_clears_new_persisted_selection_when_no_previous_selection_exists() -> Result<()>
    {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let mut row = server_row_in_window("ses_attempted", "Attempted", &fixture.window_id);
        row.session_name = "missing-session".to_owned();
        let mut app =
            SidebarApp::test_with_tmux(fixture.tmux(), Some(&fixture.window_id), vec![row]);

        app.jump_to_selected();

        assert_eq!(fixture.selected_target()?, None);
        assert!(
            app.last_error()
                .is_some_and(|error| error.contains("jump failed"))
        );
        Ok(())
    }

    #[test]
    fn rollback_does_not_overwrite_newer_persisted_selection() -> Result<()> {
        let Some(fixture) = SidebarTmuxFixture::new()? else {
            return Ok(());
        };
        let previous = server_row_in_window("ses_previous", "Previous", &fixture.window_id);
        fixture.set_selected_row(&previous)?;
        let rows = vec![server_row_in_window(
            "ses_attempted",
            "Attempted",
            &fixture.window_id,
        )];
        let app =
            SidebarApp::test_with_tmux(fixture.tmux(), Some(&fixture.window_id), rows.clone());
        let rollback = app
            .persist_selection_before_jump(&rows[0])?
            .ok_or_else(|| anyhow::anyhow!("expected persisted selection rollback"))?;
        let newer = server_row_in_window("ses_newer", "Newer", &fixture.window_id);
        fixture.set_selected_row(&newer)?;

        app.restore_persisted_selection(rollback)?;

        assert_eq!(fixture.selected_target()?, Some(newer.identity));
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
    fn jump_failure_does_not_run_selection_hooks() -> Result<()> {
        let dir = tempdir()?;
        let marker = dir.path().join("marker");
        let rows = vec![row_from_view(
            &agent_state(AgentStatus::Waiting, 100, "not-a-window", "%missing"),
            100,
        )];
        let mut app = SidebarApp::test(Some("not-a-window"), rows);
        app.selection_hooks = vec![SidebarSelectionHookConfig {
            command: format!("touch '{}'", marker.display()),
            agent_kind: Some("opencode".to_owned()),
            producer_kind: None,
            timeout_ms: Some(1000),
        }];

        app.jump_to_selected();

        assert!(!marker.exists());
        assert!(!app.should_quit());
        assert!(
            app.last_error()
                .is_some_and(|error| error.contains("jump failed"))
        );
        Ok(())
    }

    #[test]
    fn delete_selected_session_removes_row_without_quitting_sidebar() {
        let rows = vec![server_row("ses_a", "First"), server_row("ses_b", "Second")];
        let mut app = SidebarApp::test(Some("@1"), rows);
        app.next();

        app.delete_selected_session();

        assert!(!app.should_quit());
        assert_eq!(app.rows().len(), 1);
        assert_eq!(app.rows()[0].title, "First");
        assert_eq!(selected_index(&app), Some(0));
        assert_eq!(app.last_error(), None);
    }

    fn server_row(session_id: &str, title: &str) -> SidebarRow {
        server_row_in_window(session_id, title, "@1")
    }

    fn server_row_in_window(session_id: &str, title: &str, window_id: &str) -> SidebarRow {
        let mut report = report_state(AgentStatus::Working, 100, window_id, "%server");
        report.key = session_key("opencode", session_id);
        report.directory_key = Some(format!("/repo/{window_id}/{session_id}"));
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
}
