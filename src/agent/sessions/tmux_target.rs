//! Live tmux navigation policy for resolved agent workspaces.
//!
//! This module derives ordered physical window and pane candidates from one
//! canonical workspace attachment and one tmux snapshot. It owns no tmux or Git
//! IO; callers provide only the path-to-workspace lookup needed to classify live
//! panes.

use std::collections::{BTreeSet, HashMap};

use crate::agent::workspace::AgentWorkspaceAttachment;
use crate::tmux::TmuxPaneSnapshot;

use super::model::{
    AgentTmuxTarget, AgentTmuxUnavailableReason, AgentTmuxWindowCandidate, ResolvedAgentTarget,
};

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
pub(super) fn resolve_live_tmux_target(
    target: &mut ResolvedAgentTarget,
    attachment: &AgentWorkspaceAttachment,
    panes: &[TmuxPaneSnapshot],
    mut attachment_for_path: impl FnMut(&str) -> Option<AgentWorkspaceAttachment>,
) -> AgentTmuxTarget {
    let matches = window_workspace_matches(attachment, panes, &mut attachment_for_path);
    if matches.is_empty() {
        return AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::Missing);
    }

    let mut common_sessions = matches[0].sessions.keys().cloned().collect::<BTreeSet<_>>();
    for window in &matches[1..] {
        common_sessions.retain(|session_id| window.sessions.contains_key(session_id));
    }
    if common_sessions.len() != 1 {
        return cross_session_target(&matches);
    }
    let Some(session_id) = common_sessions.pop_first() else {
        return cross_session_target(&matches);
    };
    let mut ordered_windows = matches
        .iter()
        .filter_map(|window| {
            window
                .sessions
                .get(&session_id)
                .map(|session| (window, session))
        })
        .collect::<Vec<_>>();
    if ordered_windows.len() != matches.len() {
        return cross_session_target(&matches);
    }
    ordered_windows.sort_by_key(|(window, session)| window_sort_key(window, session));
    let session_name = ordered_windows[0].1.session_name.clone();

    let mut candidates = Vec::with_capacity(ordered_windows.len());
    for (index, (window, session)) in ordered_windows.into_iter().enumerate() {
        let mut panes = window.matching_panes.iter().collect::<Vec<_>>();
        panes.sort_by_key(|pane| pane_sort_key(pane));
        if index == 0 {
            enrich_target_from_window_match(target, window, session, &session_name, panes[0]);
        }
        candidates.push(AgentTmuxWindowCandidate {
            window_id: window.window_id.clone(),
            pane_ids: panes
                .into_iter()
                .map(|pane| pane.identity.pane_id.clone())
                .collect(),
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
        .flat_map(|window| {
            window
                .sessions
                .values()
                .map(|session| session.session_name.clone())
        })
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::CrossSession { session_names })
}

#[derive(Debug, Clone)]
struct WindowWorkspaceMatch {
    window_id: String,
    sessions: HashMap<String, WindowSessionMatch>,
    matching_panes: Vec<TmuxPaneSnapshot>,
}

#[derive(Debug, Clone)]
struct WindowWorkspaceAccumulator {
    window_id: String,
    sessions: HashMap<String, WindowSessionMatch>,
    matching_panes: HashMap<String, TmuxPaneSnapshot>,
}

#[derive(Debug, Clone)]
struct WindowSessionMatch {
    session_name: String,
    window_index: String,
    window_name: String,
    active: bool,
    last: bool,
}

fn window_workspace_matches(
    attachment: &AgentWorkspaceAttachment,
    panes: &[TmuxPaneSnapshot],
    attachment_for_path: &mut impl FnMut(&str) -> Option<AgentWorkspaceAttachment>,
) -> Vec<WindowWorkspaceMatch> {
    let mut windows = HashMap::<String, WindowWorkspaceAccumulator>::new();
    for pane in panes
        .iter()
        .filter(|pane| pane.kmux_role.as_deref() != Some("sidebar"))
    {
        let Some(workspace) = pane
            .placement
            .current_path
            .as_deref()
            .and_then(&mut *attachment_for_path)
        else {
            continue;
        };
        let entry = windows
            .entry(pane.identity.window_id.clone())
            .or_insert_with(|| WindowWorkspaceAccumulator {
                window_id: pane.identity.window_id.clone(),
                sessions: HashMap::new(),
                matching_panes: HashMap::new(),
            });
        if workspace.key() == attachment.key() {
            entry
                .sessions
                .entry(pane.identity.session_id.clone())
                .or_insert_with(|| WindowSessionMatch {
                    session_name: pane.placement.session_name.clone(),
                    window_index: pane.placement.window_index.clone(),
                    window_name: pane.placement.window_name.clone(),
                    active: pane.activity.window_active,
                    last: pane.activity.window_last,
                });
            entry
                .matching_panes
                .entry(pane.identity.pane_id.clone())
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
    let preference = if pane.activity.pane_active {
        0
    } else if pane.activity.pane_last {
        1
    } else {
        2
    };
    (
        preference,
        pane.placement.pane_index.parse().unwrap_or(u64::MAX),
        &pane.identity.pane_id,
    )
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
    target.tmux_pane_id = Some(pane.identity.pane_id.clone());
    target.tmux_pane_title = pane.title.clone();
    target.tmux_pane_current_command = pane.current_command.clone();
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tmux::{TmuxPaneActivity, TmuxPaneGeometry, TmuxPaneIdentity, TmuxPanePlacement};

    #[test]
    fn missing_workspace_match_does_not_enrich_the_target() {
        let attachment = AgentWorkspaceAttachment::for_test("/repo/project-alpha");
        let mut target = ResolvedAgentTarget::default();

        let result = resolve_live_tmux_target(&mut target, &attachment, &[], |_| None);

        assert_eq!(
            result,
            AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::Missing)
        );
        assert_eq!(target, ResolvedAgentTarget::default());
    }

    #[test]
    fn matches_across_sessions_are_reported_without_choosing_a_target() {
        let attachment = AgentWorkspaceAttachment::for_test("/repo/project-alpha");
        let panes = [
            pane_snapshot("$2", "review", "@2", "%2", "2", "1"),
            pane_snapshot("$1", "project", "@1", "%1", "1", "1"),
        ];
        let mut target = ResolvedAgentTarget::default();

        let result = resolve_live_tmux_target(&mut target, &attachment, &panes, |path| {
            Some(AgentWorkspaceAttachment::for_test(path))
        });

        assert_eq!(
            result,
            AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::CrossSession {
                session_names: vec!["project".to_owned(), "review".to_owned()],
            })
        );
        assert_eq!(target, ResolvedAgentTarget::default());
    }

    #[test]
    fn candidates_and_panes_follow_live_activity_preference() {
        let attachment = AgentWorkspaceAttachment::for_test("/repo/project-alpha");
        let mut inactive = pane_snapshot("$1", "project", "@1", "%1", "1", "2");
        inactive.activity.pane_active = false;
        inactive.activity.window_active = false;
        inactive.activity.window_last = true;
        let mut active_later_pane = pane_snapshot("$1", "project", "@2", "%3", "2", "2");
        active_later_pane.activity.pane_active = false;
        active_later_pane.activity.pane_last = true;
        let active_pane = pane_snapshot("$1", "project", "@2", "%2", "2", "1");
        let mut sidebar = pane_snapshot("$1", "project", "@3", "%4", "0", "1");
        sidebar.kmux_role = Some("sidebar".to_owned());
        let panes = [inactive, active_later_pane, active_pane, sidebar];
        let mut target = ResolvedAgentTarget::default();

        let result = resolve_live_tmux_target(&mut target, &attachment, &panes, |path| {
            Some(AgentWorkspaceAttachment::for_test(path))
        });

        assert_eq!(
            result,
            AgentTmuxTarget::Windows {
                session_name: "project".to_owned(),
                candidates: vec![
                    AgentTmuxWindowCandidate {
                        window_id: "@2".to_owned(),
                        pane_ids: vec!["%2".to_owned(), "%3".to_owned()],
                    },
                    AgentTmuxWindowCandidate {
                        window_id: "@1".to_owned(),
                        pane_ids: vec!["%1".to_owned()],
                    },
                ],
            }
        );
        assert_eq!(target.tmux_session_name.as_deref(), Some("project"));
        assert_eq!(target.tmux_window_id.as_deref(), Some("@2"));
        assert_eq!(target.tmux_pane_id.as_deref(), Some("%2"));
    }

    fn pane_snapshot(
        session_id: &str,
        session_name: &str,
        window_id: &str,
        pane_id: &str,
        window_index: &str,
        pane_index: &str,
    ) -> TmuxPaneSnapshot {
        TmuxPaneSnapshot {
            identity: TmuxPaneIdentity {
                session_id: session_id.to_owned(),
                window_id: window_id.to_owned(),
                pane_id: pane_id.to_owned(),
            },
            placement: TmuxPanePlacement {
                session_name: session_name.to_owned(),
                window_name: format!("{session_name}-window"),
                window_index: window_index.to_owned(),
                pane_index: pane_index.to_owned(),
                current_path: Some("/repo/project-alpha".to_owned()),
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
            current_command: Some("example-agent".to_owned()),
            kmux_role: None,
        }
    }
}
