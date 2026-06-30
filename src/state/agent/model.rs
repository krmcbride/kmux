use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Working,
    Waiting,
    Done,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Done => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AgentSessionKey {
    pub agent_kind: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AgentObservationKey {
    pub session: AgentSessionKey,
    pub producer_kind: String,
    pub producer_instance: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
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
pub struct AgentObservationState {
    pub key: AgentObservationKey,
    #[serde(default)]
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

impl AgentObservationState {
    pub fn effective_created_at(&self) -> u64 {
        if self.created_at != 0 {
            return self.created_at;
        }

        [
            self.status_changed_at,
            self.status_observed_at,
            Some(self.observed_at),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn effective_created_at_falls_back_for_old_observation_state() {
        let mut observation = test_observation("tui", "default/%1", AgentStatus::Working, 300);
        observation.created_at = 0;
        observation.status_observed_at = Some(250);
        observation.observed_at = 400;

        assert_eq!(observation.effective_created_at(), 250);
    }

    fn test_observation(
        producer_kind: &str,
        producer_instance: &str,
        status: AgentStatus,
        status_changed_at: u64,
    ) -> AgentObservationState {
        AgentObservationState {
            key: AgentObservationKey {
                session: AgentSessionKey {
                    agent_kind: "opencode".to_owned(),
                    session_id: "ses_root".to_owned(),
                },
                producer_kind: producer_kind.to_owned(),
                producer_instance: producer_instance.to_owned(),
            },
            created_at: status_changed_at,
            status: Some(status),
            status_observed_at: Some(status_changed_at),
            status_changed_at: Some(status_changed_at),
            working_elapsed_secs: 0,
            observed_at: status_changed_at,
            title: None,
            context: None,
            target: AgentLocationHints::default(),
        }
    }
}
