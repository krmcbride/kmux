use super::model::{AgentObservationState, AgentStatus};

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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AgentLocationHints, AgentObservationKey, AgentSessionKey};

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
            key: AgentObservationKey {
                session: AgentSessionKey {
                    agent_kind: "opencode".to_owned(),
                    session_id: "ses_root".to_owned(),
                },
                producer_kind: producer_kind.to_owned(),
                producer_instance: producer_instance.to_owned(),
            },
            created_at: status_changed_at,
            status: Some(status),
            status_observed_at: Some(status_changed_at),
            status_changed_at: Some(status_changed_at),
            working_elapsed_secs: 0,
            observed_at: status_changed_at,
            title: None,
            context: None,
            target: AgentLocationHints {
                kmux_worktree_handle: Some("feature".to_owned()),
                git_worktree_path: Some("/repo__worktrees/feature".to_owned()),
                git_branch: Some("feature".to_owned()),
                ..AgentLocationHints::default()
            },
        }
    }
}
