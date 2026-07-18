//! Query predicates for matching workspace activity to kmux inventory.
//!
//! The functions here do not load state themselves; callers provide already-built
//! workspace aggregates and a workspace target.

use std::path::Path;

use crate::agent::workspace_activity::WorkspaceActivity;
use crate::paths::same_path;

#[derive(Debug, Clone)]
/// Workspace identity used when matching workspace activity aggregates.
pub struct WorkspaceTarget<'a> {
    path: &'a Path,
}

impl<'a> WorkspaceTarget<'a> {
    /// Build a workspace target from the canonical Git worktree root.
    pub fn new(path: &'a Path) -> Self {
        Self { path }
    }
}

/// Return whether an activity's canonical Git root matches a workspace target.
pub fn activity_matches_workspace(
    activity: &WorkspaceActivity,
    target: &WorkspaceTarget<'_>,
) -> bool {
    same_path(Path::new(activity.workspace_key()), target.path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sessions::{
        AgentTmuxTarget, AgentTmuxUnavailableReason, ResolvedAgentSession, ResolvedAgentTarget,
        ResolvedAgentWorkspace,
    };
    use crate::agent::workspace_activity::workspace_activities_from_sessions;
    use crate::state::{AgentSessionKey, AgentStatus};

    #[test]
    fn workspace_match_uses_canonical_activity_identity() {
        let activities = workspace_activities_from_sessions(vec![resolved_session()]);
        let matching = WorkspaceTarget::new(Path::new("/repo/project-alpha"));
        let different = WorkspaceTarget::new(Path::new("/repo/project-beta"));

        assert!(activity_matches_workspace(&activities[0], &matching));
        assert!(!activity_matches_workspace(&activities[0], &different));
    }

    fn resolved_session() -> ResolvedAgentSession {
        let key = AgentSessionKey {
            agent_kind: "opencode".to_owned(),
            session_id: "ses".to_owned(),
        };
        ResolvedAgentSession {
            key,
            workspace: ResolvedAgentWorkspace::from_canonical_root(
                "/repo/project-alpha".into(),
                "/repo/project-alpha".to_owned(),
            )
            .expect("workspace identity should be valid"),
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
}
