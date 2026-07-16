//! Serializable model for external agent observation state.
//!
//! These types define the JSON contract written by `set-agent-status` producers
//! and read by status/sidebar presentation.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
/// Persisted lifecycle status reported by an external agent producer.
pub enum AgentStatus {
    Working,
    Waiting,
    Done,
}

impl AgentStatus {
    /// Return the serialized status label used in tables and persisted state.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Done => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Logical agent session identity shared by multiple observation producers.
pub struct AgentSessionKey {
    pub agent_kind: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
/// Unique persisted observation identity for one session producer.
pub struct AgentObservationKey {
    pub session: AgentSessionKey,
    pub producer_kind: String,
    pub producer_instance: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
/// Optional location metadata used to attach agent sessions to tmux and Git.
pub struct AgentLocationHints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_window_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_session_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_window_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_pane_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_pane_current_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_pane_current_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_repo_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_repo_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kmux_workspace_slug: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub git_branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
/// Latest observed state from one producer for one logical agent session.
pub struct AgentObservationState {
    pub key: AgentObservationKey,
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_observed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_changed_at: Option<u64>,
    pub working_elapsed_secs: u64,
    pub observed_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default)]
    pub target: AgentLocationHints,
}
