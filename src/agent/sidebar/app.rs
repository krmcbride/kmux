use anyhow::Result;
use ratatui::widgets::ListState;

use crate::agent::active::active_agents;
use crate::agent::sidebar::model::{
    SidebarRow, build_rows_with_working_icon, row_index_by_pane, row_index_by_window,
};
use crate::state::{AgentStatus, StateStore};
use crate::tmux::{Tmux, TmuxPaneVisibility};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionMode {
    FollowHost,
    Manual,
}

pub(super) struct SidebarApp {
    tmux: Tmux,
    store: StateStore,
    sleeping_icon: String,
    working_frames: Vec<String>,
    idle_after_seconds: u64,
    spinner_frame: usize,
    rows: Vec<SidebarRow>,
    list_state: ListState,
    sidebar_pane_id: Option<String>,
    host_window_id: Option<String>,
    selection_mode: SelectionMode,
    selected_pane_id: Option<String>,
    selected_window_id: Option<String>,
    sidebar_has_focus: bool,
    window_visible: bool,
    last_error: Option<String>,
    should_quit: bool,
    disable_requested: bool,
}

impl SidebarApp {
    pub(super) fn new(
        tmux: Tmux,
        store: StateStore,
        sleeping_icon: String,
        working_frames: Vec<String>,
        idle_after_seconds: u64,
    ) -> Self {
        let context = tmux.current_context().ok().flatten();
        let host_window_id = context.as_ref().map(|context| context.window_id.clone());
        let sidebar_pane_id = context.map(|context| context.pane_id);
        Self {
            tmux,
            store,
            sleeping_icon,
            working_frames,
            idle_after_seconds,
            spinner_frame: 0,
            rows: Vec::new(),
            list_state: ListState::default(),
            sidebar_pane_id,
            host_window_id,
            selection_mode: SelectionMode::FollowHost,
            selected_pane_id: None,
            selected_window_id: None,
            sidebar_has_focus: false,
            window_visible: true,
            last_error: None,
            should_quit: false,
            disable_requested: false,
        }
    }

    #[cfg(test)]
    pub(super) fn test(host_window_id: Option<&str>, rows: Vec<SidebarRow>) -> Self {
        let mut app = Self {
            tmux: Tmux::new(),
            store: test_state_store(),
            sleeping_icon: crate::agent::sidebar::model::TEST_SLEEPING_ICON.to_owned(),
            working_frames: Vec::new(),
            idle_after_seconds: crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
            spinner_frame: 0,
            rows,
            list_state: ListState::default(),
            sidebar_pane_id: None,
            host_window_id: host_window_id.map(str::to_owned),
            selection_mode: SelectionMode::FollowHost,
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

    pub(super) fn refresh_rows(&mut self) -> bool {
        let visibility = self.sidebar_visibility();
        self.refresh_rows_for_visibility(visibility)
    }

    fn refresh_rows_for_visibility(&mut self, visibility: TmuxPaneVisibility) -> bool {
        self.window_visible = visibility.window_visible;
        self.update_selection_mode_for_focus(visibility.pane_has_focus);
        if !self.should_refresh_model(visibility) {
            self.sync_selection();
            return false;
        }

        match active_agents(&self.store, &self.tmux) {
            Ok(agents) => {
                let working_icon = self.working_icon().map(str::to_owned);
                self.rows = build_rows_with_working_icon(
                    &agents,
                    crate::state::now_unix_seconds(),
                    &self.sleeping_icon,
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

    pub(super) fn request_disable(&mut self) {
        self.disable_requested = true;
        self.should_quit = true;
    }

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

    pub(super) fn previous(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let selected = self.list_state.selected().unwrap_or(0);
        self.select_index_manual(selected.saturating_sub(1));
    }

    pub(super) fn select_first(&mut self) {
        if !self.rows.is_empty() {
            self.select_index_manual(0);
        }
    }

    pub(super) fn select_last(&mut self) {
        if !self.rows.is_empty() {
            self.select_index_manual(self.rows.len() - 1);
        }
    }

    pub(super) fn jump_to_selected(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        if let Err(error) = self.select_row_target(&row) {
            let _ = self.refresh_rows();
            self.last_error = Some(format!("jump failed: {error}"));
        } else {
            self.reset_after_successful_jump(&row);
        }
    }

    pub(super) fn should_quit(&self) -> bool {
        self.should_quit
    }

    pub(super) fn disable_requested(&self) -> bool {
        self.disable_requested
    }

    pub(super) fn rows(&self) -> &[SidebarRow] {
        &self.rows
    }

    pub(super) fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }

    #[cfg(test)]
    pub(super) fn selected_index(&self) -> Option<usize> {
        self.list_state.selected()
    }

    pub(super) fn active_index(&self) -> Option<usize> {
        self.host_window_id
            .as_deref()
            .and_then(|window_id| row_index_by_window(&self.rows, window_id))
    }

    pub(super) fn cursor_index(&self) -> Option<usize> {
        self.sidebar_has_focus
            .then(|| self.list_state.selected())
            .flatten()
    }

    pub(super) fn list_state_mut(&mut self) -> &mut ListState {
        &mut self.list_state
    }

    pub(super) fn window_visible(&self) -> bool {
        self.window_visible
    }

    pub(super) fn should_animate_spinner(&self) -> bool {
        self.window_visible
            && !self.working_frames.is_empty()
            && self
                .rows
                .iter()
                .any(|row| row.status == AgentStatus::Working && !row.is_idle)
    }

    pub(super) fn tick_spinner(&mut self) {
        if !self.should_animate_spinner() {
            return;
        }

        self.advance_spinner_frame();
        let Some(icon) = self.working_icon().map(str::to_owned) else {
            return;
        };
        for row in &mut self.rows {
            if row.status == AgentStatus::Working && !row.is_idle {
                row.icon.clone_from(&icon);
            }
        }
    }

    fn should_refresh_model(&self, visibility: TmuxPaneVisibility) -> bool {
        visibility.window_visible || self.host_row().is_some_and(|row| !row.is_idle)
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
                .host_window_id
                .as_deref()
                .and_then(|window_id| row_index_by_window(&self.rows, window_id)),
            SelectionMode::Manual => Some(
                self.selected_pane_id
                    .as_deref()
                    .and_then(|pane_id| row_index_by_pane(&self.rows, pane_id))
                    .or_else(|| {
                        self.selected_window_id
                            .as_deref()
                            .and_then(|window_id| row_index_by_window(&self.rows, window_id))
                    })
                    .or_else(|| {
                        self.list_state
                            .selected()
                            .filter(|idx| *idx < self.rows.len())
                    })
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
        let _ = self.tmux.switch_client_to_session(&row.session_name);
        self.tmux.select_pane(&row.pane_id)
    }

    fn selected_row(&self) -> Option<&SidebarRow> {
        self.list_state
            .selected()
            .and_then(|index| self.rows.get(index))
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
            self.selected_pane_id = Some(row.pane_id.clone());
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
fn test_state_store() -> StateStore {
    use std::time::{SystemTime, UNIX_EPOCH};

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    StateStore::test_with_path(std::env::temp_dir().join(format!(
        "kmux-sidebar-test-empty-{}-{nanos}",
        std::process::id()
    )))
    .expect("test state store should be created")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sidebar::model::{TEST_SLEEPING_ICON, agent_state};
    use crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS;
    use crate::state::AgentStatus;

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
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Working, 100, "@1", "%1"),
                100,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Waiting, 100, "@2", "%2"),
                100,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Done, 0, "@3", "%3"),
                DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS + 1,
                TEST_SLEEPING_ICON,
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
        let rows = vec![SidebarRow::from_agent(
            &agent_state(AgentStatus::Waiting, 100, "@1", "%1"),
            100,
            TEST_SLEEPING_ICON,
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
        let rows = vec![SidebarRow::from_agent(
            &agent_state(AgentStatus::Working, 100, "@1", "%1"),
            100,
            TEST_SLEEPING_ICON,
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
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Done, 0, "@1", "%1"),
                DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS + 1,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Waiting, 100, "@2", "%2"),
                100,
                TEST_SLEEPING_ICON,
            ),
        ];
        let mut app = SidebarApp::test(Some("@1"), rows);
        app.next();
        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(app.selected_index(), Some(1));

        let refreshed = app.refresh_rows_for_visibility(TmuxPaneVisibility {
            pane_has_focus: false,
            window_visible: false,
        });

        assert!(!refreshed);
        assert!(!app.window_visible());
        assert_eq!(app.selection_mode, SelectionMode::FollowHost);
        assert_eq!(app.selected_index(), Some(0));
        assert_eq!(app.rows().len(), 2);
        assert_eq!(app.rows()[0].primary, "feature-sidebar");
    }

    #[test]
    fn hidden_missing_host_refresh_skips_model_rebuild() {
        let rows = vec![SidebarRow::from_agent(
            &agent_state(AgentStatus::Working, 100, "@other", "%1"),
            100,
            TEST_SLEEPING_ICON,
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
        let rows = vec![SidebarRow::from_agent(
            &agent_state(AgentStatus::Working, 100, "@1", "%1"),
            100,
            TEST_SLEEPING_ICON,
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
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Working, 100, "@1", "%1"),
                100,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Waiting, 100, "@2", "%2"),
                100,
                TEST_SLEEPING_ICON,
            ),
        ];
        let mut app = SidebarApp::test(Some("@2"), rows);

        assert_eq!(app.selected_index(), Some(1));
        assert_eq!(app.active_index(), Some(1));
        assert_eq!(app.cursor_index(), None);

        app.previous();

        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(app.selected_index(), Some(0));
        assert_eq!(app.active_index(), Some(1));
        assert_eq!(app.cursor_index(), Some(0));
    }

    #[test]
    fn selection_clears_when_followed_host_window_has_no_agent_row() {
        let rows = vec![SidebarRow::from_agent(
            &agent_state(AgentStatus::Working, 100, "@1", "%1"),
            100,
            TEST_SLEEPING_ICON,
        )];
        let mut app = SidebarApp::test(Some("@missing"), rows);

        assert_eq!(app.selection_mode, SelectionMode::FollowHost);
        assert_eq!(app.selected_index(), None);

        app.next();

        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(app.selected_index(), Some(0));
    }

    #[test]
    fn manual_selection_survives_empty_refresh_and_pane_id_change() {
        let rows = vec![
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Working, 100, "@1", "%1"),
                100,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Waiting, 100, "@2", "%2"),
                100,
                TEST_SLEEPING_ICON,
            ),
        ];
        let mut app = SidebarApp::test(Some("@1"), rows);

        app.next();
        assert_eq!(app.selected_index(), Some(1));

        app.rows = Vec::new();
        app.sync_selection();
        app.rows = vec![
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Working, 200, "@1", "%10"),
                200,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Waiting, 200, "@2", "%20"),
                200,
                TEST_SLEEPING_ICON,
            ),
        ];
        app.sync_selection();

        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(app.selected_index(), Some(1));
        assert_eq!(app.selected_window_id.as_deref(), Some("@2"));
        assert_eq!(app.selected_pane_id.as_deref(), Some("%20"));
    }

    #[test]
    fn manual_selection_returns_to_host_when_sidebar_loses_focus() {
        let rows = vec![
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Working, 100, "@1", "%1"),
                100,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Waiting, 100, "@2", "%2"),
                100,
                TEST_SLEEPING_ICON,
            ),
        ];
        let mut app = SidebarApp::test(Some("@1"), rows);

        app.next();
        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(app.selected_index(), Some(1));

        app.update_selection_mode_for_focus(false);
        app.sync_selection();

        assert_eq!(app.selection_mode, SelectionMode::FollowHost);
        assert_eq!(app.selected_index(), Some(0));
        assert_eq!(app.active_index(), Some(0));
        assert_eq!(app.cursor_index(), None);
        assert_eq!(app.selected_window_id.as_deref(), Some("@1"));
        assert_eq!(app.selected_pane_id.as_deref(), Some("%1"));
    }

    #[test]
    fn successful_cross_window_jump_marks_source_sidebar_hidden() {
        let rows = vec![
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Working, 100, "@1", "%1"),
                100,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Waiting, 100, "@2", "%2"),
                100,
                TEST_SLEEPING_ICON,
            ),
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
        let rows = vec![SidebarRow::from_agent(
            &agent_state(AgentStatus::Working, 100, "@1", "%1"),
            100,
            TEST_SLEEPING_ICON,
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
    fn jump_failure_is_reported_without_panicking_or_quitting() {
        let rows = vec![SidebarRow::from_agent(
            &agent_state(AgentStatus::Waiting, 100, "not-a-window", "%missing"),
            100,
            TEST_SLEEPING_ICON,
        )];
        let mut app = SidebarApp::test(Some("not-a-window"), rows);

        app.jump_to_selected();

        assert!(!app.should_quit());
        assert!(
            app.last_error()
                .is_some_and(|error| error.contains("jump failed"))
        );
    }
}
