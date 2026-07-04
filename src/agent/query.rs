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
    /// Match any workspace hint supplied by an agent view for summary badges.
    AnyHint,
}

#[derive(Debug, Clone)]
/// Workspace identity used when matching agent observations.
pub struct WorkspaceTarget<'a> {
    workspace_slug: Option<String>,
    branch: Option<String>,
    path: &'a Path,
}

impl<'a> WorkspaceTarget<'a> {
    /// Build a workspace target from the identifiers known for a Git worktree.
    pub fn new(workspace_slug: Option<String>, branch: Option<String>, path: &'a Path) -> Self {
        Self {
            workspace_slug,
            branch,
            path,
        }
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

// Identity mode trusts explicit path hints above branch/slug hints because agents
// can move between windows while preserving stale logical metadata.
fn view_matches_workspace_identity(view: &AgentSessionView, target: &WorkspaceTarget<'_>) -> bool {
    let report_path = view
        .target
        .git_worktree_path
        .as_deref()
        .or(view.target.directory.as_deref());
    if let Some(report_path) = report_path {
        return path_matches(report_path, target.path);
    }

    view.target.git_branch.as_deref() == target.branch.as_deref()
        && target
            .workspace_slug
            .as_deref()
            .is_some_and(|slug| view.target.kmux_workspace_slug.as_deref() == Some(slug))
}

// Summary mode is intentionally looser so list/status badges still show agents
// that only reported one useful workspace hint.
fn view_has_any_workspace_hint(view: &AgentSessionView, target: &WorkspaceTarget<'_>) -> bool {
    target
        .workspace_slug
        .as_deref()
        .is_some_and(|slug| view.target.kmux_workspace_slug.as_deref() == Some(slug))
        || view.target.git_branch.as_deref() == target.branch.as_deref()
        || view
            .target
            .git_worktree_path
            .as_deref()
            .is_some_and(|path| path_matches(path, target.path))
        || view
            .target
            .directory
            .as_deref()
            .is_some_and(|path| path_matches(path, target.path))
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
    fn identity_match_requires_branch_and_slug_without_path_hint() {
        let target = target(
            "feature",
            "feature/auth",
            "/repo/project__worktrees/feature",
        );
        let matching = view_with_target(AgentLocationHints {
            kmux_workspace_slug: Some("feature".to_owned()),
            git_branch: Some("feature/auth".to_owned()),
            ..AgentLocationHints::default()
        });
        let slug_only = view_with_target(AgentLocationHints {
            kmux_workspace_slug: Some("feature".to_owned()),
            git_branch: Some("other".to_owned()),
            ..AgentLocationHints::default()
        });
        let branch_only = view_with_target(AgentLocationHints {
            kmux_workspace_slug: Some("other".to_owned()),
            git_branch: Some("feature/auth".to_owned()),
            ..AgentLocationHints::default()
        });

        assert!(view_matches_workspace(
            &matching,
            &target,
            WorkspaceMatchMode::Identity
        ));
        assert!(!view_matches_workspace(
            &slug_only,
            &target,
            WorkspaceMatchMode::Identity
        ));
        assert!(!view_matches_workspace(
            &branch_only,
            &target,
            WorkspaceMatchMode::Identity
        ));
    }

    #[test]
    fn identity_match_preserves_branchless_slug_matching() {
        let target = WorkspaceTarget::new(
            Some("detached".to_owned()),
            None,
            Path::new("/repo/project__worktrees/detached"),
        );
        let view = view_with_target(AgentLocationHints {
            kmux_workspace_slug: Some("detached".to_owned()),
            git_branch: None,
            ..AgentLocationHints::default()
        });

        assert!(view_matches_workspace(
            &view,
            &target,
            WorkspaceMatchMode::Identity
        ));
    }

    #[test]
    fn any_hint_match_accepts_slug_branch_worktree_path_or_directory() {
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

        for view in [&slug, &branch, &worktree_path, &directory] {
            assert!(view_matches_workspace(
                view,
                &target,
                WorkspaceMatchMode::AnyHint
            ));
        }
    }

    #[test]
    fn any_hint_match_preserves_branchless_branch_matching() {
        let target = WorkspaceTarget::new(None, None, Path::new("/repo/project"));
        let view = view_with_target(AgentLocationHints::default());

        assert!(view_matches_workspace(
            &view,
            &target,
            WorkspaceMatchMode::AnyHint
        ));
    }

    fn target<'a>(workspace_slug: &str, branch: &str, path: &'a str) -> WorkspaceTarget<'a> {
        WorkspaceTarget::new(
            Some(workspace_slug.to_owned()),
            Some(branch.to_owned()),
            Path::new(path),
        )
    }

    fn view_with_target(target: AgentLocationHints) -> AgentSessionView {
        AgentSessionView {
            key: AgentSessionKey {
                agent_kind: "opencode".to_owned(),
                session_id: "ses".to_owned(),
            },
            directory_key: None,
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
