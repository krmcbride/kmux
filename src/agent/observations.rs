//! Application service for mutating persisted agent observations.
//!
//! CLI workflows translate integration flags into these command shapes, while
//! this module owns state mutation, timing policy, and target hint merging.

use anyhow::Result;

use crate::paths::{infer_repo_metadata_from_paths, path_basename};
use crate::state::{
    AgentLocationHints, AgentObservationKey, AgentObservationState, AgentSessionKey, AgentStatus,
    StateStore, next_observation_timing, now_unix_seconds,
};

/// Persistence command to apply to the agent observation store.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ObservationCommand {
    DeleteSession(AgentSessionKey),
    DeleteObservation(AgentObservationKey),
    Upsert(Box<ObservationUpdate>),
}

/// Sanitized application input for recording one producer observation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationUpdate {
    pub key: AgentObservationKey,
    pub status: Option<AgentStatus>,
    pub title: Option<String>,
    pub context: Option<String>,
    pub target: LocationUpdate,
}

/// Sanitized location update reported by an external producer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct LocationUpdate {
    pub tmux_instance: Option<String>,
    pub git_repo_name: Option<String>,
    pub git_repo_path: Option<String>,
    pub git_branch: Option<String>,
    pub directory: Option<String>,
}

/// Apply one observation command to the store using the current wall clock time.
pub fn apply_observation_command(store: &StateStore, command: ObservationCommand) -> Result<()> {
    apply_observation_command_at(store, command, now_unix_seconds())
}

fn apply_observation_command_at(
    store: &StateStore,
    command: ObservationCommand,
    now: u64,
) -> Result<()> {
    match command {
        ObservationCommand::DeleteSession(session) => store.delete_session(&session)?,
        ObservationCommand::DeleteObservation(key) => store.delete_observation(&key)?,
        ObservationCommand::Upsert(update) => upsert_observation(store, *update, now)?,
    }

    Ok(())
}

fn upsert_observation(store: &StateStore, update: ObservationUpdate, now: u64) -> Result<()> {
    let key = update.key.clone();
    let state_key = key.clone();
    let inferred_repo = infer_repo_metadata_from_paths(&[update.target.directory.as_deref()]);
    store.mutate_observation(&key, move |previous| {
        let status_supplied = update.status.is_some();
        let timing = next_observation_timing(previous.as_ref(), update.status, now);
        let mut state = previous.unwrap_or_else(|| AgentObservationState {
            key: state_key.clone(),
            created_at: now,
            status: None,
            status_observed_at: None,
            status_changed_at: None,
            working_elapsed_secs: 0,
            observed_at: now,
            title: None,
            context: None,
            target: AgentLocationHints::default(),
        });

        state.key = state_key;
        if status_supplied {
            state.status = update.status;
            state.status_observed_at = Some(now);
        }
        state.status_changed_at = timing.status_changed_at;
        state.working_elapsed_secs = timing.working_elapsed_secs;
        state.observed_at = now;
        if let Some(title) = update.title {
            state.title = Some(title);
        }
        if let Some(context) = update.context {
            state.context = Some(context);
        }
        update.target.apply_to(&mut state.target);
        clear_obsolete_producer_hints(&mut state.target);
        apply_inferred_repo_metadata(&mut state.target, inferred_repo);

        Ok(Some(state))
    })
}

impl LocationUpdate {
    fn apply_to(self, target: &mut AgentLocationHints) {
        apply_optional(&mut target.tmux_instance, self.tmux_instance);
        apply_optional(&mut target.git_repo_name, self.git_repo_name);
        apply_optional(&mut target.git_repo_path, self.git_repo_path);
        apply_optional(&mut target.git_branch, self.git_branch);

        // Directory is the producer's current location, so omitted or blank values
        // replace the previous directory rather than preserving stale routing data.
        target.directory = self.directory;
    }
}

// These fields used to be producer inputs. New reports should not preserve old
// local values that could affect resolved tmux focus or derived Git roots.
fn clear_obsolete_producer_hints(target: &mut AgentLocationHints) {
    target.tmux_pane_id = None;
    target.tmux_window_id = None;
    target.git_worktree_path = None;
}

// Partial updates should not erase existing fields with omitted or blank strings.
fn apply_optional(target: &mut Option<String>, value: Option<String>) {
    if let Some(value) = value {
        *target = Some(value);
    }
}

// Fill missing repo fields opportunistically from path hints so sparse producer
// reports still show useful repo/branch labels.
fn apply_inferred_repo_metadata(
    target: &mut AgentLocationHints,
    metadata: crate::paths::RepoMetadata,
) {
    if target.git_repo_path.is_none() {
        target.git_repo_path = metadata.repo_path.clone();
    }
    if target.git_repo_name.is_none() {
        target.git_repo_name = target
            .git_repo_path
            .as_deref()
            .and_then(path_basename)
            .or(metadata.repo_name);
    }
    if target.git_branch.is_none() {
        target.git_branch = metadata.branch;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::test_support::store_with_path;
    use tempfile::TempDir;

    #[test]
    fn upsert_creates_observation_with_status_and_display_fields() -> Result<()> {
        let temp = TempDir::new()?;
        let store = store_with_path(temp.path().join("state"))?;
        let key = observation_key("ses_root", "server", "default");
        let command = ObservationCommand::Upsert(Box::new(ObservationUpdate {
            key: key.clone(),
            status: Some(AgentStatus::Working),
            title: Some("Build feature".to_owned()),
            context: Some("12K".to_owned()),
            target: LocationUpdate {
                directory: Some("/repo/project".to_owned()),
                git_branch: Some("main".to_owned()),
                ..LocationUpdate::default()
            },
        }));

        apply_observation_command_at(&store, command, 100)?;
        let observation = store
            .get_observation(&key)?
            .ok_or_else(|| anyhow::anyhow!("expected observation to be stored"))?;
        assert_eq!(observation.status, Some(AgentStatus::Working));
        assert_eq!(observation.status_observed_at, Some(100));
        assert_eq!(observation.status_changed_at, Some(100));
        assert_eq!(observation.title.as_deref(), Some("Build feature"));
        assert_eq!(observation.context.as_deref(), Some("12K"));
        assert_eq!(
            observation.target.directory.as_deref(),
            Some("/repo/project")
        );
        assert_eq!(observation.target.git_branch.as_deref(), Some("main"));
        Ok(())
    }

    #[test]
    fn statusless_update_preserves_status_timing_and_replaces_directory() -> Result<()> {
        let temp = TempDir::new()?;
        let store = store_with_path(temp.path().join("state"))?;
        let key = observation_key("ses_root", "server", "default");
        apply_observation_command_at(
            &store,
            ObservationCommand::Upsert(Box::new(ObservationUpdate {
                key: key.clone(),
                status: Some(AgentStatus::Working),
                title: Some("Initial".to_owned()),
                context: None,
                target: LocationUpdate {
                    directory: Some("/repo/project".to_owned()),
                    ..LocationUpdate::default()
                },
            })),
            100,
        )?;

        apply_observation_command_at(
            &store,
            ObservationCommand::Upsert(Box::new(ObservationUpdate {
                key: key.clone(),
                status: None,
                title: Some("Renamed".to_owned()),
                context: Some("context".to_owned()),
                target: LocationUpdate::default(),
            })),
            150,
        )?;

        let observation = store
            .get_observation(&key)?
            .ok_or_else(|| anyhow::anyhow!("expected observation to be stored"))?;
        assert_eq!(observation.status, Some(AgentStatus::Working));
        assert_eq!(observation.status_observed_at, Some(100));
        assert_eq!(observation.status_changed_at, Some(100));
        assert_eq!(observation.observed_at, 150);
        assert_eq!(observation.title.as_deref(), Some("Renamed"));
        assert_eq!(observation.context.as_deref(), Some("context"));
        assert_eq!(observation.target.directory, None);
        Ok(())
    }

    #[test]
    fn upsert_clears_obsolete_producer_target_hints() -> Result<()> {
        let temp = TempDir::new()?;
        let store = store_with_path(temp.path().join("state"))?;
        let key = observation_key("ses_root", "server", "default");
        let mut previous = AgentObservationState {
            key: key.clone(),
            created_at: 100,
            status: Some(AgentStatus::Working),
            status_observed_at: Some(100),
            status_changed_at: Some(100),
            working_elapsed_secs: 0,
            observed_at: 100,
            title: None,
            context: None,
            target: AgentLocationHints {
                tmux_pane_id: Some("%old".to_owned()),
                tmux_window_id: Some("@old".to_owned()),
                git_worktree_path: Some("/repo/old".to_owned()),
                directory: Some("/repo/old".to_owned()),
                ..AgentLocationHints::default()
            },
        };
        store.upsert_observation(&previous)?;

        apply_observation_command_at(
            &store,
            ObservationCommand::Upsert(Box::new(ObservationUpdate {
                key: key.clone(),
                status: Some(AgentStatus::Working),
                title: None,
                context: None,
                target: LocationUpdate {
                    directory: Some("/repo/new".to_owned()),
                    ..LocationUpdate::default()
                },
            })),
            120,
        )?;

        previous = store
            .get_observation(&key)?
            .ok_or_else(|| anyhow::anyhow!("expected observation to be stored"))?;
        assert_eq!(previous.target.tmux_pane_id, None);
        assert_eq!(previous.target.tmux_window_id, None);
        assert_eq!(previous.target.git_worktree_path, None);
        assert_eq!(previous.target.directory.as_deref(), Some("/repo/new"));
        Ok(())
    }

    #[test]
    fn delete_observation_removes_only_matching_producer() -> Result<()> {
        let temp = TempDir::new()?;
        let store = store_with_path(temp.path().join("state"))?;
        let server = observation_key("ses_root", "server", "default");
        let tui = observation_key("ses_root", "tui", "default/%1");
        upsert_test_observation(&store, server.clone(), 100)?;
        upsert_test_observation(&store, tui.clone(), 100)?;

        apply_observation_command_at(
            &store,
            ObservationCommand::DeleteObservation(server.clone()),
            120,
        )?;

        assert_eq!(store.get_observation(&server)?, None);
        assert!(store.get_observation(&tui)?.is_some());
        Ok(())
    }

    #[test]
    fn delete_session_removes_all_producer_observations() -> Result<()> {
        let temp = TempDir::new()?;
        let store = store_with_path(temp.path().join("state"))?;
        let server = observation_key("ses_root", "server", "default");
        let tui = observation_key("ses_root", "tui", "default/%1");
        let session = server.session.clone();
        upsert_test_observation(&store, server, 100)?;
        upsert_test_observation(&store, tui, 100)?;

        apply_observation_command_at(&store, ObservationCommand::DeleteSession(session), 120)?;

        assert!(store.list_observations()?.is_empty());
        Ok(())
    }

    fn upsert_test_observation(
        store: &StateStore,
        key: AgentObservationKey,
        now: u64,
    ) -> Result<()> {
        apply_observation_command_at(
            store,
            ObservationCommand::Upsert(Box::new(ObservationUpdate {
                key,
                status: Some(AgentStatus::Working),
                title: None,
                context: None,
                target: LocationUpdate::default(),
            })),
            now,
        )?;
        Ok(())
    }

    fn observation_key(
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
