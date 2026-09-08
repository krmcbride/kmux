use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cli;
use crate::state::workspace::WorkspaceStateStore;

use super::context::load_repo_context;
use super::project_session;
use super::resolve::{resolve_current_kmux_workspace, resolve_workspace};
use crate::paths::same_path;
use crate::workspace::WorkspaceRecord;

/// Remove a kmux workspace, its worktree, local branch, tmux window, and owned parent link.
pub(super) fn run(args: cli::RemoveArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux_resolution = project_session::resolve(&repo.paths)?;
    let resolved = resolve_remove_target(&repo, args.name.as_deref())?;

    if same_path(resolved.path(), &repo.paths.main_worktree) {
        bail!(
            "cannot remove the main worktree at {}",
            resolved.path().display()
        );
    }
    if resolved.policy().authority() != crate::workspace::Authority::Kmux {
        bail!(
            "kmux has no lifecycle authority for this external worktree; use 'kmux workspace close' to close its presentation"
        );
    }
    if !resolved.is_live() {
        bail!("workspace registration is stale; refusing to remove a replacement path");
    }
    let branch_to_delete = match resolved.branch() {
        Some(branch)
            if resolved.policy().retention() == Some(crate::workspace::Retention::Persistent) =>
        {
            if resolved.policy().owned_branch() != Some(branch) {
                bail!(
                    "workspace branch changed; kmux does not own branch '{}'",
                    branch
                );
            }
            if !args.force && !repo.git.branch_is_safely_deletable(branch)? {
                bail!(
                    "branch '{}' is not safely merged; use --force to delete the workspace anyway",
                    branch
                );
            }
            Some(branch)
        }
        Some(_) => None,
        None => {
            let head = resolved.git().and_then(|entry| entry.head.as_deref());
            if head.is_none() || head != resolved.policy().creation_anchor() {
                bail!(
                    "detached workspace has committed work beyond its creation anchor; recovery is required before removal"
                );
            }
            None
        }
    };
    let state_store = WorkspaceStateStore::new(&repo.paths.git_common_dir);
    let (mut state, _) = super::resolve::load_workspace_state(&repo)?;
    // Removing a parent branch is metadata-only for descendants: warn about
    // dangling child links instead of silently reparenting or deleting them.
    let remaining_children = branch_to_delete
        .map(|branch| state.children_of(branch))
        .unwrap_or_default();

    leave_worktree_before_removal(&repo.paths.main_worktree)?;
    // Refresh live tmux evidence at the last responsible moment. The held
    // project lifecycle lock prevents another kmux lifecycle command from
    // changing project windows between this check and Git removal.
    let window_id = tmux_resolution.prepare_workspace_removal(&resolved, &repo.config)?;
    repo.git.remove_worktree(resolved.path(), args.force)?;
    if let Some(branch) = branch_to_delete {
        repo.git.delete_local_branch(branch, true)?;
    }
    if let Some(directory) = resolved.policy().allocation_directory() {
        // Remove only the empty directory this create reserved, never a shared root.
        if let Err(error) = std::fs::remove_dir(directory) {
            eprintln!(
                "workspace removed; allocation directory {} remains: {error}",
                directory.display()
            );
        }
    }
    if let Some(policy) = state.policy_for_path(resolved.path()) {
        let id = policy.id().to_owned();
        state.remove_policy(&id);
    }
    if let Some(branch) = branch_to_delete {
        state.remove_parent(branch);
    }
    state_store.save(&state)?;
    if !remaining_children.is_empty() {
        eprintln!(
            "warning: parent links still reference removed branch '{}': {}",
            branch_to_delete.unwrap_or(""),
            remaining_children.join(", ")
        );
    }

    if let Some(window_id) = window_id {
        tmux_resolution.kill_prepared_window(&window_id)?;
    }

    println!("removed {}", resolved.workspace_slug());
    Ok(())
}

// Leave the worktree before deleting it so the process cwd remains valid and
// later Git and tmux subprocesses inherit the existing main-worktree directory.
fn leave_worktree_before_removal(main_worktree: &Path) -> Result<()> {
    std::env::set_current_dir(main_worktree).with_context(|| {
        format!(
            "failed to change directory to {} before removing worktree",
            main_worktree.display()
        )
    })
}

// Support both explicit removal by name and short-form removal from inside a kmux worktree.
fn resolve_remove_target(
    repo: &super::context::RepoContext,
    name: Option<&str>,
) -> Result<WorkspaceRecord> {
    if let Some(name) = name {
        return resolve_workspace(repo, name);
    }

    resolve_current_kmux_workspace(repo, "workspace remove")
}
