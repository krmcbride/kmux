use std::path::Path;

use crate::agent::sessions::AgentSessionView;
use crate::paths::same_path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WorktreeMatchMode {
    /// Match the worktree identified by a status view without falling back from
    /// conflicting path hints to looser branch or handle metadata.
    Identity,
    /// Match any worktree hint supplied by an agent view for summary badges.
    AnyHint,
}

#[derive(Debug, Clone)]
pub(crate) struct WorktreeTarget<'a> {
    handle: Option<String>,
    branch: Option<String>,
    path: &'a Path,
}

impl<'a> WorktreeTarget<'a> {
    pub(crate) fn new(handle: Option<String>, branch: Option<String>, path: &'a Path) -> Self {
        Self {
            handle,
            branch,
            path,
        }
    }
}

pub(crate) fn view_matches_worktree(
    view: &AgentSessionView,
    target: &WorktreeTarget<'_>,
    mode: WorktreeMatchMode,
) -> bool {
    match mode {
        WorktreeMatchMode::Identity => view_matches_worktree_identity(view, target),
        WorktreeMatchMode::AnyHint => view_has_any_worktree_hint(view, target),
    }
}

fn view_matches_worktree_identity(view: &AgentSessionView, target: &WorktreeTarget<'_>) -> bool {
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
            .handle
            .as_deref()
            .is_some_and(|handle| view.target.kmux_worktree_handle.as_deref() == Some(handle))
}

fn view_has_any_worktree_hint(view: &AgentSessionView, target: &WorktreeTarget<'_>) -> bool {
    target
        .handle
        .as_deref()
        .is_some_and(|handle| view.target.kmux_worktree_handle.as_deref() == Some(handle))
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
            kmux_worktree_handle: Some("feature".to_owned()),
            git_branch: Some("feature".to_owned()),
            git_worktree_path: Some("/repo/project__worktrees/old".to_owned()),
            ..AgentLocationHints::default()
        });
        let target = target("feature", "feature", "/repo/project__worktrees/new");

        assert!(!view_matches_worktree(
            &view,
            &target,
            WorktreeMatchMode::Identity
        ));

        view.target.git_worktree_path = Some("/repo/project__worktrees/new".to_owned());
        assert!(view_matches_worktree(
            &view,
            &target,
            WorktreeMatchMode::Identity
        ));
    }

    #[test]
    fn identity_match_requires_branch_and_handle_without_path_hint() {
        let target = target(
            "feature",
            "feature/auth",
            "/repo/project__worktrees/feature",
        );
        let matching = view_with_target(AgentLocationHints {
            kmux_worktree_handle: Some("feature".to_owned()),
            git_branch: Some("feature/auth".to_owned()),
            ..AgentLocationHints::default()
        });
        let handle_only = view_with_target(AgentLocationHints {
            kmux_worktree_handle: Some("feature".to_owned()),
            git_branch: Some("other".to_owned()),
            ..AgentLocationHints::default()
        });
        let branch_only = view_with_target(AgentLocationHints {
            kmux_worktree_handle: Some("other".to_owned()),
            git_branch: Some("feature/auth".to_owned()),
            ..AgentLocationHints::default()
        });

        assert!(view_matches_worktree(
            &matching,
            &target,
            WorktreeMatchMode::Identity
        ));
        assert!(!view_matches_worktree(
            &handle_only,
            &target,
            WorktreeMatchMode::Identity
        ));
        assert!(!view_matches_worktree(
            &branch_only,
            &target,
            WorktreeMatchMode::Identity
        ));
    }

    #[test]
    fn identity_match_preserves_branchless_handle_matching() {
        let target = WorktreeTarget::new(
            Some("detached".to_owned()),
            None,
            Path::new("/repo/project__worktrees/detached"),
        );
        let view = view_with_target(AgentLocationHints {
            kmux_worktree_handle: Some("detached".to_owned()),
            git_branch: None,
            ..AgentLocationHints::default()
        });

        assert!(view_matches_worktree(
            &view,
            &target,
            WorktreeMatchMode::Identity
        ));
    }

    #[test]
    fn any_hint_match_accepts_handle_branch_worktree_path_or_directory() {
        let target = target(
            "feature",
            "feature/auth",
            "/repo/project__worktrees/feature",
        );
        let handle = view_with_target(AgentLocationHints {
            kmux_worktree_handle: Some("feature".to_owned()),
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

        for view in [&handle, &branch, &worktree_path, &directory] {
            assert!(view_matches_worktree(
                view,
                &target,
                WorktreeMatchMode::AnyHint
            ));
        }
    }

    #[test]
    fn any_hint_match_preserves_branchless_branch_matching() {
        let target = WorktreeTarget::new(None, None, Path::new("/repo/project"));
        let view = view_with_target(AgentLocationHints::default());

        assert!(view_matches_worktree(
            &view,
            &target,
            WorktreeMatchMode::AnyHint
        ));
    }

    fn target<'a>(handle: &str, branch: &str, path: &'a str) -> WorktreeTarget<'a> {
        WorktreeTarget::new(
            Some(handle.to_owned()),
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
            created_at: 100,
            status: AgentStatus::Working,
            status_changed_at: 100,
            working_elapsed_secs: 0,
            observed_at: 100,
            title: None,
            context: None,
            target,
        }
    }
}
