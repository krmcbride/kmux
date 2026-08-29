//! Resolved agent session, workspace, and tmux navigation models.
//!
//! These application-facing shapes carry the settled result of observation
//! reconciliation. Persisted observation records and live tmux snapshots remain
//! inputs owned by sibling policy modules rather than leaking into consumers.

use std::path::PathBuf;

use anyhow::Result;

use crate::state::{AgentSessionKey, AgentStatus};
use crate::workspace::WorkspaceIdentity;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Resolved workspace identity associated with an agent session.
pub struct ResolvedAgentWorkspace {
    key: String,
    path: String,
    reported_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One logical agent session reconciled from all of its reporter observations.
pub struct ResolvedAgentSession {
    pub key: AgentSessionKey,
    pub workspace: ResolvedAgentWorkspace,
    pub tmux_target: AgentTmuxTarget,
    pub created_at: u64,
    pub status: AgentStatus,
    pub status_observed_at: u64,
    pub status_changed_at: u64,
    pub working_elapsed_secs: u64,
    pub observed_at: u64,
    pub title: Option<String>,
    pub context: Option<String>,
    pub target: ResolvedAgentTarget,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Resolved navigation, display, and workspace facts for an agent session.
pub struct ResolvedAgentTarget {
    pub tmux_pane_id: Option<String>,
    pub tmux_window_id: Option<String>,
    pub tmux_session_name: Option<String>,
    pub tmux_window_name: Option<String>,
    pub tmux_pane_title: Option<String>,
    pub tmux_pane_current_command: Option<String>,
    pub git_repo_name: Option<String>,
    pub git_repo_path: Option<String>,
    pub git_branch: Option<String>,
    pub directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Live tmux navigation candidates or a focused unavailability reason.
pub enum AgentTmuxTarget {
    Windows {
        session_name: String,
        candidates: Vec<AgentTmuxWindowCandidate>,
    },
    Unavailable(AgentTmuxUnavailableReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One matching physical window and its preferred matching pane order.
pub struct AgentTmuxWindowCandidate {
    pub window_id: String,
    pub pane_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Reason a workspace activity row cannot currently be routed through tmux.
pub enum AgentTmuxUnavailableReason {
    Missing,
    CrossSession { session_names: Vec<String> },
}

/// Return the shared application priority for an agent activity status.
///
/// Workspace primary-session selection and defensive same-window badge collapse
/// both use this ordering so presentation surfaces cannot disagree about which
/// status is most important.
pub fn activity_status_priority(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Waiting => 3,
        AgentStatus::Working => 2,
        AgentStatus::Done => 1,
    }
}

impl ResolvedAgentWorkspace {
    /// Build a resolved workspace from a canonical Git worktree root.
    pub fn from_canonical_root(
        canonical_worktree_root: PathBuf,
        reported_path: String,
    ) -> Result<Self> {
        let identity = WorkspaceIdentity::from_canonical_root(canonical_worktree_root)?;
        let path = identity.root().display().to_string();
        Ok(Self {
            key: path.clone(),
            path,
            reported_path,
        })
    }

    /// Return the stable grouping key for this workspace.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Return the canonical Git worktree root as display text.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Return the path originally reported by the reporter before Git-root resolution.
    pub fn reported_path(&self) -> &str {
        &self.reported_path
    }
}

impl ResolvedAgentSession {
    /// Return elapsed time for the current status at `now`.
    ///
    /// Working rows accumulate prior working time plus the active working span;
    /// waiting and done rows show time since their current status began.
    pub fn elapsed_secs(&self, now: u64) -> u64 {
        let status_age = now.saturating_sub(self.status_changed_at);
        match self.status {
            AgentStatus::Working => self.working_elapsed_secs.saturating_add(status_age),
            AgentStatus::Waiting | AgentStatus::Done => status_age,
        }
    }

    /// Return the canonical workspace grouping key.
    pub fn workspace_key(&self) -> &str {
        self.workspace.key()
    }

    /// Return the canonical Git worktree path.
    pub fn workspace_path(&self) -> &str {
        self.workspace.path()
    }

    /// Return the best known Git repo name for display.
    pub fn git_repo_name(&self) -> Option<&str> {
        self.target.git_repo_name.as_deref()
    }

    /// Return the best known main Git repository path for display.
    pub fn git_repo_path(&self) -> Option<&str> {
        self.target.git_repo_path.as_deref()
    }

    /// Return the resolved Git worktree path used for matching and display.
    pub fn git_worktree_path(&self) -> &str {
        self.workspace_path()
    }

    /// Return the best known Git branch name for display and filtering.
    pub fn git_branch(&self) -> Option<&str> {
        self.target.git_branch.as_deref()
    }

    /// Return the latest reporter-provided directory, if one was provided.
    pub fn directory(&self) -> Option<&str> {
        self.target
            .directory
            .as_deref()
            .or_else(|| Some(self.workspace.reported_path()))
    }

    /// Return the resolved tmux window id for navigation.
    pub fn tmux_window_id(&self) -> Option<&str> {
        self.target.tmux_window_id.as_deref()
    }

    /// Return the resolved tmux window name for display.
    pub fn tmux_window_name(&self) -> Option<&str> {
        self.target.tmux_window_name.as_deref()
    }

    /// Return the exact tmux session selected by reconciliation.
    pub fn tmux_session_name(&self) -> Option<&str> {
        self.target.tmux_session_name.as_deref()
    }

    /// Return the preferred matching non-sidebar pane ID.
    pub fn tmux_pane_id(&self) -> Option<&str> {
        self.target.tmux_pane_id.as_deref()
    }

    /// Return the live tmux pane title captured for display fallback.
    pub fn tmux_pane_title(&self) -> Option<&str> {
        self.target.tmux_pane_title.as_deref()
    }

    /// Return the live tmux pane command captured for display fallback.
    pub fn tmux_pane_current_command(&self) -> Option<&str> {
        self.target.tmux_pane_current_command.as_deref()
    }
}
