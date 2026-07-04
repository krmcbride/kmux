//! Reconciliation of persisted agent observations with live tmux state.
//!
//! External producers report a current directory for each logical session. This
//! module attaches those observations to live tmux windows by matching that
//! directory to live window directory metadata.

use std::collections::BTreeMap;

use anyhow::Result;

use crate::paths::infer_repo_metadata_from_paths;
use crate::state::{
    AgentLocationHints, AgentObservationState, AgentSessionKey, AgentStatus, StateStore,
};
use crate::tmux::{Tmux, TmuxPaneSnapshot, TmuxWindow};

use super::directory::{AgentDirectoryAttachment, AgentDirectoryResolver};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Reconciled view of one logical agent session for UI and status output.
pub struct AgentSessionView {
    pub key: AgentSessionKey,
    pub directory_key: Option<String>,
    pub created_at: u64,
    pub status: AgentStatus,
    pub status_observed_at: u64,
    pub status_changed_at: u64,
    pub working_elapsed_secs: u64,
    pub observed_at: u64,
    pub title: Option<String>,
    pub context: Option<String>,
    pub target: AgentLocationHints,
}

impl AgentSessionView {
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
}

/// Reconcile persisted agent observations with live tmux window state.
pub fn session_views(store: &StateStore, tmux: &Tmux) -> Result<Vec<AgentSessionView>> {
    let tmux_instance = tmux.instance_id();
    let observations = store
        .list_observations()?
        .into_iter()
        .filter(|observation| is_candidate_for_tmux_instance(observation, &tmux_instance))
        .collect::<Vec<_>>();
    if observations.is_empty() {
        return Ok(Vec::new());
    }

    let panes = tmux.list_pane_snapshots().unwrap_or_default();
    let windows = tmux.list_windows(None).unwrap_or_default();
    Ok(reconcile_session_views(
        observations,
        &panes,
        &windows,
        &tmux_instance,
    ))
}

#[derive(Debug, Clone)]
struct EnrichedObservation {
    state: AgentObservationState,
    directory_attachment: Option<AgentDirectoryAttachment>,
    resolved_target: Option<AgentLocationHints>,
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

// Group observations by logical agent session after attaching each reported
// directory to exactly one live tmux window directory.
fn reconcile_session_views(
    observations: Vec<AgentObservationState>,
    panes: &[TmuxPaneSnapshot],
    windows: &[TmuxWindow],
    tmux_instance: &str,
) -> Vec<AgentSessionView> {
    let mut directory_resolver = AgentDirectoryResolver::default();

    let mut grouped = BTreeMap::<AgentSessionKey, Vec<EnrichedObservation>>::new();
    for observation in observations {
        let directory_attachment = directory_resolver.attachment_for_hints(&observation.target);
        let resolved_target = resolve_observation_target(
            &observation,
            directory_attachment.as_ref(),
            panes,
            windows,
            tmux_instance,
            &mut directory_resolver,
        );
        grouped
            .entry(observation.key.session.clone())
            .or_default()
            .push(EnrichedObservation {
                state: observation,
                directory_attachment,
                resolved_target,
            });
    }

    let views = grouped
        .into_iter()
        .filter_map(|(key, observations)| session_view_from_observations(key, &observations))
        .collect::<Vec<_>>();
    collapse_directory_views(views)
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
    let newest_observed_at = observations
        .iter()
        .map(|observation| observation.state.observed_at)
        .max()?;
    let location_observation = observations
        .iter()
        .filter(|observation| observation.state.observed_at == newest_observed_at)
        .find(|observation| observation.resolved_target.is_some())?;
    let mut target = location_observation.resolved_target.clone()?;
    merge_target_metadata(&mut target, observations);
    target.agent_workspace_id = newest_agent_workspace_id(observations);
    enrich_missing_repo_metadata(&mut target);

    let status_changed_at = status_observation.state.status_changed_at?;
    let status_observed_at = observation_status_observed_at(&status_observation.state);
    let directory_key = location_observation
        .directory_attachment
        .as_ref()
        .map(|attachment| attachment.key().to_owned());
    Some(AgentSessionView {
        key,
        directory_key,
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

fn collapse_directory_views(views: Vec<AgentSessionView>) -> Vec<AgentSessionView> {
    let mut by_target = BTreeMap::<String, AgentSessionView>::new();
    for view in views {
        let key = view_group_key(&view);
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

fn view_group_key(view: &AgentSessionView) -> String {
    view.directory_key
        .as_ref()
        .map(|key| format!("directory:{key}"))
        .or_else(|| {
            view.target
                .tmux_window_id
                .as_ref()
                .map(|window_id| format!("window:{window_id}"))
        })
        .unwrap_or_else(|| format!("session:{}/{}", view.key.agent_kind, view.key.session_id))
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

// A producer-visible row exists only when the reported directory matches exactly
// one live tmux window directory.
fn resolve_observation_target(
    observation: &AgentObservationState,
    directory_attachment: Option<&AgentDirectoryAttachment>,
    panes: &[TmuxPaneSnapshot],
    windows: &[TmuxWindow],
    tmux_instance: &str,
    directory_resolver: &mut AgentDirectoryResolver,
) -> Option<AgentLocationHints> {
    let attachment = directory_attachment?;
    let window =
        unique_window_by_directory_attachment(attachment, panes, windows, directory_resolver)?;
    let mut target = AgentLocationHints::default();
    apply_directory_attachment(&mut target, attachment);
    enrich_target_from_window(&mut target, window, tmux_instance);
    merge_resolved_observation_metadata(&mut target, &observation.target);
    enrich_missing_repo_metadata(&mut target);
    Some(target)
}

fn unique_window_by_directory_attachment<'a>(
    attachment: &AgentDirectoryAttachment,
    panes: &[TmuxPaneSnapshot],
    windows: &'a [TmuxWindow],
    directory_resolver: &mut AgentDirectoryResolver,
) -> Option<&'a TmuxWindow> {
    let marked_matches = windows
        .iter()
        .filter(|window| marked_window_directory_matches(attachment, window, directory_resolver))
        .collect::<Vec<_>>();
    match marked_matches.as_slice() {
        [window] => Some(window),
        [] => unique_unmarked_window_by_directory_attachment(
            attachment,
            panes,
            windows,
            directory_resolver,
        ),
        _ => None,
    }
}

fn unique_unmarked_window_by_directory_attachment<'a>(
    attachment: &AgentDirectoryAttachment,
    panes: &[TmuxPaneSnapshot],
    windows: &'a [TmuxWindow],
    directory_resolver: &mut AgentDirectoryResolver,
) -> Option<&'a TmuxWindow> {
    let unmarked_matches = windows
        .iter()
        .filter(|window| {
            unmarked_window_directory_matches(attachment, window, panes, directory_resolver)
        })
        .collect::<Vec<_>>();
    match unmarked_matches.as_slice() {
        [window] => Some(window),
        _ => None,
    }
}

fn marked_window_directory_matches(
    attachment: &AgentDirectoryAttachment,
    window: &TmuxWindow,
    directory_resolver: &mut AgentDirectoryResolver,
) -> bool {
    directory_resolver.attachment_matches_path(attachment, window.kmux_workspace_path.as_deref())
}

fn unmarked_window_directory_matches(
    attachment: &AgentDirectoryAttachment,
    window: &TmuxWindow,
    panes: &[TmuxPaneSnapshot],
    directory_resolver: &mut AgentDirectoryResolver,
) -> bool {
    directory_resolver
        .attachment_matches_path(attachment, unmarked_single_pane_directory(window, panes))
}

fn unmarked_single_pane_directory<'a>(
    window: &TmuxWindow,
    panes: &'a [TmuxPaneSnapshot],
) -> Option<&'a str> {
    if window.kmux_workspace_path.is_some() {
        return None;
    }

    let mut candidates = panes.iter().filter(|pane| {
        pane.window_id == window.window_id && pane.kmux_role.as_deref() != Some("sidebar")
    });
    let pane = candidates.next()?;
    candidates
        .next()
        .is_none()
        .then_some(pane.current_path.as_deref())
        .flatten()
}

fn apply_directory_attachment(
    target: &mut AgentLocationHints,
    attachment: &AgentDirectoryAttachment,
) {
    if target.directory.is_none() {
        target.directory = Some(attachment.reported_path().to_owned());
    }
    target.git_worktree_path = Some(attachment.path().to_owned());
}

fn enrich_target_from_window(
    target: &mut AgentLocationHints,
    window: &TmuxWindow,
    tmux_instance: &str,
) {
    target.tmux_instance = Some(tmux_instance.to_owned());
    target.tmux_window_id = Some(window.window_id.clone());
    target.tmux_session_name = Some(window.session_name.clone());
    target.tmux_window_name = Some(window.window_name.clone());
    if window.kmux_workspace_path.is_some() {
        target.git_worktree_path = window.kmux_workspace_path.clone();
    }
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
    use tempfile::tempdir;

    #[test]
    fn merges_tui_and_server_observations_into_one_session_view() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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
            &[],
            &[window_snapshot("@1", "project", Some(&directory))],
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
        assert_eq!(views[0].target.tmux_pane_id, None);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn collapses_multiple_sessions_in_one_directory_to_one_primary_view() {
        let root = tempdir().expect("root temp directory should be created");
        let root = root.path().display().to_string();
        let feature = tempdir().expect("feature temp directory should be created");
        let feature = feature.path().display().to_string();
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
            &[],
            &[
                window_snapshot("@1", "project", Some(&root)),
                window_snapshot("@2", "feature", Some(&feature)),
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
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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

        let views = reconcile_session_views(
            vec![done, waiting],
            &[],
            &[window_snapshot("@1", "project", Some(&directory))],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].key.session_id, "ses_waiting");
        assert_eq!(views[0].status, AgentStatus::Waiting);
    }

    #[test]
    fn directory_only_observation_attaches_to_matching_kmux_window() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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
            &[],
            &[window_snapshot("@1", "feature", Some(&directory))],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.tmux_pane_id, None);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn directory_observation_attaches_to_unmarked_single_pane_window() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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
            &[unmarked_window_snapshot("@1", "nvim")],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
        assert_eq!(views[0].target.tmux_pane_id, None);
    }

    #[test]
    fn git_worktree_path_without_directory_does_not_attach() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
            &directory,
        );
        server.target.directory = None;
        server.target.git_worktree_path = Some(directory.clone());

        let views = reconcile_session_views(
            vec![server],
            &[],
            &[window_snapshot("@1", "project", Some(&directory))],
            "default",
        );

        assert!(views.is_empty());
    }

    #[test]
    fn codex_like_directory_only_observation_attaches_by_window_path() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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
            &[],
            &[window_snapshot("@1", "codex", Some(&directory))],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].key.agent_kind, "codex");
        assert_eq!(views[0].key.session_id, "thread_123");
        assert_eq!(views[0].target.tmux_pane_id, None);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn duplicate_windows_for_directory_are_ambiguous_without_exact_pane() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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
            &[],
            &[
                window_snapshot("@1", "project-a", Some(&directory)),
                window_snapshot("@2", "project-b", Some(&directory)),
            ],
            "default",
        );

        assert!(views.is_empty());
    }

    #[test]
    fn marked_workspace_path_wins_over_unmarked_single_pane_fallback() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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
            &[
                window_snapshot("@1", "project", Some(&directory)),
                unmarked_window_snapshot("@2", "scratch"),
            ],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn unmarked_multi_pane_window_does_not_attach_by_current_path() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
        let other = tempdir().expect("other temp directory should be created");
        let other = other.path().display().to_string();
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
            &[unmarked_window_snapshot("@1", "project")],
            "default",
        );

        assert!(views.is_empty());
    }

    #[test]
    fn observation_without_matching_kmux_window_is_hidden() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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
            &[],
            &[window_snapshot("@1", "project", None)],
            "default",
        );

        assert!(views.is_empty());
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

        let views = reconcile_session_views(vec![server], &[], &[], "default");

        assert!(views.is_empty());
    }

    #[test]
    fn latest_observation_must_resolve_to_live_window() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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

        let views = reconcile_session_views(
            vec![old, newest],
            &[],
            &[window_snapshot("@1", "project", Some(&directory))],
            "default",
        );

        assert!(views.is_empty());
    }

    #[test]
    fn metadata_only_observations_can_update_title_without_status() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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

        let views = reconcile_session_views(
            vec![status, metadata],
            &[],
            &[window_snapshot("@1", "project", Some(&directory))],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].status, AgentStatus::Working);
        assert_eq!(views[0].title.as_deref(), Some("Renamed"));
    }

    #[test]
    fn metadata_only_update_does_not_refresh_status_precedence() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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

        let views = reconcile_session_views(
            vec![stale_working, waiting],
            &[],
            &[window_snapshot("@1", "project", Some(&directory))],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].status, AgentStatus::Waiting);
        assert_eq!(views[0].title.as_deref(), Some("Renamed"));
    }

    #[test]
    fn newer_missing_agent_workspace_id_clears_stale_scope() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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

        let views = reconcile_session_views(
            vec![old_workspace, cleared_workspace],
            &[],
            &[window_snapshot("@1", "project", Some(&directory))],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.agent_workspace_id, None);
    }

    #[test]
    fn equal_timestamp_missing_agent_workspace_id_clears_stale_scope() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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

        let views = reconcile_session_views(
            vec![old_workspace, cleared_workspace],
            &[],
            &[window_snapshot("@1", "project", Some(&directory))],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.agent_workspace_id, None);
    }

    #[test]
    fn equal_timestamp_conflicting_agent_workspace_ids_clear_scope() {
        let directory = tempdir().expect("temp directory should be created");
        let directory = directory.path().display().to_string();
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

        let views = reconcile_session_views(
            vec![first_workspace, second_workspace],
            &[],
            &[window_snapshot("@1", "project", Some(&directory))],
            "default",
        );

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

    fn window_snapshot(
        window_id: &str,
        window_name: &str,
        worktree_path: Option<&str>,
    ) -> TmuxWindow {
        TmuxWindow {
            session_name: "project".to_owned(),
            window_id: window_id.to_owned(),
            window_index: "1".to_owned(),
            window_name: window_name.to_owned(),
            active: true,
            kmux_workspace_path: worktree_path.map(str::to_owned),
        }
    }

    fn unmarked_window_snapshot(window_id: &str, window_name: &str) -> TmuxWindow {
        TmuxWindow {
            session_name: "project".to_owned(),
            window_id: window_id.to_owned(),
            window_index: "1".to_owned(),
            window_name: window_name.to_owned(),
            active: true,
            kmux_workspace_path: None,
        }
    }

    fn pane_snapshot(
        pane_id: &str,
        window_id: &str,
        current_path: &str,
        kmux_role: Option<&str>,
    ) -> TmuxPaneSnapshot {
        TmuxPaneSnapshot {
            session_name: "project".to_owned(),
            window_id: window_id.to_owned(),
            window_index: "1".to_owned(),
            window_name: "project".to_owned(),
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
