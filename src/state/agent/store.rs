//! Filesystem store for agent observation JSON files.
//!
//! The store uses the user's XDG state directory because observations are local
//! process telemetry, not repo metadata. It owns filename construction,
//! pruning of stale files, and transactional atomic writes from short-lived
//! producers.

use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, ensure};
use directories::BaseDirs;
use sha2::{Digest, Sha256};

use crate::telemetry;

use super::model::{AgentObservationKey, AgentObservationState, AgentSessionKey};

#[derive(Debug, Clone)]
/// XDG-backed store for external agent observations.
pub struct StateStore {
    base_path: PathBuf,
}

// Lock a stable sibling file because atomic replacement changes an observation
// JSON file's inode and would make locking the target itself ineffective.
const OBSERVATION_LOCK_FILENAME: &str = "agent-observations.lock";

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
    #[cfg(test)]
    pub fn upsert_observation(&self, state: &AgentObservationState) -> Result<()> {
        let _lock = self.lock_observation_mutations()?;
        self.upsert_observation_unlocked(state)
    }

    /// Mutate one producer observation from its latest committed state.
    ///
    /// The closure runs while the cross-process observation lock is held. It
    /// must avoid slow external work and must not call another mutating store
    /// method, which would attempt to acquire the same lock recursively.
    pub fn mutate_observation(
        &self,
        key: &AgentObservationKey,
        mutation: impl FnOnce(Option<AgentObservationState>) -> Result<Option<AgentObservationState>>,
    ) -> Result<()> {
        let _lock = self.lock_observation_mutations()?;
        let previous = self.get_observation_unlocked(key)?;
        match mutation(previous)? {
            Some(state) => {
                ensure!(
                    state.key == *key,
                    "observation transaction returned a different key"
                );
                self.upsert_observation_unlocked(&state)
            }
            None => self.delete_observation_unlocked(key),
        }
    }

    /// Load one observation by key, ignoring stale files whose embedded key does not match.
    #[cfg(test)]
    pub fn get_observation(
        &self,
        key: &AgentObservationKey,
    ) -> Result<Option<AgentObservationState>> {
        self.get_observation_unlocked(key)
    }

    /// List valid observations, pruning invalid JSON or non-canonical files.
    pub fn list_observations(&self) -> Result<Vec<AgentObservationState>> {
        let _lock = self.lock_observation_mutations()?;
        self.list_observations_unlocked()
    }

    fn list_observations_unlocked(&self) -> Result<Vec<AgentObservationState>> {
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
        let _lock = self.lock_observation_mutations()?;
        self.delete_observation_unlocked(key)
    }

    /// Delete every producer observation associated with one agent session.
    pub fn delete_session(&self, session: &AgentSessionKey) -> Result<()> {
        self.delete_sessions(std::slice::from_ref(session))
    }

    /// Delete every producer observation for the selected logical sessions.
    ///
    /// Requested keys are normalized before the stable observation lock is
    /// acquired. Listing, stale-file pruning, and all matching deletes then run
    /// under that one lock so cooperating producers observe one serialized
    /// store mutation.
    pub fn delete_sessions(&self, sessions: &[AgentSessionKey]) -> Result<()> {
        let sessions = sessions.iter().collect::<BTreeSet<_>>();
        let _lock = self.lock_observation_mutations()?;
        for observation in self.list_observations_unlocked()? {
            if sessions.contains(&observation.key.session) {
                self.delete_observation_unlocked(&observation.key)?;
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

    fn observation_lock_path(&self) -> PathBuf {
        self.base_path.join(OBSERVATION_LOCK_FILENAME)
    }

    fn lock_observation_mutations(&self) -> Result<ObservationMutationLock> {
        let path = self.observation_lock_path();
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let file = options
            .open(&path)
            .with_context(|| format!("failed to open observation lock {}", path.display()))?;
        file.lock()
            .with_context(|| format!("failed to lock observation store {}", path.display()))?;
        Ok(ObservationMutationLock { file })
    }

    fn get_observation_unlocked(
        &self,
        key: &AgentObservationKey,
    ) -> Result<Option<AgentObservationState>> {
        Ok(read_observation_file(&self.observation_path(key))?
            .filter(|observation| observation.key == *key))
    }

    fn upsert_observation_unlocked(&self, state: &AgentObservationState) -> Result<()> {
        let path = self.observation_path(&state.key);
        let content = serde_json::to_vec_pretty(state)?;
        write_atomic(&path, &content)
    }

    fn delete_observation_unlocked(&self, key: &AgentObservationKey) -> Result<()> {
        delete_file_if_exists(&self.observation_path(key))
    }
}

struct ObservationMutationLock {
    file: File,
}

impl Drop for ObservationMutationLock {
    fn drop(&mut self) {
        let _ = self.file.unlock();
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

// Keep arbitrary external identities out of filesystem component lengths. The
// persisted JSON remains the readable source of truth and is validated against
// the requested key when loaded.
fn observation_filename(key: &AgentObservationKey) -> String {
    let mut digest = Sha256::new();
    digest.update(b"kmux-agent-observation-key-v1\0");
    for component in [
        &key.session.agent_kind,
        &key.session.session_id,
        &key.producer_kind,
        &key.producer_instance,
    ] {
        let bytes = component.as_bytes();
        digest.update((bytes.len() as u64).to_be_bytes());
        digest.update(bytes);
    }
    format!("v1-{:x}.json", digest.finalize())
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, mpsc};
    use std::thread;
    use std::time::Duration;

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
            target: AgentLocationHints {
                tmux_instance: Some("test".to_owned()),
                git_repo_name: Some("repo".to_owned()),
                git_repo_path: Some("/repo".to_owned()),
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
    fn unsupported_pre_release_observation_fields_are_pruned() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let state = test_observation("server", "default", AgentStatus::Working, 100);
        let mut persisted = serde_json::to_value(&state)?;
        let object = persisted
            .as_object_mut()
            .ok_or_else(|| anyhow::anyhow!("observation should serialize as an object"))?;
        object.insert(
            "metadata".to_owned(),
            serde_json::json!({"workspace_id": "wrk_example"}),
        );
        assert_persisted_observation_is_pruned(&store, &state, &persisted)?;
        Ok(())
    }

    #[test]
    fn obsolete_pre_release_location_fields_are_pruned() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let state = test_observation("server", "default", AgentStatus::Working, 100);
        let mut persisted = serde_json::to_value(&state)?;
        persisted
            .get_mut("target")
            .and_then(serde_json::Value::as_object_mut)
            .ok_or_else(|| anyhow::anyhow!("target should serialize as an object"))?
            .insert("tmux_pane_id".to_owned(), serde_json::json!("%old"));

        assert_persisted_observation_is_pruned(&store, &state, &persisted)?;
        Ok(())
    }

    #[test]
    fn observation_filenames_are_stable_and_fixed_length() {
        let key = test_observation_key("ses_123", "server", "http://127.0.0.1:4096");
        let filename = observation_filename(&key);

        assert_eq!(filename.len(), 72);
        assert!(filename.starts_with("v1-"));
        assert!(filename.ends_with(".json"));
        assert_eq!(
            filename,
            "v1-f1877b65b83300e8ea777010f7712189be6590120c1822b2fa699c6598f69b33.json"
        );
    }

    #[test]
    fn observation_filename_components_have_unambiguous_boundaries() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let first = test_observation_key("a", "bc", "default");
        let second = test_observation_key("ab", "c", "default");

        assert_ne!(
            store.observation_path(&first),
            store.observation_path(&second)
        );
        Ok(())
    }

    #[test]
    fn long_observation_keys_round_trip_with_bounded_filenames() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let state = test_observation("server", &"instance".repeat(512), AgentStatus::Working, 100);
        let path = store.observation_path(&state.key);

        assert_eq!(
            path.file_name()
                .and_then(|name| name.to_str())
                .map(str::len),
            Some(72)
        );
        store.upsert_observation(&state)?;
        assert_eq!(store.list_observations()?, vec![state.clone()]);
        store.delete_observation(&state.key)?;
        assert!(store.list_observations()?.is_empty());
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
    fn old_reversible_observation_filenames_are_pruned() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let state = test_observation("server", "default", AgentStatus::Working, 100);
        let non_canonical_path = store
            .observations_dir()
            .join("6f70656e636f6465__7365735f726f6f74__736572766572__64656661756c74.json");
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
    fn interrupted_atomic_write_temporary_files_are_ignored() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let temporary_path = store
            .observations_dir()
            .join("observation.json.123.456.tmp");
        fs::write(&temporary_path, "partial json")?;

        assert!(store.list_observations()?.is_empty());
        assert!(temporary_path.exists());
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
    fn delete_sessions_removes_selected_sessions_and_prunes_stale_files() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::with_path(temp.path().join("state"))?;
        let first_session = test_session_key("opencode", "ses_project_alpha");
        let second_session = test_session_key("codex", "ses_project_beta");
        let unrelated_session = test_session_key("opencode", "ses_project_gamma");

        let mut first_tui = test_observation("tui", "default/%1", AgentStatus::Working, 100);
        first_tui.key.session.clone_from(&first_session);
        let mut first_server =
            test_observation("server", "http://127.0.0.1:4096", AgentStatus::Waiting, 101);
        first_server.key.session.clone_from(&first_session);
        let mut second_server =
            test_observation("server", "http://127.0.0.1:4097", AgentStatus::Done, 102);
        second_server.key.session.clone_from(&second_session);
        let mut unrelated = test_observation("server", "default", AgentStatus::Working, 103);
        unrelated.key.session.clone_from(&unrelated_session);

        for observation in [&first_tui, &first_server, &second_server, &unrelated] {
            store.upsert_observation(observation)?;
        }
        let corrupt_path = store.observations_dir().join("corrupt.json");
        fs::write(&corrupt_path, "not json")?;
        let noncanonical_path = store.observations_dir().join("old-format.json");
        fs::write(
            &noncanonical_path,
            serde_json::to_vec_pretty(&first_server)?,
        )?;
        store.delete_sessions(&[
            second_session,
            first_session.clone(),
            test_session_key("opencode", "ses_missing"),
            first_session,
        ])?;

        assert!(!corrupt_path.exists());
        assert!(!noncanonical_path.exists());
        assert_eq!(store.list_observations()?, vec![unrelated]);
        Ok(())
    }

    #[test]
    fn concurrent_mutations_see_the_latest_committed_observation() -> Result<()> {
        let temp = TempDir::new()?;
        let store = Arc::new(StateStore::with_path(temp.path().join("state"))?);
        let initial = test_observation("tui", "default/%1", AgentStatus::Working, 100);
        let key = initial.key.clone();
        store.upsert_observation(&initial)?;

        let (first_entered_tx, first_entered_rx) = mpsc::channel();
        let (release_first_tx, release_first_rx) = mpsc::channel();
        let first_store = Arc::clone(&store);
        let first_key = key.clone();
        let first = thread::spawn(move || {
            first_store.mutate_observation(&first_key, move |previous| {
                first_entered_tx.send(())?;
                release_first_rx.recv_timeout(Duration::from_secs(2))?;
                let mut state = previous
                    .ok_or_else(|| anyhow::anyhow!("first mutation expected prior state"))?;
                state.title = Some("First mutation".to_owned());
                Ok(Some(state))
            })
        });
        first_entered_rx.recv_timeout(Duration::from_secs(2))?;

        let competing_lock = OpenOptions::new()
            .read(true)
            .write(true)
            .open(store.observation_lock_path())?;
        assert!(matches!(
            competing_lock.try_lock(),
            Err(fs::TryLockError::WouldBlock)
        ));

        let (second_entered_tx, second_entered_rx) = mpsc::channel();
        let second_store = Arc::clone(&store);
        let second_key = key.clone();
        let second = thread::spawn(move || {
            second_store.mutate_observation(&second_key, move |previous| {
                second_entered_tx.send(())?;
                let mut state = previous
                    .ok_or_else(|| anyhow::anyhow!("second mutation expected prior state"))?;
                ensure!(state.title.as_deref() == Some("First mutation"));
                state.context = Some("Second mutation".to_owned());
                Ok(Some(state))
            })
        });

        release_first_tx.send(())?;
        first
            .join()
            .map_err(|_| anyhow::anyhow!("first mutation thread panicked"))??;
        second_entered_rx.recv_timeout(Duration::from_secs(2))?;
        second
            .join()
            .map_err(|_| anyhow::anyhow!("second mutation thread panicked"))??;

        let state = store
            .get_observation(&key)?
            .ok_or_else(|| anyhow::anyhow!("expected committed observation"))?;
        assert_eq!(state.title.as_deref(), Some("First mutation"));
        assert_eq!(state.context.as_deref(), Some("Second mutation"));
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
            target: AgentLocationHints {
                git_branch: Some("feature".to_owned()),
                ..AgentLocationHints::default()
            },
        }
    }

    fn assert_persisted_observation_is_pruned(
        store: &StateStore,
        state: &AgentObservationState,
        persisted: &serde_json::Value,
    ) -> Result<()> {
        let path = store.observation_path(&state.key);
        fs::write(&path, serde_json::to_vec_pretty(persisted)?)?;

        assert!(store.list_observations()?.is_empty());
        assert!(!path.exists());
        Ok(())
    }

    fn test_observation_key(
        session_id: &str,
        producer_kind: &str,
        producer_instance: &str,
    ) -> AgentObservationKey {
        AgentObservationKey {
            session: test_session_key("opencode", session_id),
            producer_kind: producer_kind.to_owned(),
            producer_instance: producer_instance.to_owned(),
        }
    }

    fn test_session_key(agent_kind: &str, session_id: &str) -> AgentSessionKey {
        AgentSessionKey {
            agent_kind: agent_kind.to_owned(),
            session_id: session_id.to_owned(),
        }
    }
}
