//! Query predicates for matching agent session views to kmux workspaces.
//!
//! The functions here do not load state themselves; callers provide already-built
//! `AgentSessionView` values and a workspace target, then choose the matching
//! strictness needed for identity-sensitive actions or summary badges.

use std::path::Path;

use crate::agent::sessions::AgentSessionView;
use crate::paths::same_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Matching strategy for relating agent observations to a workspace.
pub enum WorkspaceMatchMode {
    /// Match the workspace identified by a status view without falling back from
    /// conflicting path hints to looser branch or slug metadata.
    Identity,
    /// Match the resolved Git-root identity for summary badges.
    AnyHint,
}

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

/// Return whether an agent view matches a workspace according to the requested mode.
pub fn view_matches_workspace(
    view: &AgentSessionView,
    target: &WorkspaceTarget<'_>,
    mode: WorkspaceMatchMode,
) -> bool {
    match mode {
        WorkspaceMatchMode::Identity => view_matches_workspace_identity(view, target),
        WorkspaceMatchMode::AnyHint => view_has_any_workspace_hint(view, target),
    }
}

// Identity mode requires filesystem identity. Branch and slug are useful display
// hints, but they are not strong enough for identity-sensitive matching.
fn view_matches_workspace_identity(view: &AgentSessionView, target: &WorkspaceTarget<'_>) -> bool {
    view.target
        .git_worktree_path
        .as_deref()
        .or(view.target.directory.as_deref())
        .is_some_and(|path| path_matches(path, target.path))
}

// Summary mode uses the same Git-root identity. Branches, slugs, and raw
// directories are display hints rather than workspace identity.
fn view_has_any_workspace_hint(view: &AgentSessionView, target: &WorkspaceTarget<'_>) -> bool {
    view_matches_workspace_identity(view, target)
}

fn path_matches(path: &str, target: &Path) -> bool {
    same_path(Path::new(path), target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AgentLocationHints, AgentSessionKey, AgentStatus};

    #[test]
    fn identity_match_uses_path_when_path_hint_exists() {
        let mut view = view_with_target(AgentLocationHints {
            kmux_workspace_slug: Some("feature".to_owned()),
            git_branch: Some("feature".to_owned()),
            git_worktree_path: Some("/repo/project__worktrees/old".to_owned()),
            ..AgentLocationHints::default()
        });
        let target = target("feature", "feature", "/repo/project__worktrees/new");

        assert!(!view_matches_workspace(
            &view,
            &target,
            WorkspaceMatchMode::Identity
        ));

        view.target.git_worktree_path = Some("/repo/project__worktrees/new".to_owned());
        assert!(view_matches_workspace(
            &view,
            &target,
            WorkspaceMatchMode::Identity
        ));
    }

    #[test]
    fn identity_match_requires_path_hint() {
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

        assert!(!view_matches_workspace(
            &slug_and_branch,
            &target,
            WorkspaceMatchMode::Identity
        ));
    }

    #[test]
    fn any_hint_match_accepts_worktree_path_or_directory() {
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
            assert!(view_matches_workspace(
                view,
                &target,
                WorkspaceMatchMode::AnyHint
            ));
        }
        for view in [&slug, &branch] {
            assert!(!view_matches_workspace(
                view,
                &target,
                WorkspaceMatchMode::AnyHint
            ));
        }
    }

    #[test]
    fn any_hint_match_requires_path_hint() {
        let target = WorkspaceTarget::new(Path::new("/repo/project"));
        let view = view_with_target(AgentLocationHints::default());

        assert!(!view_matches_workspace(
            &view,
            &target,
            WorkspaceMatchMode::AnyHint
        ));
    }

    fn target<'a>(_workspace_slug: &str, _branch: &str, path: &'a str) -> WorkspaceTarget<'a> {
        WorkspaceTarget::new(Path::new(path))
    }

    fn view_with_target(target: AgentLocationHints) -> AgentSessionView {
        AgentSessionView {
            key: AgentSessionKey {
                agent_kind: "opencode".to_owned(),
                session_id: "ses".to_owned(),
            },
            workspace_key: None,
            tmux_target: crate::agent::sessions::AgentTmuxTarget::None,
            created_at: 100,
            status: AgentStatus::Working,
            status_observed_at: 100,
            status_changed_at: 100,
            working_elapsed_secs: 0,
            observed_at: 100,
            title: None,
            context: None,
            target,
        }
    }
}
