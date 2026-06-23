use std::path::PathBuf;

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use crate::config::Config;
use crate::git::WorktreeInfo;

use super::context::RepoContext;

#[derive(Debug)]
pub(super) struct ResolvedWorktree {
    pub(super) handle: String,
    pub(super) path: PathBuf,
    pub(super) branch: Option<String>,
}

#[derive(Serialize)]
pub(super) struct ListItem {
    pub(super) handle: String,
    pub(super) branch: Option<String>,
    pub(super) path: String,
}

pub(super) fn resolve_worktree(repo: &RepoContext, name: &str) -> Result<ResolvedWorktree> {
    for candidate in name_candidates(&repo.config, name) {
        if let Some(worktree) = find_kmux_worktree_by_name(repo, &candidate)? {
            return resolved_from_worktree(worktree);
        }
    }

    bail!("worktree '{}' not found", name)
}

pub(super) fn resolved_from_worktree(worktree: WorktreeInfo) -> Result<ResolvedWorktree> {
    let handle = worktree
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "could not determine handle from {}",
                worktree.path.display()
            )
        })?;

    Ok(ResolvedWorktree {
        handle,
        path: worktree.path,
        branch: worktree.branch,
    })
}

pub(super) fn list_items(repo: &RepoContext) -> Result<Vec<ListItem>> {
    let mut items = repo
        .git
        .worktrees()?
        .into_iter()
        .filter(|worktree| is_kmux_worktree(repo, &worktree.path))
        .map(resolved_from_worktree)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .map(|worktree| ListItem {
            handle: worktree.handle,
            branch: worktree.branch,
            path: worktree.path.display().to_string(),
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.handle.cmp(&right.handle));
    Ok(items)
}

pub(super) fn find_kmux_worktree_by_name(
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

pub(super) fn find_kmux_worktree_by_handle(
    repo: &RepoContext,
    handle: &str,
) -> Result<Option<WorktreeInfo>> {
    Ok(repo
        .git
        .worktrees()?
        .into_iter()
        .filter(|worktree| is_kmux_worktree(repo, &worktree.path))
        .find(|worktree| {
            worktree.path == repo.paths.handle_path(handle)
                || worktree
                    .path
                    .file_name()
                    .is_some_and(|file_name| file_name == handle)
        }))
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
