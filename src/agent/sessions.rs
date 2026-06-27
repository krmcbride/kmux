use std::collections::{BTreeMap, HashMap};
use std::path::Path;

use anyhow::Result;

use crate::paths::{infer_repo_metadata_from_paths, same_path};
use crate::state::{
    AgentLocationHints, AgentObservationState, AgentSessionKey, AgentStatus, StateStore,
};
use crate::tmux::{Tmux, TmuxPaneSnapshot, TmuxWindow};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentSessionView {
    pub key: AgentSessionKey,
    pub status: AgentStatus,
    pub status_changed_at: u64,
    pub working_elapsed_secs: u64,
    pub observed_at: u64,
    pub title: Option<String>,
    pub context: Option<String>,
    pub target: AgentLocationHints,
}

impl AgentSessionView {
    pub fn elapsed_secs(&self, now: u64) -> u64 {
        let status_age = now.saturating_sub(self.status_changed_at);
        match self.status {
            AgentStatus::Working => self.working_elapsed_secs.saturating_add(status_age),
            AgentStatus::Waiting | AgentStatus::Done => status_age,
        }
    }
}

#[derive(Debug, Clone)]
struct EnrichedObservation {
    state: AgentObservationState,
    resolved_target: Option<AgentLocationHints>,
    location_rank: u8,
}

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

fn is_candidate_for_tmux_instance(observation: &AgentObservationState, instance_id: &str) -> bool {
    observation
        .target
        .tmux_instance
        .as_deref()
        .is_none_or(|target_instance| target_instance == instance_id)
}

fn reconcile_session_views(
    observations: Vec<AgentObservationState>,
    panes: &[TmuxPaneSnapshot],
    windows: &[TmuxWindow],
    tmux_instance: &str,
) -> Vec<AgentSessionView> {
    let pane_by_id = panes
        .iter()
        .map(|pane| (pane.pane_id.clone(), pane))
        .collect::<HashMap<_, _>>();
    let window_by_id = windows
        .iter()
        .map(|window| (window.window_id.clone(), window))
        .collect::<HashMap<_, _>>();

    let mut grouped = BTreeMap::<AgentSessionKey, Vec<EnrichedObservation>>::new();
    for observation in observations {
        let (resolved_target, location_rank) = resolve_observation_target(
            &observation,
            &pane_by_id,
            &window_by_id,
            panes,
            windows,
            tmux_instance,
        );
        grouped
            .entry(observation.key.session.clone())
            .or_default()
            .push(EnrichedObservation {
                state: observation,
                resolved_target,
                location_rank,
            });
    }

    grouped
        .into_iter()
        .filter_map(|(key, observations)| session_view_from_observations(key, &observations))
        .collect()
}

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
                observation.location_rank,
            )
        })?;
    let location_observation = observations
        .iter()
        .filter(|observation| observation.resolved_target.is_some())
        .max_by_key(|observation| (observation.location_rank, observation.state.observed_at))?;
    let mut target = location_observation.resolved_target.clone()?;
    merge_target_metadata(&mut target, observations);
    enrich_missing_repo_metadata(&mut target);

    let status_changed_at = status_observation.state.status_changed_at?;
    Some(AgentSessionView {
        key,
        status: status_observation.state.status?,
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

fn status_priority(status: Option<AgentStatus>) -> u8 {
    match status {
        Some(AgentStatus::Waiting) => 3,
        Some(AgentStatus::Working) => 2,
        Some(AgentStatus::Done) => 1,
        None => 0,
    }
}

fn resolve_observation_target(
    observation: &AgentObservationState,
    pane_by_id: &HashMap<String, &TmuxPaneSnapshot>,
    window_by_id: &HashMap<String, &TmuxWindow>,
    panes: &[TmuxPaneSnapshot],
    windows: &[TmuxWindow],
    tmux_instance: &str,
) -> (Option<AgentLocationHints>, u8) {
    if let Some(pane_id) = observation.target.pane_id.as_deref()
        && let Some(pane) = pane_by_id.get(pane_id)
        && !is_sidebar_pane(pane)
        && pane_matches_target(
            &observation.target,
            pane,
            window_by_id.get(&pane.window_id).copied(),
        )
    {
        let mut target = observation.target.clone();
        enrich_target_from_pane(&mut target, pane, tmux_instance);
        if let Some(window) = window_by_id.get(&pane.window_id) {
            enrich_target_from_window(&mut target, window, tmux_instance);
        }
        enrich_missing_repo_metadata(&mut target);
        return (Some(target), 5);
    }

    if let Some(window_id) = observation.target.window_id.as_deref()
        && let Some(window) = window_by_id.get(window_id)
        && window_matches_target(&observation.target, window)
    {
        let mut target = observation.target.clone();
        enrich_target_from_window(&mut target, window, tmux_instance);
        enrich_missing_repo_metadata(&mut target);
        return (Some(target), 4);
    }

    if let Some((window, pane)) =
        unique_window_pane_by_current_path(&observation.target, panes, windows)
    {
        let mut target = observation.target.clone();
        enrich_target_from_pane(&mut target, pane, tmux_instance);
        enrich_target_from_window(&mut target, window, tmux_instance);
        enrich_missing_repo_metadata(&mut target);
        return (Some(target), 3);
    }

    if let Some(window) = unique_window_by_worktree_metadata(&observation.target, windows) {
        let mut target = observation.target.clone();
        enrich_target_from_window(&mut target, window, tmux_instance);
        enrich_missing_repo_metadata(&mut target);
        return (Some(target), 2);
    }

    if let Some(window) = window_by_session_and_name(&observation.target, windows) {
        let mut target = observation.target.clone();
        enrich_target_from_window(&mut target, window, tmux_instance);
        enrich_missing_repo_metadata(&mut target);
        return (Some(target), 1);
    }

    (None, 0)
}

fn unique_window_pane_by_current_path<'a>(
    target: &AgentLocationHints,
    panes: &'a [TmuxPaneSnapshot],
    windows: &'a [TmuxWindow],
) -> Option<(&'a TmuxWindow, &'a TmuxPaneSnapshot)> {
    let mut by_window = BTreeMap::<&str, (&TmuxWindow, Vec<&TmuxPaneSnapshot>)>::new();
    for pane in panes {
        if is_sidebar_pane(pane) || !target_matches_path(target, pane.current_path.as_deref()) {
            continue;
        }
        let window = windows
            .iter()
            .find(|window| window.window_id == pane.window_id)?;
        by_window
            .entry(window.window_id.as_str())
            .or_insert_with(|| (window, Vec::new()))
            .1
            .push(pane);
    }

    let mut matches = by_window.into_values();
    let (window, panes) = matches.next()?;
    if matches.next().is_some() {
        return None;
    }

    panes
        .iter()
        .copied()
        .find(|pane| pane.pane_active)
        .or_else(|| panes.first().copied())
        .map(|pane| (window, pane))
}

fn pane_matches_target(
    target: &AgentLocationHints,
    pane: &TmuxPaneSnapshot,
    window: Option<&TmuxWindow>,
) -> bool {
    if target
        .window_id
        .as_deref()
        .is_some_and(|window_id| window_id != pane.window_id)
    {
        return false;
    }
    if !session_window_hints_match(target, &pane.session_name, &pane.window_name) {
        return false;
    }

    path_hints_match(
        target,
        [
            pane.current_path.as_deref(),
            window.and_then(|window| window.kmux_worktree_path.as_deref()),
        ],
    )
}

fn window_matches_target(target: &AgentLocationHints, window: &TmuxWindow) -> bool {
    session_window_hints_match(target, &window.session_name, &window.window_name)
        && path_hints_match(target, [window.kmux_worktree_path.as_deref(), None])
}

fn session_window_hints_match(
    target: &AgentLocationHints,
    session_name: &str,
    window_name: &str,
) -> bool {
    target
        .session_name
        .as_deref()
        .is_none_or(|target_session| target_session == session_name)
        && target
            .window_name
            .as_deref()
            .is_none_or(|target_window| target_window == window_name)
}

fn path_hints_match<const N: usize>(
    target: &AgentLocationHints,
    candidates: [Option<&str>; N],
) -> bool {
    if target.directory.is_none() && target.worktree_path.is_none() {
        return true;
    }

    candidates
        .into_iter()
        .flatten()
        .any(|candidate| target_matches_path(target, Some(candidate)))
}

fn unique_window_by_worktree_metadata<'a>(
    target: &AgentLocationHints,
    windows: &'a [TmuxWindow],
) -> Option<&'a TmuxWindow> {
    unique_window(
        windows
            .iter()
            .filter(|window| target_matches_path(target, window.kmux_worktree_path.as_deref())),
    )
}

fn target_matches_path(target: &AgentLocationHints, candidate: Option<&str>) -> bool {
    let Some(candidate) = candidate else {
        return false;
    };
    [target.directory.as_deref(), target.worktree_path.as_deref()]
        .into_iter()
        .flatten()
        .any(|path| same_path(Path::new(path), Path::new(candidate)))
}

fn window_by_session_and_name<'a>(
    target: &AgentLocationHints,
    windows: &'a [TmuxWindow],
) -> Option<&'a TmuxWindow> {
    let (Some(session_name), Some(window_name)) = (
        target.session_name.as_deref(),
        target.window_name.as_deref(),
    ) else {
        return None;
    };

    windows.iter().find(|window| {
        window.session_name == session_name
            && window.window_name == window_name
            && window_matches_target(target, window)
    })
}

fn unique_window<'a>(mut windows: impl Iterator<Item = &'a TmuxWindow>) -> Option<&'a TmuxWindow> {
    let window = windows.next()?;
    windows.next().is_none().then_some(window)
}

fn enrich_target_from_pane(
    target: &mut AgentLocationHints,
    pane: &TmuxPaneSnapshot,
    tmux_instance: &str,
) {
    target.tmux_instance = Some(tmux_instance.to_owned());
    target.pane_id = Some(pane.pane_id.clone());
    target.window_id = Some(pane.window_id.clone());
    target.session_name = Some(pane.session_name.clone());
    target.window_name = Some(pane.window_name.clone());
    if pane.title.is_some() {
        target.pane_title = pane.title.clone();
    }
    if pane.current_command.is_some() {
        target.pane_current_command = pane.current_command.clone();
    }
    if pane.current_path.is_some() {
        target.pane_current_path = pane.current_path.clone();
    }
}

fn enrich_target_from_window(
    target: &mut AgentLocationHints,
    window: &TmuxWindow,
    tmux_instance: &str,
) {
    target.tmux_instance = Some(tmux_instance.to_owned());
    target.window_id = Some(window.window_id.clone());
    target.session_name = Some(window.session_name.clone());
    target.window_name = Some(window.window_name.clone());
    if window.kmux_worktree_handle.is_some() {
        target.worktree_handle = window.kmux_worktree_handle.clone();
    }
    if window.kmux_worktree_path.is_some() {
        target.worktree_path = window.kmux_worktree_path.clone();
    }
    if window.kmux_worktree_branch.is_some() {
        target.branch = window.kmux_worktree_branch.clone();
    }
}

fn merge_target_metadata(target: &mut AgentLocationHints, observations: &[EnrichedObservation]) {
    let mut sorted = observations.iter().collect::<Vec<_>>();
    sorted.sort_by_key(|observation| observation.state.observed_at);
    for observation in sorted.into_iter().rev() {
        fill_missing_target_metadata(target, &observation.state.target);
        if let Some(resolved) = &observation.resolved_target {
            fill_missing_target_metadata(target, resolved);
        }
    }
}

fn fill_missing_target_metadata(target: &mut AgentLocationHints, fallback: &AgentLocationHints) {
    if target.tmux_instance.is_none() {
        target.tmux_instance = fallback.tmux_instance.clone();
    }
    if target.pane_id.is_none() {
        target.pane_id = fallback.pane_id.clone();
    }
    if target.window_id.is_none() {
        target.window_id = fallback.window_id.clone();
    }
    if target.session_name.is_none() {
        target.session_name = fallback.session_name.clone();
    }
    if target.window_name.is_none() {
        target.window_name = fallback.window_name.clone();
    }
    if target.pane_title.is_none() {
        target.pane_title = fallback.pane_title.clone();
    }
    if target.pane_current_command.is_none() {
        target.pane_current_command = fallback.pane_current_command.clone();
    }
    if target.pane_current_path.is_none() {
        target.pane_current_path = fallback.pane_current_path.clone();
    }
    if target.repo_name.is_none() {
        target.repo_name = fallback.repo_name.clone();
    }
    if target.repo_path.is_none() {
        target.repo_path = fallback.repo_path.clone();
    }
    if target.worktree_handle.is_none() {
        target.worktree_handle = fallback.worktree_handle.clone();
    }
    if target.worktree_path.is_none() {
        target.worktree_path = fallback.worktree_path.clone();
    }
    if target.branch.is_none() {
        target.branch = fallback.branch.clone();
    }
    if target.directory.is_none() {
        target.directory = fallback.directory.clone();
    }
}

fn enrich_missing_repo_metadata(target: &mut AgentLocationHints) {
    if target.repo_name.is_some() && target.repo_path.is_some() && target.branch.is_some() {
        return;
    }

    let metadata = infer_repo_metadata_from_paths(&[
        target.directory.as_deref(),
        target.worktree_path.as_deref(),
        target.pane_current_path.as_deref(),
    ]);
    if target.repo_name.is_none() {
        target.repo_name = metadata.repo_name;
    }
    if target.repo_path.is_none() {
        target.repo_path = metadata.repo_path;
    }
    if target.branch.is_none() {
        target.branch = metadata.branch;
    }
}

fn is_sidebar_pane(pane: &TmuxPaneSnapshot) -> bool {
    pane.kmux_role.as_deref() == Some("sidebar")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AgentObservationKey, AgentObservationState};

    #[test]
    fn merges_tui_and_server_observations_into_one_session_view() {
        let tui = observation(
            "tui",
            "default/%1",
            Some(AgentStatus::Done),
            100,
            Some("Pane title"),
        );
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Waiting),
            200,
            Some("Server title"),
        );
        server.context = Some("55.2K".to_owned());
        server.target.pane_id = None;
        server.target.window_id = None;

        let views = reconcile_session_views(
            vec![tui, server],
            &[pane_snapshot("%1", "@1", "/repo/project", None)],
            &[window_snapshot("@1", "project", Some("/repo/project"))],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].status, AgentStatus::Waiting);
        assert_eq!(views[0].title.as_deref(), Some("Server title"));
        assert_eq!(views[0].context.as_deref(), Some("55.2K"));
        assert_eq!(views[0].target.pane_id.as_deref(), Some("%1"));
        assert_eq!(views[0].target.window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn keeps_four_sessions_from_two_directories_as_four_views() {
        let observations = [
            observation_for_session("ses_a", "tui", "default/%1", "/repo/project", "A"),
            observation_for_session("ses_a", "server", "server", "/repo/project", "A server"),
            observation_for_session("ses_b", "tui", "default/%1", "/repo/project", "B"),
            observation_for_session("ses_b", "server", "server", "/repo/project", "B server"),
            observation_for_session(
                "ses_c",
                "tui",
                "default/%2",
                "/repo/project__worktrees/feature",
                "C",
            ),
            observation_for_session(
                "ses_c",
                "server",
                "server",
                "/repo/project__worktrees/feature",
                "C server",
            ),
            observation_for_session(
                "ses_d",
                "tui",
                "default/%2",
                "/repo/project__worktrees/feature",
                "D",
            ),
            observation_for_session(
                "ses_d",
                "server",
                "server",
                "/repo/project__worktrees/feature",
                "D server",
            ),
        ];

        let views = reconcile_session_views(
            observations.into_iter().collect(),
            &[
                pane_snapshot("%1", "@1", "/repo/project", None),
                pane_snapshot("%2", "@2", "/repo/project__worktrees/feature", None),
            ],
            &[
                window_snapshot("@1", "project", Some("/repo/project")),
                window_snapshot("@2", "feature", Some("/repo/project__worktrees/feature")),
            ],
            "default",
        );

        assert_eq!(views.len(), 4);
        assert_eq!(
            views
                .iter()
                .filter(|view| view.target.window_id.as_deref() == Some("@1"))
                .count(),
            2
        );
        assert_eq!(
            views
                .iter()
                .filter(|view| view.target.window_id.as_deref() == Some("@2"))
                .count(),
            2
        );
    }

    #[test]
    fn resolves_server_only_observation_by_non_sidebar_pane_current_path() {
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
        );
        server.target.pane_id = None;
        server.target.window_id = None;
        server.target.directory = Some("/repo/project__worktrees/feature".to_owned());
        server.target.worktree_path = None;

        let views = reconcile_session_views(
            vec![server],
            &[
                pane_snapshot("%sidebar", "@1", "/repo/project", Some("sidebar")),
                pane_snapshot("%2", "@1", "/repo/project__worktrees/feature", None),
            ],
            &[window_snapshot("@1", "feature", None)],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.pane_id.as_deref(), Some("%2"));
        assert_eq!(views[0].target.window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn resolves_server_only_observation_when_one_window_has_multiple_matching_panes() {
        let mut server = observation(
            "server",
            "http://127.0.0.1:4096",
            Some(AgentStatus::Working),
            100,
            Some("Server only"),
        );
        server.target.pane_id = None;
        server.target.window_id = None;
        server.target.session_name = None;
        server.target.window_name = None;
        server.target.directory = Some("/repo/project".to_owned());
        server.target.worktree_path = None;
        let mut inactive = pane_snapshot("%2", "@1", "/repo/project", None);
        inactive.pane_active = false;
        let active = pane_snapshot("%3", "@1", "/repo/project", None);

        let views = reconcile_session_views(
            vec![server],
            &[inactive, active],
            &[window_snapshot("@1", "project", None)],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].target.pane_id.as_deref(), Some("%3"));
        assert_eq!(views[0].target.window_id.as_deref(), Some("@1"));
    }

    #[test]
    fn stale_exact_pane_hint_is_rejected_when_path_hints_conflict() {
        let mut stale = observation(
            "tui",
            "default/%1",
            Some(AgentStatus::Working),
            100,
            Some("Stale pane"),
        );
        stale.target.session_name = None;
        stale.target.window_name = None;
        stale.target.directory = Some("/repo/old".to_owned());
        stale.target.worktree_path = Some("/repo/old".to_owned());

        let views = reconcile_session_views(
            vec![stale],
            &[pane_snapshot("%1", "@1", "/repo/new", None)],
            &[window_snapshot("@1", "project", Some("/repo/new"))],
            "default",
        );

        assert!(views.is_empty());
    }

    #[test]
    fn session_window_name_fallback_rejects_conflicting_path_hints() {
        let mut stale = observation(
            "server",
            "server",
            Some(AgentStatus::Working),
            100,
            Some("Stale window"),
        );
        stale.target.pane_id = None;
        stale.target.window_id = None;
        stale.target.directory = Some("/repo/old".to_owned());
        stale.target.worktree_path = Some("/repo/old".to_owned());

        let views = reconcile_session_views(
            vec![stale],
            &[pane_snapshot("%1", "@1", "/repo/new", None)],
            &[window_snapshot("@1", "project", Some("/repo/new"))],
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
        );
        server.target.pane_id = None;
        server.target.window_id = None;
        server.target.session_name = None;
        server.target.window_name = None;
        server.target.directory = Some("/repo/missing".to_owned());
        server.target.worktree_path = None;

        let views = reconcile_session_views(
            vec![server],
            &[pane_snapshot("%2", "@1", "/repo/project", None)],
            &[window_snapshot("@1", "project", None)],
            "default",
        );

        assert!(views.is_empty());
    }

    #[test]
    fn metadata_only_observations_can_update_title_without_status() {
        let status = observation(
            "tui",
            "default/%1",
            Some(AgentStatus::Working),
            100,
            Some("Old"),
        );
        let mut metadata = observation("server", "server", None, 200, Some("Renamed"));
        metadata.target.pane_id = None;
        metadata.target.window_id = None;

        let views = reconcile_session_views(
            vec![status, metadata],
            &[pane_snapshot("%1", "@1", "/repo/project", None)],
            &[window_snapshot("@1", "project", Some("/repo/project"))],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].status, AgentStatus::Working);
        assert_eq!(views[0].title.as_deref(), Some("Renamed"));
    }

    #[test]
    fn metadata_only_update_does_not_refresh_status_precedence() {
        let mut stale_working = observation(
            "tui",
            "default/%1",
            Some(AgentStatus::Working),
            100,
            Some("Renamed"),
        );
        stale_working.observed_at = 300;
        let waiting = observation(
            "server",
            "server",
            Some(AgentStatus::Waiting),
            200,
            Some("Waiting"),
        );

        let views = reconcile_session_views(
            vec![stale_working, waiting],
            &[pane_snapshot("%1", "@1", "/repo/project", None)],
            &[window_snapshot("@1", "project", Some("/repo/project"))],
            "default",
        );

        assert_eq!(views.len(), 1);
        assert_eq!(views[0].status, AgentStatus::Waiting);
        assert_eq!(views[0].title.as_deref(), Some("Renamed"));
    }

    fn observation(
        producer_kind: &str,
        producer_instance: &str,
        status: Option<AgentStatus>,
        observed_at: u64,
        title: Option<&str>,
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
            status,
            status_observed_at: status.map(|_| observed_at),
            status_changed_at,
            working_elapsed_secs: 0,
            observed_at,
            title: title.map(str::to_owned),
            context: None,
            target: AgentLocationHints {
                tmux_instance: Some("default".to_owned()),
                pane_id: Some("%1".to_owned()),
                window_id: Some("@1".to_owned()),
                session_name: Some("project".to_owned()),
                window_name: Some("project".to_owned()),
                directory: Some("/repo/project".to_owned()),
                worktree_path: Some("/repo/project".to_owned()),
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
        );
        observation.key.session.session_id = session_id.to_owned();
        observation.target.directory = Some(directory.to_owned());
        observation.target.worktree_path = Some(directory.to_owned());
        if directory.contains("feature") {
            observation.target.pane_id = Some("%2".to_owned());
            observation.target.window_id = Some("@2".to_owned());
            observation.target.window_name = Some("feature".to_owned());
        }
        if producer_kind == "server" {
            observation.target.pane_id = None;
            observation.target.window_id = None;
        }
        observation
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
            window_name: if window_id == "@2" {
                "feature"
            } else {
                "project"
            }
            .to_owned(),
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
            kmux_worktree_handle: Some(window_name.to_owned()),
            kmux_worktree_path: worktree_path.map(str::to_owned),
            kmux_worktree_branch: Some(window_name.to_owned()),
        }
    }
}
