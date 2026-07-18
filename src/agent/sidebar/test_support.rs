use std::ops::{Deref, DerefMut};

use anyhow::Result;
use tempfile::{TempDir, tempdir};

use crate::agent::sessions::{
    AgentTmuxTarget, AgentTmuxWindowCandidate, ResolvedAgentSession, ResolvedAgentWorkspace,
};
use crate::agent::sidebar::model::{SidebarIcons, SidebarRow, build_rows_with_working_icon};
use crate::agent::sidebar::selection::{
    SELECTED_TARGET_OPTION, decode_selected_target, encode_selected_target,
};
use crate::agent::test_support::resolved_agent_session;
use crate::agent::workspace_activity::{WorkspaceActivity, workspace_activities_from_sessions};
use crate::config::{DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS, StatusIcons};
use crate::state::test_support::StateStoreFixture;
use crate::state::{AgentSessionKey, AgentStatus, StateStore};
use crate::tmux::Tmux;
use crate::tmux::test_support::{TmuxFixture, create_test_session};

use super::app::SidebarApp;
use super::model::SidebarRowIdentity;

/// Sleeping icon used by sidebar tests to assert idle-row rendering.
pub(super) const TEST_SLEEPING_ICON: &str = "z";

/// Sidebar app test harness that owns its temporary state for the app lifetime.
pub(super) struct TestSidebarApp {
    app: SidebarApp,
    _state: StateStoreFixture,
}

impl TestSidebarApp {
    /// Wrap a sidebar app with the fixture that owns its backing state path.
    pub fn new(app: SidebarApp, state: StateStoreFixture) -> Self {
        Self { app, _state: state }
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

/// Isolated tmux workspace shared by sidebar action and app unit tests.
pub(super) struct SidebarTmuxFixture {
    pub fixture: TmuxFixture,
    pub window_id: String,
    temp: TempDir,
}

impl SidebarTmuxFixture {
    /// Create one test session and one workspace window on an isolated socket.
    pub fn new() -> Result<Option<Self>> {
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
            window_id,
            temp,
        }))
    }

    /// Clone the isolated tmux adapter for app construction.
    pub fn tmux(&self) -> Tmux {
        self.fixture.tmux.clone()
    }

    /// Open agent state under the fixture's owned temporary directory.
    pub fn state_store(&self) -> Result<StateStore> {
        crate::state::test_support::store_with_path(self.temp.path().join("state"))
    }

    /// Persist the selected-target representation for a sidebar row.
    pub fn set_selected_row(&self, row: &SidebarRow) -> Result<()> {
        let encoded = encode_selected_target(&row.identity)?;
        self.set_raw_selected_target(&encoded)
    }

    /// Persist an exact selected-target payload for decoder and rollback tests.
    pub fn set_raw_selected_target(&self, value: &str) -> Result<()> {
        self.fixture
            .tmux
            .set_window_option(&self.window_id, SELECTED_TARGET_OPTION, value)
    }

    /// Read the exact persisted selected-target payload.
    pub fn raw_selected_target(&self) -> Result<Option<String>> {
        self.fixture
            .tmux
            .show_window_option(&self.window_id, SELECTED_TARGET_OPTION)
    }

    /// Decode the currently persisted sidebar row identity.
    pub fn selected_target(&self) -> Result<Option<SidebarRowIdentity>> {
        Ok(self
            .raw_selected_target()?
            .as_deref()
            .and_then(decode_selected_target))
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
