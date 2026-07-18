//! Agent-owned baseline fixtures shared by unit tests.

use crate::agent::sessions::{
    AgentTmuxTarget, AgentTmuxUnavailableReason, ResolvedAgentSession, ResolvedAgentTarget,
    ResolvedAgentWorkspace,
};
use crate::state::{AgentSessionKey, AgentStatus};

/// Build a neutral resolved session baseline for focused field mutation in tests.
pub(super) fn resolved_agent_session() -> ResolvedAgentSession {
    ResolvedAgentSession {
        key: AgentSessionKey {
            agent_kind: "example-agent".to_owned(),
            session_id: "ses_project_alpha".to_owned(),
        },
        workspace: ResolvedAgentWorkspace::from_canonical_root(
            "/repo/project-alpha".into(),
            "/repo/project-alpha".to_owned(),
        )
        .expect("test workspace should be valid"),
        tmux_target: AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::Missing),
        created_at: 100,
        status: AgentStatus::Working,
        status_observed_at: 100,
        status_changed_at: 100,
        working_elapsed_secs: 0,
        observed_at: 100,
        title: None,
        context: None,
        target: ResolvedAgentTarget::default(),
    }
}
