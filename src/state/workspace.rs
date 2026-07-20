//! Git-common-dir-backed workspace graph persistence.
//!
//! This module stores branch parent relationships and merge-base anchors with
//! the repo's shared Git metadata so all worktrees for a clone see the same
//! graph. It is intentionally separate from `state::agent`, which stores
//! external agent observations in XDG state.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
/// Repo-local kmux workspace graph metadata persisted under Git's common dir.
pub struct WorkspaceState {
    pub version: u32,
    pub parents: Vec<WorkspaceParentLink>,
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
        let mut parents = BTreeMap::new();
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
        self.parents
            .sort_by(|left, right| left.branch.cmp(&right.branch));
        self.parents
            .dedup_by(|left, right| left.branch == right.branch);
    }
}

impl Default for WorkspaceState {
    fn default() -> Self {
        Self {
            version: CURRENT_VERSION,
            parents: Vec::new(),
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
        if state.version != CURRENT_VERSION {
            bail!(
                "unsupported kmux workspace state version {}; expected {}",
                state.version,
                CURRENT_VERSION
            );
        }
        state.normalize();
        Ok(state)
    }

    /// Persist workspace graph state with the current schema version and stable ordering.
    pub fn save(&self, state: &WorkspaceState) -> Result<()> {
        let mut state = state.clone();
        state.version = CURRENT_VERSION;
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
}
