use anyhow::{Result, bail};

use crate::cli;
use crate::state::workspace::{WorkspaceParentLink, WorkspaceState, WorkspaceStateStore};

use super::context::{RepoContext, load_repo_context};
use super::project_session;
use super::resolve::{resolve_current_kmux_workspace, resolve_workspace};
use crate::workspace::WorkspaceRecord;

/// Set or replace a workspace parent link without changing branches, worktrees, or tmux windows.
pub(super) fn run(args: cli::ParentArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let _lifecycle_lock = project_session::lock_project_lifecycle(&repo.paths)?;
    let (resolved, parent) = resolve_target(&repo, args)?;
    let child = resolved.branch().ok_or_else(|| {
        anyhow::anyhow!(
            "workspace '{}' has no known git branch and cannot have a parent",
            resolved.workspace_slug()
        )
    })?;

    let anchor = record_parent(&repo, child, &parent)?;
    println!(
        "set parent of {child} to {parent} @ {}",
        short_anchor(&anchor)
    );
    Ok(())
}

/// Validate and persist a parent link while the caller holds the project lifecycle lock.
///
/// Add already holds the lock through its resolved tmux context; the standalone
/// parent command acquires it before target and state resolution.
pub(super) fn record_parent(repo: &RepoContext, child: &str, parent: &str) -> Result<String> {
    if child == parent {
        bail!("workspace branch '{child}' cannot be its own parent");
    }
    if !repo.git.local_branch_exists(parent)? {
        bail!("parent branch '{parent}' does not exist locally");
    }
    let store = WorkspaceStateStore::new(&repo.paths.git_common_dir);
    let mut state = store.load()?;
    validate_no_cycle(&state, child, parent)?;
    let anchor = repo
        .git
        .merge_base(child, parent)?
        .ok_or_else(|| anyhow::anyhow!("branches '{child}' and '{parent}' have no merge base"))?;
    state.set_parent(WorkspaceParentLink::new(
        child.to_owned(),
        parent.to_owned(),
        anchor.clone(),
    ));
    store.save(&state)?;

    Ok(anchor)
}

/// Fail if assigning `parent` to `child` would introduce a workspace parent cycle.
pub(super) fn validate_no_cycle(state: &WorkspaceState, child: &str, parent: &str) -> Result<()> {
    if state.would_create_cycle(child, parent) {
        bail!("setting parent of '{child}' to '{parent}' would create a cycle");
    }
    Ok(())
}

// `kmux parent <parent> <child>` targets an explicit child; `kmux parent <parent>`
// discovers the child from the current kmux workspace.
fn resolve_target(repo: &RepoContext, args: cli::ParentArgs) -> Result<(WorkspaceRecord, String)> {
    let cli::ParentArgs { parent, child } = args;
    let resolved = match child {
        Some(child) => resolve_workspace(repo, &child)?,
        None => resolve_current_kmux_workspace(repo, "parent")?,
    };

    Ok((resolved, parent))
}

// Keep command output readable while retaining enough of the recorded anchor to identify it.
fn short_anchor(anchor: &str) -> String {
    anchor.chars().take(12).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::workspace::WorkspaceParentLink;

    #[test]
    fn validate_no_cycle_rejects_cycle() {
        let mut state = WorkspaceState::default();
        state.set_parent(WorkspaceParentLink::new(
            "feature/grandchild".to_owned(),
            "feature/child".to_owned(),
            "abc".to_owned(),
        ));

        let error = validate_no_cycle(&state, "feature/child", "feature/grandchild")
            .expect_err("cycle should fail");

        assert!(error.to_string().contains("would create a cycle"));
    }
}
