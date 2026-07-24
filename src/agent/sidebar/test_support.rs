use std::cell::RefCell;
use std::collections::HashMap;
use std::ops::{Deref, DerefMut};
use std::rc::Rc;

use anyhow::{Result, anyhow};

use crate::agent::sessions::{
    AgentTmuxTarget, AgentTmuxWindowCandidate, ResolvedAgentSession, ResolvedAgentWorkspace,
};
use crate::agent::sidebar::model::{SidebarIcons, SidebarRow, build_rows_with_working_icon};
use crate::agent::test_support::resolved_agent_session;
use crate::agent::workspace_activity::{WorkspaceActivity, workspace_activities_from_sessions};
use crate::config::{DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS, StatusIcons};
use crate::state::{AgentSessionKey, AgentStatus};
use crate::tmux::TmuxPaneVisibility;

use super::actions::{
    SidebarDeleteWorkspaceRowIntent, SidebarDeleteWorkspaceRowOutcome, SidebarJumpExecution,
    SidebarJumpFailure, SidebarJumpIntent, SidebarJumpOutcome,
};
use super::app::SidebarApp;
use super::model::SidebarRowIdentity;
use super::rows::{SidebarRefreshRowsIntent, SidebarRowsSnapshot};

/// Sleeping icon used by sidebar tests to assert idle-row rendering.
pub(super) const TEST_SLEEPING_ICON: &str = "z";

/// In-memory row source for sidebar application and presentation tests.
pub(super) struct TestSidebarRows {
    state: Rc<RefCell<TestSidebarRowsState>>,
}

/// Controls the next row-source observations returned to a sidebar app test.
#[derive(Clone)]
pub(super) struct TestSidebarRowsControl {
    state: Rc<RefCell<TestSidebarRowsState>>,
}

struct TestSidebarRowsState {
    visibility: TmuxPaneVisibility,
    rows: Vec<SidebarRow>,
    next_error: Option<String>,
}

impl TestSidebarRows {
    /// Create a row source and a control handle sharing the same in-memory state.
    pub fn new(rows: Vec<SidebarRow>) -> (Self, TestSidebarRowsControl) {
        let state = Rc::new(RefCell::new(TestSidebarRowsState {
            visibility: TmuxPaneVisibility {
                pane_has_focus: false,
                window_visible: true,
            },
            rows,
            next_error: None,
        }));
        (
            Self {
                state: Rc::clone(&state),
            },
            TestSidebarRowsControl { state },
        )
    }

    /// Return the configured pane visibility observation.
    pub fn visibility(&self) -> TmuxPaneVisibility {
        self.state.borrow().visibility
    }

    /// Return configured rows or the next injected refresh error.
    pub fn load(
        &self,
        _intent: SidebarRefreshRowsIntent<'_>,
        visibility: TmuxPaneVisibility,
    ) -> Result<SidebarRowsSnapshot> {
        let mut state = self.state.borrow_mut();
        if let Some(error) = state.next_error.take() {
            return Err(anyhow!(error));
        }
        let rows = state.rows.clone();
        let activity_count = rows.len();
        Ok(SidebarRowsSnapshot {
            visibility,
            rows,
            activity_count,
        })
    }
}

impl TestSidebarRowsControl {
    /// Set pane focus and window visibility for the next refresh.
    pub fn set_visibility(&self, visibility: TmuxPaneVisibility) {
        self.state.borrow_mut().visibility = visibility;
    }

    /// Replace the rows returned by subsequent successful refreshes.
    pub fn set_rows(&self, rows: Vec<SidebarRow>) {
        self.state.borrow_mut().rows = rows;
    }

    /// Make the next refresh fail with the provided message.
    pub fn fail_next_refresh(&self, error: impl Into<String>) {
        self.state.borrow_mut().next_error = Some(error.into());
    }
}

/// In-memory action service for sidebar application and presentation tests.
pub(super) struct TestSidebarActions {
    state: Rc<RefCell<TestSidebarActionsState>>,
}

/// Controls action outcomes and persisted selections for a sidebar app test.
#[derive(Clone)]
pub(super) struct TestSidebarActionsControl {
    state: Rc<RefCell<TestSidebarActionsState>>,
}

enum TestSelectionOption {
    Valid(SidebarRowIdentity),
    Malformed,
}

#[derive(Default)]
struct TestSidebarActionsState {
    selection_options: HashMap<String, TestSelectionOption>,
    next_jump: Option<SidebarJumpExecution>,
    next_delete_error: Option<String>,
}

impl TestSidebarActions {
    /// Create an action service and a control handle sharing in-memory state.
    pub fn new() -> (Self, TestSidebarActionsControl) {
        let state = Rc::new(RefCell::new(TestSidebarActionsState::default()));
        (
            Self {
                state: Rc::clone(&state),
            },
            TestSidebarActionsControl { state },
        )
    }

    /// Return a valid persisted selection, ignoring malformed values.
    pub fn persisted_selection_identity(&self, window_id: &str) -> Option<SidebarRowIdentity> {
        match self.state.borrow().selection_options.get(window_id) {
            Some(TestSelectionOption::Valid(identity)) => Some(identity.clone()),
            Some(TestSelectionOption::Malformed) | None => None,
        }
    }

    /// Return whether any persisted selection value exists for the window.
    pub fn selection_option_exists(&self, window_id: &str) -> bool {
        self.state
            .borrow()
            .selection_options
            .contains_key(window_id)
    }

    /// Store the selected row identity for the window.
    pub fn persist_selection_identity(
        &self,
        window_id: &str,
        identity: &SidebarRowIdentity,
    ) -> Result<()> {
        self.state.borrow_mut().selection_options.insert(
            window_id.to_owned(),
            TestSelectionOption::Valid(identity.clone()),
        );
        Ok(())
    }

    /// Execute the configured jump outcome or succeed with the requested row.
    pub fn execute_jump(&self, intent: SidebarJumpIntent) -> SidebarJumpExecution {
        self.state.borrow_mut().next_jump.take().unwrap_or_else(|| {
            SidebarJumpExecution::Succeeded(Box::new(SidebarJumpOutcome {
                row: intent.row,
                persistence_warning: None,
            }))
        })
    }

    /// Execute the configured deletion outcome or succeed with the request data.
    pub fn execute_delete_workspace_row(
        &self,
        intent: SidebarDeleteWorkspaceRowIntent,
    ) -> Result<SidebarDeleteWorkspaceRowOutcome> {
        if let Some(error) = self.state.borrow_mut().next_delete_error.take() {
            return Err(anyhow!(error));
        }
        Ok(SidebarDeleteWorkspaceRowOutcome {
            index: intent.index,
            row: intent.row,
        })
    }
}

impl TestSidebarActionsControl {
    /// Store a valid selection as if another sidebar process had written it.
    pub fn set_persisted_selection(&self, window_id: &str, identity: SidebarRowIdentity) {
        self.state
            .borrow_mut()
            .selection_options
            .insert(window_id.to_owned(), TestSelectionOption::Valid(identity));
    }

    /// Store an existing selection option whose payload cannot be decoded.
    pub fn set_malformed_selection(&self, window_id: &str) {
        self.state
            .borrow_mut()
            .selection_options
            .insert(window_id.to_owned(), TestSelectionOption::Malformed);
    }

    /// Return the valid persisted selection for assertions.
    pub fn persisted_selection(&self, window_id: &str) -> Option<SidebarRowIdentity> {
        match self.state.borrow().selection_options.get(window_id) {
            Some(TestSelectionOption::Valid(identity)) => Some(identity.clone()),
            Some(TestSelectionOption::Malformed) | None => None,
        }
    }

    /// Return whether a valid or malformed selection option exists.
    pub fn selection_option_exists(&self, window_id: &str) -> bool {
        self.state
            .borrow()
            .selection_options
            .contains_key(window_id)
    }

    /// Configure the next jump with its resolved row and optional warning.
    pub fn succeed_next_jump(&self, row: SidebarRow, persistence_warning: Option<String>) {
        self.state.borrow_mut().next_jump = Some(SidebarJumpExecution::Succeeded(Box::new(
            SidebarJumpOutcome {
                row,
                persistence_warning,
            },
        )));
    }

    /// Configure the next jump failure and optional rollback failure.
    pub fn fail_next_jump(&self, error: impl Into<String>, rollback_error: Option<String>) {
        self.state.borrow_mut().next_jump =
            Some(SidebarJumpExecution::Failed(SidebarJumpFailure {
                error: anyhow!(error.into()),
                rollback_error: rollback_error.map(|error| anyhow!(error)),
            }));
    }

    /// Configure the next workspace-row deletion to fail.
    pub fn fail_next_delete(&self, error: impl Into<String>) {
        self.state.borrow_mut().next_delete_error = Some(error.into());
    }
}

/// Sidebar app test harness with controls for its in-memory services.
pub(super) struct TestSidebarApp {
    app: SidebarApp,
    rows: TestSidebarRowsControl,
    actions: TestSidebarActionsControl,
}

impl TestSidebarApp {
    /// Wrap an app with controls for its injected in-memory services.
    pub fn new(
        app: SidebarApp,
        rows: TestSidebarRowsControl,
        actions: TestSidebarActionsControl,
    ) -> Self {
        Self { app, rows, actions }
    }

    /// Return the row-source control handle.
    pub fn rows_control(&self) -> &TestSidebarRowsControl {
        &self.rows
    }

    /// Return the action-service control handle.
    pub fn actions_control(&self) -> &TestSidebarActionsControl {
        &self.actions
    }
}

impl Deref for TestSidebarApp {
    type Target = SidebarApp;

    fn deref(&self) -> &Self::Target {
        &self.app
    }
}

impl DerefMut for TestSidebarApp {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.app
    }
}

/// Build deterministic sidebar icons for tests.
pub(super) fn test_icons() -> SidebarIcons {
    SidebarIcons::from_config(&StatusIcons {
        working: Some("?".to_owned()),
        waiting: Some("?".to_owned()),
        done: Some("?".to_owned()),
        sleeping: Some(TEST_SLEEPING_ICON.to_owned()),
        ..StatusIcons::default()
    })
}

/// Build the first sidebar row generated from a single agent session view.
pub(super) fn row_from_view(view: &ResolvedAgentSession, now: u64) -> SidebarRow {
    let activity = workspace_activities_from_sessions(vec![view.clone()]).remove(0);
    row_from_activity(&activity, now)
}

/// Build the sidebar row generated from one workspace activity aggregate.
pub(super) fn row_from_activity(activity: &WorkspaceActivity, now: u64) -> SidebarRow {
    let icons = test_icons();
    build_rows_with_working_icon(
        std::slice::from_ref(activity),
        now,
        &icons,
        None,
        DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS,
    )
    .remove(0)
}

/// Build an agent session view with stable tmux, repo, workspace, and pane metadata.
pub(super) fn report_state(
    status: AgentStatus,
    status_changed_at: u64,
    window_id: &str,
    pane_id: &str,
) -> ResolvedAgentSession {
    let mut session = resolved_agent_session();
    session.key = AgentSessionKey {
        agent_kind: "opencode".to_owned(),
        session_id: format!("ses_{pane_id}"),
    };
    session.workspace = resolved_workspace(format!("/repo__worktrees/feature-sidebar/{window_id}"));
    session.tmux_target = AgentTmuxTarget::Windows {
        session_name: "project".to_owned(),
        candidates: vec![AgentTmuxWindowCandidate {
            window_id: window_id.to_owned(),
            pane_ids: vec![pane_id.to_owned()],
        }],
    };
    session.created_at = status_changed_at;
    session.status = status;
    session.status_observed_at = status_changed_at;
    session.status_changed_at = status_changed_at;
    session.observed_at = status_changed_at;
    session.target.tmux_pane_id = Some(pane_id.to_owned());
    session.target.tmux_window_id = Some(window_id.to_owned());
    session.target.tmux_session_name = Some("project".to_owned());
    session.target.tmux_window_name = Some("kmux-feature-sidebar".to_owned());
    session.target.tmux_pane_title = Some("Implement sidebar".to_owned());
    session.target.tmux_pane_current_command = Some("nvim".to_owned());
    session.target.git_repo_name = Some("kmux".to_owned());
    session.target.git_repo_path = Some("/repo".to_owned());
    session.target.git_branch = Some("feature/sidebar".to_owned());
    session
}

/// Replace the logical session key on a test session fixture.
pub(super) fn set_session_key(view: &mut ResolvedAgentSession, key: AgentSessionKey) {
    view.key = key;
}

/// Replace the resolved workspace identity on a test session view.
pub(super) fn set_workspace(view: &mut ResolvedAgentSession, path: impl ToString) {
    view.workspace = resolved_workspace(path);
}

fn resolved_workspace(path: impl ToString) -> ResolvedAgentWorkspace {
    let path = path.to_string();
    ResolvedAgentWorkspace::from_canonical_root(path.clone().into(), path)
        .expect("test workspace should be valid")
}
