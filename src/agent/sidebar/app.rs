use anyhow::Result;
use ratatui::widgets::ListState;

use crate::agent::active::active_agents;
use crate::agent::sidebar::model::{
    SidebarRow, build_rows_with_working_icon, row_index_by_pane, row_index_by_window,
};
use crate::state::{AgentStatus, StateStore};
use crate::tmux::Tmux;

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
    spinner_frame: usize,
    rows: Vec<SidebarRow>,
    list_state: ListState,
    sidebar_pane_id: Option<String>,
    host_window_id: Option<String>,
    selection_mode: SelectionMode,
    selected_pane_id: Option<String>,
    selected_window_id: Option<String>,
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
    ) -> Self {
        let context = tmux.current_context().ok().flatten();
        let host_window_id = context.as_ref().map(|context| context.window_id.clone());
        let sidebar_pane_id = context.map(|context| context.pane_id);
        Self {
            tmux,
            store,
            sleeping_icon,
            working_frames,
            spinner_frame: 0,
            rows: Vec::new(),
            list_state: ListState::default(),
            sidebar_pane_id,
            host_window_id,
            selection_mode: SelectionMode::FollowHost,
            selected_pane_id: None,
            selected_window_id: None,
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
            spinner_frame: 0,
            rows,
            list_state: ListState::default(),
            sidebar_pane_id: None,
            host_window_id: host_window_id.map(str::to_owned),
            selection_mode: SelectionMode::FollowHost,
            selected_pane_id: None,
            selected_window_id: None,
            last_error: None,
            should_quit: false,
            disable_requested: false,
        };
        app.sync_selection();
        app
    }

    pub(super) fn refresh_rows(&mut self) {
        let sidebar_has_focus = self.sidebar_has_focus();
        match active_agents(&self.store, &self.tmux) {
            Ok(agents) => {
                let working_icon = self.working_icon().map(str::to_owned);
                self.rows = build_rows_with_working_icon(
                    &agents,
                    crate::state::now_unix_seconds(),
                    &self.sleeping_icon,
                    working_icon.as_deref(),
                );
                self.last_error = None;
                self.update_selection_mode_for_focus(sidebar_has_focus);
                self.sync_selection();
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
            }
        }
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
            self.refresh_rows();
            self.last_error = Some(format!("jump failed: {error}"));
        } else {
            self.selection_mode = SelectionMode::FollowHost;
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

    pub(super) fn selected_index(&self) -> Option<usize> {
        self.list_state.selected()
    }

    pub(super) fn list_state_mut(&mut self) -> &mut ListState {
        &mut self.list_state
    }

    pub(super) fn should_animate_spinner(&self) -> bool {
        !self.working_frames.is_empty()
            && self
                .rows
                .iter()
                .any(|row| row.status == AgentStatus::Working && !row.is_stale)
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
            if row.status == AgentStatus::Working && !row.is_stale {
                row.icon.clone_from(&icon);
            }
        }
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

    fn select_index_manual(&mut self, index: usize) {
        self.selection_mode = SelectionMode::Manual;
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

    fn sidebar_has_focus(&self) -> bool {
        self.sidebar_pane_id
            .as_deref()
            .is_some_and(|pane_id| self.tmux.pane_has_focus(pane_id).unwrap_or(false))
    }

    fn update_selection_mode_for_focus(&mut self, sidebar_has_focus: bool) {
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
                crate::agent::sidebar::model::STALE_AFTER_SECONDS + 1,
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

        app.previous();

        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(app.selected_index(), Some(0));
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
        assert_eq!(app.selected_window_id.as_deref(), Some("@1"));
        assert_eq!(app.selected_pane_id.as_deref(), Some("%1"));
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
