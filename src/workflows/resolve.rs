use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use crate::config::Config;
use crate::git::WorktreeInfo;
use crate::slug::workspace_slug_from_branch;

use super::context::RepoContext;

#[derive(Debug)]
pub(super) struct ResolvedWorkspace {
    pub(super) workspace_slug: String,
    pub(super) path: PathBuf,
    pub(super) branch: Option<String>,
}

#[derive(Serialize)]
pub(super) struct WorkspaceListItem {
    pub(super) workspace_slug: String,
    pub(super) git_branch: Option<String>,
    pub(super) git_worktree_path: String,
    pub(super) is_main: bool,
    pub(super) created_at: Option<u64>,
}

pub(super) fn resolve_workspace(repo: &RepoContext, name: &str) -> Result<ResolvedWorkspace> {
    for candidate in name_candidates(&repo.config, name) {
        if let Some(worktree) = find_kmux_workspace_by_name(repo, &candidate)? {
            return resolved_from_kmux_worktree(repo, worktree);
        }
    }

    bail!("workspace '{}' not found", name)
}

pub(super) fn resolved_from_worktree(worktree: WorktreeInfo) -> Result<ResolvedWorkspace> {
    let workspace_slug = worktree
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "could not determine workspace slug from {}",
                worktree.path.display()
            )
        })?;

    Ok(ResolvedWorkspace {
        workspace_slug,
        path: worktree.path,
        branch: worktree.branch,
    })
}

pub(super) fn resolved_from_kmux_worktree(
    repo: &RepoContext,
    worktree: WorktreeInfo,
) -> Result<ResolvedWorkspace> {
    let resolved = resolved_from_worktree(worktree)?;
    validate_branch_derived_workspace_slug(repo, &resolved)?;
    Ok(resolved)
}

pub(super) fn list_items(repo: &RepoContext) -> Result<Vec<WorkspaceListItem>> {
    let mut worktrees = repo.git.worktrees()?;
    let mut items = Vec::new();

    if let Some(main) = worktrees
        .iter()
        .find(|worktree| worktree.path == repo.paths.main_worktree)
    {
        items.push(list_item_from_worktree(main.clone(), true)?);
    }

    items.extend(
        worktrees
            .drain(..)
            .filter(|worktree| is_kmux_worktree(repo, &worktree.path))
            .filter(|worktree| is_strict_kmux_workspace(repo, worktree))
            .map(|worktree| list_item_from_worktree(worktree, false))
            .collect::<Result<Vec<_>>>()?,
    );

    items.sort_by(|left, right| match (left.is_main, right.is_main) {
        (true, false) => std::cmp::Ordering::Less,
        (false, true) => std::cmp::Ordering::Greater,
        _ => left.workspace_slug.cmp(&right.workspace_slug),
    });
    Ok(items)
}

pub(super) fn strict_kmux_workspaces(repo: &RepoContext) -> Result<Vec<ResolvedWorkspace>> {
    let mut workspaces = Vec::new();
    for worktree in repo.git.worktrees()? {
        if !is_kmux_worktree(repo, &worktree.path) {
            continue;
        }

        let resolved = resolved_from_kmux_worktree(repo, worktree)?;
        if resolved.branch.is_none() {
            bail!(
                "workspace '{}' has no known git branch and cannot be restored by kmux",
                resolved.workspace_slug
            );
        }
        workspaces.push(resolved);
    }

    workspaces.sort_by(|left, right| left.workspace_slug.cmp(&right.workspace_slug));
    Ok(workspaces)
}

pub(super) fn find_kmux_workspace_by_name(
    repo: &RepoContext,
    name: &str,
) -> Result<Option<WorktreeInfo>> {
    Ok(repo
        .git
        .worktrees()?
        .into_iter()
        .filter(|worktree| is_kmux_worktree(repo, &worktree.path))
        .find(|worktree| {
            worktree.branch.as_deref() == Some(name)
                || worktree
                    .path
                    .file_name()
                    .is_some_and(|file_name| file_name == name)
        }))
}

pub(super) fn find_kmux_workspace_by_slug(
    repo: &RepoContext,
    workspace_slug: &str,
) -> Result<Option<WorktreeInfo>> {
    Ok(repo
        .git
        .worktrees()?
        .into_iter()
        .filter(|worktree| is_kmux_worktree(repo, &worktree.path))
        .find(|worktree| {
            worktree.path == repo.paths.workspace_path(workspace_slug)
                || worktree
                    .path
                    .file_name()
                    .is_some_and(|file_name| file_name == workspace_slug)
        }))
}

fn list_item_from_worktree(worktree: WorktreeInfo, is_main: bool) -> Result<WorkspaceListItem> {
    let resolved = resolved_from_worktree(worktree)?;
    let created_at = std::fs::metadata(&resolved.path)
        .ok()
        .and_then(|metadata| metadata.created().or_else(|_| metadata.modified()).ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());

    Ok(WorkspaceListItem {
        workspace_slug: resolved.workspace_slug,
        git_branch: resolved.branch,
        git_worktree_path: resolved.path.display().to_string(),
        is_main,
        created_at,
    })
}

fn name_candidates(config: &Config, name: &str) -> Vec<String> {
    let mut candidates = vec![name.to_owned()];
    if let Some(stripped) = name.strip_prefix(config.window_prefix())
        && !stripped.is_empty()
    {
        candidates.push(stripped.to_owned());
    }
    candidates
}

fn is_kmux_worktree(repo: &RepoContext, path: &std::path::Path) -> bool {
    path.parent() == Some(repo.paths.worktree_base_dir.as_path())
}

fn is_strict_kmux_workspace(repo: &RepoContext, worktree: &WorktreeInfo) -> bool {
    if !is_kmux_worktree(repo, &worktree.path) {
        return false;
    }
    let Some(branch) = worktree.branch.as_deref() else {
        return false;
    };
    let Ok(expected_slug) = workspace_slug_from_branch(branch) else {
        return false;
    };
    worktree
        .path
        .file_name()
        .is_some_and(|file_name| file_name == expected_slug.as_str())
}

fn validate_branch_derived_workspace_slug(
    repo: &RepoContext,
    resolved: &ResolvedWorkspace,
) -> Result<()> {
    if !is_kmux_worktree(repo, &resolved.path) {
        return Ok(());
    }
    let Some(branch) = resolved.branch.as_deref() else {
        return Ok(());
    };
    let expected_slug = workspace_slug_from_branch(branch)?;
    if resolved.workspace_slug != expected_slug {
        bail!(
            "branch '{}' is checked out at non-derived kmux workspace path '{}'; expected '{}'",
            branch,
            resolved.workspace_slug,
            expected_slug
        );
    }
    Ok(())
}
