//! Filesystem store for agent observation JSON files.
//!
//! The store uses the user's XDG state directory because observations are local
//! process telemetry, not repo metadata. It owns filename construction,
//! pruning of stale files, and atomic writes from short-lived producers.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use directories::BaseDirs;

use crate::telemetry;

use super::model::{AgentObservationKey, AgentObservationState, AgentSessionKey};

#[derive(Debug, Clone)]
/// XDG-backed store for external agent observations.
pub struct StateStore {
    base_path: PathBuf,
}

/// Return the current Unix timestamp in seconds, saturating to zero on clock errors.
pub fn now_unix_seconds() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

impl StateStore {
    /// Open the XDG-backed kmux agent-observation state store.
    pub fn new() -> Result<Self> {
        let base_dirs = BaseDirs::new().context("could not determine state directory")?;
        let state_root = base_dirs
            .state_dir()
            .unwrap_or_else(|| base_dirs.data_local_dir());
        Self::with_path(state_root.join("kmux"))
    }

    /// Insert or replace one producer's latest observation for an agent session.
    pub fn upsert_observation(&self, state: &AgentObservationState) -> Result<()> {
        let path = self.observation_path(&state.key);
        let content = serde_json::to_vec_pretty(state)?;
        write_atomic(&path, &content)
    }

    /// Load one observation by key, ignoring stale files whose embedded key does not match.
    pub fn get_observation(
        &self,
        key: &AgentObservationKey,
    ) -> Result<Option<AgentObservationState>> {
        Ok(read_observation_file(&self.observation_path(key))?
            .filter(|observation| observation.key == *key))
    }

    /// List valid observations, pruning invalid JSON or non-canonical files.
    pub fn list_observations(&self) -> Result<Vec<AgentObservationState>> {
        let observations_dir = self.observations_dir();
        let result = telemetry::timed_result_event!(
            "agent_observations.list",
            { dir = %observations_dir.display(), },
            || {
                if !observations_dir.exists() {
                    return Ok(ObservationListTelemetry::default());
                }

                let mut telemetry = ObservationListTelemetry::default();
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
                        telemetry.files += 1;
                        match read_observation_file(&path)? {
                            Some(observation) => {
                                let canonical_path = self.observation_path(&observation.key);
                                if path != canonical_path {
                                    telemetry.pruned += 1;
                                    delete_file_if_exists(&path)?;
                                    continue;
                                }
                                telemetry.observations.push(observation);
                            }
                            None => {
                                telemetry.pruned += 1;
                                delete_file_if_exists(&path)?;
                            }
                        }
                    }
                }

                telemetry
                    .observations
                    .sort_by(|left, right| left.key.cmp(&right.key));
                telemetry
                    .observations
                    .dedup_by(|left, right| left.key == right.key);
                Ok(telemetry)
            },
            ok |telemetry| {
                files = telemetry.files,
                observations = telemetry.observations.len(),
                pruned = telemetry.pruned,
            },
        );

        result.map(|telemetry| telemetry.observations)
    }

    /// Delete a single observation file if it exists.
    pub fn delete_observation(&self, key: &AgentObservationKey) -> Result<()> {
        delete_file_if_exists(&self.observation_path(key))
    }

    /// Delete every producer observation associated with one agent session.
    pub fn delete_session(&self, session: &AgentSessionKey) -> Result<()> {
        for observation in self.list_observations()? {
            if &observation.key.session == session {
                self.delete_observation(&observation.key)?;
            }
        }
        Ok(())
    }

    /// Open a store at an explicit base path for tests and controlled callers.
    pub(super) fn with_path(base_path: impl Into<PathBuf>) -> Result<Self> {
        let base_path = base_path.into();
        fs::create_dir_all(base_path.join("agent-observations"))
            .with_context(|| format!("failed to create state directory {}", base_path.display()))?;
        Ok(Self { base_path })
    }

    fn observations_dir(&self) -> PathBuf {
        self.base_path.join("agent-observations")
    }

    fn observation_path(&self, key: &AgentObservationKey) -> PathBuf {
        self.observations_dir().join(observation_filename(key))
    }
}

#[derive(Default)]
struct ObservationListTelemetry {
    observations: Vec<AgentObservationState>,
    files: usize,
    pruned: usize,
}

// Agent observation files are external-agent telemetry. Invalid JSON is treated
// as stale state so sidebar/status rendering can recover without user cleanup.
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

// Write observations atomically because status updates may be produced by
// independent short-lived processes.
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

// Deletion races are harmless because producers can refresh observations later.
fn delete_file_if_exists(path: &Path) -> Result<()> {
    match fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| format!("failed to delete {}", path.display())),
    }
}

// Filenames encode every identity component so multiple producers can report
// the same logical session without overwriting each other.
fn observation_filename(key: &AgentObservationKey) -> String {
    format!(
        "{}__{}__{}__{}.json",
        filename_component(&key.session.agent_kind),
        filename_component(&key.session.session_id),
        filename_component(&key.producer_kind),
        filename_component(&key.producer_instance),
    )
}

// Hex-encode arbitrary key text into portable filename components without
// introducing collisions between escaped and literal separator characters.
fn filename_component(value: &str) -> String {
    value.bytes().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AgentLocationHints, AgentStatus};
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
            metadata: [("workspace_id".to_owned(), "wrk_01KTEST".to_owned())]
                .into_iter()
                .collect(),
            metadata_cleared: Default::default(),
            target: AgentLocationHints {
                tmux_instance: Some("test".to_owned()),
                tmux_pane_id: Some("%1".to_owned()),
                tmux_window_id: Some("@1".to_owned()),
                tmux_session_name: Some("project".to_owned()),
                tmux_window_name: Some("kmux-feature-auth".to_owned()),
                tmux_pane_title: Some("Agent title".to_owned()),
                tmux_pane_current_command: Some("opencode".to_owned()),
                tmux_pane_current_path: Some("/repo__worktrees/feature-auth".to_owned()),
                git_repo_name: Some("repo".to_owned()),
                git_repo_path: Some("/repo".to_owned()),
                kmux_workspace_slug: Some("feature-auth".to_owned()),
                git_worktree_path: Some("/repo__worktrees/feature-auth".to_owned()),
                git_branch: Some("feature/auth".to_owned()),
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
    fn non_canonical_observation_filenames_are_pruned() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let state = test_observation("server", "default", AgentStatus::Working, 100);
        let non_canonical_path = store
            .observations_dir()
            .join("opencode__ses_root__server__default.json");
        fs::write(&non_canonical_path, serde_json::to_vec_pretty(&state)?)?;

        assert!(store.list_observations()?.is_empty());

        assert!(!non_canonical_path.exists());
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
            metadata: Default::default(),
            metadata_cleared: Default::default(),
            target: AgentLocationHints {
                kmux_workspace_slug: Some("feature".to_owned()),
                git_worktree_path: Some("/repo__worktrees/feature".to_owned()),
                git_branch: Some("feature".to_owned()),
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
