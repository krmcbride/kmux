use std::collections::{BTreeSet, HashMap, HashSet};

use anyhow::{Result, bail};

use crate::config::Config;
use crate::git::WorktreeInfo;
use crate::state::workspace::{WorkspaceState, WorkspaceStateStore};
use crate::workspace::{
    WorkspaceInventoryItem, WorkspaceRecord, is_kmux_worktree, is_strict_kmux_workspace,
    validated_kmux_record,
};

use super::context::RepoContext;
use crate::paths::same_path;

/// Resolve a user-supplied workspace name, slug, or window-prefixed slug.
pub(super) fn resolve_workspace(repo: &RepoContext, name: &str) -> Result<WorkspaceRecord> {
    let (state, entries) = load_workspace_state(repo)?;
    let candidates = name_candidates(&repo.config, name);
    let canonical = std::path::Path::new(name).canonicalize().ok();
    // Exact IDs and canonical paths remain usable even when a label or branch
    // happens to spell another workspace's identity.
    if let Some(policy) = state
        .policies()
        .iter()
        .find(|policy| policy.id() == name)
        .or_else(|| {
            canonical
                .as_deref()
                .and_then(|path| state.policy_for_path(path))
        })
    {
        return WorkspaceRecord::from_policy(
            policy.clone(),
            entries
                .iter()
                .find(|entry| policy.matches_registration(entry))
                .cloned(),
        );
    }
    let matches = state
        .policies()
        .iter()
        .filter(|policy| {
            let branch = entries
                .iter()
                .find(|entry| policy.matches_registration(entry))
                .and_then(|entry| entry.branch.as_deref());
            !policy.retired()
                && candidates.iter().any(|candidate| {
                    candidate == policy.id()
                        || candidate == policy.label()
                        || candidate == policy.window_slug()
                        || branch == Some(candidate.as_str())
                })
        })
        .collect::<Vec<_>>();
    match matches.as_slice() {
        [policy] => WorkspaceRecord::from_policy(
            (*policy).clone(),
            entries
                .iter()
                .find(|entry| policy.matches_registration(entry))
                .cloned(),
        ),
        [] => bail!("workspace '{}' not found", name),
        _ => bail!(
            "workspace selector '{}' is ambiguous; use its full workspace ID or canonical path",
            name
        ),
    }
}

/// Resolve the current registered checkout; the caller enforces lifecycle authority.
pub(super) fn resolve_current_kmux_workspace(
    repo: &RepoContext,
    command_name: &str,
) -> Result<WorkspaceRecord> {
    if same_path(&repo.paths.current_worktree, &repo.paths.main_worktree) {
        bail!("{command_name} requires a workspace name when run from the main worktree");
    }
    resolve_workspace(repo, &repo.paths.current_worktree.to_string_lossy())
}

/// Resolve a Git worktree and require its kmux path slug to match its branch name.
pub(super) fn resolved_from_kmux_worktree(
    repo: &RepoContext,
    worktree: WorktreeInfo,
) -> Result<WorkspaceRecord> {
    validated_kmux_record(&repo.paths, worktree, false)
}

/// Build the full workspace inventory, enriched with parent metadata and tree depth.
pub(super) fn list_items(repo: &RepoContext) -> Result<Vec<WorkspaceInventoryItem>> {
    let _lock = super::project_session::lock_project_lifecycle(&repo.paths)?;
    let (state, worktrees) = load_workspace_state(repo)?;
    let mut items = state
        .policies()
        .iter()
        .map(|policy| {
            let git = worktrees
                .iter()
                .find(|entry| policy.matches_registration(entry))
                .cloned();
            list_item_from_record(WorkspaceRecord::from_policy(policy.clone(), git)?)
        })
        .collect::<Result<Vec<_>>>()?;
    apply_parent_state(&mut items, &state);
    Ok(parent_tree_order(items))
}

/// Reconcile persisted policy from current Git inventory under the caller's lifecycle lock.
pub(super) fn load_workspace_state(
    repo: &RepoContext,
) -> Result<(WorkspaceState, Vec<WorktreeInfo>)> {
    let store = WorkspaceStateStore::new(&repo.paths.git_common_dir);
    let mut state = store.load()?;
    let mut worktrees = repo.git.worktrees()?;
    if state.version == 1 {
        for entry in &mut worktrees {
            if is_strict_kmux_workspace(&repo.paths, entry)
                && entry.path.is_dir()
                && entry.prunable.is_none()
                && !entry.bare
            {
                entry.kmux_binding = Some(repo.git.claim_worktree(&entry.path)?);
            }
        }
    }
    if state.reconcile(&repo.paths, &worktrees)? {
        store.save(&state)?;
    }
    Ok((state, worktrees))
}

/// Return live external worktrees and remembered owned/primary presentations.
pub(super) fn restorable_workspaces(repo: &RepoContext) -> Result<Vec<WorkspaceRecord>> {
    let (state, entries) = load_workspace_state(repo)?;
    let mut records = state
        .policies()
        .iter()
        .filter(|policy| {
            policy.authority() == crate::workspace::Authority::External || policy.presentation()
        })
        .map(|policy| {
            WorkspaceRecord::from_policy(
                policy.clone(),
                entries
                    .iter()
                    .find(|entry| policy.matches_registration(entry))
                    .cloned(),
            )
        })
        .collect::<Result<Vec<_>>>()?;
    records.retain(WorkspaceRecord::is_live);
    records.sort_by(|left, right| left.workspace_slug().cmp(right.workspace_slug()));
    Ok(records)
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
fn list_item_from_record(record: WorkspaceRecord) -> Result<WorkspaceInventoryItem> {
    let created_at = std::fs::metadata(record.path())
        .ok()
        .and_then(|metadata| metadata.created().or_else(|_| metadata.modified()).ok())
        .and_then(|time| time.duration_since(std::time::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs());

    Ok(WorkspaceInventoryItem::from_record(record, created_at))
}

// Parent metadata is allowed to reference branches/worktrees that are not
// currently present; missing parents are rendered as roots with a parent label.
fn apply_parent_state(items: &mut [WorkspaceInventoryItem], state: &WorkspaceState) {
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
fn parent_tree_order(mut items: Vec<WorkspaceInventoryItem>) -> Vec<WorkspaceInventoryItem> {
    let branch_set = items
        .iter()
        .filter_map(|item| item.git_branch().map(ToOwned::to_owned))
        .collect::<BTreeSet<_>>();
    let mut children = HashMap::<String, Vec<usize>>::new();
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
    children: &HashMap<String, Vec<usize>>,
    items: &mut [WorkspaceInventoryItem],
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

fn item_order_key(item: &WorkspaceInventoryItem) -> (bool, String) {
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

#[cfg(test)]
mod tests {
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn parent_tree_order_is_depth_first_with_stable_siblings() -> Result<()> {
        let main = inventory_item("project-alpha", "main", true)?;
        let mut child = inventory_item("feature-child", "feature/child", false)?;
        child.set_parent_state("main".to_owned(), "anchor-child".to_owned());
        let mut sibling = inventory_item("feature-sibling", "feature/sibling", false)?;
        sibling.set_parent_state("main".to_owned(), "anchor-sibling".to_owned());
        let mut grandchild = inventory_item("feature-grandchild", "feature/grandchild", false)?;
        grandchild.set_parent_state("feature/child".to_owned(), "anchor-grandchild".to_owned());
        let mut missing_parent = inventory_item("feature-orphan", "feature/orphan", false)?;
        missing_parent.set_parent_state("missing-parent".to_owned(), "anchor-orphan".to_owned());

        let ordered = parent_tree_order(vec![sibling, missing_parent, grandchild, child, main]);

        assert_eq!(
            ordered
                .iter()
                .map(|item| (item.git_branch(), item.tree_depth()))
                .collect::<Vec<_>>(),
            [
                (Some("main"), 0),
                (Some("feature/child"), 1),
                (Some("feature/grandchild"), 2),
                (Some("feature/sibling"), 1),
                (Some("feature/orphan"), 0),
            ]
        );
        Ok(())
    }

    fn inventory_item(
        workspace_slug: &str,
        branch: &str,
        is_main: bool,
    ) -> Result<WorkspaceInventoryItem> {
        let record = WorkspaceRecord::from_worktree(
            WorktreeInfo {
                path: PathBuf::from("/repo").join(workspace_slug),
                head: None,
                branch: Some(branch.to_owned()),
                detached: false,
                bare: false,
                locked: None,
                prunable: None,
                kmux_binding: None,
            },
            is_main,
        )?;
        Ok(WorkspaceInventoryItem::from_record(record, None))
    }
}
