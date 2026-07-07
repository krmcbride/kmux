use std::collections::{BTreeMap, BTreeSet, HashSet};

use anyhow::{Result, anyhow, bail};

use crate::config::Config;
use crate::git::WorktreeInfo;
use crate::state::workspace::{WorkspaceState, WorkspaceStateStore};
use crate::workspace::{
    WorkspaceInventoryItem, WorkspaceRecord, is_kmux_worktree, strict_kmux_workspace_records,
    validated_kmux_record,
};

use super::context::RepoContext;
use crate::paths::same_path;

pub(super) type ResolvedWorkspace = WorkspaceRecord;
pub(super) type WorkspaceListItem = WorkspaceInventoryItem;

/// Resolve a user-supplied workspace name, slug, or window-prefixed slug.
pub(super) fn resolve_workspace(repo: &RepoContext, name: &str) -> Result<ResolvedWorkspace> {
    for candidate in name_candidates(&repo.config, name) {
        if let Some(worktree) = find_kmux_workspace_by_name(repo, &candidate)? {
            return resolved_from_kmux_worktree(repo, worktree);
        }
    }

    bail!("workspace '{}' not found", name)
}

/// Resolve the current worktree as a strict kmux workspace for short-form commands.
pub(super) fn resolve_current_kmux_workspace(
    repo: &RepoContext,
    command_name: &str,
) -> Result<ResolvedWorkspace> {
    if same_path(&repo.paths.current_worktree, &repo.paths.main_worktree) {
        bail!("{command_name} requires a workspace name when run from the main worktree");
    }

    let is_current_kmux_worktree = repo
        .paths
        .current_worktree
        .parent()
        .is_some_and(|parent| same_path(parent, &repo.paths.worktree_base_dir));
    if !is_current_kmux_worktree {
        bail!("current worktree is not kmux-managed; pass a workspace name explicitly");
    }

    let current = repo
        .git
        .worktrees()?
        .into_iter()
        .find(|worktree| same_path(&worktree.path, &repo.paths.current_worktree))
        .ok_or_else(|| {
            anyhow!(
                "current worktree {} is not registered with git",
                repo.paths.current_worktree.display()
            )
        })?;

    resolved_from_kmux_worktree(repo, current)
}

/// Resolve a Git worktree and require its kmux path slug to match its branch name.
pub(super) fn resolved_from_kmux_worktree(
    repo: &RepoContext,
    worktree: WorktreeInfo,
) -> Result<ResolvedWorkspace> {
    validated_kmux_record(&repo.paths, worktree, false)
}

/// Build the full workspace inventory, enriched with parent metadata and tree depth.
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
        strict_kmux_workspace_records(&repo.paths, worktrees.drain(..))?
            .into_iter()
            .map(list_item_from_record)
            .collect::<Result<Vec<_>>>()?,
    );

    let state = WorkspaceStateStore::new(&repo.paths.git_common_dir).load()?;
    apply_parent_state(&mut items, &state);
    Ok(parent_tree_order(items))
}

/// Return strict kmux workspaces suitable for tmux restore.
///
/// Strict workspaces live under the kmux worktree base and use the branch-derived slug.
pub(super) fn strict_kmux_workspaces(repo: &RepoContext) -> Result<Vec<ResolvedWorkspace>> {
    let mut workspaces = Vec::new();
    for worktree in repo.git.worktrees()? {
        if !is_kmux_worktree(&repo.paths, &worktree.path) {
            continue;
        }

        let resolved = resolved_from_kmux_worktree(repo, worktree)?;
        if resolved.branch().is_none() {
            bail!(
                "workspace '{}' has no known git branch and cannot be restored by kmux",
                resolved.workspace_slug()
            );
        }
        workspaces.push(resolved);
    }

    workspaces.sort_by(|left, right| left.workspace_slug().cmp(right.workspace_slug()));
    Ok(workspaces)
}

/// Find a kmux worktree by exact branch name or workspace slug/name.
pub(super) fn find_kmux_workspace_by_name(
    repo: &RepoContext,
    name: &str,
) -> Result<Option<WorktreeInfo>> {
    Ok(repo
        .git
        .worktrees()?
        .into_iter()
        .filter(|worktree| is_kmux_worktree(&repo.paths, &worktree.path))
        .find(|worktree| {
            worktree.branch.as_deref() == Some(name)
                || worktree
                    .path
                    .file_name()
                    .is_some_and(|file_name| file_name == name)
        }))
}

/// Find a kmux worktree by derived workspace slug or expected workspace path.
pub(super) fn find_kmux_workspace_by_slug(
    repo: &RepoContext,
    workspace_slug: &str,
) -> Result<Option<WorktreeInfo>> {
    Ok(repo
        .git
        .worktrees()?
        .into_iter()
        .filter(|worktree| is_kmux_worktree(&repo.paths, &worktree.path))
        .find(|worktree| {
            worktree.path == repo.paths.workspace_path(workspace_slug)
                || worktree
                    .path
                    .file_name()
                    .is_some_and(|file_name| file_name == workspace_slug)
        }))
}

// Filesystem creation time is best-effort list metadata; unsupported platforms
// fall back to modified time, then omit the field.
fn list_item_from_worktree(worktree: WorktreeInfo, is_main: bool) -> Result<WorkspaceListItem> {
    let record = WorkspaceRecord::from_worktree(worktree, is_main)?;
    list_item_from_record(record)
}

fn list_item_from_record(record: WorkspaceRecord) -> Result<WorkspaceListItem> {
    let created_at = std::fs::metadata(record.path())
        .ok()
        .and_then(|metadata| metadata.created().or_else(|_| metadata.modified()).ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());

    Ok(WorkspaceInventoryItem::from_record(record, created_at))
}

// Parent metadata is allowed to reference branches/worktrees that are not
// currently present; missing parents are rendered as roots with a parent label.
fn apply_parent_state(items: &mut [WorkspaceListItem], state: &WorkspaceState) {
    for item in items {
        let Some(branch) = item.git_branch() else {
            continue;
        };
        let Some(link) = state.parent_for(branch) else {
            continue;
        };
        item.set_parent_state(link.parent.clone(), link.anchor.clone());
    }
}

// Order inventory as a forest. Links to absent parents become roots, and the
// visited fallback handles hand-edited cyclic state defensively.
fn parent_tree_order(mut items: Vec<WorkspaceListItem>) -> Vec<WorkspaceListItem> {
    let branch_set = items
        .iter()
        .filter_map(|item| item.git_branch().map(ToOwned::to_owned))
        .collect::<BTreeSet<_>>();
    let mut children = BTreeMap::<String, Vec<usize>>::new();
    for (index, item) in items.iter().enumerate() {
        if let Some(parent) = item.git_parent_branch()
            && branch_set.contains(parent)
        {
            children.entry(parent.to_owned()).or_default().push(index);
        }
    }

    for child_indexes in children.values_mut() {
        child_indexes.sort_by(|left, right| {
            item_order_key(&items[*left]).cmp(&item_order_key(&items[*right]))
        });
    }

    let mut roots = items
        .iter()
        .enumerate()
        .filter_map(|(index, item)| {
            let parent_is_present = item
                .git_parent_branch()
                .is_some_and(|parent| branch_set.contains(parent));
            (!parent_is_present).then_some(index)
        })
        .collect::<Vec<_>>();
    roots.sort_by(|left, right| item_order_key(&items[*left]).cmp(&item_order_key(&items[*right])));

    let mut ordered = Vec::new();
    let mut visited = HashSet::new();
    for root in roots {
        visit_parent_tree(root, 0, &children, &mut items, &mut visited, &mut ordered);
    }

    let mut remaining = (0..items.len())
        .filter(|index| !visited.contains(index))
        .collect::<Vec<_>>();
    remaining
        .sort_by(|left, right| item_order_key(&items[*left]).cmp(&item_order_key(&items[*right])));
    for index in remaining {
        visit_parent_tree(index, 0, &children, &mut items, &mut visited, &mut ordered);
    }

    ordered
        .into_iter()
        .map(|index| items[index].clone())
        .collect()
}

fn visit_parent_tree(
    index: usize,
    depth: usize,
    children: &BTreeMap<String, Vec<usize>>,
    items: &mut [WorkspaceListItem],
    visited: &mut HashSet<usize>,
    ordered: &mut Vec<usize>,
) {
    if !visited.insert(index) {
        return;
    }
    items[index].set_tree_depth(depth);
    ordered.push(index);

    let Some(branch) = items[index].git_branch().map(ToOwned::to_owned) else {
        return;
    };
    let Some(child_indexes) = children.get(&branch) else {
        return;
    };
    for child in child_indexes {
        visit_parent_tree(*child, depth + 1, children, items, visited, ordered);
    }
}

fn item_order_key(item: &WorkspaceListItem) -> (bool, String) {
    (!item.is_main(), item.workspace_slug().to_owned())
}

// Accept a raw slug or a tmux window name with the configured prefix stripped.
fn name_candidates(config: &Config, name: &str) -> Vec<String> {
    let mut candidates = vec![name.to_owned()];
    if let Some(stripped) = name.strip_prefix(config.window_prefix())
        && !stripped.is_empty()
    {
        candidates.push(stripped.to_owned());
    }
    candidates
}
