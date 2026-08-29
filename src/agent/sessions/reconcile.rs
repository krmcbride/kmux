//! Persisted-observation reconciliation into resolved agent sessions.
//!
//! External reporters report a current directory for each logical session. This
//! module attaches those observations to Git worktree roots, chooses status and
//! location precedence, and assembles the resolved session model. Live tmux
//! navigation policy is delegated to the sibling target owner.

use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::Result;

use crate::paths::infer_repo_metadata_from_paths;
use crate::state::{AgentLocationHints, AgentObservationState, AgentSessionKey, StateStore};
use crate::telemetry;
use crate::tmux::{Tmux, TmuxPaneSnapshot};

use super::model::{
    AgentTmuxTarget, ResolvedAgentSession, ResolvedAgentTarget, ResolvedAgentWorkspace,
    activity_status_priority,
};
use super::tmux_target::resolve_live_tmux_target;
use crate::agent::workspace::{AgentWorkspaceAttachment, AgentWorkspaceResolver};

#[cfg(test)]
use super::model::AgentTmuxUnavailableReason;
#[cfg(test)]
use crate::state::AgentStatus;

/// Reconcile persisted observations into logical agent sessions with live tmux state.
///
/// Tmux snapshot failures are treated as an empty live snapshot set so status and
/// sidebar rendering remain available; telemetry records whether the snapshot
/// query succeeded.
pub fn resolved_agent_sessions(
    store: &StateStore,
    tmux: &Tmux,
) -> Result<Vec<ResolvedAgentSession>> {
    let result = telemetry::timed_result_event!(
        "resolved_agent_sessions",
        {},
        || {
            let tmux_instance = tmux.instance_id();
            let observations = store.list_observations()?;
            let observation_count = observations
                .iter()
                .filter(|observation| is_candidate_for_tmux_instance(observation, &tmux_instance))
                .count();
            if observation_count == 0 {
                return Ok(ResolvedSessionsTelemetry {
                    sessions: Vec::new(),
                    observations: 0,
                    panes: 0,
                    tmux_snapshot_ok: true,
                });
            }

            let (panes, tmux_snapshot_ok) =
                pane_snapshots_or_empty(tmux.list_pane_snapshots());
            let pane_count = panes.len();
            let mut workspace_resolver = AgentWorkspaceResolver::default();
            let sessions = reconcile_agent_sessions(
                observations,
                &panes,
                &tmux_instance,
                &mut workspace_resolver,
            );
            Ok(ResolvedSessionsTelemetry {
                sessions,
                observations: observation_count,
                panes: pane_count,
                tmux_snapshot_ok,
            })
        },
        ok |telemetry_result| {
            observations = telemetry_result.observations,
            panes = telemetry_result.panes,
            sessions = telemetry_result.sessions.len(),
            tmux_snapshot_ok = telemetry_result.tmux_snapshot_ok,
        },
    );

    result.map(|telemetry_result| telemetry_result.sessions)
}

fn pane_snapshots_or_empty(panes: Result<Vec<TmuxPaneSnapshot>>) -> (Vec<TmuxPaneSnapshot>, bool) {
    match panes {
        Ok(panes) => (panes, true),
        Err(_) => (Vec::new(), false),
    }
}

struct ResolvedSessionsTelemetry {
    sessions: Vec<ResolvedAgentSession>,
    observations: usize,
    panes: usize,
    tmux_snapshot_ok: bool,
}

#[derive(Debug, Clone)]
struct EnrichedObservation {
    state: AgentObservationState,
    workspace_attachment: Option<AgentWorkspaceAttachment>,
    resolved_target: Option<ResolvedObservationTarget>,
}

#[derive(Debug, Clone)]
struct ResolvedObservationTarget {
    target: ResolvedAgentTarget,
    tmux_target: AgentTmuxTarget,
}

trait AgentWorkspaceLookup {
    fn attachment_for_hints(
        &mut self,
        target: &AgentLocationHints,
    ) -> Option<AgentWorkspaceAttachment>;

    fn attachment_for_path(&mut self, path: &str) -> Option<AgentWorkspaceAttachment>;
}

impl AgentWorkspaceLookup for AgentWorkspaceResolver {
    fn attachment_for_hints(
        &mut self,
        target: &AgentLocationHints,
    ) -> Option<AgentWorkspaceAttachment> {
        AgentWorkspaceResolver::attachment_for_hints(self, target)
    }

    fn attachment_for_path(&mut self, path: &str) -> Option<AgentWorkspaceAttachment> {
        AgentWorkspaceResolver::attachment_for_path(self, path)
    }
}

// Ignore observations scoped to another tmux socket. Unscoped observations remain
// eligible because server-side reporters may not know the active tmux instance.
fn is_candidate_for_tmux_instance(observation: &AgentObservationState, instance_id: &str) -> bool {
    observation
        .target
        .tmux_instance
        .as_deref()
        .is_none_or(|target_instance| target_instance == instance_id)
}

// Pure session reconciliation policy over observation, tmux pane, and workspace
// attachment facts. Callers supply the attachment capability so tests can bypass
// concrete XDG state, Git discovery, and tmux subprocesses.
fn reconcile_agent_sessions(
    observations: Vec<AgentObservationState>,
    panes: &[TmuxPaneSnapshot],
    tmux_instance: &str,
    workspace_resolver: &mut impl AgentWorkspaceLookup,
) -> Vec<ResolvedAgentSession> {
    let mut grouped = BTreeMap::<AgentSessionKey, Vec<EnrichedObservation>>::new();
    for observation in observations {
        if !is_candidate_for_tmux_instance(&observation, tmux_instance) {
            continue;
        }
        let workspace_attachment = workspace_resolver.attachment_for_hints(&observation.target);
        let resolved_target = resolve_observation_target(
            &observation,
            workspace_attachment.as_ref(),
            panes,
            workspace_resolver,
        );
        grouped
            .entry(observation.key.session.clone())
            .or_default()
            .push(EnrichedObservation {
                state: observation,
                workspace_attachment,
                resolved_target,
            });
    }

    grouped
        .into_iter()
        .filter_map(|(key, observations)| resolved_session_from_observations(key, &observations))
        .collect()
}

// Choose one status observation and one location observation for a session, then
// merge newer display and location fields around that resolved target.
fn resolved_session_from_observations(
    key: AgentSessionKey,
    observations: &[EnrichedObservation],
) -> Option<ResolvedAgentSession> {
    let status_observation = observations
        .iter()
        .filter(|observation| observation.state.status.is_some())
        .max_by_key(|observation| {
            (
                observation_status_observed_at(&observation.state),
                observation
                    .state
                    .status
                    .map(activity_status_priority)
                    .unwrap_or(0),
                observation.state.observed_at,
            )
        })?;
    let location_observation = best_location_observation(observations)?;
    let resolved_target = location_observation.resolved_target.clone()?;
    let mut target = resolved_target.target;
    merge_target_metadata(&mut target, observations);
    enrich_missing_repo_metadata(&mut target);

    let status_changed_at = status_observation.state.status_changed_at?;
    let status_observed_at = observation_status_observed_at(&status_observation.state);
    let workspace = location_observation
        .workspace_attachment
        .as_ref()
        .and_then(resolved_workspace_from_attachment)?;
    Some(ResolvedAgentSession {
        key,
        workspace,
        tmux_target: resolved_target.tmux_target,
        created_at: observations
            .iter()
            .map(|observation| observation.state.created_at)
            .min()
            .unwrap_or(status_changed_at),
        status: status_observation.state.status?,
        status_observed_at,
        status_changed_at,
        working_elapsed_secs: status_observation.state.working_elapsed_secs,
        observed_at: observations
            .iter()
            .map(|observation| observation.state.observed_at)
            .max()
            .unwrap_or(status_changed_at),
        title: newest_value(observations, |observation| {
            observation.state.title.as_deref()
        }),
        context: newest_value(observations, |observation| {
            observation.state.context.as_deref()
        }),
        target,
    })
}

fn resolved_workspace_from_attachment(
    attachment: &AgentWorkspaceAttachment,
) -> Option<ResolvedAgentWorkspace> {
    ResolvedAgentWorkspace::from_canonical_root(
        PathBuf::from(attachment.path()),
        attachment.reported_path().to_owned(),
    )
    .ok()
}

fn best_location_observation(observations: &[EnrichedObservation]) -> Option<&EnrichedObservation> {
    let newest_observed_at = observations
        .iter()
        .map(|observation| observation.state.observed_at)
        .max()?;
    let latest_workspace_key = observations
        .iter()
        .filter(|observation| observation.state.observed_at == newest_observed_at)
        .find(|observation| observation.resolved_target.is_some())?
        .workspace_attachment
        .as_ref()?
        .key()
        .to_owned();

    observations
        .iter()
        .filter(|observation| observation.resolved_target.is_some())
        .filter(|observation| {
            observation
                .workspace_attachment
                .as_ref()
                .is_some_and(|attachment| attachment.key() == latest_workspace_key)
        })
        .max_by_key(|observation| {
            (
                observation_location_precision(observation),
                observation.state.observed_at,
            )
        })
}

fn observation_location_precision(observation: &EnrichedObservation) -> u8 {
    let Some(resolved) = &observation.resolved_target else {
        return 0;
    };
    match &resolved.tmux_target {
        AgentTmuxTarget::Windows { .. } if resolved.target.tmux_pane_id.is_some() => 4,
        AgentTmuxTarget::Windows { .. } => 3,
        AgentTmuxTarget::Unavailable(_) => 1,
    }
}

fn observation_status_observed_at(observation: &AgentObservationState) -> u64 {
    observation
        .status_observed_at
        .or(observation.status_changed_at)
        .unwrap_or(observation.observed_at)
}

fn newest_value(
    observations: &[EnrichedObservation],
    value: impl Fn(&EnrichedObservation) -> Option<&str>,
) -> Option<String> {
    observations
        .iter()
        .filter_map(|observation| {
            value(observation).map(|value| (observation.state.observed_at, value.to_owned()))
        })
        .max_by_key(|(observed_at, _)| *observed_at)
        .map(|(_, value)| value)
}

// An observation can participate in workspace activity when its reported
// directory resolves to a Git worktree root. Live tmux facts then provide exact
// navigation candidates or an explicit unavailable result.
fn resolve_observation_target(
    observation: &AgentObservationState,
    workspace_attachment: Option<&AgentWorkspaceAttachment>,
    panes: &[TmuxPaneSnapshot],
    workspace_resolver: &mut impl AgentWorkspaceLookup,
) -> Option<ResolvedObservationTarget> {
    let attachment = workspace_attachment?;
    let mut target = ResolvedAgentTarget::default();
    apply_workspace_attachment(&mut target, attachment);
    let tmux_target = resolve_live_tmux_target(&mut target, attachment, panes, |path| {
        workspace_resolver.attachment_for_path(path)
    });
    merge_resolved_observation_metadata(&mut target, &observation.target);
    enrich_missing_repo_metadata(&mut target);
    Some(ResolvedObservationTarget {
        target,
        tmux_target,
    })
}

fn apply_workspace_attachment(
    target: &mut ResolvedAgentTarget,
    attachment: &AgentWorkspaceAttachment,
) {
    if target.directory.is_none() {
        target.directory = Some(attachment.reported_path().to_owned());
    }
}

// Merge newest display/routing metadata first. Live tmux target fields come only
// from the matched kmux window, not from reporter hints.
fn merge_target_metadata(target: &mut ResolvedAgentTarget, observations: &[EnrichedObservation]) {
    let mut sorted = observations.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|observation| observation.state.observed_at);
    for observation in sorted.into_iter().rev() {
        merge_resolved_observation_metadata(target, &observation.state.target);
    }
}

fn merge_resolved_observation_metadata(
    target: &mut ResolvedAgentTarget,
    fallback: &AgentLocationHints,
) {
    if target.git_repo_name.is_none() {
        target.git_repo_name = fallback.git_repo_name.clone();
    }
    if target.git_repo_path.is_none() {
        target.git_repo_path = fallback.git_repo_path.clone();
    }
    if target.git_branch.is_none() {
        target.git_branch = fallback.git_branch.clone();
    }
    if target.directory.is_none() {
        target.directory = fallback.directory.clone();
    }
}

// Repo metadata can be recovered from any live path hint when agents did not
// report it directly.
fn enrich_missing_repo_metadata(target: &mut ResolvedAgentTarget) {
    if target.git_repo_name.is_some()
        && target.git_repo_path.is_some()
        && target.git_branch.is_some()
    {
        return;
    }

    let metadata = infer_repo_metadata_from_paths(&[target.directory.as_deref()]);
    if target.git_repo_name.is_none() {
        target.git_repo_name = metadata.repo_name;
    }
    if target.git_repo_path.is_none() {
        target.git_repo_path = metadata.repo_path;
    }
    if target.git_branch.is_none() {
        target.git_branch = metadata.branch;
    }
}

#[cfg(test)]
mod tests;
