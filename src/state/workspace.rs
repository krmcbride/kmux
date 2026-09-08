//! Git-common-dir-backed workspace graph persistence.
//!
//! This module stores workspace policy, source relationships, and commit anchors with
//! the repo's shared Git metadata so all worktrees for a clone see the same
//! graph. It is intentionally separate from `state::agent`, which stores
//! external agent observations in XDG state.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::git::WorktreeInfo;
use crate::paths::RepoPaths;
use crate::workspace::{
    Authority, LineageParent, Retention, WorkspaceLineage, WorkspacePolicy,
    is_strict_kmux_workspace,
};

const CURRENT_VERSION: u32 = 3;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// Repo-local kmux workspace graph metadata persisted under Git's common dir.
pub struct WorkspaceState {
    pub version: u32,
    // Unmatched legacy child branches remain as ref-based historical metadata.
    pub parents: Vec<WorkspaceParentLink>,
    workspaces: Vec<WorkspacePolicy>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Parent relationship for one branch, anchored at the branch merge base.
pub struct WorkspaceParentLink {
    pub branch: String,
    pub parent: String,
    pub anchor: String,
}

#[derive(Debug, Clone)]
/// Store for workspace graph metadata scoped to one Git repository.
pub struct WorkspaceStateStore {
    path: PathBuf,
}

/// Process-owned exclusive lock for one repository's workspace lifecycle.
pub struct WorkspaceLifecycleLock {
    file: File,
}

impl WorkspaceState {
    /// Add registered paths and migrate strict legacy ownership exactly once.
    ///
    /// Missing records remain available as stale inventory and lineage history.
    /// The caller supplies canonical Git paths and holds the repository lifecycle lock.
    pub fn reconcile(&mut self, paths: &RepoPaths, worktrees: &[WorktreeInfo]) -> Result<bool> {
        let before = self.clone();
        for policy in &mut self.workspaces {
            if policy.authority() == Authority::Kmux
                && !policy.retired()
                && !worktrees
                    .iter()
                    .any(|entry| policy.matches_registration(entry))
            {
                policy.retire();
            }
        }
        let mut discovered = Vec::new();
        for entry in worktrees {
            if self.policy_for_path(&entry.path).is_some() {
                continue;
            }
            let primary = entry.path == paths.main_worktree;
            let policy = if !primary
                && self.version == 1
                && is_strict_kmux_workspace(paths, entry)
                && let Some(id) = &entry.kmux_binding
            {
                let label = entry
                    .path
                    .file_name()
                    .ok_or_else(|| anyhow::anyhow!("missing worktree basename"))?
                    .to_string_lossy()
                    .into_owned();
                WorkspacePolicy::owned(
                    id.clone(),
                    entry.path.clone(),
                    label,
                    Retention::Persistent,
                    // A migrated checkout's current HEAD is not its original creation anchor.
                    None,
                    entry.branch.clone(),
                )?
            } else {
                WorkspacePolicy::observed(entry.path.clone(), primary)
            };
            discovered.push(policy);
        }
        // Legacy names already identify windows. Reserve them before assigning new
        // primary/external display names, regardless of Git's inventory ordering.
        discovered.sort_by_key(|policy| policy.authority() != Authority::Kmux);
        for mut policy in discovered {
            if policy.authority() != Authority::Kmux {
                self.disambiguate_observation(&mut policy)?;
            }
            self.upsert_policy(policy)?;
        }
        if self.version < 3 {
            self.migrate_lineage(worktrees)?;
        }
        self.version = CURRENT_VERSION;
        self.normalize();
        Ok(*self != before)
    }

    /// Return persisted workspace intent, including records no longer registered with Git.
    pub fn policies(&self) -> &[WorkspacePolicy] {
        &self.workspaces
    }

    /// Look up the policy bound to a canonical worktree path.
    pub fn policy_for_path(&self, path: &Path) -> Option<&WorkspacePolicy> {
        self.workspaces
            .iter()
            .find(|policy| !policy.retired() && policy.path() == path)
    }

    /// Replace one explicit policy after validating identity and presentation collisions.
    pub fn upsert_policy(&mut self, policy: WorkspacePolicy) -> Result<()> {
        policy.validate()?;
        if let Some(parent_id) = policy.lineage().and_then(|link| link.parent.workspace_id())
            && self.would_create_cycle(policy.id(), parent_id)
        {
            bail!(
                "setting parent of '{}' would create a cycle",
                policy.label()
            );
        }
        for existing in &self.workspaces {
            if existing.id() != policy.id()
                && !existing.retired()
                && !policy.retired()
                && (existing.path() == policy.path()
                    || existing.window_slug() == policy.window_slug()
                    || existing.presentation_slug() == policy.presentation_slug())
            {
                bail!(
                    "workspace identity or window name conflicts with '{}'",
                    existing.id()
                );
            }
        }
        self.workspaces
            .retain(|existing| existing.id() != policy.id());
        self.workspaces.push(policy);
        self.normalize();
        Ok(())
    }

    /// Retain an explicitly removed workspace's identity and ancestry as history.
    pub fn remove_policy(&mut self, id: &str) {
        if let Some(policy) = self.workspaces.iter_mut().find(|policy| policy.id() == id) {
            policy.retire();
        }
    }

    /// Find a stable identity, including a historical retired registration.
    pub fn policy_by_id(&self, id: &str) -> Option<&WorkspacePolicy> {
        self.workspaces.iter().find(|policy| policy.id() == id)
    }

    /// Assign source metadata without changing worktree or branch authority.
    pub fn set_lineage(&mut self, child_id: &str, lineage: WorkspaceLineage) -> Result<()> {
        let mut policy = self
            .policy_by_id(child_id)
            .ok_or_else(|| anyhow::anyhow!("workspace '{child_id}' not found"))?
            .clone();
        policy.set_lineage(lineage);
        self.upsert_policy(policy)
    }

    /// Return readable child labels that still reference a workspace identity.
    pub fn children_of(&self, parent_id: &str) -> Vec<String> {
        let mut children = self
            .workspaces
            .iter()
            .filter(|policy| {
                policy.lineage().and_then(|link| link.parent.workspace_id()) == Some(parent_id)
            })
            .map(|policy| policy.label().to_owned())
            .collect::<Vec<_>>();
        children.sort();
        children
    }

    /// Check a proposed replacement edge against stable workspace identities.
    pub fn would_create_cycle(&self, child_id: &str, parent_id: &str) -> bool {
        let mut visited = BTreeSet::new();
        let mut cursor = parent_id;
        loop {
            if cursor == child_id || !visited.insert(cursor) {
                return true;
            }
            let Some(next) = self
                .policy_by_id(cursor)
                .and_then(WorkspacePolicy::lineage)
                .and_then(|lineage| lineage.parent.workspace_id())
            else {
                return false;
            };
            cursor = next;
        }
    }

    // Only newly discovered observations receive a suffix; stable IDs, paths,
    // saved presentation names, and legacy owned labels remain unchanged.
    fn disambiguate_observation(&self, policy: &mut WorkspacePolicy) -> Result<()> {
        let base = policy.window_slug().to_owned();
        let mut suffix = 0;
        while self.workspaces.iter().any(|existing| {
            !existing.retired()
                && (existing.window_slug() == policy.window_slug()
                    || existing.presentation_slug() == policy.presentation_slug())
        }) {
            suffix += 1;
            policy.name_observation(format!("{base}-{suffix}"))?;
        }
        Ok(())
    }

    // Resolve legacy branch relationships once. Unknown child refs stay explicit
    // history; a later branch appearance must never silently rebind ancestry.
    fn migrate_lineage(&mut self, worktrees: &[WorktreeInfo]) -> Result<()> {
        let links = std::mem::take(&mut self.parents);
        for link in links {
            let Some(child_id) = self
                .unique_branch_policy(worktrees, &link.branch)
                .map(|p| p.id().to_owned())
            else {
                self.parents.push(link);
                continue;
            };
            if self
                .policy_by_id(&child_id)
                .and_then(WorkspacePolicy::lineage)
                .is_some()
            {
                continue;
            }
            let parent = self
                .unique_branch_policy(worktrees, &link.parent)
                .map(|policy| LineageParent::workspace(policy, Some(&link.parent)))
                .unwrap_or_else(|| LineageParent::GitRef {
                    reference: link.parent.clone(),
                });
            self.set_lineage(&child_id, WorkspaceLineage::new(parent, link.anchor))?;
        }
        Ok(())
    }

    // Forced duplicate branch checkouts are ambiguous and remain ref-based history.
    fn unique_branch_policy(
        &self,
        worktrees: &[WorktreeInfo],
        branch: &str,
    ) -> Option<&WorkspacePolicy> {
        let mut matches = worktrees
            .iter()
            .filter(|entry| entry.branch.as_deref() == Some(branch))
            .filter_map(|entry| self.policy_for_path(&entry.path));
        let first = matches.next()?;
        matches.next().is_none().then_some(first)
    }

    // Keep state deterministic on disk and collapse duplicate entries from hand edits.
    fn normalize(&mut self) {
        self.workspaces
            .sort_by(|left, right| left.id().cmp(right.id()));
        self.parents
            .sort_by(|left, right| left.branch.cmp(&right.branch));
        self.parents
            .dedup_by(|left, right| left.branch == right.branch);
    }
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            version: 1,
            parents: Vec::new(),
            workspaces: Vec::new(),
        }
    }
}

impl WorkspaceStateStore {
    /// Create a store rooted at `<git-common-dir>/kmux/state.json`.
    pub fn new(git_common_dir: &Path) -> Self {
        Self {
            path: git_common_dir.join("kmux/state.json"),
        }
    }

    /// Serialize workspace lifecycle mutations for this Git common repository.
    ///
    /// The operating system releases this advisory file lock if the process
    /// exits unexpectedly. The stable sibling lock file must not be removed,
    /// because replacing its inode would let concurrent processes bypass it.
    pub fn lock_lifecycle(&self) -> Result<WorkspaceLifecycleLock> {
        let path = self.path.with_file_name("lifecycle.lock");
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create lifecycle lock directory {}",
                    parent.display()
                )
            })?;
        }
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("failed to open lifecycle lock {}", path.display()))?;
        file.lock()
            .with_context(|| format!("failed to lock workspace lifecycle {}", path.display()))?;
        Ok(WorkspaceLifecycleLock { file })
    }

    /// Load workspace state; absent state retains the one-time legacy compatibility import.
    pub fn load(&self) -> Result<WorkspaceState> {
        let content = match fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(WorkspaceState::default());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", self.path.display()));
            }
        };

        let mut state: WorkspaceState = serde_json::from_str(&content)
            .with_context(|| format!("failed to parse {}", self.path.display()))?;
        if !(1..=CURRENT_VERSION).contains(&state.version) {
            bail!(
                "unsupported kmux workspace state version {}; expected {}",
                state.version,
                CURRENT_VERSION
            );
        }
        let mut ids = BTreeSet::new();
        let mut paths = BTreeSet::new();
        let mut names = BTreeSet::new();
        for policy in &state.workspaces {
            policy.validate()?;
            if !ids.insert(policy.id())
                || (!policy.retired()
                    && (!paths.insert(policy.path()) || !names.insert(policy.window_slug())))
            {
                bail!("duplicate workspace identity, path, or window name in state");
            }
        }
        state.normalize();
        Ok(state)
    }

    /// Persist validated workspace state with stable ordering; reconciliation upgrades v1.
    pub fn save(&self, state: &WorkspaceState) -> Result<()> {
        let mut state = state.clone();
        state.normalize();
        let content = serde_json::to_vec_pretty(&state)?;
        write_atomic(&self.path, &content)
    }
}

impl Drop for WorkspaceLifecycleLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

// Write through a sibling temporary file so interrupted saves do not leave a
// partially-written JSON state file behind.
fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create state directory {}", parent.display()))?;
    }

    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos());
    let tmp_path = path.with_extension(format!("json.{}.{nanos}.tmp", std::process::id()));
    fs::write(&tmp_path, content).with_context(|| {
        format!(
            "failed to write temporary state file {}",
            tmp_path.display()
        )
    })?;
    fs::rename(&tmp_path, path)
        .with_context(|| format!("failed to replace state file {}", path.display()))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn link(branch: &str, parent: &str, anchor: &str) -> WorkspaceParentLink {
        WorkspaceParentLink {
            branch: branch.to_owned(),
            parent: parent.to_owned(),
            anchor: anchor.to_owned(),
        }
    }

    #[test]
    fn missing_state_file_loads_as_empty_state() -> Result<()> {
        let temp = TempDir::new()?;
        let store = WorkspaceStateStore::new(temp.path());

        assert_eq!(store.load()?, WorkspaceState::default());
        Ok(())
    }

    #[test]
    fn state_store_round_trips_parent_links() -> Result<()> {
        let temp = TempDir::new()?;
        let store = WorkspaceStateStore::new(temp.path());
        let mut state = WorkspaceState::default();
        state.parents.push(link("feature/auth", "main", "abc123"));

        store.save(&state)?;

        assert_eq!(store.load()?, state);
        Ok(())
    }

    #[test]
    fn state_store_writes_links_in_stable_branch_order() -> Result<()> {
        let temp = TempDir::new()?;
        let store = WorkspaceStateStore::new(temp.path());
        let mut state = WorkspaceState::default();
        state.parents.push(link("feature/z", "main", "z"));
        state.parents.push(link("feature/a", "main", "a"));

        store.save(&state)?;
        let loaded = store.load()?;

        assert_eq!(loaded.parents[0].branch, "feature/a");
        assert_eq!(loaded.parents[1].branch, "feature/z");
        Ok(())
    }

    #[test]
    fn lifecycle_lock_serializes_processes_and_releases_on_drop() -> Result<()> {
        let temp = TempDir::new()?;
        let store = WorkspaceStateStore::new(temp.path());
        let lock = store.lock_lifecycle()?;
        let competing = OpenOptions::new()
            .read(true)
            .write(true)
            .open(temp.path().join("kmux/lifecycle.lock"))?;

        assert!(matches!(
            competing.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));
        drop(lock);
        competing.try_lock()?;
        competing.unlock()?;
        Ok(())
    }

    #[test]
    fn lineage_replacement_and_retirement_preserve_identity_and_descendants() -> Result<()> {
        let mut state = WorkspaceState::default();
        let parent = owned("parent")?;
        let child = owned("child")?;
        state.upsert_policy(parent.clone())?;
        state.upsert_policy(child.clone())?;
        state.set_lineage(
            child.id(),
            WorkspaceLineage::new(
                LineageParent::GitRef {
                    reference: "main".to_owned(),
                },
                "old".to_owned(),
            ),
        )?;
        let lineage =
            WorkspaceLineage::new(LineageParent::workspace(&parent, None), "new".to_owned());
        state.set_lineage(child.id(), lineage.clone())?;
        state.remove_policy(parent.id());
        assert!(
            state
                .policy_by_id(parent.id())
                .expect("historical parent")
                .retired()
        );
        assert_eq!(
            state
                .policy_by_id(child.id())
                .and_then(WorkspacePolicy::lineage),
            Some(&lineage)
        );
        assert_eq!(state.children_of(parent.id()), ["child"]);
        assert_eq!(
            state.policy_by_id(child.id()).expect("child").retention(),
            Some(Retention::Ephemeral)
        );
        Ok(())
    }

    #[test]
    fn malformed_json_reports_error() -> Result<()> {
        let temp = TempDir::new()?;
        let store = WorkspaceStateStore::new(temp.path());
        let path = temp.path().join("kmux/state.json");
        fs::create_dir_all(path.parent().expect("state path should have parent"))?;
        fs::write(&path, "not json")?;

        let error = store.load().expect_err("malformed state should fail");

        assert!(error.to_string().contains("failed to parse"));
        Ok(())
    }

    #[test]
    fn unsupported_version_reports_error() -> Result<()> {
        let temp = TempDir::new()?;
        let store = WorkspaceStateStore::new(temp.path());
        let path = temp.path().join("kmux/state.json");
        fs::create_dir_all(path.parent().expect("state path should have parent"))?;
        fs::write(&path, r#"{"version":999,"parents":[]}"#)?;

        let error = store.load().expect_err("future state version should fail");

        assert!(
            error
                .to_string()
                .contains("unsupported kmux workspace state version")
        );
        Ok(())
    }

    #[test]
    fn cycle_detection_follows_workspace_ids_across_promotion_and_reparenting() -> Result<()> {
        let mut state = WorkspaceState::default();
        let parent = owned("parent")?;
        let child = owned("child")?;
        let leaf = owned("leaf")?;
        for policy in [&parent, &child, &leaf] {
            state.upsert_policy(policy.clone())?;
        }
        state.set_lineage(
            child.id(),
            WorkspaceLineage::new(LineageParent::workspace(&parent, None), "abc".to_owned()),
        )?;
        state.set_lineage(
            leaf.id(),
            WorkspaceLineage::new(LineageParent::workspace(&child, None), "abc".to_owned()),
        )?;
        let mut promoted = state.policy_by_id(child.id()).expect("child").clone();
        promoted.promote(Some("renamed"))?;
        state.upsert_policy(promoted)?;
        assert!(state.would_create_cycle(parent.id(), leaf.id()));
        assert!(state.would_create_cycle(child.id(), child.id()));
        let before = state.clone();
        assert!(
            state
                .set_lineage(
                    parent.id(),
                    WorkspaceLineage::new(LineageParent::workspace(&leaf, None), "abc".to_owned())
                )
                .is_err()
        );
        assert_eq!(state, before);
        state.set_lineage(
            leaf.id(),
            WorkspaceLineage::new(
                LineageParent::GitRef {
                    reference: "main".to_owned(),
                },
                "abc".to_owned(),
            ),
        )?;
        assert!(!state.would_create_cycle(parent.id(), leaf.id()));
        Ok(())
    }

    fn owned(label: &str) -> Result<WorkspacePolicy> {
        WorkspacePolicy::owned(
            format!("ws-{label}"),
            PathBuf::from("/repo").join(label),
            label.to_owned(),
            Retention::Ephemeral,
            Some("abc".to_owned()),
            None,
        )
    }

    #[test]
    fn migration_is_one_time_and_retention_survives_checkout_changes() -> Result<()> {
        let temp = TempDir::new()?;
        let paths = RepoPaths {
            current_worktree: temp.path().join("project-alpha"),
            main_worktree: temp.path().join("project-alpha"),
            git_common_dir: temp.path().join("project-alpha/.git"),
            worktree_base_dir: temp.path().join("project-alpha__worktrees"),
        };
        let mut legacy = registration(paths.workspace_path("feature-alpha"), Some("feature/alpha"));
        legacy.kmux_binding = Some("ws-owned-alpha".to_owned());
        let mut state = WorkspaceState::default();
        state.parents.push(link("feature/alpha", "main", "anchor"));
        assert!(state.reconcile(&paths, &[legacy.clone()])?);
        let original = state
            .policy_for_path(&legacy.path)
            .expect("migrated policy")
            .clone();
        assert_eq!(original.authority(), Authority::Kmux);
        assert_eq!(original.retention(), Some(Retention::Persistent));
        assert_eq!(
            original.lineage().expect("lineage").parent,
            LineageParent::GitRef {
                reference: "main".to_owned()
            }
        );
        assert!(state.parents.is_empty());

        legacy.branch = None;
        legacy.detached = true;
        assert!(!state.reconcile(&paths, &[legacy.clone()])?);
        assert_eq!(state.policy_for_path(&legacy.path), Some(&original));
        let external = registration(paths.workspace_path("feature-later"), Some("feature/later"));
        state.reconcile(&paths, &[legacy, external.clone()])?;
        assert_eq!(
            state
                .policy_for_path(&external.path)
                .expect("external")
                .authority(),
            Authority::External
        );
        Ok(())
    }

    #[test]
    fn migration_reserves_legacy_names_before_naming_external_observations() -> Result<()> {
        let temp = TempDir::new()?;
        let paths = RepoPaths {
            current_worktree: temp.path().join("project-alpha"),
            main_worktree: temp.path().join("project-alpha"),
            git_common_dir: temp.path().join("project-alpha/.git"),
            worktree_base_dir: temp.path().join("project-alpha__worktrees"),
        };
        let external = registration(temp.path().join("external/workspace"), None);
        let observed = WorkspacePolicy::observed(external.path.clone(), false);
        let mut legacy = registration(
            paths.workspace_path(observed.label()),
            Some(observed.label()),
        );
        legacy.kmux_binding = Some("ws-owned-legacy".to_owned());
        let mut entries = vec![
            registration(paths.main_worktree.clone(), Some("main")),
            external.clone(),
            legacy.clone(),
        ];
        let mut state = WorkspaceState::default();
        state.reconcile(&paths, &entries)?;
        let named = state
            .policy_for_path(&external.path)
            .expect("external policy");
        assert_eq!(named.id(), observed.id());
        assert_eq!(named.authority(), Authority::External);
        assert_eq!(named.label(), format!("{}-1", observed.label()));
        let imported = state.policy_for_path(&legacy.path).expect("legacy policy");
        assert_eq!(imported.label(), observed.label());
        assert_eq!(imported.authority(), Authority::Kmux);
        assert!(!state.reconcile(&paths, &entries)?);

        entries.reverse();
        let mut reordered = WorkspaceState::default();
        reordered.reconcile(&paths, &entries)?;
        assert_eq!(reordered, state);
        Ok(())
    }

    #[test]
    fn replacement_registration_retires_authority_and_preserves_history() -> Result<()> {
        let temp = TempDir::new()?;
        let path = temp.path().join("workspace");
        fs::create_dir(&path)?;
        let paths = RepoPaths {
            current_worktree: temp.path().join("project-alpha"),
            main_worktree: temp.path().join("project-alpha"),
            git_common_dir: temp.path().join("project-alpha/.git"),
            worktree_base_dir: temp.path().join("project-alpha__worktrees"),
        };
        let mut state = WorkspaceState {
            version: CURRENT_VERSION,
            ..WorkspaceState::default()
        };
        state.upsert_policy(WorkspacePolicy::owned(
            "ws-original".to_owned(),
            path.clone(),
            "original".to_owned(),
            Retention::Ephemeral,
            Some("anchor".to_owned()),
            None,
        )?)?;
        let replacement = registration(path.clone(), Some("publication/one"));
        state.reconcile(&paths, std::slice::from_ref(&replacement))?;
        assert_eq!(state.policies().len(), 2);
        let original = state
            .policies()
            .iter()
            .find(|p| p.id() == "ws-original")
            .expect("history");
        assert!(original.retired());
        assert!(!original.matches_registration(&replacement));
        assert_eq!(original.retention(), Some(Retention::Ephemeral));
        assert_eq!(
            state
                .policy_for_path(&path)
                .expect("replacement")
                .authority(),
            Authority::External
        );
        assert!(!state.reconcile(&paths, &[replacement])?);
        Ok(())
    }

    fn registration(path: PathBuf, branch: Option<&str>) -> WorktreeInfo {
        WorktreeInfo {
            path,
            head: Some("anchor".to_owned()),
            branch: branch.map(ToOwned::to_owned),
            detached: branch.is_none(),
            bare: false,
            locked: None,
            prunable: None,
            kmux_binding: None,
        }
    }
}
