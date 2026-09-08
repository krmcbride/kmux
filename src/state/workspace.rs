//! Git-common-dir-backed workspace graph persistence.
//!
//! This module stores branch parent relationships and merge-base anchors with
//! the repo's shared Git metadata so all worktrees for a clone see the same
//! graph. It is intentionally separate from `state::agent`, which stores
//! external agent observations in XDG state.

use std::collections::{BTreeSet, HashMap};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

use crate::git::WorktreeInfo;
use crate::paths::RepoPaths;
use crate::workspace::{Authority, Retention, WorkspacePolicy, is_strict_kmux_workspace};

const CURRENT_VERSION: u32 = 2;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// Repo-local kmux workspace graph metadata persisted under Git's common dir.
pub struct WorkspaceState {
    pub version: u32,
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
                    entry.head.clone(),
                    entry.branch.clone(),
                )?
            } else {
                WorkspacePolicy::observed(entry.path.clone(), primary)
            };
            self.upsert_policy(policy)?;
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

    /// Forget an explicitly removed owned workspace without affecting other records.
    pub fn remove_policy(&mut self, id: &str) {
        self.workspaces.retain(|policy| policy.id() != id);
    }
    /// Return the parent link recorded for a branch, if kmux knows one.
    pub fn parent_for(&self, branch: &str) -> Option<&WorkspaceParentLink> {
        self.parents.iter().find(|link| link.branch == branch)
    }

    /// Insert or replace a branch parent link and keep persisted ordering stable.
    pub fn set_parent(&mut self, link: WorkspaceParentLink) {
        self.parents
            .retain(|existing| existing.branch != link.branch);
        self.parents.push(link);
        self.normalize();
    }

    /// Remove the parent link owned by `branch`, leaving any child links untouched.
    pub fn remove_parent(&mut self, branch: &str) -> bool {
        let before = self.parents.len();
        self.parents.retain(|link| link.branch != branch);
        before != self.parents.len()
    }

    /// Return branches that currently name `parent` as their parent branch.
    pub fn children_of(&self, parent: &str) -> Vec<String> {
        self.parents
            .iter()
            .filter(|link| link.parent == parent)
            .map(|link| link.branch.clone())
            .collect()
    }

    /// Check whether assigning `parent` to `branch` would create a parent cycle.
    ///
    /// The proposed edge replaces any existing edge for `branch`, which lets callers
    /// validate both new links and reparenting through the same path.
    pub fn would_create_cycle(&self, branch: &str, parent: &str) -> bool {
        let mut parents = HashMap::new();
        for link in &self.parents {
            if link.branch != branch {
                parents.insert(link.branch.as_str(), link.parent.as_str());
            }
        }

        let mut visited = BTreeSet::new();
        let mut cursor = parent;
        visited.insert(cursor);
        while let Some(next) = parents.get(cursor) {
            let next = *next;
            if next == branch {
                return true;
            }
            if !visited.insert(next) {
                return false;
            }
            cursor = next;
        }
        false
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

impl WorkspaceParentLink {
    /// Build a parent link for `branch` with its parent branch and merge-base anchor.
    pub fn new(branch: String, parent: String, anchor: String) -> Self {
        Self {
            branch,
            parent,
            anchor,
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

    /// Load workspace graph state, returning an empty current-version state when absent.
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
        if state.version != 1 && state.version != CURRENT_VERSION {
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
        WorkspaceParentLink::new(branch.to_owned(), parent.to_owned(), anchor.to_owned())
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
        state.set_parent(link("feature/auth", "main", "abc123"));

        store.save(&state)?;

        assert_eq!(store.load()?, state);
        Ok(())
    }

    #[test]
    fn state_store_writes_links_in_stable_branch_order() -> Result<()> {
        let temp = TempDir::new()?;
        let store = WorkspaceStateStore::new(temp.path());
        let mut state = WorkspaceState::default();
        state.set_parent(link("feature/z", "main", "z"));
        state.set_parent(link("feature/a", "main", "a"));

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
    fn set_parent_replaces_existing_branch_link() {
        let mut state = WorkspaceState::default();
        state.set_parent(link("feature/auth", "main", "old"));
        state.set_parent(link("feature/auth", "feature/base", "new"));

        assert_eq!(
            state.parents,
            vec![link("feature/auth", "feature/base", "new")]
        );
    }

    #[test]
    fn remove_parent_deletes_only_requested_branch_link() {
        let mut state = WorkspaceState::default();
        state.set_parent(link("feature/auth", "main", "auth"));
        state.set_parent(link("feature/ui", "main", "ui"));

        assert!(state.remove_parent("feature/auth"));
        assert!(!state.remove_parent("feature/missing"));

        assert_eq!(state.parents, vec![link("feature/ui", "main", "ui")]);
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
    fn cycle_detection_follows_existing_parent_links() {
        let mut state = WorkspaceState::default();
        state.set_parent(link("feature/b", "feature/a", "b"));
        state.set_parent(link("feature/c", "feature/b", "c"));

        assert!(state.would_create_cycle("feature/a", "feature/c"));
        assert!(!state.would_create_cycle("feature/c", "main"));
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
        state.set_parent(link("feature/alpha", "main", "anchor"));
        assert!(state.reconcile(&paths, &[legacy.clone()])?);
        let original = state
            .policy_for_path(&legacy.path)
            .expect("migrated policy")
            .clone();
        assert_eq!(original.authority(), Authority::Kmux);
        assert_eq!(original.retention(), Some(Retention::Persistent));
        assert!(state.parent_for("feature/alpha").is_some());

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
