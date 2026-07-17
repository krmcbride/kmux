//! Query predicates for matching agent session views to kmux workspaces.
//!
//! The functions here do not load state themselves; callers provide already-built
//! `ResolvedAgentSession` values and a workspace target.

use std::path::Path;

use crate::agent::sessions::ResolvedAgentSession;
use crate::paths::same_path;

#[derive(Debug, Clone)]
/// Workspace identity used when matching agent observations.
pub struct WorkspaceTarget<'a> {
    path: &'a Path,
}

impl<'a> WorkspaceTarget<'a> {
    /// Build a workspace target from the canonical Git worktree root.
    pub fn new(path: &'a Path) -> Self {
        Self { path }
    }
}

/// Return whether an agent view matches a workspace by filesystem identity.
///
/// A resolved workspace identity wins over stale path hints when available;
/// otherwise the worktree path or reported directory must match the target.
pub fn view_matches_workspace(view: &ResolvedAgentSession, target: &WorkspaceTarget<'_>) -> bool {
    if let Some(workspace) = view.workspace.as_ref() {
        return same_path(workspace.identity().root(), target.path);
    }

    view.git_worktree_path()
        .or_else(|| view.directory())
        .is_some_and(|path| path_matches(path, target.path))
}

fn path_matches(path: &str, target: &Path) -> bool {
    same_path(Path::new(path), target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sessions::ResolvedAgentWorkspace;
    use crate::state::{AgentLocationHints, AgentSessionKey, AgentStatus};
    use std::path::PathBuf;

    #[test]
    fn workspace_match_uses_path_when_path_hint_exists() {
        let mut view = view_with_target(AgentLocationHints {
            kmux_workspace_slug: Some("feature".to_owned()),
            git_branch: Some("feature".to_owned()),
            git_worktree_path: Some("/repo/project__worktrees/old".to_owned()),
            ..AgentLocationHints::default()
        });
        let target = target("feature", "feature", "/repo/project__worktrees/new");

        assert!(!view_matches_workspace(&view, &target));

        view.target.git_worktree_path = Some("/repo/project__worktrees/new".to_owned());
        assert!(view_matches_workspace(&view, &target));
    }

    #[test]
    fn workspace_match_requires_path_hint() {
        let target = target(
            "feature",
            "feature/auth",
            "/repo/project__worktrees/feature",
        );
        let slug_and_branch = view_with_target(AgentLocationHints {
            kmux_workspace_slug: Some("feature".to_owned()),
            git_branch: Some("feature/auth".to_owned()),
            ..AgentLocationHints::default()
        });

        assert!(!view_matches_workspace(&slug_and_branch, &target));
    }

    #[test]
    fn workspace_match_prefers_resolved_workspace_identity_over_stale_hints() {
        let target = target("feature", "feature", "/repo/project__worktrees/new");
        let mut view = view_with_target(AgentLocationHints {
            git_worktree_path: Some("/repo/project__worktrees/old".to_owned()),
            ..AgentLocationHints::default()
        });
        view.workspace = Some(
            ResolvedAgentWorkspace::from_canonical_root(
                PathBuf::from("/repo/project__worktrees/new"),
                "/repo/project__worktrees/new".to_owned(),
            )
            .expect("workspace identity should be valid"),
        );

        assert!(view_matches_workspace(&view, &target));
    }

    #[test]
    fn workspace_match_accepts_worktree_path_or_directory() {
        let target = target(
            "feature",
            "feature/auth",
            "/repo/project__worktrees/feature",
        );
        let slug = view_with_target(AgentLocationHints {
            kmux_workspace_slug: Some("feature".to_owned()),
            ..AgentLocationHints::default()
        });
        let branch = view_with_target(AgentLocationHints {
            git_branch: Some("feature/auth".to_owned()),
            ..AgentLocationHints::default()
        });
        let worktree_path = view_with_target(AgentLocationHints {
            git_worktree_path: Some("/repo/project__worktrees/feature".to_owned()),
            ..AgentLocationHints::default()
        });
        let directory = view_with_target(AgentLocationHints {
            directory: Some("/repo/project__worktrees/feature".to_owned()),
            ..AgentLocationHints::default()
        });

        for view in [&worktree_path, &directory] {
            assert!(view_matches_workspace(view, &target));
        }
        for view in [&slug, &branch] {
            assert!(!view_matches_workspace(view, &target));
        }
    }

    #[test]
    fn workspace_match_requires_any_path_hint() {
        let target = WorkspaceTarget::new(Path::new("/repo/project"));
        let view = view_with_target(AgentLocationHints::default());

        assert!(!view_matches_workspace(&view, &target));
    }

    fn target<'a>(_workspace_slug: &str, _branch: &str, path: &'a str) -> WorkspaceTarget<'a> {
        WorkspaceTarget::new(Path::new(path))
    }

    fn view_with_target(target: AgentLocationHints) -> ResolvedAgentSession {
        let key = AgentSessionKey {
            agent_kind: "opencode".to_owned(),
            session_id: "ses".to_owned(),
        };
        ResolvedAgentSession {
            member_session_keys: vec![key.clone()],
            key,
            workspace: None,
            tmux_target: crate::agent::sessions::AgentTmuxTarget::None,
            created_at: 100,
            status: AgentStatus::Working,
            status_observed_at: 100,
            status_changed_at: 100,
            working_elapsed_secs: 0,
            observed_at: 100,
            title: None,
            context: None,
            target: target.into(),
        }
    }
}
