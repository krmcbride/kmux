use super::*;
use crate::agent::workspace_activity::workspace_activities_from_sessions;
use crate::state::{AgentObservationKey, AgentObservationState};
use crate::tmux::{TmuxPaneActivity, TmuxPaneGeometry, TmuxPaneIdentity, TmuxPanePlacement};
use std::collections::HashMap;

#[derive(Default)]
struct FakeWorkspaceResolver {
    attachments: HashMap<String, AgentWorkspaceAttachment>,
}

impl FakeWorkspaceResolver {
    fn with_path(path: &str) -> Self {
        Self::with_paths(&[path])
    }

    fn with_paths(paths: &[&str]) -> Self {
        Self {
            attachments: paths
                .iter()
                .map(|path| ((*path).to_owned(), AgentWorkspaceAttachment::for_test(path)))
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

fn reconcile_with_fake_workspace_resolver(
    observations: Vec<AgentObservationState>,
    panes: &[TmuxPaneSnapshot],
    tmux_instance: &str,
    workspace_paths: &[&str],
) -> Vec<ResolvedAgentSession> {
    let mut resolver = FakeWorkspaceResolver::with_paths(workspace_paths);
    reconcile_agent_sessions(observations, panes, tmux_instance, &mut resolver)
}

#[test]
fn pure_reconciliation_returns_no_sessions_without_observations() {
    let mut resolver = FakeWorkspaceResolver::with_path("/repo/project");

    let views = reconcile_agent_sessions(Vec::new(), &[], "default", &mut resolver);

    assert!(views.is_empty());
}

#[test]
fn tmux_snapshot_failure_becomes_an_empty_snapshot() {
    let (panes, snapshot_ok) = pane_snapshots_or_empty(Err(anyhow::anyhow!("tmux unavailable")));

    assert!(panes.is_empty());
    assert!(!snapshot_ok);
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
    let directory = "/repo/project-alpha";
    let first = observation(
        "reporter-a",
        "instance-1",
        Some(AgentStatus::Done),
        100,
        Some("First title"),
        directory,
    );
    let mut second = observation(
        "reporter-b",
        "instance-2",
        Some(AgentStatus::Waiting),
        200,
        Some("Second title"),
        directory,
    );
    second.context = Some("55.2K".to_owned());

    let views = reconcile_with_fake_workspace_resolver(
        vec![first, second],
        &[pane_snapshot("%1", "@1", directory, None)],
        "default",
        &[directory],
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
    let root = "/repo/project-alpha";
    let feature = "/repo/project-beta";
    let observations = [
        observation_for_session("ses_a", "reporter-a", "instance-1", root, "A"),
        observation_for_session("ses_a", "reporter-b", "instance-2", root, "A second"),
        observation_for_session("ses_b", "reporter-a", "instance-1", root, "B"),
        observation_for_session("ses_b", "reporter-b", "instance-2", root, "B second"),
        observation_for_session("ses_c", "reporter-a", "instance-1", feature, "C"),
        observation_for_session("ses_c", "reporter-b", "instance-2", feature, "C second"),
        observation_for_session("ses_d", "reporter-a", "instance-1", feature, "D"),
        observation_for_session("ses_d", "reporter-b", "instance-2", feature, "D second"),
    ];

    let sessions = reconcile_with_fake_workspace_resolver(
        observations.into_iter().collect(),
        &[
            pane_snapshot("%1", "@1", root, None),
            pane_snapshot("%2", "@2", feature, None),
        ],
        "default",
        &[root, feature],
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
    let directory = "/repo/project-alpha";
    let first = observation(
        "reporter-a",
        "instance-1",
        Some(AgentStatus::Working),
        100,
        Some("First"),
        directory,
    );
    let second = observation(
        "reporter-b",
        "instance-2",
        Some(AgentStatus::Working),
        200,
        Some("Second"),
        directory,
    );
    let views = reconcile_with_fake_workspace_resolver(
        vec![first, second],
        &[pane_snapshot("%1", "@1", directory, None)],
        "default",
        &[directory],
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
    let directory = "/repo/project-alpha";
    let server = observation(
        "server",
        "http://127.0.0.1:4096",
        Some(AgentStatus::Working),
        100,
        Some("Server only"),
        directory,
    );
    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[pane_snapshot("%1", "@1", directory, None)],
        "default",
        &[directory],
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
    let directory = "/repo/project-alpha";
    let server = observation(
        "server",
        "http://127.0.0.1:4096",
        Some(AgentStatus::Working),
        100,
        Some("Server only"),
        directory,
    );
    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[
            pane_snapshot("%sidebar", "@1", "/repo/sidebar", Some("sidebar")),
            pane_snapshot("%1", "@1", directory, None),
        ],
        "default",
        &[directory],
    );

    assert_eq!(views.len(), 1);
    assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
    assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
}

#[test]
fn codex_like_directory_only_observation_attaches_by_window_path() {
    let directory = "/repo/project-alpha";
    let mut server = observation(
        "server",
        "codex-app-server",
        Some(AgentStatus::Waiting),
        100,
        Some("Codex task"),
        directory,
    );
    server.key.session.agent_kind = "codex".to_owned();
    server.key.session.session_id = "thread_123".to_owned();

    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[pane_snapshot("%1", "@1", directory, None)],
        "default",
        &[directory],
    );

    assert_eq!(views.len(), 1);
    assert_eq!(views[0].key.agent_kind, "codex");
    assert_eq!(views[0].key.session_id, "thread_123");
    assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
    assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
}

#[test]
fn duplicate_windows_for_workspace_choose_a_matching_window() {
    let directory = "/repo/project-alpha";
    let server = observation(
        "server",
        "http://127.0.0.1:4096",
        Some(AgentStatus::Working),
        100,
        Some("Server only"),
        directory,
    );
    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[
            pane_snapshot("%1", "@1", directory, None),
            pane_snapshot("%2", "@2", directory, None),
        ],
        "default",
        &[directory],
    );

    assert_eq!(views.len(), 1);
    assert_window_candidates(&views[0], "project", &["@1", "@2"]);
    assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
}

#[test]
fn current_matching_window_precedes_previous_and_index_order() {
    let directory = "/repo/project-alpha";
    let server = directory_only_observation(directory);
    let mut previous = pane_snapshot("%1", "@1", directory, None);
    previous.activity.window_active = false;
    previous.activity.window_last = true;
    previous.placement.window_index = "1".to_owned();
    let mut current = pane_snapshot("%2", "@2", directory, None);
    current.placement.window_index = "9".to_owned();

    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[previous, current],
        "default",
        &[directory],
    );

    assert_window_candidates(&views[0], "project", &["@2", "@1"]);
    assert_eq!(views[0].tmux_window_id(), Some("@2"));
}

#[test]
fn scratch_window_sidebar_does_not_override_previous_matching_window() {
    let directory = "/repo/project-alpha";
    let scratch = "/repo/project-beta";
    let server = directory_only_observation(directory);
    let mut current_scratch = pane_snapshot("%scratch", "@9", scratch, None);
    current_scratch.placement.window_index = "9".to_owned();
    let mut lowest = pane_snapshot("%1", "@1", directory, None);
    lowest.activity.window_active = false;
    lowest.placement.window_index = "1".to_owned();
    let mut previous = pane_snapshot("%2", "@2", directory, None);
    previous.activity.window_active = false;
    previous.activity.window_last = true;
    previous.placement.window_index = "8".to_owned();

    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[current_scratch, lowest, previous],
        "default",
        &[directory, scratch],
    );

    assert_window_candidates(&views[0], "project", &["@2", "@1"]);
    assert_eq!(views[0].tmux_window_id(), Some("@2"));
}

#[test]
fn matching_windows_fall_back_by_parsed_index_then_window_id() {
    let directory = "/repo/project-alpha";
    let server = directory_only_observation(directory);
    let mut high = pane_snapshot("%2", "@2", directory, None);
    high.activity.window_active = false;
    high.placement.window_index = "70000".to_owned();
    let mut tied_later = pane_snapshot("%9", "@9", directory, None);
    tied_later.activity.window_active = false;
    tied_later.placement.window_index = "65536".to_owned();
    let mut tied_first = pane_snapshot("%1", "@1", directory, None);
    tied_first.activity.window_active = false;
    tied_first.placement.window_index = "65536".to_owned();

    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[high, tied_later, tied_first],
        "default",
        &[directory],
    );

    assert_window_candidates(&views[0], "project", &["@1", "@9", "@2"]);
    assert_eq!(views[0].tmux_window_id(), Some("@1"));
}

#[test]
fn linked_windows_are_deduplicated_before_common_session_selection() {
    let directory = "/repo/project-alpha";
    let server = directory_only_observation(directory);
    let mut project_link = pane_snapshot_in_session("project", "%1", "@1", directory, None);
    project_link.activity.window_active = false;
    let mut linked_copy = pane_snapshot_in_session("linked", "%1", "@1", directory, None);
    linked_copy.activity.window_active = false;
    let mut project_only = pane_snapshot_in_session("project", "%2", "@2", directory, None);
    project_only.activity.window_active = false;

    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[project_link, linked_copy, project_only],
        "default",
        &[directory],
    );

    assert_window_candidates(&views[0], "project", &["@1", "@2"]);
}

#[test]
fn mixed_single_and_multi_root_windows_choose_a_matching_window() {
    let directory = "/repo/project-alpha";
    let other = "/repo/project-beta";
    let server = observation(
        "server",
        "http://127.0.0.1:4096",
        Some(AgentStatus::Working),
        100,
        Some("Server only"),
        directory,
    );
    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[
            pane_snapshot("%1", "@1", directory, None),
            pane_snapshot("%2", "@2", directory, None),
            pane_snapshot("%3", "@2", other, None),
        ],
        "default",
        &[directory, other],
    );

    assert_eq!(views.len(), 1);
    assert_window_candidates(&views[0], "project", &["@1", "@2"]);
    assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
}

#[test]
fn mixed_matching_windows_across_sessions_use_no_jump_target() {
    let directory = "/repo/project-alpha";
    let other = "/repo/project-beta";
    let server = observation(
        "server",
        "http://127.0.0.1:4096",
        Some(AgentStatus::Working),
        100,
        Some("Server only"),
        directory,
    );
    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[
            pane_snapshot("%1", "@1", directory, None),
            pane_snapshot_in_session("other", "%2", "@2", directory, None),
            pane_snapshot_in_session("other", "%3", "@2", other, None),
        ],
        "default",
        &[directory, other],
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
    let directory = "/repo/project-alpha";
    let pane_report = observation(
        "reporter-a",
        "instance-1",
        Some(AgentStatus::Working),
        100,
        Some("Example report"),
        directory,
    );
    let views = reconcile_with_fake_workspace_resolver(
        vec![pane_report],
        &[
            pane_snapshot("%1", "@1", directory, None),
            pane_snapshot("%2", "@2", directory, None),
        ],
        "default",
        &[directory],
    );

    assert_eq!(views.len(), 1);
    assert_window_candidates(&views[0], "project", &["@1", "@2"]);
    assert_eq!(views[0].target.tmux_window_id.as_deref(), Some("@1"));
    assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
}

#[test]
fn single_matching_workspace_window_gets_exact_window_target() {
    let directory = "/repo/project-alpha";
    let server = observation(
        "server",
        "http://127.0.0.1:4096",
        Some(AgentStatus::Working),
        100,
        Some("Server only"),
        directory,
    );
    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[pane_snapshot("%2", "@2", directory, None)],
        "default",
        &[directory],
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
    let directory = "/repo/project-alpha";
    let other = "/repo/project-beta";
    let server = observation(
        "server",
        "http://127.0.0.1:4096",
        Some(AgentStatus::Working),
        100,
        Some("Server only"),
        directory,
    );
    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[
            pane_snapshot("%1", "@1", directory, None),
            pane_snapshot("%2", "@1", other, None),
        ],
        "default",
        &[directory, other],
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
    let directory = "/repo/project-alpha";
    let server = observation(
        "server",
        "http://127.0.0.1:4096",
        Some(AgentStatus::Working),
        100,
        Some("Server only"),
        directory,
    );
    let mut first = pane_snapshot("%1", "@1", directory, None);
    first.activity.pane_active = false;
    first.placement.pane_index = "1".to_owned();
    let mut second = pane_snapshot("%2", "@1", directory, None);
    second.activity.pane_active = true;
    second.placement.pane_index = "2".to_owned();
    let mut sidebar = pane_snapshot("%sidebar", "@1", "/repo/sidebar", Some("sidebar"));
    sidebar.activity.pane_active = false;
    sidebar.placement.pane_index = "0".to_owned();

    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[sidebar, first, second],
        "default",
        &[directory],
    );

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
    let directory = "/repo/project-alpha";
    let server = directory_only_observation(directory);
    let mut first = pane_snapshot("%1", "@1", directory, None);
    first.activity.pane_active = false;
    first.placement.pane_index = "1".to_owned();
    let mut previous = pane_snapshot("%2", "@1", directory, None);
    previous.activity.pane_active = false;
    previous.activity.pane_last = true;
    previous.placement.pane_index = "8".to_owned();
    let mut sidebar = pane_snapshot("%sidebar", "@1", directory, Some("sidebar"));
    sidebar.placement.pane_index = "0".to_owned();

    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[sidebar, first, previous],
        "default",
        &[directory],
    );

    assert_candidate_panes(&views[0], "@1", &["%2", "%1"]);
    assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%2"));
}

#[test]
fn matching_panes_fall_back_by_parsed_index_then_pane_id() {
    let directory = "/repo/project-alpha";
    let server = directory_only_observation(directory);
    let mut high = pane_snapshot("%2", "@1", directory, None);
    high.activity.pane_active = false;
    high.placement.pane_index = "70000".to_owned();
    let mut tied_later = pane_snapshot("%9", "@1", directory, None);
    tied_later.activity.pane_active = false;
    tied_later.placement.pane_index = "65536".to_owned();
    let mut tied_first = pane_snapshot("%1", "@1", directory, None);
    tied_first.activity.pane_active = false;
    tied_first.placement.pane_index = "65536".to_owned();

    let views = reconcile_with_fake_workspace_resolver(
        vec![server],
        &[high, tied_later, tied_first],
        "default",
        &[directory],
    );

    assert_candidate_panes(&views[0], "@1", &["%1", "%9", "%2"]);
    assert_eq!(views[0].target.tmux_pane_id.as_deref(), Some("%1"));
}

#[test]
fn observation_without_matching_tmux_window_uses_no_jump_target() {
    let directory = "/repo/project-alpha";
    let server = observation(
        "server",
        "http://127.0.0.1:4096",
        Some(AgentStatus::Working),
        100,
        Some("Server only"),
        directory,
    );
    let views = reconcile_with_fake_workspace_resolver(vec![server], &[], "default", &[directory]);

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
        "/repo/unresolved",
    );
    let views = reconcile_with_fake_workspace_resolver(vec![server], &[], "default", &[]);

    assert!(views.is_empty());
}

#[test]
fn latest_observation_must_resolve_to_live_window() {
    let directory = "/repo/project-alpha";
    let old = observation(
        "reporter-a",
        "instance-1",
        Some(AgentStatus::Working),
        100,
        Some("Old"),
        directory,
    );
    let newest = observation(
        "reporter-b",
        "instance-2",
        Some(AgentStatus::Working),
        200,
        Some("Newest"),
        "/repo/unresolved",
    );

    let views =
        reconcile_with_fake_workspace_resolver(vec![old, newest], &[], "default", &[directory]);

    assert!(views.is_empty());
}

#[test]
fn statusless_observations_can_update_title() {
    let directory = "/repo/project-alpha";
    let status = observation(
        "reporter-a",
        "instance-1",
        Some(AgentStatus::Working),
        100,
        Some("Old"),
        directory,
    );
    let update = observation(
        "reporter-b",
        "instance-2",
        None,
        200,
        Some("Renamed"),
        directory,
    );

    let views =
        reconcile_with_fake_workspace_resolver(vec![status, update], &[], "default", &[directory]);

    assert_eq!(views.len(), 1);
    assert_eq!(views[0].status, AgentStatus::Working);
    assert_eq!(views[0].title.as_deref(), Some("Renamed"));
}

#[test]
fn statusless_update_does_not_refresh_status_precedence() {
    let directory = "/repo/project-alpha";
    let mut stale_working = observation(
        "reporter-a",
        "instance-1",
        Some(AgentStatus::Working),
        100,
        Some("Renamed"),
        directory,
    );
    stale_working.observed_at = 300;
    let waiting = observation(
        "reporter-b",
        "instance-2",
        Some(AgentStatus::Waiting),
        200,
        Some("Waiting"),
        directory,
    );

    let views = reconcile_with_fake_workspace_resolver(
        vec![stale_working, waiting],
        &[],
        "default",
        &[directory],
    );

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
    let session_id = match session_name {
        "project" => "$1",
        "linked" => "$2",
        _ => "$3",
    };
    TmuxPaneSnapshot {
        identity: TmuxPaneIdentity {
            session_id: session_id.to_owned(),
            window_id: window_id.to_owned(),
            pane_id: pane_id.to_owned(),
        },
        placement: TmuxPanePlacement {
            session_name: session_name.to_owned(),
            window_name: format!("{session_name}-window"),
            window_index: "1".to_owned(),
            pane_index: "1".to_owned(),
            current_path: Some(current_path.to_owned()),
        },
        geometry: TmuxPaneGeometry {
            pane_left: 0,
            pane_width: 80,
            window_width: 120,
            window_layout: crate::tmux::test_support::test_window_layout(&[pane_id]),
        },
        activity: TmuxPaneActivity {
            pane_active: true,
            pane_last: false,
            window_active: true,
            window_last: false,
            session_attached: true,
        },
        title: Some("pane title".to_owned()),
        current_command: Some("opencode".to_owned()),
        kmux_role: kmux_role.map(str::to_owned),
    }
}
