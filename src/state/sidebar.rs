//! XDG-backed persistence for sidebar UI state.
//!
//! Sidebar state is local UI coordination data, not Git repository metadata. It
//! lives under the user's XDG state directory so separate sidebar TUI processes
//! can agree on lightweight preferences without IPC or tmux broadcasts.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};

use crate::state::AgentSessionKey;

const CURRENT_VERSION: u32 = 1;
const STATE_FILENAME: &str = "sidebar-selection.json";

#[derive(Debug, Clone)]
/// XDG-backed store for sidebar logical selection state.
pub struct SidebarSelectionStore {
    path: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct SidebarSelectionState {
    version: u32,
    selections: Vec<SidebarWindowSelection>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SidebarWindowSelection {
    tmux_instance: String,
    tmux_window_id: String,
    session: AgentSessionKey,
}

impl SidebarSelectionStore {
    /// Open the XDG-backed sidebar selection state store.
    pub fn new() -> Result<Self> {
        let base_dirs = BaseDirs::new().context("could not determine state directory")?;
        let state_root = base_dirs
            .state_dir()
            .unwrap_or_else(|| base_dirs.data_local_dir());
        Ok(Self::with_path(state_root.join("kmux")))
    }

    /// Return the preferred logical session for a tmux window, if one was stored.
    pub fn selection_for_window(
        &self,
        tmux_instance: &str,
        tmux_window_id: &str,
    ) -> Result<Option<AgentSessionKey>> {
        Ok(self
            .load_state()?
            .selection_for_window(tmux_instance, tmux_window_id)
            .cloned())
    }

    /// Set the preferred logical session for a tmux window.
    pub fn set_selection_for_window(
        &self,
        tmux_instance: &str,
        tmux_window_id: &str,
        session: AgentSessionKey,
    ) -> Result<()> {
        let mut state = self.load_state()?;
        state.set_selection_for_window(tmux_instance, tmux_window_id, session);
        self.save_state(&state)
    }

    /// Clear the preferred logical session for a tmux window.
    pub fn clear_selection_for_window(
        &self,
        tmux_instance: &str,
        tmux_window_id: &str,
    ) -> Result<()> {
        let mut state = self.load_state()?;
        state.clear_selection_for_window(tmux_instance, tmux_window_id);
        self.save_state(&state)
    }

    fn with_path(base_path: impl Into<PathBuf>) -> Self {
        Self {
            path: base_path.into().join(STATE_FILENAME),
        }
    }

    fn load_state(&self) -> Result<SidebarSelectionState> {
        let content = match fs::read_to_string(&self.path) {
            Ok(content) => content,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return Ok(SidebarSelectionState::current());
            }
            Err(error) => {
                return Err(error)
                    .with_context(|| format!("failed to read {}", self.path.display()));
            }
        };

        let Ok(mut state) = serde_json::from_str::<SidebarSelectionState>(&content) else {
            return Ok(SidebarSelectionState::current());
        };
        if state.version != CURRENT_VERSION {
            return Ok(SidebarSelectionState::current());
        }
        state.normalize();
        Ok(state)
    }

    fn save_state(&self, state: &SidebarSelectionState) -> Result<()> {
        let mut state = state.clone();
        state.version = CURRENT_VERSION;
        state.normalize();
        let content = serde_json::to_vec_pretty(&state)?;
        write_atomic(&self.path, &content)
    }
}

impl SidebarSelectionState {
    fn current() -> Self {
        Self {
            version: CURRENT_VERSION,
            selections: Vec::new(),
        }
    }

    fn selection_for_window(
        &self,
        tmux_instance: &str,
        tmux_window_id: &str,
    ) -> Option<&AgentSessionKey> {
        self.selections
            .iter()
            .find(|selection| {
                selection.tmux_instance == tmux_instance
                    && selection.tmux_window_id == tmux_window_id
            })
            .map(|selection| &selection.session)
    }

    fn set_selection_for_window(
        &mut self,
        tmux_instance: &str,
        tmux_window_id: &str,
        session: AgentSessionKey,
    ) {
        self.clear_selection_for_window(tmux_instance, tmux_window_id);
        self.selections.push(SidebarWindowSelection {
            tmux_instance: tmux_instance.to_owned(),
            tmux_window_id: tmux_window_id.to_owned(),
            session,
        });
        self.normalize();
    }

    fn clear_selection_for_window(&mut self, tmux_instance: &str, tmux_window_id: &str) {
        self.selections.retain(|selection| {
            selection.tmux_instance != tmux_instance || selection.tmux_window_id != tmux_window_id
        });
    }

    fn normalize(&mut self) {
        self.selections.sort_by(|left, right| {
            (&left.tmux_instance, &left.tmux_window_id)
                .cmp(&(&right.tmux_instance, &right.tmux_window_id))
        });
        self.selections.dedup_by(|left, right| {
            left.tmux_instance == right.tmux_instance && left.tmux_window_id == right.tmux_window_id
        });
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
pub(super) mod test_support {
    /// Open a sidebar selection store at a caller-provided path for tests.
    pub fn store_with_path(
        base_path: impl Into<std::path::PathBuf>,
    ) -> super::SidebarSelectionStore {
        super::SidebarSelectionStore::with_path(base_path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn key(agent_kind: &str, session_id: &str) -> AgentSessionKey {
        AgentSessionKey {
            agent_kind: agent_kind.to_owned(),
            session_id: session_id.to_owned(),
        }
    }

    #[test]
    fn missing_file_returns_no_selection() -> Result<()> {
        let temp = TempDir::new()?;
        let store = SidebarSelectionStore::with_path(temp.path().join("state"));

        assert_eq!(store.selection_for_window("default", "@1")?, None);
        Ok(())
    }

    #[test]
    fn state_store_round_trips_selection() -> Result<()> {
        let temp = TempDir::new()?;
        let store = SidebarSelectionStore::with_path(temp.path().join("state"));
        let session = key("opencode", "ses_123");

        store.set_selection_for_window("default", "@1", session.clone())?;

        assert_eq!(store.selection_for_window("default", "@1")?, Some(session));
        Ok(())
    }

    #[test]
    fn set_selection_overwrites_existing_window_entry() -> Result<()> {
        let temp = TempDir::new()?;
        let store = SidebarSelectionStore::with_path(temp.path().join("state"));

        store.set_selection_for_window("default", "@1", key("opencode", "ses_first"))?;
        store.set_selection_for_window("default", "@1", key("opencode", "ses_second"))?;

        assert_eq!(
            store.selection_for_window("default", "@1")?,
            Some(key("opencode", "ses_second"))
        );
        Ok(())
    }

    #[test]
    fn clear_selection_removes_only_matching_window_entry() -> Result<()> {
        let temp = TempDir::new()?;
        let store = SidebarSelectionStore::with_path(temp.path().join("state"));
        store.set_selection_for_window("default", "@1", key("opencode", "ses_first"))?;
        store.set_selection_for_window("default", "@2", key("opencode", "ses_second"))?;

        store.clear_selection_for_window("default", "@1")?;

        assert_eq!(store.selection_for_window("default", "@1")?, None);
        assert_eq!(
            store.selection_for_window("default", "@2")?,
            Some(key("opencode", "ses_second"))
        );
        Ok(())
    }

    #[test]
    fn selections_are_isolated_by_tmux_instance_and_window() -> Result<()> {
        let temp = TempDir::new()?;
        let store = SidebarSelectionStore::with_path(temp.path().join("state"));
        store.set_selection_for_window("default", "@1", key("opencode", "ses_default"))?;
        store.set_selection_for_window("custom", "@1", key("opencode", "ses_custom"))?;
        store.set_selection_for_window("default", "@2", key("opencode", "ses_other"))?;

        assert_eq!(
            store.selection_for_window("default", "@1")?,
            Some(key("opencode", "ses_default"))
        );
        assert_eq!(
            store.selection_for_window("custom", "@1")?,
            Some(key("opencode", "ses_custom"))
        );
        assert_eq!(
            store.selection_for_window("default", "@2")?,
            Some(key("opencode", "ses_other"))
        );
        Ok(())
    }

    #[test]
    fn malformed_state_file_is_treated_as_missing() -> Result<()> {
        let temp = TempDir::new()?;
        let base = temp.path().join("state");
        fs::create_dir_all(&base)?;
        fs::write(base.join(STATE_FILENAME), "not json")?;
        let store = SidebarSelectionStore::with_path(base);

        assert_eq!(store.selection_for_window("default", "@1")?, None);
        Ok(())
    }

    #[test]
    fn unsupported_state_version_is_treated_as_missing() -> Result<()> {
        let temp = TempDir::new()?;
        let base = temp.path().join("state");
        fs::create_dir_all(&base)?;
        fs::write(
            base.join(STATE_FILENAME),
            r#"{
  "version": 999,
  "selections": [
    {
      "tmux_instance": "default",
      "tmux_window_id": "@1",
      "session": { "agent_kind": "opencode", "session_id": "ses_123" }
    }
  ]
}"#,
        )?;
        let store = SidebarSelectionStore::with_path(base);

        assert_eq!(store.selection_for_window("default", "@1")?, None);
        Ok(())
    }

    #[test]
    fn serialized_state_shape_is_stable() -> Result<()> {
        let temp = TempDir::new()?;
        let store = SidebarSelectionStore::with_path(temp.path().join("state"));

        store.set_selection_for_window("default", "@1", key("opencode", "ses_123"))?;

        let content = fs::read_to_string(temp.path().join("state").join(STATE_FILENAME))?;
        let json: serde_json::Value = serde_json::from_str(&content)?;
        assert_eq!(json["version"], CURRENT_VERSION);
        assert_eq!(json["selections"][0]["tmux_instance"], "default");
        assert_eq!(json["selections"][0]["tmux_window_id"], "@1");
        assert_eq!(json["selections"][0]["session"]["agent_kind"], "opencode");
        assert_eq!(json["selections"][0]["session"]["session_id"], "ses_123");
        Ok(())
    }
}
