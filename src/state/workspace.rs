use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};

const CURRENT_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct WorkspaceState {
    pub version: u32,
    pub parents: Vec<WorkspaceParentLink>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceParentLink {
    pub branch: String,
    pub parent: String,
    pub anchor: String,
}

#[derive(Debug, Clone)]
pub struct WorkspaceStateStore {
    path: PathBuf,
}

impl WorkspaceState {
    pub fn parent_for(&self, branch: &str) -> Option<&WorkspaceParentLink> {
        self.parents.iter().find(|link| link.branch == branch)
    }

    pub fn set_parent(&mut self, link: WorkspaceParentLink) {
        self.parents
            .retain(|existing| existing.branch != link.branch);
        self.parents.push(link);
        self.normalize();
    }

    pub fn remove_parent(&mut self, branch: &str) -> bool {
        let before = self.parents.len();
        self.parents.retain(|link| link.branch != branch);
        before != self.parents.len()
    }

    pub fn children_of(&self, parent: &str) -> Vec<String> {
        self.parents
            .iter()
            .filter(|link| link.parent == parent)
            .map(|link| link.branch.clone())
            .collect()
    }

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
    pub fn new(branch: String, parent: String, anchor: String) -> Self {
        Self {
            branch,
            parent,
            anchor,
        }
    }
}

impl WorkspaceStateStore {
    pub fn new(git_common_dir: &Path) -> Self {
        Self {
            path: git_common_dir.join("kmux/state.json"),
        }
    }

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

    pub fn save(&self, state: &WorkspaceState) -> Result<()> {
        let mut state = state.clone();
        state.version = CURRENT_VERSION;
        state.normalize();
        let content = serde_json::to_vec_pretty(&state)?;
        write_atomic(&self.path, &content)
    }
}

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
