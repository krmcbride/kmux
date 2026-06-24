use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum AgentStatus {
    Working,
    Waiting,
    Done,
}

impl AgentStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Working => "working",
            Self::Waiting => "waiting",
            Self::Done => "done",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneKey {
    pub backend: String,
    pub instance: String,
    pub pane_id: String,
}

impl PaneKey {
    pub fn new_tmux(instance: impl Into<String>, pane_id: impl Into<String>) -> Self {
        Self {
            backend: "tmux".to_owned(),
            instance: instance.into(),
            pane_id: pane_id.into(),
        }
    }

    fn filename(&self) -> String {
        format!(
            "{}__{}__{}.json",
            filename_component(&self.backend),
            filename_component(&self.instance),
            filename_component(&self.pane_id)
        )
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentState {
    pub pane_key: PaneKey,
    pub status: AgentStatus,
    pub icon: String,
    #[serde(alias = "updated_at")]
    pub status_changed_at: u64,
    #[serde(default)]
    pub observed_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_current_command: Option<String>,
    pub worktree_handle: Option<String>,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub session_name: String,
    pub window_name: String,
    pub window_id: String,
}

impl AgentState {
    fn normalize_timestamps(mut self) -> Self {
        if self.observed_at == 0 {
            self.observed_at = self.status_changed_at;
        }
        self
    }
}

#[derive(Debug, Clone)]
pub struct StateStore {
    base_path: PathBuf,
}

impl StateStore {
    pub fn new() -> Result<Self> {
        let base_dirs = BaseDirs::new().context("could not determine state directory")?;
        let state_root = base_dirs
            .state_dir()
            .unwrap_or_else(|| base_dirs.data_local_dir());
        Self::with_path(state_root.join("kmux"))
    }

    pub fn upsert_agent(&self, state: &AgentState) -> Result<()> {
        let path = self.agent_path(&state.pane_key);
        let content = serde_json::to_vec_pretty(state)?;
        write_atomic(&path, &content)
    }

    pub fn get_agent(&self, key: &PaneKey) -> Result<Option<AgentState>> {
        read_agent_file(&self.agent_path(key))
    }

    pub fn list_agents(&self) -> Result<Vec<AgentState>> {
        let agents_dir = self.agents_dir();
        if !agents_dir.exists() {
            return Ok(Vec::new());
        }

        let mut agents = Vec::new();
        for entry in fs::read_dir(&agents_dir)
            .with_context(|| format!("failed to read state directory {}", agents_dir.display()))?
        {
            let entry = entry?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                match read_agent_file(&path)? {
                    Some(agent) => agents.push(agent),
                    None => delete_file_if_exists(&path)?,
                }
            }
        }

        agents.sort_by(|left, right| {
            left.worktree_handle
                .cmp(&right.worktree_handle)
                .then_with(|| left.pane_key.pane_id.cmp(&right.pane_key.pane_id))
        });
        Ok(agents)
    }

    pub fn delete_agent(&self, key: &PaneKey) -> Result<()> {
        delete_file_if_exists(&self.agent_path(key))
    }

    pub fn migrate_worktree(
        &self,
        old_handle: &str,
        new_handle: &str,
        old_path: &Path,
        new_path: &Path,
        old_window_name: &str,
        new_window_name: &str,
    ) -> Result<usize> {
        let mut migrated = 0;
        for mut agent in self.list_agents()? {
            let matches_handle = agent.worktree_handle.as_deref() == Some(old_handle);
            let matches_path = agent
                .worktree_path
                .as_deref()
                .is_some_and(|path| Path::new(path) == old_path);
            let matches_window = agent.window_name == old_window_name;

            if matches_handle || matches_path || matches_window {
                agent.worktree_handle = Some(new_handle.to_owned());
                agent.worktree_path = Some(new_path.display().to_string());
                agent.window_name = new_window_name.to_owned();
                agent.observed_at = now_unix_seconds();
                self.upsert_agent(&agent)?;
                migrated += 1;
            }
        }
        Ok(migrated)
    }

    #[cfg(test)]
    pub fn test_with_path(base_path: impl Into<PathBuf>) -> Result<Self> {
        Self::with_path(base_path)
    }

    fn with_path(base_path: impl Into<PathBuf>) -> Result<Self> {
        let base_path = base_path.into();
        fs::create_dir_all(base_path.join("agents"))
            .with_context(|| format!("failed to create state directory {}", base_path.display()))?;
        Ok(Self { base_path })
    }

    fn agents_dir(&self) -> PathBuf {
        self.base_path.join("agents")
    }

    fn agent_path(&self, key: &PaneKey) -> PathBuf {
        self.agents_dir().join(key.filename())
    }
}

pub fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn read_agent_file(path: &Path) -> Result<Option<AgentState>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    Ok(serde_json::from_str::<AgentState>(&content)
        .ok()
        .map(AgentState::normalize_timestamps))
}

fn write_atomic(path: &Path, content: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
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

fn delete_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to delete {}", path.display())),
    }
}

fn filename_component(value: &str) -> String {
    let mut component = String::new();
    for byte in value.bytes() {
        let ch = byte as char;
        if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '_' | '-') {
            component.push(ch);
        } else {
            component.push_str(&format!("_x{byte:02X}"));
        }
    }
    component
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn state_store_round_trips_agent_state() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let key = PaneKey::new_tmux("test", "%1");
        let state = AgentState {
            pane_key: key.clone(),
            status: AgentStatus::Working,
            icon: "W".to_owned(),
            status_changed_at: 42,
            observed_at: 43,
            pane_title: Some("Agent title".to_owned()),
            pane_current_command: Some("nvim".to_owned()),
            worktree_handle: Some("feature-auth".to_owned()),
            worktree_path: Some("/repo__worktrees/feature-auth".to_owned()),
            branch: Some("feature/auth".to_owned()),
            session_name: "project".to_owned(),
            window_name: "kmux-feature-auth".to_owned(),
            window_id: "@1".to_owned(),
        };

        store.upsert_agent(&state)?;

        assert_eq!(store.list_agents()?, vec![state]);
        store.delete_agent(&key)?;
        assert!(store.list_agents()?.is_empty());
        Ok(())
    }

    #[test]
    fn corrupt_agent_state_is_pruned() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let corrupt_path = store.agents_dir().join("bad.json");
        fs::write(&corrupt_path, "not json")?;

        assert!(store.list_agents()?.is_empty());
        assert!(!corrupt_path.exists());
        Ok(())
    }

    #[test]
    fn legacy_updated_at_state_deserializes_to_split_timestamps() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let key = PaneKey::new_tmux("test", "%1");
        let content = serde_json::json!({
            "pane_key": {
                "backend": "tmux",
                "instance": "test",
                "pane_id": "%1"
            },
            "status": "working",
            "icon": "W",
            "updated_at": 42,
            "worktree_handle": "feature-auth",
            "worktree_path": "/repo__worktrees/feature-auth",
            "branch": "feature/auth",
            "session_name": "project",
            "window_name": "kmux-feature-auth",
            "window_id": "@1"
        });
        fs::write(store.agent_path(&key), serde_json::to_vec_pretty(&content)?)?;

        let agents = store.list_agents()?;

        assert_eq!(agents[0].status_changed_at, 42);
        assert_eq!(agents[0].observed_at, 42);
        let serialized = serde_json::to_string(&agents[0])?;
        assert!(serialized.contains("status_changed_at"));
        assert!(serialized.contains("observed_at"));
        assert!(!serialized.contains("updated_at"));
        Ok(())
    }

    #[test]
    fn migrates_matching_worktree_state() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let state = AgentState {
            pane_key: PaneKey::new_tmux("test", "%1"),
            status: AgentStatus::Done,
            icon: "D".to_owned(),
            status_changed_at: 42,
            observed_at: 43,
            pane_title: None,
            pane_current_command: None,
            worktree_handle: Some("old".to_owned()),
            worktree_path: Some("/repo__worktrees/old".to_owned()),
            branch: Some("feature/original".to_owned()),
            session_name: "project".to_owned(),
            window_name: "kmux-old".to_owned(),
            window_id: "@1".to_owned(),
        };
        store.upsert_agent(&state)?;
        let before = now_unix_seconds();

        assert_eq!(
            store.migrate_worktree(
                "old",
                "new",
                Path::new("/repo__worktrees/old"),
                Path::new("/repo__worktrees/new"),
                "kmux-old",
                "kmux-new"
            )?,
            1
        );

        let agents = store.list_agents()?;
        assert_eq!(agents[0].worktree_handle.as_deref(), Some("new"));
        assert_eq!(
            agents[0].worktree_path.as_deref(),
            Some("/repo__worktrees/new")
        );
        assert_eq!(agents[0].window_name, "kmux-new");
        assert_eq!(agents[0].branch.as_deref(), Some("feature/original"));
        assert_eq!(agents[0].status_changed_at, 42);
        assert!(agents[0].observed_at >= before);
        Ok(())
    }
}
