//! Reconciliation of persisted agent observations with live tmux state.
//!
//! External producers report a current directory for each logical session. This
//! module attaches those observations to Git worktree roots, then derives the
//! best honest live tmux target for selection.

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
    identity: WorkspaceIdentity,
    key: String,
    path: String,
    reported_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Reconciled application model for one logical agent session.
pub struct ResolvedAgentSession {
    pub key: AgentSessionKey,
    pub workspace: Option<ResolvedAgentWorkspace>,
    pub workspace_key: Option<String>,
    pub tmux_target: AgentTmuxTarget,
    pub created_at: u64,
    pub status: AgentStatus,
    pub status_observed_at: u64,
    pub status_changed_at: u64,
    pub working_elapsed_secs: u64,
    pub observed_at: u64,
    pub title: Option<String>,
    pub context: Option<String>,
    /// Merged raw hints retained for legacy callers and unchanged hook payloads.
    pub target: AgentLocationHints,
}

/// Compatibility name for callers that have not moved to resolved-session wording yet.
pub type AgentSessionView = ResolvedAgentSession;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Precision of the live tmux target associated with an agent row.
pub enum AgentTmuxTarget {
    Window,
    Session,
    None,
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
            identity,
            key: path.clone(),
            path,
            reported_path,
        })
    }

    /// Return the canonical workspace identity.
    pub fn identity(&self) -> &WorkspaceIdentity {
        &self.identity
    }

    /// Return the stable grouping key for this workspace.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Return the canonical Git worktree root as display text.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Return the path originally reported by the producer before Git-root resolution.
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

    /// Return the resolved workspace grouping key, if the session is attached to Git.
    pub fn workspace_key(&self) -> Option<&str> {
        self.workspace
            .as_ref()
            .map(ResolvedAgentWorkspace::key)
            .or(self.workspace_key.as_deref())
    }

    /// Return the resolved canonical Git worktree path, if available.
    pub fn workspace_path(&self) -> Option<&str> {
        self.workspace
            .as_ref()
            .map(ResolvedAgentWorkspace::path)
            .or(self.target.git_worktree_path.as_deref())
    }

    /// Return the raw compatibility hints merged from producer observations.
    pub fn location_hints(&self) -> &AgentLocationHints {
        &self.target
    }

    /// Return the best known Git repo name for display.
    pub fn git_repo_name(&self) -> Option<&str> {
        self.target.git_repo_name.as_deref()
    }

    /// Return the best known main Git repository path for display.
    pub fn git_repo_path(&self) -> Option<&str> {
        self.target.git_repo_path.as_deref()
    }

    /// Return the kmux workspace slug reported or inferred for this session.
    pub fn kmux_workspace_slug(&self) -> Option<&str> {
        self.target.kmux_workspace_slug.as_deref()
    }

    /// Return the resolved Git worktree path used for matching and display.
    pub fn git_worktree_path(&self) -> Option<&str> {
        self.workspace_path()
    }

    /// Return the best known Git branch name for display and filtering.
    pub fn git_branch(&self) -> Option<&str> {
        self.target.git_branch.as_deref()
    }

    /// Return the latest producer-reported directory, if one was provided.
    pub fn directory(&self) -> Option<&str> {
        self.target.directory.as_deref().or_else(|| {
            self.workspace
                .as_ref()
                .map(ResolvedAgentWorkspace::reported_path)
        })
    }

    /// Return the resolved tmux session name for navigation.
    pub fn tmux_session_name(&self) -> Option<&str> {
        self.target.tmux_session_name.as_deref()
    }

    /// Return the resolved tmux window id for navigation.
    pub fn tmux_window_id(&self) -> Option<&str> {
        self.target.tmux_window_id.as_deref()
    }

    /// Return the resolved tmux window name for display.
    pub fn tmux_window_name(&self) -> Option<&str> {
        self.target.tmux_window_name.as_deref()
    }

    /// Return the resolved tmux pane id when live pane precision is available.
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

    /// Return whether the session has an exact tmux window navigation target.
    pub fn is_window_tmux_target(&self) -> bool {
        self.tmux_target == AgentTmuxTarget::Window
    }

    /// Return the stable key used to collapse multiple logical sessions into one row.
    pub fn collapse_group_key(&self) -> String {
        self.workspace_key()
            .map(|key| format!("workspace:{key}"))
            .or_else(|| {
                self.tmux_window_id()
                    .map(|window_id| format!("window:{window_id}"))
            })
            .unwrap_or_else(|| format!("session:{}/{}", self.key.agent_kind, self.key.session_id))
    }
}

/// Reconcile persisted agent observations with live tmux window state.
pub fn session_views(store: &StateStore, tmux: &Tmux) -> Result<Vec<AgentSessionView>> {
    let result = telemetry::timed_result_event!(
        "session_views",
        {},
        || {
            let tmux_instance = tmux.instance_id();
            let observations = store.list_observations()?;
            let observation_count = observations
                .iter()
                .filter(|observation| is_candidate_for_tmux_instance(observation, &tmux_instance))
                .count();
            if observation_count == 0 {
                return Ok(SessionViewsTelemetry {
                    views: Vec::new(),
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
            let views = reconcile_agent_sessions(
                observations,
                &panes,
                &tmux_instance,
                &mut workspace_resolver,
            );
            Ok(SessionViewsTelemetry {
                views,
                observations: observation_count,
                panes: pane_count,
                tmux_snapshot_ok,
            })
        },
        ok |telemetry_result| {
            observations = telemetry_result.observations,
            panes = telemetry_result.panes,
            views = telemetry_result.views.len(),
            tmux_snapshot_ok = telemetry_result.tmux_snapshot_ok,
        },
    );

    result.map(|telemetry_result| telemetry_result.views)
}

struct SessionViewsTelemetry {
    views: Vec<AgentSessionView>,
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
    target: AgentLocationHints,
    tmux_target: AgentTmuxTarget,
}

trait AgentWorkspaceLookup {
    fn attachment_for_hints(
        &mut self,
        target: &AgentLocationHints,
    ) -> Option<AgentWorkspaceAttachment>;

    fn attachment_for_path(&mut self, path: &str) -> Option<AgentWorkspaceAttachment>;

    fn attachment_matches_path(
        &mut self,
        attachment: &AgentWorkspaceAttachment,
        candidate: Option<&str>,
    ) -> bool {
        candidate
            .and_then(|path| self.attachment_for_path(path))
            .is_some_and(|candidate_attachment| candidate_attachment.key() == attachment.key())
    }
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

    fn attachment_matches_path(
        &mut self,
        attachment: &AgentWorkspaceAttachment,
        candidate: Option<&str>,
    ) -> bool {
        AgentWorkspaceResolver::attachment_matches_path(self, attachment, candidate)
    }
}

// Ignore observations scoped to another tmux socket, but accept unscoped legacy
// observations so older agents still appear in status views.
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
fn reconcile_session_views(
    observations: Vec<AgentObservationState>,
    panes: &[TmuxPaneSnapshot],
    tmux_instance: &str,
) -> Vec<AgentSessionView> {
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
            tmux_instance,
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

    let views = grouped
        .into_iter()
        .filter_map(|(key, observations)| session_view_from_observations(key, &observations))
        .collect::<Vec<_>>();
    collapse_workspace_views(views)
}

// Choose one status observation and one location observation for a session, then
// merge newer metadata fields around that resolved target.
fn session_view_from_observations(
    key: AgentSessionKey,
    observations: &[EnrichedObservation],
) -> Option<AgentSessionView> {
    let status_observation = observations
        .iter()
        .filter(|observation| observation.state.status.is_some())
        .max_by_key(|observation| {
            (
                observation_status_observed_at(&observation.state),
                status_priority(observation.state.status),
                observation.state.observed_at,
            )
        })?;
    let location_observation = best_location_observation(observations)?;
    let resolved_target = location_observation.resolved_target.clone()?;
    let mut target = resolved_target.target;
    merge_target_metadata(&mut target, observations);
    target.agent_workspace_id = newest_agent_workspace_id(observations);
    enrich_missing_repo_metadata(&mut target);

    let status_changed_at = status_observation.state.status_changed_at?;
    let status_observed_at = observation_status_observed_at(&status_observation.state);
    let workspace = location_observation
        .workspace_attachment
        .as_ref()
        .and_then(ResolvedAgentWorkspace::from_attachment);
    let workspace_key = location_observation
        .workspace_attachment
        .as_ref()
        .map(|attachment| attachment.key().to_owned());
    Some(AgentSessionView {
        key,
        workspace,
        workspace_key,
        tmux_target: resolved_target.tmux_target,
        created_at: observations
            .iter()
            .map(|observation| observation.state.effective_created_at())
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
    match resolved.tmux_target {
        AgentTmuxTarget::Window if resolved.target.tmux_pane_id.is_some() => 4,
        AgentTmuxTarget::Window => 3,
        AgentTmuxTarget::Session => 2,
        AgentTmuxTarget::None => 1,
    }
}

fn collapse_workspace_views(views: Vec<AgentSessionView>) -> Vec<AgentSessionView> {
    let mut by_target = BTreeMap::<String, AgentSessionView>::new();
    for view in views {
        let key = view.collapse_group_key();
        match by_target.get_mut(&key) {
            Some(current) if primary_view_is_better(&view, current) => {
                *current = view;
            }
            Some(_) => {}
            None => {
                by_target.insert(key, view);
            }
        }
    }
    by_target.into_values().collect()
}

fn primary_view_is_better(candidate: &AgentSessionView, current: &AgentSessionView) -> bool {
    let candidate_rank = status_priority(Some(candidate.status));
    let current_rank = status_priority(Some(current.status));
    candidate_rank > current_rank
        || candidate_rank == current_rank
            && (candidate.status_observed_at > current.status_observed_at
                || candidate.status_observed_at == current.status_observed_at
                    && (candidate.observed_at > current.observed_at
                        || candidate.observed_at == current.observed_at
                            && candidate.key < current.key))
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

// Workspace IDs are routing scope, so a newer observation that omits the field
// must clear older values instead of letting stale scopes drive selection hooks.
fn newest_agent_workspace_id(observations: &[EnrichedObservation]) -> Option<String> {
    let observed_at = observations
        .iter()
        .map(|observation| observation.state.observed_at)
        .max()?;
    let mut workspace_id = None;

    for observation in observations
        .iter()
        .filter(|observation| observation.state.observed_at == observed_at)
    {
        let candidate = observation.state.target.agent_workspace_id.as_deref()?;
        if workspace_id.is_some_and(|workspace_id| workspace_id != candidate) {
            return None;
        }
        workspace_id = Some(candidate);
    }

    workspace_id.map(str::to_owned)
}

fn status_priority(status: Option<AgentStatus>) -> u8 {
    match status {
        Some(AgentStatus::Waiting) => 3,
        Some(AgentStatus::Working) => 2,
        Some(AgentStatus::Done) => 1,
        None => 0,
    }
}

// A producer-visible row exists when the reported directory resolves to a Git
// worktree root. Live tmux facts only determine how precise selection can be.
fn resolve_observation_target(
    observation: &AgentObservationState,
    workspace_attachment: Option<&AgentWorkspaceAttachment>,
    panes: &[TmuxPaneSnapshot],
    tmux_instance: &str,
    workspace_resolver: &mut impl AgentWorkspaceLookup,
) -> Option<ResolvedObservationTarget> {
    let attachment = workspace_attachment?;
    let mut target = AgentLocationHints::default();
    apply_workspace_attachment(&mut target, attachment);
    let tmux_target = enrich_target_from_live_tmux(
        &mut target,
        attachment,
        observation.target.tmux_pane_id.as_deref(),
        panes,
        tmux_instance,
        workspace_resolver,
    );
    merge_resolved_observation_metadata(&mut target, &observation.target);
    enrich_missing_repo_metadata(&mut target);
    Some(ResolvedObservationTarget {
        target,
        tmux_target,
    })
}

fn enrich_target_from_live_tmux(
    target: &mut AgentLocationHints,
    attachment: &AgentWorkspaceAttachment,
    preferred_pane_id: Option<&str>,
    panes: &[TmuxPaneSnapshot],
    tmux_instance: &str,
    workspace_resolver: &mut impl AgentWorkspaceLookup,
) -> AgentTmuxTarget {
    if let Some(pane) =
        preferred_pane_workspace_match(attachment, preferred_pane_id, panes, workspace_resolver)
    {
        enrich_target_from_pane(target, pane, tmux_instance);
        return AgentTmuxTarget::Window;
    }

    let matches = window_workspace_matches(attachment, panes, workspace_resolver);
    if let [window] = matches.as_slice()
        && window.workspace_count == 1
    {
        enrich_target_from_window_match(target, window, tmux_instance);
        return AgentTmuxTarget::Window;
    }

    let sessions = matches
        .iter()
        .map(|window| window.session_name.as_str())
        .collect::<BTreeSet<_>>();
    if let Some(session_name) = single_session_name(&sessions) {
        target.tmux_instance = Some(tmux_instance.to_owned());
        target.tmux_session_name = Some(session_name.to_owned());
        return AgentTmuxTarget::Session;
    }

    AgentTmuxTarget::None
}

#[derive(Debug, Clone)]
struct WindowWorkspaceMatch {
    session_name: String,
    window_id: String,
    window_name: String,
    workspace_count: usize,
}

#[derive(Debug, Clone)]
struct WindowWorkspaceAccumulator {
    session_name: String,
    window_id: String,
    window_name: String,
    workspace_keys: BTreeSet<String>,
    matches_attachment: bool,
}

fn preferred_pane_workspace_match<'a>(
    attachment: &AgentWorkspaceAttachment,
    preferred_pane_id: Option<&str>,
    panes: &'a [TmuxPaneSnapshot],
    workspace_resolver: &mut impl AgentWorkspaceLookup,
) -> Option<&'a TmuxPaneSnapshot> {
    let preferred_pane_id = preferred_pane_id?;
    panes.iter().find(|pane| {
        pane.pane_id == preferred_pane_id
            && pane.kmux_role.as_deref() != Some("sidebar")
            && workspace_resolver.attachment_matches_path(attachment, pane.current_path.as_deref())
    })
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
                    session_name: pane.session_name.clone(),
                    window_id: pane.window_id.clone(),
                    window_name: pane.window_name.clone(),
                    workspace_keys: BTreeSet::new(),
                    matches_attachment: false,
                });
        if workspace.key() == attachment.key() {
            entry.matches_attachment = true;
        }
        entry.workspace_keys.insert(workspace.key().to_owned());
    }

    windows
        .into_values()
        .filter(|window| window.matches_attachment)
        .map(|window| WindowWorkspaceMatch {
            session_name: window.session_name,
            window_id: window.window_id,
            window_name: window.window_name,
            workspace_count: window.workspace_keys.len(),
        })
        .collect()
}

fn single_session_name<'a>(sessions: &BTreeSet<&'a str>) -> Option<&'a str> {
    let mut sessions = sessions.iter().copied();
    let session = sessions.next()?;
    sessions.next().is_none().then_some(session)
}

fn apply_workspace_attachment(
    target: &mut AgentLocationHints,
    attachment: &AgentWorkspaceAttachment,
) {
    if target.directory.is_none() {
        target.directory = Some(attachment.reported_path().to_owned());
    }
    target.git_worktree_path = Some(attachment.path().to_owned());
}

fn enrich_target_from_pane(
    target: &mut AgentLocationHints,
    pane: &TmuxPaneSnapshot,
    tmux_instance: &str,
) {
    target.tmux_instance = Some(tmux_instance.to_owned());
    target.tmux_session_name = Some(pane.session_name.clone());
    target.tmux_window_id = Some(pane.window_id.clone());
    target.tmux_window_name = Some(pane.window_name.clone());
    target.tmux_pane_id = Some(pane.pane_id.clone());
    target.tmux_pane_title = pane.title.clone();
    target.tmux_pane_current_command = pane.current_command.clone();
    target.tmux_pane_current_path = pane.current_path.clone();
}

fn enrich_target_from_window_match(
    target: &mut AgentLocationHints,
    window: &WindowWorkspaceMatch,
    tmux_instance: &str,
) {
    target.tmux_instance = Some(tmux_instance.to_owned());
    target.tmux_session_name = Some(window.session_name.clone());
    target.tmux_window_id = Some(window.window_id.clone());
    target.tmux_window_name = Some(window.window_name.clone());
}

// Merge newest display/routing metadata first. Live tmux target fields come only
// from the matched kmux window, not from producer hints.
fn merge_target_metadata(target: &mut AgentLocationHints, observations: &[EnrichedObservation]) {
    let mut sorted = observations.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|observation| observation.state.observed_at);
    for observation in sorted.into_iter().rev() {
        merge_resolved_observation_metadata(target, &observation.state.target);
    }
}

fn merge_resolved_observation_metadata(
    target: &mut AgentLocationHints,
    fallback: &AgentLocationHints,
) {
    if target.git_repo_name.is_none() {
        target.git_repo_name = fallback.git_repo_name.clone();
    }
    if target.git_repo_path.is_none() {
        target.git_repo_path = fallback.git_repo_path.clone();
    }
    if target.kmux_workspace_slug.is_none() {
        target.kmux_workspace_slug = fallback.kmux_workspace_slug.clone();
    }
    if target.agent_workspace_id.is_none() {
        target.agent_workspace_id = fallback.agent_workspace_id.clone();
    }
    if target.git_worktree_path.is_none() {
        target.git_worktree_path = fallback.git_worktree_path.clone();
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
fn enrich_missing_repo_metadata(target: &mut AgentLocationHints) {
    if target.git_repo_name.is_some()
        && target.git_repo_path.is_some()
        && target.git_branch.is_some()
    {
        return;
    }

    let metadata = infer_repo_metadata_from_paths(&[
        target.directory.as_deref(),
        target.git_worktree_path.as_deref(),
        target.tmux_pane_current_path.as_deref(),
    ]);
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
    use crate::state::{AgentObservationKey, AgentObservationState};
    use std::collections::BTreeMap;
    use std::fs;
    use std::path::Path;
    use std::process::Command;
    use tempfile::{TempDir, tempdir};

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
    fn pure_reconciliation_returns_no_views_without_observations() {
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
        assert_eq!(views[0].workspace_key(), Some("/repo/project"));
        assert_eq!(views[0].workspace_path(), Some("/repo/project"));
        assert_eq!(views[0].tmux_target, AgentTmuxTarget::Window);
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
        assert_eq!(views[0].tmux_target, AgentTmuxTarget::None);
        assert_eq!(views[0].tmux_window_id(), None);
    }

    #[test]
    fn pure_reconciliation_degrades_duplicate_windows_to_session_target() {
        let mut observation = observation(
            "server",
            "server",
            Some(AgentStatus::Working),
            100,
            Some("Session target"),
            "/repo/project",
        );
        observation.target.tmux_pane_id = None;
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
        assert_eq!(views[0].tmux_target, AgentTmuxTarget::Session);
        assert_eq!(views[0].tmux_session_name(), Some("project"));
        assert_eq!(views[0].tmux_window_id(), None);
    }

    #[test]
    fn pure_reconciliation_uses_no_target_for_matching_windows_across_sessions() {
        let mut observation = observation(
            "server",
            "server",
            Some(AgentStatus::Working),
            100,
            Some("Ambiguous"),
            "/repo/project",
        );
        observation.target.tmux_pane_id = None;
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
        assert_eq!(views[0].tmux_target, AgentTmuxTarget::None);
        assert_eq!(views[0].tmux_session_name(), None);
        assert_eq!(views[0].tmux_window_id(), None);
    }

    #[test]
    fn merges_tui_and_server_observations_into_one_session_view() {
        let (_directory_temp, directory) = git_repo_path();
        let tui = observation(
            "tui",
            "default/%1",
            Some(AgentStatus::Done),
            100,
            Some("Pane title"),
            &directory,
        );
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Waiting),
            200,
            Some("Server title"),
            &directory,
        );
        server.context = Some("55.2K".to_owned());
        server.target.agent_workspace_id = Some("wrk_01KTEST".to_owned());
        server.target.tmux_pane_id = None;
        server.target.tmux_window_id = None;

        let views = reconcile_session_views(
            vec![tui, server],
            &[pane_snapshot("%1", "@1", &directory, None)],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].status, AgentStatus::Waiting);
        assert_eq!(views[0].title.as_deref(), Some("Server title"));
        assert_eq!(views[0].context.as_deref(), Some("55.2K"));
        assert_eq!(
            views[0].target.agent_workspace_id.as_deref(),
            Some("wrk_01KTEST")
        );
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn collapses_multiple_sessions_in_one_directory_to_one_primary_view() {
        let (_root_temp, root) = git_repo_path();
        let (_feature_temp, feature) = git_repo_path();
        let observations = [
            observation_for_session("ses_a", "tui", "default/%1", &root, "A"),
            observation_for_session("ses_a", "server", "server", &root, "A server"),
            observation_for_session("ses_b", "tui", "default/%1", &root, "B"),
            observation_for_session("ses_b", "server", "server", &root, "B server"),
            observation_for_session("ses_c", "tui", "default/%2", &feature, "C"),
            observation_for_session("ses_c", "server", "server", &feature, "C server"),
            observation_for_session("ses_d", "tui", "default/%2", &feature, "D"),
            observation_for_session("ses_d", "server", "server", &feature, "D server"),
        ];

        let views = reconcile_session_views(
            observations.into_iter().collect(),
            &[
                pane_snapshot("%1", "@1", &root, None),
                pane_snapshot("%2", "@2", &feature, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 2);
        assert!(views.iter().any(|view| {
            view.key.session_id == "ses_a" && view.target.tmux_window_id.as_deref() == Some("@1")
        }));
        assert!(views.iter().any(|view| {
            view.key.session_id == "ses_c" && view.target.tmux_window_id.as_deref() == Some("@2")
        }));
        assert_eq!(
            views
                .iter()
                .filter(|view| view.target.tmux_window_id.as_deref() == Some("@1"))
                .count(),
            1
        );
        assert_eq!(
            views
                .iter()
                .filter(|view| view.target.tmux_window_id.as_deref() == Some("@2"))
                .count(),
            1
        );
    }

    #[test]
    fn same_directory_primary_view_prefers_waiting_status() {
        let (_directory_temp, directory) = git_repo_path();
        let mut done = observation_for_session("ses_done", "tui", "default/%1", &directory, "Done");
        done.status = Some(AgentStatus::Done);
        done.status_observed_at = Some(300);
        done.status_changed_at = Some(300);
        done.observed_at = 300;
        let mut waiting =
            observation_for_session("ses_waiting", "tui", "default/%1", &directory, "Waiting");
        waiting.status = Some(AgentStatus::Waiting);
        waiting.status_observed_at = Some(100);
        waiting.status_changed_at = Some(100);

        let views = reconcile_session_views(vec![done, waiting], &[], "default");

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].key.session_id, "ses_waiting");
        assert_eq!(views[0].status, AgentStatus::Waiting);
    }

    #[test]
    fn precise_live_pane_location_beats_newer_directory_only_observation() {
        let (_directory_temp, directory) = git_repo_path();
        let tui = observation(
            "tui",
            "default/%1",
            Some(AgentStatus::Working),
            100,
            Some("TUI"),
            &directory,
        );
        let mut server = observation(
            "server",
            "server",
            Some(AgentStatus::Working),
            200,
            Some("Server"),
            &directory,
        );
        server.target.tmux_pane_id = None;
        server.target.tmux_window_id = None;

        let views = reconcile_session_views(
            vec![tui, server],
            &[
                pane_snapshot("%1", "@1", &directory, None),
                pane_snapshot("%2", "@2", &directory, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].tmux_target, AgentTmuxTarget::Window);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
        assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
        assert_eq!(views[0].title.as_deref(), Some("Server"));
    }

    #[test]
    fn directory_only_observation_attaches_to_matching_kmux_window() {
        let (_directory_temp, directory) = git_repo_path();
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        server.target.tmux_pane_id = None;
        server.target.tmux_window_id = None;
        server.target.git_worktree_path = None;

        let views = reconcile_session_views(
            vec![server],
            &[pane_snapshot("%1", "@1", &directory, None)],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.tmux_pane_id, None);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
        assert_eq!(views[0].tmux_target, AgentTmuxTarget::Window);
    }

    #[test]
    fn directory_observation_attaches_to_unmarked_single_pane_window() {
        let (_directory_temp, directory) = git_repo_path();
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        server.target.tmux_pane_id = None;
        server.target.tmux_window_id = None;
        server.target.git_worktree_path = None;

        let views = reconcile_session_views(
            vec![server],
            &[
                pane_snapshot("%sidebar", "@1", "/tmp/kmux", Some("sidebar")),
                pane_snapshot("%1", "@1", &directory, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
        assert_eq!(views[0].target.tmux_pane_id, None);
    }

    #[test]
    fn git_worktree_path_without_directory_does_not_attach() {
        let (_directory_temp, directory) = git_repo_path();
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        server.target.directory = None;
        server.target.git_worktree_path = Some(directory);

        let views = reconcile_session_views(vec![server], &[], "default");

        assert!(views.is_empty());
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
        server.target.tmux_pane_id = None;
        server.target.tmux_window_id = None;
        server.target.tmux_session_name = None;
        server.target.tmux_window_name = None;
        server.target.git_worktree_path = None;

        let views = reconcile_session_views(
            vec![server],
            &[pane_snapshot("%1", "@1", &directory, None)],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].key.agent_kind, "codex");
        assert_eq!(views[0].key.session_id, "thread_123");
        assert_eq!(views[0].target.tmux_pane_id, None);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn duplicate_windows_for_workspace_degrade_to_session_target() {
        let (_directory_temp, directory) = git_repo_path();
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        server.target.tmux_pane_id = None;
        server.target.tmux_window_id = None;
        server.target.tmux_session_name = None;
        server.target.tmux_window_name = None;
        server.target.git_worktree_path = None;

        let views = reconcile_session_views(
            vec![server],
            &[
                pane_snapshot("%1", "@1", &directory, None),
                pane_snapshot("%2", "@2", &directory, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].tmux_target, AgentTmuxTarget::Session);
        assert_eq!(
            views[0].target.tmux_session_name.as_deref(),
            Some("project")
        );
        assert_eq!(views[0].target.tmux_window_id, None);
    }

    #[test]
    fn mixed_single_and_multi_root_windows_degrade_to_session_target() {
        let (_directory_temp, directory) = git_repo_path();
        let (_other_temp, other) = git_repo_path();
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        server.target.tmux_pane_id = None;
        server.target.tmux_window_id = None;
        server.target.tmux_session_name = None;
        server.target.tmux_window_name = None;
        server.target.git_worktree_path = None;

        let views = reconcile_session_views(
            vec![server],
            &[
                pane_snapshot("%1", "@1", &directory, None),
                pane_snapshot("%2", "@2", &directory, None),
                pane_snapshot("%3", "@2", &other, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].tmux_target, AgentTmuxTarget::Session);
        assert_eq!(
            views[0].target.tmux_session_name.as_deref(),
            Some("project")
        );
        assert_eq!(views[0].target.tmux_window_id, None);
    }

    #[test]
    fn mixed_matching_windows_across_sessions_use_no_jump_target() {
        let (_directory_temp, directory) = git_repo_path();
        let (_other_temp, other) = git_repo_path();
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        server.target.tmux_pane_id = None;
        server.target.tmux_window_id = None;
        server.target.tmux_session_name = None;
        server.target.tmux_window_name = None;
        server.target.git_worktree_path = None;

        let views = reconcile_session_views(
            vec![server],
            &[
                pane_snapshot("%1", "@1", &directory, None),
                pane_snapshot_in_session("other", "%2", "@2", &directory, None),
                pane_snapshot_in_session("other", "%3", "@2", &other, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].tmux_target, AgentTmuxTarget::None);
        assert_eq!(views[0].target.tmux_session_name, None);
        assert_eq!(views[0].target.tmux_window_id, None);
    }

    #[test]
    fn duplicate_unmarked_windows_use_matching_live_pane_as_tiebreaker() {
        let (_directory_temp, directory) = git_repo_path();
        let mut tui = observation(
            "tui",
            "default/%2",
            Some(AgentStatus::Working),
            100,
            Some("TUI"),
            &directory,
        );
        tui.target.tmux_pane_id = Some("%2".to_owned());
        tui.target.tmux_window_id = None;
        tui.target.tmux_session_name = None;
        tui.target.tmux_window_name = None;
        tui.target.git_worktree_path = None;

        let views = reconcile_session_views(
            vec![tui],
            &[
                pane_snapshot("%1", "@1", &directory, None),
                pane_snapshot("%2", "@2", &directory, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@2"));
    }

    #[test]
    fn single_matching_workspace_window_gets_exact_window_target() {
        let (_directory_temp, directory) = git_repo_path();
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        server.target.tmux_pane_id = None;
        server.target.tmux_window_id = None;
        server.target.tmux_session_name = None;
        server.target.tmux_window_name = None;
        server.target.git_worktree_path = None;

        let views = reconcile_session_views(
            vec![server],
            &[pane_snapshot("%2", "@2", &directory, None)],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@2"));
        assert_eq!(views[0].tmux_target, AgentTmuxTarget::Window);
    }

    #[test]
    fn multi_root_window_degrades_to_session_target_without_pane_hint() {
        let (_directory_temp, directory) = git_repo_path();
        let (_other_temp, other) = git_repo_path();
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        server.target.tmux_pane_id = None;
        server.target.tmux_window_id = None;

        let views = reconcile_session_views(
            vec![server],
            &[
                pane_snapshot("%1", "@1", &directory, None),
                pane_snapshot("%2", "@1", &other, None),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].tmux_target, AgentTmuxTarget::Session);
        assert_eq!(views[0].target.tmux_window_id, None);
    }

    #[test]
    fn observation_without_matching_tmux_window_uses_no_jump_target() {
        let (_directory_temp, directory) = git_repo_path();
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        server.target.tmux_pane_id = None;
        server.target.tmux_window_id = None;
        server.target.tmux_session_name = None;
        server.target.tmux_window_name = None;
        server.target.git_worktree_path = None;

        let views = reconcile_session_views(vec![server], &[], "default");

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].tmux_target, AgentTmuxTarget::None);
        assert_eq!(views[0].target.tmux_window_id, None);
    }

    #[test]
    fn unresolved_observations_are_not_in_default_views() {
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            "/tmp/does-not-exist/kmux-agent",
        );
        server.target.tmux_pane_id = None;
        server.target.tmux_window_id = None;
        server.target.tmux_session_name = None;
        server.target.tmux_window_name = None;
        server.target.git_worktree_path = None;

        let views = reconcile_session_views(vec![server], &[], "default");

        assert!(views.is_empty());
    }

    #[test]
    fn latest_observation_must_resolve_to_live_window() {
        let (_directory_temp, directory) = git_repo_path();
        let old = observation(
            "tui",
            "default/%1",
            Some(AgentStatus::Working),
            100,
            Some("Old"),
            &directory,
        );
        let newest = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            200,
            Some("Newest"),
            "/tmp/does-not-exist/kmux-agent",
        );

        let views = reconcile_session_views(vec![old, newest], &[], "default");

        assert!(views.is_empty());
    }

    #[test]
    fn metadata_only_observations_can_update_title_without_status() {
        let (_directory_temp, directory) = git_repo_path();
        let status = observation(
            "tui",
            "default/%1",
            Some(AgentStatus::Working),
            100,
            Some("Old"),
            &directory,
        );
        let mut metadata = observation("server", "server", None, 200, Some("Renamed"), &directory);
        metadata.target.tmux_pane_id = None;
        metadata.target.tmux_window_id = None;

        let views = reconcile_session_views(vec![status, metadata], &[], "default");

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].status, AgentStatus::Working);
        assert_eq!(views[0].title.as_deref(), Some("Renamed"));
    }

    #[test]
    fn metadata_only_update_does_not_refresh_status_precedence() {
        let (_directory_temp, directory) = git_repo_path();
        let mut stale_working = observation(
            "tui",
            "default/%1",
            Some(AgentStatus::Working),
            100,
            Some("Renamed"),
            &directory,
        );
        stale_working.observed_at = 300;
        let waiting = observation(
            "server",
            "server",
            Some(AgentStatus::Waiting),
            200,
            Some("Waiting"),
            &directory,
        );

        let views = reconcile_session_views(vec![stale_working, waiting], &[], "default");

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].status, AgentStatus::Waiting);
        assert_eq!(views[0].title.as_deref(), Some("Renamed"));
    }

    #[test]
    fn newer_missing_agent_workspace_id_clears_stale_scope() {
        let (_directory_temp, directory) = git_repo_path();
        let mut old_workspace = observation(
            "tui",
            "default/%1",
            Some(AgentStatus::Working),
            100,
            Some("Old workspace"),
            &directory,
        );
        old_workspace.target.agent_workspace_id = Some("wrk_old".to_owned());
        let mut cleared_workspace =
            observation("server", "server", None, 200, Some("Cleared"), &directory);
        cleared_workspace.target.tmux_pane_id = None;
        cleared_workspace.target.tmux_window_id = None;

        let views = reconcile_session_views(vec![old_workspace, cleared_workspace], &[], "default");

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.agent_workspace_id, None);
    }

    #[test]
    fn equal_timestamp_missing_agent_workspace_id_clears_stale_scope() {
        let (_directory_temp, directory) = git_repo_path();
        let mut old_workspace = observation(
            "tui",
            "default/%1",
            Some(AgentStatus::Working),
            100,
            Some("Old workspace"),
            &directory,
        );
        old_workspace.target.agent_workspace_id = Some("wrk_old".to_owned());
        let mut cleared_workspace =
            observation("server", "server", None, 100, Some("Cleared"), &directory);
        cleared_workspace.target.tmux_pane_id = None;
        cleared_workspace.target.tmux_window_id = None;

        let views = reconcile_session_views(vec![old_workspace, cleared_workspace], &[], "default");

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.agent_workspace_id, None);
    }

    #[test]
    fn equal_timestamp_conflicting_agent_workspace_ids_clear_scope() {
        let (_directory_temp, directory) = git_repo_path();
        let mut first_workspace = observation(
            "tui",
            "default/%1",
            Some(AgentStatus::Working),
            100,
            Some("First workspace"),
            &directory,
        );
        first_workspace.target.agent_workspace_id = Some("wrk_first".to_owned());
        let mut second_workspace =
            observation("server", "server", None, 100, Some("Second"), &directory);
        second_workspace.target.agent_workspace_id = Some("wrk_second".to_owned());
        second_workspace.target.tmux_pane_id = None;
        second_workspace.target.tmux_window_id = None;

        let views =
            reconcile_session_views(vec![first_workspace, second_workspace], &[], "default");

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.agent_workspace_id, None);
    }

    fn observation(
        producer_kind: &str,
        producer_instance: &str,
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
                producer_kind: producer_kind.to_owned(),
                producer_instance: producer_instance.to_owned(),
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
                tmux_pane_id: Some("%1".to_owned()),
                tmux_window_id: Some("@1".to_owned()),
                tmux_session_name: Some("project".to_owned()),
                tmux_window_name: Some("project".to_owned()),
                directory: Some(directory.to_owned()),
                git_worktree_path: Some(directory.to_owned()),
                ..AgentLocationHints::default()
            },
        }
    }

    fn observation_for_session(
        session_id: &str,
        producer_kind: &str,
        producer_instance: &str,
        directory: &str,
        title: &str,
    ) -> AgentObservationState {
        let mut observation = observation(
            producer_kind,
            producer_instance,
            Some(AgentStatus::Working),
            100,
            Some(title),
            directory,
        );
        observation.key.session.session_id = session_id.to_owned();
        observation.target.directory = Some(directory.to_owned());
        observation.target.git_worktree_path = Some(directory.to_owned());
        if producer_kind == "server" {
            observation.target.tmux_pane_id = None;
            observation.target.tmux_window_id = None;
        }
        observation
    }

    fn git_repo_path() -> (TempDir, String) {
        let temp = tempdir().expect("temp directory should be created");
        let repo = temp.path().join("project");
        fs::create_dir(&repo).expect("repo directory should be created");
        run_git(&repo, &["init", "--initial-branch", "main"]);
        run_git(&repo, &["config", "user.email", "test@example.invalid"]);
        run_git(&repo, &["config", "user.name", "Test User"]);
        fs::write(repo.join("README.md"), "test\n").expect("readme should be written");
        run_git(&repo, &["add", "README.md"]);
        run_git(&repo, &["commit", "-m", "initial"]);
        let path = repo.display().to_string();
        (temp, path)
    }

    fn run_git(cwd: &Path, args: &[&str]) {
        let output = Command::new("git")
            .args(args)
            .current_dir(cwd)
            .output()
            .expect("git command should run");
        assert!(
            output.status.success(),
            "git {} failed\nstdout: {}\nstderr: {}",
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }

    fn pane_snapshot(
        pane_id: &str,
        window_id: &str,
        current_path: &str,
        kmux_role: Option<&str>,
    ) -> TmuxPaneSnapshot {
        pane_snapshot_in_session("project", pane_id, window_id, current_path, kmux_role)
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
            title: Some("pane title".to_owned()),
            current_command: Some("opencode".to_owned()),
            current_path: Some(current_path.to_owned()),
            pane_active: true,
            window_active: true,
            session_attached: true,
            kmux_role: kmux_role.map(str::to_owned),
        }
    }
}
