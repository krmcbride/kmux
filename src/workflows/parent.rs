use anyhow::{Result, bail};

use crate::cli;
use crate::state::workspace::{WorkspaceParentLink, WorkspaceState, WorkspaceStateStore};

use super::context::{RepoContext, load_repo_context};
use super::resolve::{resolve_current_kmux_workspace, resolve_workspace};
use crate::workspace::WorkspaceRecord;

/// Set or replace a workspace parent link without changing branches, worktrees, or tmux windows.
pub(super) fn run(args: cli::ParentArgs) -> Result<()> {
    let repo = load_repo_context()?;
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

/// Validate and persist a parent link, returning the merge-base anchor that was recorded.
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

// `kmux parent <child> <parent>` targets an explicit child; `kmux parent <parent>`
// targets the current kmux workspace for the short form.
fn resolve_target(repo: &RepoContext, args: cli::ParentArgs) -> Result<(WorkspaceRecord, String)> {
    if let Some(parent) = args.parent {
        return Ok((resolve_workspace(repo, &args.child_or_parent)?, parent));
    }

    Ok((
        resolve_current_kmux_workspace(repo, "parent")?,
        args.child_or_parent,
    ))
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
