//! Reconciliation of persisted agent observations with live tmux state.
//!
//! External reporters report a current directory for each logical session. This
//! module attaches those observations to Git worktree roots, then derives the
//! ordered live tmux navigation candidates or an explicit unavailability reason.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use anyhow::Result;

use crate::paths::infer_repo_metadata_from_paths;
use crate::state::{
    AgentLocationHints, AgentObservationState, AgentSessionKey, AgentStatus, StateStore,
};
use crate::telemetry;
use crate::tmux::{Tmux, TmuxPaneSnapshot};
use crate::workspace::WorkspaceIdentity;

use super::workspace::{AgentWorkspaceAttachment, AgentWorkspaceResolver};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Resolved workspace identity associated with an agent session.
pub struct ResolvedAgentWorkspace {
    key: String,
    path: String,
    reported_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One logical agent session reconciled from all of its reporter observations.
pub struct ResolvedAgentSession {
    pub key: AgentSessionKey,
    pub workspace: ResolvedAgentWorkspace,
    pub tmux_target: AgentTmuxTarget,
    pub created_at: u64,
    pub status: AgentStatus,
    pub status_observed_at: u64,
    pub status_changed_at: u64,
    pub working_elapsed_secs: u64,
    pub observed_at: u64,
    pub title: Option<String>,
    pub context: Option<String>,
    pub target: ResolvedAgentTarget,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Resolved navigation, display, and workspace facts for an agent session.
pub struct ResolvedAgentTarget {
    pub tmux_pane_id: Option<String>,
    pub tmux_window_id: Option<String>,
    pub tmux_session_name: Option<String>,
    pub tmux_window_name: Option<String>,
    pub tmux_pane_title: Option<String>,
    pub tmux_pane_current_command: Option<String>,
    pub git_repo_name: Option<String>,
    pub git_repo_path: Option<String>,
    pub git_branch: Option<String>,
    pub directory: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Live tmux navigation candidates or a focused unavailability reason.
pub enum AgentTmuxTarget {
    Windows {
        session_name: String,
        candidates: Vec<AgentTmuxWindowCandidate>,
    },
    Unavailable(AgentTmuxUnavailableReason),
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One matching physical window and its preferred matching pane order.
pub struct AgentTmuxWindowCandidate {
    pub window_id: String,
    pub pane_ids: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Reason a workspace activity row cannot currently be routed through tmux.
pub enum AgentTmuxUnavailableReason {
    Missing,
    CrossSession { session_names: Vec<String> },
}

/// Return the shared application priority for an agent activity status.
///
/// Workspace primary-session selection and defensive same-window badge collapse
/// both use this ordering so presentation surfaces cannot disagree about which
/// status is most important.
pub fn activity_status_priority(status: AgentStatus) -> u8 {
    match status {
        AgentStatus::Waiting => 3,
        AgentStatus::Working => 2,
        AgentStatus::Done => 1,
    }
}

impl ResolvedAgentWorkspace {
    /// Build a resolved workspace from a canonical Git worktree root.
    pub fn from_canonical_root(
        canonical_worktree_root: PathBuf,
        reported_path: String,
    ) -> Result<Self> {
        let identity = WorkspaceIdentity::from_canonical_root(canonical_worktree_root)?;
        let path = identity.root().display().to_string();
        Ok(Self {
            key: path.clone(),
            path,
            reported_path,
        })
    }

    /// Return the stable grouping key for this workspace.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Return the canonical Git worktree root as display text.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Return the path originally reported by the reporter before Git-root resolution.
    pub fn reported_path(&self) -> &str {
        &self.reported_path
    }

    fn from_attachment(attachment: &AgentWorkspaceAttachment) -> Option<Self> {
        Self::from_canonical_root(
            PathBuf::from(attachment.path()),
            attachment.reported_path().to_owned(),
        )
        .ok()
    }
}

impl ResolvedAgentSession {
    /// Return elapsed time for the current status at `now`.
    ///
    /// Working rows accumulate prior working time plus the active working span;
    /// waiting and done rows show time since their current status began.
    pub fn elapsed_secs(&self, now: u64) -> u64 {
        let status_age = now.saturating_sub(self.status_changed_at);
        match self.status {
            AgentStatus::Working => self.working_elapsed_secs.saturating_add(status_age),
            AgentStatus::Waiting | AgentStatus::Done => status_age,
        }
    }

    /// Return the canonical workspace grouping key.
    pub fn workspace_key(&self) -> &str {
        self.workspace.key()
    }

    /// Return the canonical Git worktree path.
    pub fn workspace_path(&self) -> &str {
        self.workspace.path()
    }

    /// Return the best known Git repo name for display.
    pub fn git_repo_name(&self) -> Option<&str> {
        self.target.git_repo_name.as_deref()
    }

    /// Return the best known main Git repository path for display.
    pub fn git_repo_path(&self) -> Option<&str> {
        self.target.git_repo_path.as_deref()
    }

    /// Return the resolved Git worktree path used for matching and display.
    pub fn git_worktree_path(&self) -> &str {
        self.workspace_path()
    }

    /// Return the best known Git branch name for display and filtering.
    pub fn git_branch(&self) -> Option<&str> {
        self.target.git_branch.as_deref()
    }

    /// Return the latest reporter-provided directory, if one was provided.
    pub fn directory(&self) -> Option<&str> {
        self.target
            .directory
            .as_deref()
            .or_else(|| Some(self.workspace.reported_path()))
    }

    /// Return the resolved tmux window id for navigation.
    pub fn tmux_window_id(&self) -> Option<&str> {
        self.target.tmux_window_id.as_deref()
    }

    /// Return the resolved tmux window name for display.
    pub fn tmux_window_name(&self) -> Option<&str> {
        self.target.tmux_window_name.as_deref()
    }

    /// Return the exact tmux session selected by reconciliation.
    pub fn tmux_session_name(&self) -> Option<&str> {
        self.target.tmux_session_name.as_deref()
    }

    /// Return the preferred matching non-sidebar pane ID.
    pub fn tmux_pane_id(&self) -> Option<&str> {
        self.target.tmux_pane_id.as_deref()
    }

    /// Return the live tmux pane title captured for display fallback.
    pub fn tmux_pane_title(&self) -> Option<&str> {
        self.target.tmux_pane_title.as_deref()
    }

    /// Return the live tmux pane command captured for display fallback.
    pub fn tmux_pane_current_command(&self) -> Option<&str> {
        self.target.tmux_pane_current_command.as_deref()
    }
}

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

            let panes_result = tmux.list_pane_snapshots();
            let tmux_snapshot_ok = panes_result.is_ok();
            let panes = panes_result.unwrap_or_default();
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

// Group observations by logical agent session after assigning each reported
// directory to a Git worktree root.
#[cfg(test)]
fn reconcile_resolved_sessions(
    observations: Vec<AgentObservationState>,
    panes: &[TmuxPaneSnapshot],
    tmux_instance: &str,
) -> Vec<ResolvedAgentSession> {
    let mut workspace_resolver = AgentWorkspaceResolver::default();
    reconcile_agent_sessions(observations, panes, tmux_instance, &mut workspace_resolver)
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
        .and_then(ResolvedAgentWorkspace::from_attachment)?;
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
    let tmux_target =
        enrich_target_from_live_tmux(&mut target, attachment, panes, workspace_resolver);
    merge_resolved_observation_metadata(&mut target, &observation.target);
    enrich_missing_repo_metadata(&mut target);
    Some(ResolvedObservationTarget {
        target,
        tmux_target,
    })
}

/// Derive the complete jump policy from canonical workspace matches and one tmux snapshot.
///
/// Physical windows are deduplicated before routing because linked windows appear once per
/// owning session. A jump is available only when exactly one session owns every matching
/// physical window. Within that session, windows are ordered by current match, previous match,
/// then parsed index and stable ID. Matching non-sidebar panes use the same active, previous,
/// index, and stable-ID preference. Sidebar actions preserve this order and only revalidate
/// which candidates remain live; they do not repeat Git resolution or choose a broader target.
/// Missing matches and cross-session ownership remain explicit unavailable results so callers
/// cannot accidentally fall back to an unrelated active window.
fn enrich_target_from_live_tmux(
    target: &mut ResolvedAgentTarget,
    attachment: &AgentWorkspaceAttachment,
    panes: &[TmuxPaneSnapshot],
    workspace_resolver: &mut impl AgentWorkspaceLookup,
) -> AgentTmuxTarget {
    let matches = window_workspace_matches(attachment, panes, workspace_resolver);
    if matches.is_empty() {
        return AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::Missing);
    }

    let mut common_sessions = matches[0].sessions.keys().cloned().collect::<BTreeSet<_>>();
    for window in &matches[1..] {
        common_sessions.retain(|session_name| window.sessions.contains_key(session_name));
    }
    if common_sessions.len() != 1 {
        return cross_session_target(&matches);
    }
    let Some(session_name) = common_sessions.pop_first() else {
        return cross_session_target(&matches);
    };
    let mut ordered_windows = matches
        .iter()
        .filter_map(|window| {
            window
                .sessions
                .get(&session_name)
                .map(|session| (window, session))
        })
        .collect::<Vec<_>>();
    if ordered_windows.len() != matches.len() {
        return cross_session_target(&matches);
    }
    ordered_windows.sort_by_key(|(window, session)| window_sort_key(window, session));

    let mut candidates = Vec::with_capacity(ordered_windows.len());
    for (index, (window, session)) in ordered_windows.into_iter().enumerate() {
        let mut panes = window.matching_panes.iter().collect::<Vec<_>>();
        panes.sort_by_key(|pane| pane_sort_key(pane));
        if index == 0 {
            enrich_target_from_window_match(target, window, session, &session_name, panes[0]);
        }
        candidates.push(AgentTmuxWindowCandidate {
            window_id: window.window_id.clone(),
            pane_ids: panes.into_iter().map(|pane| pane.pane_id.clone()).collect(),
        });
    }

    AgentTmuxTarget::Windows {
        session_name,
        candidates,
    }
}

fn cross_session_target(matches: &[WindowWorkspaceMatch]) -> AgentTmuxTarget {
    let session_names = matches
        .iter()
        .flat_map(|window| window.sessions.keys().cloned())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::CrossSession { session_names })
}

#[derive(Debug, Clone)]
struct WindowWorkspaceMatch {
    window_id: String,
    sessions: BTreeMap<String, WindowSessionMatch>,
    matching_panes: Vec<TmuxPaneSnapshot>,
}

#[derive(Debug, Clone)]
struct WindowWorkspaceAccumulator {
    window_id: String,
    sessions: BTreeMap<String, WindowSessionMatch>,
    matching_panes: BTreeMap<String, TmuxPaneSnapshot>,
}

#[derive(Debug, Clone)]
struct WindowSessionMatch {
    window_index: String,
    window_name: String,
    active: bool,
    last: bool,
}

fn window_workspace_matches(
    attachment: &AgentWorkspaceAttachment,
    panes: &[TmuxPaneSnapshot],
    workspace_resolver: &mut impl AgentWorkspaceLookup,
) -> Vec<WindowWorkspaceMatch> {
    let mut windows = BTreeMap::<String, WindowWorkspaceAccumulator>::new();
    for pane in panes
        .iter()
        .filter(|pane| pane.kmux_role.as_deref() != Some("sidebar"))
    {
        let Some(workspace) = pane
            .current_path
            .as_deref()
            .and_then(|path| workspace_resolver.attachment_for_path(path))
        else {
            continue;
        };
        let entry =
            windows
                .entry(pane.window_id.clone())
                .or_insert_with(|| WindowWorkspaceAccumulator {
                    window_id: pane.window_id.clone(),
                    sessions: BTreeMap::new(),
                    matching_panes: BTreeMap::new(),
                });
        if workspace.key() == attachment.key() {
            entry
                .sessions
                .entry(pane.session_name.clone())
                .or_insert_with(|| WindowSessionMatch {
                    window_index: pane.window_index.clone(),
                    window_name: pane.window_name.clone(),
                    active: pane.window_active,
                    last: pane.window_last,
                });
            entry
                .matching_panes
                .entry(pane.pane_id.clone())
                .or_insert_with(|| pane.clone());
        }
    }

    windows
        .into_values()
        .filter(|window| !window.matching_panes.is_empty())
        .map(|window| WindowWorkspaceMatch {
            window_id: window.window_id,
            sessions: window.sessions,
            matching_panes: window.matching_panes.into_values().collect(),
        })
        .collect()
}

fn window_sort_key<'a>(
    window: &'a WindowWorkspaceMatch,
    session: &WindowSessionMatch,
) -> (u8, u64, &'a str) {
    let preference = if session.active {
        0
    } else if session.last {
        1
    } else {
        2
    };
    (
        preference,
        session.window_index.parse().unwrap_or(u64::MAX),
        &window.window_id,
    )
}

fn pane_sort_key(pane: &TmuxPaneSnapshot) -> (u8, u64, &str) {
    let preference = if pane.pane_active {
        0
    } else if pane.pane_last {
        1
    } else {
        2
    };
    (
        preference,
        pane.pane_index.parse().unwrap_or(u64::MAX),
        &pane.pane_id,
    )
}

fn apply_workspace_attachment(
    target: &mut ResolvedAgentTarget,
    attachment: &AgentWorkspaceAttachment,
) {
    if target.directory.is_none() {
        target.directory = Some(attachment.reported_path().to_owned());
    }
}

fn enrich_target_from_window_match(
    target: &mut ResolvedAgentTarget,
    window: &WindowWorkspaceMatch,
    session: &WindowSessionMatch,
    session_name: &str,
    pane: &TmuxPaneSnapshot,
) {
    target.tmux_session_name = Some(session_name.to_owned());
    target.tmux_window_id = Some(window.window_id.clone());
    target.tmux_window_name = Some(session.window_name.clone());
    target.tmux_pane_id = Some(pane.pane_id.clone());
    target.tmux_pane_title = pane.title.clone();
    target.tmux_pane_current_command = pane.current_command.clone();
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
mod tests {
    use super::*;
    use crate::agent::workspace_activity::workspace_activities_from_sessions;
    use crate::git::test_support::GitRepoFixture;
    use crate::state::{AgentObservationKey, AgentObservationState};
    use std::collections::BTreeMap;

    #[derive(Default)]
    struct FakeWorkspaceResolver {
        attachments: BTreeMap<String, AgentWorkspaceAttachment>,
    }

    impl FakeWorkspaceResolver {
        fn with_path(path: &str) -> Self {
            Self {
                attachments: [(path.to_owned(), AgentWorkspaceAttachment::for_test(path))]
                    .into_iter()
                    .collect(),
            }
        }
    }

    impl AgentWorkspaceLookup for FakeWorkspaceResolver {
        fn attachment_for_hints(
            &mut self,
            target: &AgentLocationHints,
        ) -> Option<AgentWorkspaceAttachment> {
            target
                .directory
                .as_deref()
                .and_then(|path| self.attachment_for_path(path))
        }

        fn attachment_for_path(&mut self, path: &str) -> Option<AgentWorkspaceAttachment> {
            self.attachments.get(path).cloned()
        }
    }

    #[test]
    fn pure_reconciliation_returns_no_sessions_without_observations() {
        let mut resolver = FakeWorkspaceResolver::with_path("/repo/project");

        let views = reconcile_agent_sessions(Vec::new(), &[], "default", &mut resolver);

        assert!(views.is_empty());
    }

    #[test]
    fn pure_reconciliation_ignores_observations_for_other_tmux_instances() {
        let mut observation = observation(
            "server",
            "server",
            Some(AgentStatus::Working),
            100,
            Some("Wrong tmux"),
            "/repo/project",
        );
        observation.target.tmux_instance = Some("other".to_owned());
        let mut resolver = FakeWorkspaceResolver::with_path("/repo/project");

        let views = reconcile_agent_sessions(vec![observation], &[], "default", &mut resolver);

        assert!(views.is_empty());
    }

    #[test]
    fn pure_reconciliation_builds_resolved_workspace_and_live_window_target() {
        let observation = observation(
            "server",
            "server",
            Some(AgentStatus::Working),
            100,
            Some("Resolved"),
            "/repo/project",
        );
        let mut resolver = FakeWorkspaceResolver::with_path("/repo/project");

        let views = reconcile_agent_sessions(
            vec![observation],
            &[pane_snapshot("%1", "@1", "/repo/project", None)],
            "default",
            &mut resolver,
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].workspace_key(), "/repo/project");
        assert_eq!(views[0].workspace_path(), "/repo/project");
        assert!(matches!(
            views[0].tmux_target,
            AgentTmuxTarget::Windows { .. }
        ));
        assert_eq!(views[0].tmux_window_id(), Some("@1"));
    }

    #[test]
    fn pure_reconciliation_uses_no_tmux_target_without_pane_snapshots() {
        let observation = observation(
            "server",
            "server",
            Some(AgentStatus::Working),
            100,
            Some("No panes"),
            "/repo/project",
        );
        let mut resolver = FakeWorkspaceResolver::with_path("/repo/project");

        let views = reconcile_agent_sessions(vec![observation], &[], "default", &mut resolver);

        assert_eq!(views.len(), 1);
        assert_eq!(
            views[0].tmux_target,
            AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::Missing)
        );
        assert_eq!(views[0].tmux_window_id(), None);
    }

    #[test]
    fn pure_reconciliation_orders_duplicate_windows_deterministically() {
        let observation = observation(
            "server",
            "server",
            Some(AgentStatus::Working),
            100,
            Some("Session target"),
            "/repo/project",
        );
        let mut resolver = FakeWorkspaceResolver::with_path("/repo/project");

        let views = reconcile_agent_sessions(
            vec![observation],
            &[
                pane_snapshot("%1", "@1", "/repo/project", None),
                pane_snapshot("%2", "@2", "/repo/project", None),
            ],
            "default",
            &mut resolver,
        );

        assert_eq!(views.len(), 1);
        assert_window_candidates(&views[0], "project", &["@1", "@2"]);
        assert_eq!(views[0].tmux_window_id(), Some("@1"));
    }

    #[test]
    fn pure_reconciliation_uses_no_target_for_matching_windows_across_sessions() {
        let observation = observation(
            "server",
            "server",
            Some(AgentStatus::Working),
            100,
            Some("Ambiguous"),
            "/repo/project",
        );
        let mut resolver = FakeWorkspaceResolver::with_path("/repo/project");

        let views = reconcile_agent_sessions(
            vec![observation],
            &[
                pane_snapshot_in_session("project", "%1", "@1", "/repo/project", None),
                pane_snapshot_in_session("other", "%2", "@2", "/repo/project", None),
            ],
            "default",
            &mut resolver,
        );

        assert_eq!(views.len(), 1);
        assert_eq!(
            views[0].tmux_target,
            AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::CrossSession {
                session_names: vec!["other".to_owned(), "project".to_owned()]
            })
        );
        assert_eq!(views[0].target.tmux_session_name, None);
        assert_eq!(views[0].tmux_window_id(), None);
    }

    #[test]
    fn merges_multiple_reporters_into_one_resolved_session() {
        let (_directory_temp, directory) = git_repo_path();
        let first = observation(
            "reporter-a",
            "instance-1",
            Some(AgentStatus::Done),
            100,
            Some("First title"),
            &directory,
        );
        let mut second = observation(
            "reporter-b",
            "instance-2",
            Some(AgentStatus::Waiting),
            200,
            Some("Second title"),
            &directory,
        );
        second.context = Some("55.2K".to_owned());

        let views = reconcile_resolved_sessions(
            vec![first, second],
            &[pane_snapshot("%1", "@1", &directory, None)],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].status, AgentStatus::Waiting);
        assert_eq!(views[0].title.as_deref(), Some("Second title"));
        assert_eq!(views[0].context.as_deref(), Some("55.2K"));
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn workspace_activity_collapses_multiple_sessions_by_canonical_root() {
        let (_root_temp, root) = git_repo_path();
        let (_feature_temp, feature) = git_repo_path();
        let observations = [
            observation_for_session("ses_a", "reporter-a", "instance-1", &root, "A"),
            observation_for_session("ses_a", "reporter-b", "instance-2", &root, "A second"),
            observation_for_session("ses_b", "reporter-a", "instance-1", &root, "B"),
            observation_for_session("ses_b", "reporter-b", "instance-2", &root, "B second"),
            observation_for_session("ses_c", "reporter-a", "instance-1", &feature, "C"),
            observation_for_session("ses_c", "reporter-b", "instance-2", &feature, "C second"),
            observation_for_session("ses_d", "reporter-a", "instance-1", &feature, "D"),
            observation_for_session("ses_d", "reporter-b", "instance-2", &feature, "D second"),
        ];

        let sessions = reconcile_resolved_sessions(
            observations.into_iter().collect(),
            &[
                pane_snapshot("%1", "@1", &root, None),
                pane_snapshot("%2", "@2", &feature, None),
            ],
            "default",
        );
        let views = workspace_activities_from_sessions(sessions);

        assert_eq!(views.len(), 2);
        let root_view = views
            .iter()
            .find(|view| view.tmux_window_id() == Some("@1"))
            .expect("root workspace view");
        assert_eq!(root_view.primary_session_key().session_id, "ses_a");
        assert_eq!(
            root_view
                .member_session_keys()
                .iter()
                .map(|key| key.session_id.as_str())
                .collect::<Vec<_>>(),
            ["ses_a", "ses_b"]
        );
        let feature_view = views
            .iter()
            .find(|view| view.tmux_window_id() == Some("@2"))
            .expect("feature workspace view");
        assert_eq!(feature_view.primary_session_key().session_id, "ses_c");
        assert_eq!(
            feature_view
                .member_session_keys()
                .iter()
                .map(|key| key.session_id.as_str())
                .collect::<Vec<_>>(),
            ["ses_c", "ses_d"]
        );
        assert_eq!(
            views
                .iter()
                .filter(|view| view.tmux_window_id() == Some("@1"))
                .count(),
            1
        );
        assert_eq!(
            views
                .iter()
                .filter(|view| view.tmux_window_id() == Some("@2"))
                .count(),
            1
        );
    }

    #[test]
    fn live_pane_precision_keeps_newer_directory_observation_fields() {
        let (_directory_temp, directory) = git_repo_path();
        let first = observation(
            "reporter-a",
            "instance-1",
            Some(AgentStatus::Working),
            100,
            Some("First"),
            &directory,
        );
        let second = observation(
            "reporter-b",
            "instance-2",
            Some(AgentStatus::Working),
            200,
            Some("Second"),
            &directory,
        );
        let views = reconcile_resolved_sessions(
            vec![first, second],
            &[pane_snapshot("%1", "@1", &directory, None)],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert!(matches!(
            views[0].tmux_target,
            AgentTmuxTarget::Windows { .. }
        ));
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
        assert_eq!(views[0].title.as_deref(), Some("Second"));
    }

    #[test]
    fn directory_only_observation_attaches_to_matching_kmux_window() {
        let (_directory_temp, directory) = git_repo_path();
        let server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        let views = reconcile_resolved_sessions(
            vec![server],
            &[pane_snapshot("%1", "@1", &directory, None)],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
        assert!(matches!(
            views[0].tmux_target,
            AgentTmuxTarget::Windows { .. }
        ));
    }

    #[test]
    fn directory_observation_attaches_to_unmarked_single_pane_window() {
        let (_directory_temp, directory) = git_repo_path();
        let server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        let views = reconcile_resolved_sessions(
            vec![server],
            &[
                pane_snapshot("%sidebar", "@1", "/tmp/kmux", Some("sidebar")),
                pane_snapshot("%1", "@1", &directory, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
    }

    #[test]
    fn codex_like_directory_only_observation_attaches_by_window_path() {
        let (_directory_temp, directory) = git_repo_path();
        let mut server = observation(
            "server",
            "codex-app-server",
            Some(AgentStatus::Waiting),
            100,
            Some("Codex task"),
            &directory,
        );
        server.key.session.agent_kind = "codex".to_owned();
        server.key.session.session_id = "thread_123".to_owned();

        let views = reconcile_resolved_sessions(
            vec![server],
            &[pane_snapshot("%1", "@1", &directory, None)],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].key.agent_kind, "codex");
        assert_eq!(views[0].key.session_id, "thread_123");
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn duplicate_windows_for_workspace_choose_a_matching_window() {
        let (_directory_temp, directory) = git_repo_path();
        let server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        let views = reconcile_resolved_sessions(
            vec![server],
            &[
                pane_snapshot("%1", "@1", &directory, None),
                pane_snapshot("%2", "@2", &directory, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_window_candidates(&views[0], "project", &["@1", "@2"]);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn current_matching_window_precedes_previous_and_index_order() {
        let (_directory_temp, directory) = git_repo_path();
        let server = directory_only_observation(&directory);
        let mut previous = pane_snapshot("%1", "@1", &directory, None);
        previous.window_active = false;
        previous.window_last = true;
        previous.window_index = "1".to_owned();
        let mut current = pane_snapshot("%2", "@2", &directory, None);
        current.window_index = "9".to_owned();

        let views = reconcile_resolved_sessions(vec![server], &[previous, current], "default");

        assert_window_candidates(&views[0], "project", &["@2", "@1"]);
        assert_eq!(views[0].tmux_window_id(), Some("@2"));
    }

    #[test]
    fn scratch_window_sidebar_does_not_override_previous_matching_window() {
        let (_directory_temp, directory) = git_repo_path();
        let (_scratch_temp, scratch) = git_repo_path();
        let server = directory_only_observation(&directory);
        let mut current_scratch = pane_snapshot("%scratch", "@9", &scratch, None);
        current_scratch.window_index = "9".to_owned();
        let mut lowest = pane_snapshot("%1", "@1", &directory, None);
        lowest.window_active = false;
        lowest.window_index = "1".to_owned();
        let mut previous = pane_snapshot("%2", "@2", &directory, None);
        previous.window_active = false;
        previous.window_last = true;
        previous.window_index = "8".to_owned();

        let views = reconcile_resolved_sessions(
            vec![server],
            &[current_scratch, lowest, previous],
            "default",
        );

        assert_window_candidates(&views[0], "project", &["@2", "@1"]);
        assert_eq!(views[0].tmux_window_id(), Some("@2"));
    }

    #[test]
    fn matching_windows_fall_back_by_parsed_index_then_window_id() {
        let (_directory_temp, directory) = git_repo_path();
        let server = directory_only_observation(&directory);
        let mut high = pane_snapshot("%2", "@2", &directory, None);
        high.window_active = false;
        high.window_index = "70000".to_owned();
        let mut tied_later = pane_snapshot("%9", "@9", &directory, None);
        tied_later.window_active = false;
        tied_later.window_index = "65536".to_owned();
        let mut tied_first = pane_snapshot("%1", "@1", &directory, None);
        tied_first.window_active = false;
        tied_first.window_index = "65536".to_owned();

        let views =
            reconcile_resolved_sessions(vec![server], &[high, tied_later, tied_first], "default");

        assert_window_candidates(&views[0], "project", &["@1", "@9", "@2"]);
        assert_eq!(views[0].tmux_window_id(), Some("@1"));
    }

    #[test]
    fn linked_windows_are_deduplicated_before_common_session_selection() {
        let (_directory_temp, directory) = git_repo_path();
        let server = directory_only_observation(&directory);
        let mut project_link = pane_snapshot_in_session("project", "%1", "@1", &directory, None);
        project_link.window_active = false;
        let mut linked_copy = pane_snapshot_in_session("linked", "%1", "@1", &directory, None);
        linked_copy.window_active = false;
        let mut project_only = pane_snapshot_in_session("project", "%2", "@2", &directory, None);
        project_only.window_active = false;

        let views = reconcile_resolved_sessions(
            vec![server],
            &[project_link, linked_copy, project_only],
            "default",
        );

        assert_window_candidates(&views[0], "project", &["@1", "@2"]);
    }

    #[test]
    fn mixed_single_and_multi_root_windows_choose_a_matching_window() {
        let (_directory_temp, directory) = git_repo_path();
        let (_other_temp, other) = git_repo_path();
        let server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        let views = reconcile_resolved_sessions(
            vec![server],
            &[
                pane_snapshot("%1", "@1", &directory, None),
                pane_snapshot("%2", "@2", &directory, None),
                pane_snapshot("%3", "@2", &other, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_window_candidates(&views[0], "project", &["@1", "@2"]);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn mixed_matching_windows_across_sessions_use_no_jump_target() {
        let (_directory_temp, directory) = git_repo_path();
        let (_other_temp, other) = git_repo_path();
        let server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        let views = reconcile_resolved_sessions(
            vec![server],
            &[
                pane_snapshot("%1", "@1", &directory, None),
                pane_snapshot_in_session("other", "%2", "@2", &directory, None),
                pane_snapshot_in_session("other", "%3", "@2", &other, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(
            views[0].tmux_target,
            AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::CrossSession {
                session_names: vec!["other".to_owned(), "project".to_owned()]
            })
        );
        assert_eq!(views[0].target.tmux_session_name, None);
        assert_eq!(views[0].target.tmux_window_id, None);
    }

    #[test]
    fn duplicate_unmarked_windows_choose_deterministic_live_target() {
        let (_directory_temp, directory) = git_repo_path();
        let pane_report = observation(
            "reporter-a",
            "instance-1",
            Some(AgentStatus::Working),
            100,
            Some("Example report"),
            &directory,
        );
        let views = reconcile_resolved_sessions(
            vec![pane_report],
            &[
                pane_snapshot("%1", "@1", &directory, None),
                pane_snapshot("%2", "@2", &directory, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_window_candidates(&views[0], "project", &["@1", "@2"]);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
    }

    #[test]
    fn single_matching_workspace_window_gets_exact_window_target() {
        let (_directory_temp, directory) = git_repo_path();
        let server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        let views = reconcile_resolved_sessions(
            vec![server],
            &[pane_snapshot("%2", "@2", &directory, None)],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@2"));
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%2"));
        assert!(matches!(
            views[0].tmux_target,
            AgentTmuxTarget::Windows { .. }
        ));
    }

    #[test]
    fn multi_root_window_uses_matching_live_pane_without_reporter_hint() {
        let (_directory_temp, directory) = git_repo_path();
        let (_other_temp, other) = git_repo_path();
        let server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        let views = reconcile_resolved_sessions(
            vec![server],
            &[
                pane_snapshot("%1", "@1", &directory, None),
                pane_snapshot("%2", "@1", &other, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert!(matches!(
            views[0].tmux_target,
            AgentTmuxTarget::Windows { .. }
        ));
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
    }

    #[test]
    fn multi_pane_window_uses_active_matching_non_sidebar_pane() {
        let (_directory_temp, directory) = git_repo_path();
        let server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        let mut first = pane_snapshot("%1", "@1", &directory, None);
        first.pane_active = false;
        first.pane_index = "1".to_owned();
        let mut second = pane_snapshot("%2", "@1", &directory, None);
        second.pane_active = true;
        second.pane_index = "2".to_owned();
        let mut sidebar = pane_snapshot("%sidebar", "@1", "/tmp/kmux", Some("sidebar"));
        sidebar.pane_active = false;
        sidebar.pane_index = "0".to_owned();

        let views = reconcile_resolved_sessions(vec![server], &[sidebar, first, second], "default");

        assert_eq!(views.len(), 1);
        assert!(matches!(
            views[0].tmux_target,
            AgentTmuxTarget::Windows { .. }
        ));
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%2"));
    }

    #[test]
    fn active_sidebar_yields_to_previous_matching_content_pane() {
        let (_directory_temp, directory) = git_repo_path();
        let server = directory_only_observation(&directory);
        let mut first = pane_snapshot("%1", "@1", &directory, None);
        first.pane_active = false;
        first.pane_index = "1".to_owned();
        let mut previous = pane_snapshot("%2", "@1", &directory, None);
        previous.pane_active = false;
        previous.pane_last = true;
        previous.pane_index = "8".to_owned();
        let mut sidebar = pane_snapshot("%sidebar", "@1", &directory, Some("sidebar"));
        sidebar.pane_index = "0".to_owned();

        let views =
            reconcile_resolved_sessions(vec![server], &[sidebar, first, previous], "default");

        assert_candidate_panes(&views[0], "@1", &["%2", "%1"]);
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%2"));
    }

    #[test]
    fn matching_panes_fall_back_by_parsed_index_then_pane_id() {
        let (_directory_temp, directory) = git_repo_path();
        let server = directory_only_observation(&directory);
        let mut high = pane_snapshot("%2", "@1", &directory, None);
        high.pane_active = false;
        high.pane_index = "70000".to_owned();
        let mut tied_later = pane_snapshot("%9", "@1", &directory, None);
        tied_later.pane_active = false;
        tied_later.pane_index = "65536".to_owned();
        let mut tied_first = pane_snapshot("%1", "@1", &directory, None);
        tied_first.pane_active = false;
        tied_first.pane_index = "65536".to_owned();

        let views =
            reconcile_resolved_sessions(vec![server], &[high, tied_later, tied_first], "default");

        assert_candidate_panes(&views[0], "@1", &["%1", "%9", "%2"]);
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
    }

    #[test]
    fn observation_without_matching_tmux_window_uses_no_jump_target() {
        let (_directory_temp, directory) = git_repo_path();
        let server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        let views = reconcile_resolved_sessions(vec![server], &[], "default");

        assert_eq!(views.len(), 1);
        assert_eq!(
            views[0].tmux_target,
            AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::Missing)
        );
        assert_eq!(views[0].target.tmux_window_id, None);
    }

    #[test]
    fn unresolved_observations_are_not_in_resolved_sessions() {
        let server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            "/tmp/does-not-exist/kmux-agent",
        );
        let views = reconcile_resolved_sessions(vec![server], &[], "default");

        assert!(views.is_empty());
    }

    #[test]
    fn latest_observation_must_resolve_to_live_window() {
        let (_directory_temp, directory) = git_repo_path();
        let old = observation(
            "reporter-a",
            "instance-1",
            Some(AgentStatus::Working),
            100,
            Some("Old"),
            &directory,
        );
        let newest = observation(
            "reporter-b",
            "instance-2",
            Some(AgentStatus::Working),
            200,
            Some("Newest"),
            "/tmp/does-not-exist/kmux-agent",
        );

        let views = reconcile_resolved_sessions(vec![old, newest], &[], "default");

        assert!(views.is_empty());
    }

    #[test]
    fn statusless_observations_can_update_title() {
        let (_directory_temp, directory) = git_repo_path();
        let status = observation(
            "reporter-a",
            "instance-1",
            Some(AgentStatus::Working),
            100,
            Some("Old"),
            &directory,
        );
        let update = observation(
            "reporter-b",
            "instance-2",
            None,
            200,
            Some("Renamed"),
            &directory,
        );

        let views = reconcile_resolved_sessions(vec![status, update], &[], "default");

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].status, AgentStatus::Working);
        assert_eq!(views[0].title.as_deref(), Some("Renamed"));
    }

    #[test]
    fn statusless_update_does_not_refresh_status_precedence() {
        let (_directory_temp, directory) = git_repo_path();
        let mut stale_working = observation(
            "reporter-a",
            "instance-1",
            Some(AgentStatus::Working),
            100,
            Some("Renamed"),
            &directory,
        );
        stale_working.observed_at = 300;
        let waiting = observation(
            "reporter-b",
            "instance-2",
            Some(AgentStatus::Waiting),
            200,
            Some("Waiting"),
            &directory,
        );

        let views = reconcile_resolved_sessions(vec![stale_working, waiting], &[], "default");

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].status, AgentStatus::Waiting);
        assert_eq!(views[0].title.as_deref(), Some("Renamed"));
    }

    fn observation(
        reporter_kind: &str,
        reporter_instance: &str,
        status: Option<AgentStatus>,
        observed_at: u64,
        title: Option<&str>,
        directory: &str,
    ) -> AgentObservationState {
        let status_changed_at = status.map(|_| observed_at);
        AgentObservationState {
            key: AgentObservationKey {
                session: AgentSessionKey {
                    agent_kind: "opencode".to_owned(),
                    session_id: "ses_root".to_owned(),
                },
                reporter_kind: reporter_kind.to_owned(),
                reporter_instance: reporter_instance.to_owned(),
            },
            created_at: observed_at,
            status,
            status_observed_at: status.map(|_| observed_at),
            status_changed_at,
            working_elapsed_secs: 0,
            observed_at,
            title: title.map(str::to_owned),
            context: None,
            target: AgentLocationHints {
                tmux_instance: Some("default".to_owned()),
                directory: Some(directory.to_owned()),
                ..AgentLocationHints::default()
            },
        }
    }

    fn observation_for_session(
        session_id: &str,
        reporter_kind: &str,
        reporter_instance: &str,
        directory: &str,
        title: &str,
    ) -> AgentObservationState {
        let mut observation = observation(
            reporter_kind,
            reporter_instance,
            Some(AgentStatus::Working),
            100,
            Some(title),
            directory,
        );
        observation.key.session.session_id = session_id.to_owned();
        observation.target.directory = Some(directory.to_owned());
        observation
    }

    fn git_repo_path() -> (GitRepoFixture, String) {
        let fixture = GitRepoFixture::new().expect("Git fixture should be created");
        let path = fixture.path().display().to_string();
        (fixture, path)
    }

    fn pane_snapshot(
        pane_id: &str,
        window_id: &str,
        current_path: &str,
        kmux_role: Option<&str>,
    ) -> TmuxPaneSnapshot {
        pane_snapshot_in_session("project", pane_id, window_id, current_path, kmux_role)
    }

    fn assert_window_candidates(
        view: &ResolvedAgentSession,
        expected_session: &str,
        expected_window_ids: &[&str],
    ) {
        let AgentTmuxTarget::Windows {
            session_name,
            candidates,
        } = &view.tmux_target
        else {
            assert!(matches!(&view.tmux_target, AgentTmuxTarget::Windows { .. }));
            return;
        };
        assert_eq!(session_name, expected_session);
        assert_eq!(
            candidates
                .iter()
                .map(|candidate| candidate.window_id.as_str())
                .collect::<Vec<_>>(),
            expected_window_ids
        );
    }

    fn assert_candidate_panes(
        view: &ResolvedAgentSession,
        window_id: &str,
        expected_pane_ids: &[&str],
    ) {
        let AgentTmuxTarget::Windows { candidates, .. } = &view.tmux_target else {
            assert!(matches!(&view.tmux_target, AgentTmuxTarget::Windows { .. }));
            return;
        };
        let candidate = candidates
            .iter()
            .find(|candidate| candidate.window_id == window_id);
        assert!(candidate.is_some(), "expected matching window candidate");
        let Some(candidate) = candidate else {
            return;
        };
        assert_eq!(
            candidate
                .pane_ids
                .iter()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            expected_pane_ids
        );
    }

    fn directory_only_observation(directory: &str) -> AgentObservationState {
        observation(
            "reporter-b",
            "instance-2",
            Some(AgentStatus::Working),
            100,
            Some("Workspace activity"),
            directory,
        )
    }

    fn pane_snapshot_in_session(
        session_name: &str,
        pane_id: &str,
        window_id: &str,
        current_path: &str,
        kmux_role: Option<&str>,
    ) -> TmuxPaneSnapshot {
        TmuxPaneSnapshot {
            session_name: session_name.to_owned(),
            window_id: window_id.to_owned(),
            window_index: "1".to_owned(),
            window_name: format!("{session_name}-window"),
            pane_id: pane_id.to_owned(),
            pane_index: "1".to_owned(),
            pane_left: 0,
            pane_width: 80,
            window_width: 120,
            window_layout: crate::tmux::test_support::test_window_layout(&[pane_id]),
            title: Some("pane title".to_owned()),
            current_command: Some("opencode".to_owned()),
            current_path: Some(current_path.to_owned()),
            pane_active: true,
            pane_last: false,
            window_active: true,
            window_last: false,
            session_attached: true,
            kmux_role: kmux_role.map(str::to_owned),
        }
    }
}
