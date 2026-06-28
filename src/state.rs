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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AgentSessionKey {
    pub agent_kind: String,
    pub session_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct AgentObservationKey {
    pub session: AgentSessionKey,
    pub producer_kind: String,
    pub producer_instance: String,
}

impl AgentObservationKey {
    fn filename(&self) -> String {
        format!(
            "{}__{}__{}__{}.json",
            filename_component(&self.session.agent_kind),
            filename_component(&self.session.session_id),
            filename_component(&self.producer_kind),
            filename_component(&self.producer_instance),
        )
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentLocationHints {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tmux_instance: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub window_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_current_command: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_current_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_handle: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentObservationState {
    pub key: AgentObservationKey,
    #[serde(default)]
    pub created_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<AgentStatus>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_observed_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_changed_at: Option<u64>,
    pub working_elapsed_secs: u64,
    pub observed_at: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<String>,
    #[serde(default)]
    pub target: AgentLocationHints,
}

impl AgentObservationState {
    pub fn effective_created_at(&self) -> u64 {
        if self.created_at != 0 {
            return self.created_at;
        }

        [
            self.status_changed_at,
            self.status_observed_at,
            Some(self.observed_at),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(0)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AgentObservationTiming {
    pub status_changed_at: Option<u64>,
    pub working_elapsed_secs: u64,
}

pub fn next_observation_timing(
    previous: Option<&AgentObservationState>,
    status: Option<AgentStatus>,
    now: u64,
) -> AgentObservationTiming {
    let Some(status) = status else {
        return AgentObservationTiming {
            status_changed_at: previous.and_then(|state| state.status_changed_at),
            working_elapsed_secs: previous.map_or(0, |state| state.working_elapsed_secs),
        };
    };
    let Some(previous) = previous.filter(|state| state.status.is_some()) else {
        return fresh_observation_timing(now);
    };

    if previous.status == Some(status) {
        return AgentObservationTiming {
            status_changed_at: previous.status_changed_at,
            working_elapsed_secs: match status {
                AgentStatus::Done => 0,
                AgentStatus::Working | AgentStatus::Waiting => previous.working_elapsed_secs,
            },
        };
    }

    match (previous.status, status) {
        (Some(AgentStatus::Working), AgentStatus::Waiting) => AgentObservationTiming {
            status_changed_at: Some(now),
            working_elapsed_secs: previous.working_elapsed_secs.saturating_add(
                now.saturating_sub(previous.status_changed_at.unwrap_or(previous.observed_at)),
            ),
        },
        (Some(AgentStatus::Waiting), AgentStatus::Working) => AgentObservationTiming {
            status_changed_at: Some(now),
            working_elapsed_secs: previous.working_elapsed_secs,
        },
        _ => fresh_observation_timing(now),
    }
}

fn fresh_observation_timing(now: u64) -> AgentObservationTiming {
    AgentObservationTiming {
        status_changed_at: Some(now),
        working_elapsed_secs: 0,
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

    pub fn upsert_observation(&self, state: &AgentObservationState) -> Result<()> {
        let path = self.observation_path(&state.key);
        let content = serde_json::to_vec_pretty(state)?;
        write_atomic(&path, &content)
    }

    pub fn get_observation(
        &self,
        key: &AgentObservationKey,
    ) -> Result<Option<AgentObservationState>> {
        Ok(read_observation_file(&self.observation_path(key))?
            .filter(|observation| observation.key == *key))
    }

    pub fn list_observations(&self) -> Result<Vec<AgentObservationState>> {
        let observations_dir = self.observations_dir();
        if !observations_dir.exists() {
            return Ok(Vec::new());
        }

        let mut observations = Vec::new();
        for entry in fs::read_dir(&observations_dir).with_context(|| {
            format!(
                "failed to read state directory {}",
                observations_dir.display()
            )
        })? {
            let entry = entry?;
            let path = entry.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                match read_observation_file(&path)? {
                    Some(observation) => {
                        let canonical_path = self.observation_path(&observation.key);
                        if path != canonical_path {
                            if canonical_path.exists() {
                                delete_file_if_exists(&path)?;
                                continue;
                            }
                            fs::rename(&path, &canonical_path).with_context(|| {
                                format!(
                                    "failed to migrate observation state file {} to {}",
                                    path.display(),
                                    canonical_path.display()
                                )
                            })?;
                        }
                        observations.push(observation);
                    }
                    None => delete_file_if_exists(&path)?,
                }
            }
        }

        observations.sort_by(|left, right| left.key.cmp(&right.key));
        observations.dedup_by(|left, right| left.key == right.key);
        Ok(observations)
    }

    pub fn delete_observation(&self, key: &AgentObservationKey) -> Result<()> {
        delete_file_if_exists(&self.observation_path(key))
    }

    pub fn delete_session(&self, session: &AgentSessionKey) -> Result<()> {
        for observation in self.list_observations()? {
            if &observation.key.session == session {
                self.delete_observation(&observation.key)?;
            }
        }
        Ok(())
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
        for mut observation in self.list_observations()? {
            let matches_handle = observation.target.worktree_handle.as_deref() == Some(old_handle);
            let matches_path = observation
                .target
                .worktree_path
                .as_deref()
                .is_some_and(|path| Path::new(path) == old_path);
            let matches_directory = observation
                .target
                .directory
                .as_deref()
                .is_some_and(|path| Path::new(path) == old_path);
            let matches_window = observation.target.window_name.as_deref() == Some(old_window_name);

            if matches_handle || matches_path || matches_directory || matches_window {
                observation.target.worktree_handle = Some(new_handle.to_owned());
                observation.target.worktree_path = Some(new_path.display().to_string());
                if matches_directory {
                    observation.target.directory = Some(new_path.display().to_string());
                }
                observation.target.window_name = Some(new_window_name.to_owned());
                observation.observed_at = now_unix_seconds();
                self.upsert_observation(&observation)?;
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
        fs::create_dir_all(base_path.join("agent-observations"))
            .with_context(|| format!("failed to create state directory {}", base_path.display()))?;
        Ok(Self { base_path })
    }

    fn observations_dir(&self) -> PathBuf {
        self.base_path.join("agent-observations")
    }

    fn observation_path(&self, key: &AgentObservationKey) -> PathBuf {
        self.observations_dir().join(key.filename())
    }
}

pub fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn read_observation_file(path: &Path) -> Result<Option<AgentObservationState>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };

    Ok(serde_json::from_str::<AgentObservationState>(&content).ok())
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
    value.bytes().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn state_store_round_trips_agent_observation_state() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let key = test_observation_key("ses_123", "tui", "default/%1");
        let state = AgentObservationState {
            key: key.clone(),
            created_at: 41,
            status: Some(AgentStatus::Working),
            status_observed_at: Some(42),
            status_changed_at: Some(42),
            working_elapsed_secs: 5,
            observed_at: 43,
            title: Some("OpenCode session".to_owned()),
            context: Some("163.2K (41%)".to_owned()),
            target: AgentLocationHints {
                tmux_instance: Some("test".to_owned()),
                pane_id: Some("%1".to_owned()),
                window_id: Some("@1".to_owned()),
                session_name: Some("project".to_owned()),
                window_name: Some("kmux-feature-auth".to_owned()),
                pane_title: Some("Agent title".to_owned()),
                pane_current_command: Some("opencode".to_owned()),
                pane_current_path: Some("/repo__worktrees/feature-auth".to_owned()),
                repo_name: Some("repo".to_owned()),
                repo_path: Some("/repo".to_owned()),
                worktree_handle: Some("feature-auth".to_owned()),
                worktree_path: Some("/repo__worktrees/feature-auth".to_owned()),
                branch: Some("feature/auth".to_owned()),
                directory: Some("/repo__worktrees/feature-auth".to_owned()),
            },
        };

        store.upsert_observation(&state)?;

        assert_eq!(store.list_observations()?, vec![state]);
        store.delete_observation(&key)?;
        assert!(store.list_observations()?.is_empty());
        Ok(())
    }

    #[test]
    fn observation_filename_components_are_collision_safe() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let escaped = test_observation_key("a/b", "server", "default");
        let literal = test_observation_key("a_x2Fb", "server", "default");

        assert_ne!(
            store.observation_path(&escaped),
            store.observation_path(&literal)
        );
        Ok(())
    }

    #[test]
    fn get_observation_rejects_mismatched_file_key() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let requested = test_observation_key("requested", "server", "default");
        let state = test_observation("server", "default", AgentStatus::Working, 100);
        fs::write(
            store.observation_path(&requested),
            serde_json::to_vec_pretty(&state)?,
        )?;

        assert_eq!(store.get_observation(&requested)?, None);
        Ok(())
    }

    #[test]
    fn delete_session_removes_observations_migrated_from_non_canonical_filenames() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let state = test_observation("server", "default", AgentStatus::Working, 100);
        let session = state.key.session.clone();
        let old_path = store
            .observations_dir()
            .join("opencode__ses_root__server__default.json");
        fs::write(&old_path, serde_json::to_vec_pretty(&state)?)?;

        store.delete_session(&session)?;

        assert!(!old_path.exists());
        assert!(store.list_observations()?.is_empty());
        Ok(())
    }

    #[test]
    fn corrupt_agent_observation_state_is_pruned() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let corrupt_path = store.observations_dir().join("bad.json");
        fs::write(&corrupt_path, "not json")?;

        assert!(store.list_observations()?.is_empty());
        assert!(!corrupt_path.exists());
        Ok(())
    }

    #[test]
    fn old_agent_reports_directory_is_ignored() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let old_dir = store.base_path.join("agent-reports");
        fs::create_dir_all(&old_dir)?;
        fs::write(old_dir.join("old.json"), "{}")?;

        assert!(store.list_observations()?.is_empty());
        Ok(())
    }

    #[test]
    fn observation_files_are_keyed_by_session_and_producer() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let tui = test_observation("tui", "default/%1", AgentStatus::Working, 100);
        let server = test_observation("server", "http://127.0.0.1:4096", AgentStatus::Waiting, 100);

        store.upsert_observation(&tui)?;
        store.upsert_observation(&server)?;

        let observations = store.list_observations()?;
        assert_eq!(observations.len(), 2);
        assert_eq!(observations[0].key.session, observations[1].key.session);
        assert_ne!(
            observations[0].key.producer_kind,
            observations[1].key.producer_kind
        );
        Ok(())
    }

    #[test]
    fn delete_session_removes_all_producer_observations() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let tui = test_observation("tui", "default/%1", AgentStatus::Working, 100);
        let server = test_observation("server", "http://127.0.0.1:4096", AgentStatus::Waiting, 100);
        let session = tui.key.session.clone();
        store.upsert_observation(&tui)?;
        store.upsert_observation(&server)?;

        store.delete_session(&session)?;

        assert!(store.list_observations()?.is_empty());
        Ok(())
    }

    #[test]
    fn migrates_matching_worktree_observation_state() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let mut state = test_observation("tui", "default/%1", AgentStatus::Done, 42);
        state.target.worktree_handle = Some("old".to_owned());
        state.target.worktree_path = Some("/repo__worktrees/old".to_owned());
        state.target.directory = Some("/repo__worktrees/old".to_owned());
        state.target.window_name = Some("kmux-old".to_owned());
        store.upsert_observation(&state)?;
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

        let observations = store.list_observations()?;
        assert_eq!(
            observations[0].target.worktree_handle.as_deref(),
            Some("new")
        );
        assert_eq!(
            observations[0].target.worktree_path.as_deref(),
            Some("/repo__worktrees/new")
        );
        assert_eq!(
            observations[0].target.directory.as_deref(),
            Some("/repo__worktrees/new")
        );
        assert_eq!(
            observations[0].target.window_name.as_deref(),
            Some("kmux-new")
        );
        assert_eq!(observations[0].status_changed_at, Some(42));
        assert!(observations[0].observed_at >= before);
        Ok(())
    }

    #[test]
    fn timing_accumulates_working_across_waiting_pause() {
        let mut observation = test_observation("tui", "default/%1", AgentStatus::Working, 0);

        let waiting =
            next_observation_timing(Some(&observation), Some(AgentStatus::Waiting), 20 * 60);
        assert_eq!(waiting.status_changed_at, Some(20 * 60));
        assert_eq!(waiting.working_elapsed_secs, 20 * 60);

        observation.status = Some(AgentStatus::Waiting);
        observation.status_changed_at = waiting.status_changed_at;
        observation.working_elapsed_secs = waiting.working_elapsed_secs;
        let working =
            next_observation_timing(Some(&observation), Some(AgentStatus::Working), 25 * 60);
        assert_eq!(working.status_changed_at, Some(25 * 60));
        assert_eq!(working.working_elapsed_secs, 20 * 60);
    }

    #[test]
    fn metadata_only_updates_preserve_previous_timing() {
        let observation = test_observation("tui", "default/%1", AgentStatus::Working, 300);

        let timing = next_observation_timing(Some(&observation), None, 400);

        assert_eq!(timing.status_changed_at, Some(300));
        assert_eq!(timing.working_elapsed_secs, 0);
    }

    #[test]
    fn effective_created_at_falls_back_for_old_observation_state() {
        let mut observation = test_observation("tui", "default/%1", AgentStatus::Working, 300);
        observation.created_at = 0;
        observation.status_observed_at = Some(250);
        observation.observed_at = 400;

        assert_eq!(observation.effective_created_at(), 250);
    }

    #[test]
    fn timing_starts_and_ends_runs_cleanly() {
        let done = test_observation("tui", "default/%1", AgentStatus::Done, 100);
        let started = next_observation_timing(Some(&done), Some(AgentStatus::Working), 300);
        assert_eq!(started.status_changed_at, Some(300));
        assert_eq!(started.working_elapsed_secs, 0);

        let working = test_observation("tui", "default/%1", AgentStatus::Working, 300);
        let finished = next_observation_timing(Some(&working), Some(AgentStatus::Done), 500);
        assert_eq!(finished.status_changed_at, Some(500));
        assert_eq!(finished.working_elapsed_secs, 0);

        let repeated_done = next_observation_timing(Some(&done), Some(AgentStatus::Done), 500);
        assert_eq!(repeated_done.status_changed_at, Some(100));
        assert_eq!(repeated_done.working_elapsed_secs, 0);
    }

    #[test]
    fn timing_saturates_when_clock_moves_backwards() {
        let mut observation = test_observation("tui", "default/%1", AgentStatus::Working, 300);
        observation.working_elapsed_secs = 10;

        let waiting = next_observation_timing(Some(&observation), Some(AgentStatus::Waiting), 200);

        assert_eq!(waiting.status_changed_at, Some(200));
        assert_eq!(waiting.working_elapsed_secs, 10);
    }

    fn test_observation(
        producer_kind: &str,
        producer_instance: &str,
        status: AgentStatus,
        status_changed_at: u64,
    ) -> AgentObservationState {
        AgentObservationState {
            key: test_observation_key("ses_root", producer_kind, producer_instance),
            created_at: status_changed_at,
            status: Some(status),
            status_observed_at: Some(status_changed_at),
            status_changed_at: Some(status_changed_at),
            working_elapsed_secs: 0,
            observed_at: status_changed_at,
            title: None,
            context: None,
            target: AgentLocationHints {
                worktree_handle: Some("feature".to_owned()),
                worktree_path: Some("/repo__worktrees/feature".to_owned()),
                branch: Some("feature".to_owned()),
                ..AgentLocationHints::default()
            },
        }
    }

    fn test_observation_key(
        session_id: &str,
        producer_kind: &str,
        producer_instance: &str,
    ) -> AgentObservationKey {
        AgentObservationKey {
            session: AgentSessionKey {
                agent_kind: "opencode".to_owned(),
                session_id: session_id.to_owned(),
            },
            producer_kind: producer_kind.to_owned(),
            producer_instance: producer_instance.to_owned(),
        }
    }
}
