use std::collections::HashMap;
use std::path::Path;

use anyhow::Result;

use crate::paths::same_path;
use crate::state::{AgentReportState, StateStore, TMUX_PANE_SOURCE};
use crate::tmux::{Tmux, TmuxPaneSnapshot, TmuxWindow};

pub fn active_reports(store: &StateStore, tmux: &Tmux) -> Result<Vec<AgentReportState>> {
    let instance_id = tmux.instance_id();
    let candidates = store
        .list_reports()?
        .into_iter()
        .filter(|report| is_candidate_for_tmux_instance(report, &instance_id))
        .collect::<Vec<_>>();
    if candidates.is_empty() {
        return Ok(Vec::new());
    }

    let panes = match tmux.list_pane_snapshots() {
        Ok(panes) => panes
            .into_iter()
            .map(|pane| (pane.pane_id.clone(), pane))
            .collect::<HashMap<_, _>>(),
        Err(_) => {
            prune_pane_reports(store, &candidates)?;
            return Ok(Vec::new());
        }
    };
    let windows = tmux.list_windows(None).unwrap_or_default();
    reconcile_active_reports(store, candidates, &panes, &windows, &instance_id)
}

fn is_candidate_for_tmux_instance(report: &AgentReportState, instance_id: &str) -> bool {
    if report.key.source == TMUX_PANE_SOURCE {
        return report.key.instance == instance_id;
    }

    report
        .target
        .tmux_instance
        .as_deref()
        .is_none_or(|target_instance| target_instance == instance_id)
}

fn reconcile_active_reports(
    store: &StateStore,
    reports: Vec<AgentReportState>,
    panes: &HashMap<String, TmuxPaneSnapshot>,
    windows: &[TmuxWindow],
    instance_id: &str,
) -> Result<Vec<AgentReportState>> {
    let mut active = Vec::new();
    for mut report in reports {
        if let Some(pane_id) = report.target.pane_id.as_deref() {
            let Some(pane) = panes.get(pane_id) else {
                if is_pane_bound_report(&report) {
                    store.delete_report(&report.key)?;
                }
                continue;
            };

            if report
                .target
                .window_id
                .as_deref()
                .is_some_and(|window_id| window_id != pane.window_id)
            {
                if is_pane_bound_report(&report) {
                    store.delete_report(&report.key)?;
                }
                continue;
            }

            report.target.session_name = Some(pane.session_name.clone());
            report.target.window_name = Some(pane.window_name.clone());
            report.target.window_id = Some(pane.window_id.clone());
            if pane.title.is_some() {
                report.target.pane_title = pane.title.clone();
            }
            if pane.current_command.is_some() {
                report.target.pane_current_command = pane.current_command.clone();
            }
        } else {
            let Some(window) = resolve_report_window(&report, windows) else {
                continue;
            };
            enrich_report_from_window(&mut report, window, instance_id);
        }

        if report.target.window_id.is_some() {
            active.push(report);
        }
    }
    Ok(dedupe_equivalent_reports(active))
}

fn enrich_report_from_window(
    report: &mut AgentReportState,
    window: &TmuxWindow,
    instance_id: &str,
) {
    report.target.tmux_instance = Some(instance_id.to_owned());
    report.target.window_id = Some(window.window_id.clone());
    report.target.session_name = Some(window.session_name.clone());
    report.target.window_name = Some(window.window_name.clone());
    if window.kmux_worktree_handle.is_some() {
        report.target.worktree_handle = window.kmux_worktree_handle.clone();
    }
    if window.kmux_worktree_path.is_some() {
        report.target.worktree_path = window.kmux_worktree_path.clone();
    }
    if window.kmux_worktree_branch.is_some() {
        report.target.branch = window.kmux_worktree_branch.clone();
    }
}

fn resolve_report_window<'a>(
    report: &AgentReportState,
    windows: &'a [TmuxWindow],
) -> Option<&'a TmuxWindow> {
    if let Some(window_id) = report.target.window_id.as_deref() {
        return windows.iter().find(|window| window.window_id == window_id);
    }

    if let Some(window) = unique_window(windows.iter().filter(|window| {
        report
            .target
            .worktree_path
            .as_deref()
            .zip(window.kmux_worktree_path.as_deref())
            .is_some_and(|(left, right)| same_path(Path::new(left), Path::new(right)))
    })) {
        return Some(window);
    }

    if let Some(window) = unique_window(windows.iter().filter(|window| {
        report
            .target
            .directory
            .as_deref()
            .zip(window.kmux_worktree_path.as_deref())
            .is_some_and(|(left, right)| same_path(Path::new(left), Path::new(right)))
    })) {
        return Some(window);
    }

    if let (Some(session_name), Some(window_name)) = (
        report.target.session_name.as_deref(),
        report.target.window_name.as_deref(),
    ) {
        return windows.iter().find(|window| {
            window.session_name == session_name && window.window_name == window_name
        });
    }

    None
}

fn unique_window<'a>(mut windows: impl Iterator<Item = &'a TmuxWindow>) -> Option<&'a TmuxWindow> {
    let window = windows.next()?;
    windows.next().is_none().then_some(window)
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct EquivalentReportKey {
    session_id: String,
    window_id: String,
}

fn dedupe_equivalent_reports(reports: Vec<AgentReportState>) -> Vec<AgentReportState> {
    let mut deduped = Vec::<AgentReportState>::new();
    let mut indexes = HashMap::<EquivalentReportKey, usize>::new();

    for report in reports {
        let Some(key) = equivalent_report_key(&report) else {
            deduped.push(report);
            continue;
        };

        if let Some(index) = indexes.get(&key).copied() {
            let existing = deduped[index].clone();
            deduped[index] = merge_equivalent_reports(existing, report);
        } else {
            indexes.insert(key, deduped.len());
            deduped.push(report);
        }
    }

    deduped
}

fn equivalent_report_key(report: &AgentReportState) -> Option<EquivalentReportKey> {
    let session_id = report
        .session_id
        .as_deref()
        .or_else(|| (report.key.source != TMUX_PANE_SOURCE).then_some(report.key.id.as_str()))?;
    let window_id = report.target.window_id.as_deref()?;
    Some(EquivalentReportKey {
        session_id: session_id.to_owned(),
        window_id: window_id.to_owned(),
    })
}

fn merge_equivalent_reports(
    existing: AgentReportState,
    candidate: AgentReportState,
) -> AgentReportState {
    let candidate_is_better = report_preference_key(&candidate) > report_preference_key(&existing);
    let (mut selected, fallback) = if candidate_is_better {
        (candidate, existing)
    } else {
        (existing, candidate)
    };

    if selected.session_id.is_none() {
        selected.session_id = fallback.session_id;
    }
    if selected.target.pane_id.is_none() {
        selected.target.pane_id = fallback.target.pane_id;
    }
    if selected.target.pane_title.is_none() {
        selected.target.pane_title = fallback.target.pane_title;
    }
    if selected.target.pane_current_command.is_none() {
        selected.target.pane_current_command = fallback.target.pane_current_command;
    }
    selected
}

fn report_preference_key(report: &AgentReportState) -> (u8, u64, u8) {
    (
        status_priority(report.status),
        report.observed_at,
        u8::from(report.target.pane_id.is_some()),
    )
}

fn status_priority(status: crate::state::AgentStatus) -> u8 {
    match status {
        crate::state::AgentStatus::Waiting => 3,
        crate::state::AgentStatus::Working => 2,
        crate::state::AgentStatus::Done => 1,
    }
}

fn prune_pane_reports(store: &StateStore, reports: &[AgentReportState]) -> Result<()> {
    for report in reports.iter().filter(|report| is_pane_bound_report(report)) {
        store.delete_report(&report.key)?;
    }
    Ok(())
}

fn is_pane_bound_report(report: &AgentReportState) -> bool {
    report.key.source == TMUX_PANE_SOURCE && report.target.pane_id.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AgentReportKey, AgentStatus, AgentTargetHints};

    use tempfile::TempDir;

    #[test]
    fn reconciles_reports_from_batched_tmux_snapshot() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::test_with_path(temp.path())?;
        let report = report_state("%1", Some("@1"));
        store.upsert_report(&report)?;

        let panes = HashMap::from([(
            "%1".to_owned(),
            pane_snapshot(
                "%1",
                "@1",
                "project",
                "renamed-window",
                Some("kmux"),
                Some("nvim"),
            ),
        )]);

        let active = reconcile_active_reports(&store, vec![report], &panes, &[], "test")?;

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].target.session_name.as_deref(), Some("project"));
        assert_eq!(
            active[0].target.window_name.as_deref(),
            Some("renamed-window")
        );
        assert_eq!(active[0].target.pane_title.as_deref(), Some("kmux"));
        assert_eq!(
            active[0].target.pane_current_command.as_deref(),
            Some("nvim")
        );
        Ok(())
    }

    #[test]
    fn keeps_window_resolved_reports_without_pane_targets() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::test_with_path(temp.path())?;
        let mut report = report_state("%1", Some("@1"));
        report.key = AgentReportKey::new("test-source", "instance", "session-1");
        report.target.pane_id = None;
        let windows = vec![window_snapshot(
            "project",
            "@1",
            "project-main",
            Some("project"),
            Some("/repo/project"),
            Some("main"),
        )];

        let active =
            reconcile_active_reports(&store, vec![report], &HashMap::new(), &windows, "test")?;

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].target.window_id.as_deref(), Some("@1"));
        assert_eq!(active[0].target.pane_id, None);
        Ok(())
    }

    #[test]
    fn omits_non_pane_reports_with_stale_window_ids() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::test_with_path(temp.path())?;
        let mut report = report_state("%1", Some("@missing"));
        report.key = AgentReportKey::new("opencode-server", "instance", "session-1");
        report.target.pane_id = None;

        let active = reconcile_active_reports(&store, vec![report], &HashMap::new(), &[], "test")?;

        assert!(active.is_empty());
        Ok(())
    }

    #[test]
    fn resolves_non_pane_reports_by_worktree_path() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::test_with_path(temp.path())?;
        let mut report = report_state("%1", None);
        report.key = AgentReportKey::new("opencode-server", "http://127.0.0.1:4096", "session-1");
        report.target.tmux_instance = None;
        report.target.pane_id = None;
        report.target.worktree_path = Some("/repo/project".to_owned());

        let windows = vec![window_snapshot(
            "project",
            "@7",
            "project-main",
            Some("project"),
            Some("/repo/project"),
            Some("main"),
        )];

        let active =
            reconcile_active_reports(&store, vec![report], &HashMap::new(), &windows, "test")?;

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].target.tmux_instance.as_deref(), Some("test"));
        assert_eq!(active[0].target.window_id.as_deref(), Some("@7"));
        assert_eq!(active[0].target.session_name.as_deref(), Some("project"));
        assert_eq!(
            active[0].target.window_name.as_deref(),
            Some("project-main")
        );
        assert_eq!(active[0].target.worktree_handle.as_deref(), Some("project"));
        assert_eq!(active[0].target.branch.as_deref(), Some("main"));
        Ok(())
    }

    #[test]
    fn dedupes_pane_and_server_reports_for_same_session_window() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::test_with_path(temp.path())?;
        let mut pane_report = report_state("%1", Some("@1"));
        pane_report.session_id = Some("ses_root".to_owned());
        pane_report.status = AgentStatus::Working;
        pane_report.observed_at = 100;

        let mut server_report = report_state("%server", Some("@1"));
        server_report.key =
            AgentReportKey::new("opencode-server", "http://127.0.0.1:4096", "ses_root");
        server_report.session_id = None;
        server_report.status = AgentStatus::Waiting;
        server_report.observed_at = 200;
        server_report.target.pane_id = None;
        server_report.target.pane_title = None;
        server_report.title = Some("Permission prompt".to_owned());

        let windows = vec![window_snapshot(
            "project",
            "@1",
            "project-main",
            Some("project"),
            Some("/repo/project"),
            Some("main"),
        )];

        let active = reconcile_active_reports(
            &store,
            vec![pane_report, server_report],
            &HashMap::from([(
                "%1".to_owned(),
                pane_snapshot("%1", "@1", "project", "project-main", Some("pane"), None),
            )]),
            &windows,
            "test",
        )?;

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].key.source, "opencode-server");
        assert_eq!(active[0].status, AgentStatus::Waiting);
        assert_eq!(active[0].target.pane_id.as_deref(), Some("%1"));
        assert_eq!(active[0].title.as_deref(), Some("Permission prompt"));
        Ok(())
    }

    #[test]
    fn keeps_distinct_non_pane_sessions_in_same_window() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::test_with_path(temp.path())?;
        let mut first = report_state("%server-a", None);
        first.key = AgentReportKey::new("opencode-server", "server", "ses_a");
        first.session_id = Some("ses_a".to_owned());
        first.target.pane_id = None;
        first.target.worktree_path = Some("/repo/project".to_owned());
        first.title = Some("First session".to_owned());

        let mut second = report_state("%server-b", None);
        second.key = AgentReportKey::new("opencode-server", "server", "ses_b");
        second.session_id = Some("ses_b".to_owned());
        second.target.pane_id = None;
        second.target.worktree_path = Some("/repo/project".to_owned());
        second.title = Some("Second session".to_owned());

        let windows = vec![window_snapshot(
            "project",
            "@1",
            "project-main",
            Some("project"),
            Some("/repo/project"),
            Some("main"),
        )];

        let active = reconcile_active_reports(
            &store,
            vec![first, second],
            &HashMap::new(),
            &windows,
            "test",
        )?;

        assert_eq!(active.len(), 2);
        assert_eq!(active[0].target.window_id.as_deref(), Some("@1"));
        assert_eq!(active[1].target.window_id.as_deref(), Some("@1"));
        assert_ne!(active[0].key.id, active[1].key.id);
        Ok(())
    }

    #[test]
    fn prunes_missing_or_reused_pane_bound_reports() -> Result<()> {
        let temp = TempDir::new()?;
        let store = StateStore::test_with_path(temp.path())?;
        let missing = report_state("%1", Some("@1"));
        let reused = report_state("%2", Some("@2"));
        store.upsert_report(&missing)?;
        store.upsert_report(&reused)?;

        let panes = HashMap::from([(
            "%2".to_owned(),
            pane_snapshot("%2", "@not-recorded", "project", "other", None, None),
        )]);

        let active = reconcile_active_reports(
            &store,
            vec![missing.clone(), reused.clone()],
            &panes,
            &[],
            "test",
        )?;

        assert!(active.is_empty());
        assert!(store.get_report(&missing.key)?.is_none());
        assert!(store.get_report(&reused.key)?.is_none());
        Ok(())
    }

    #[test]
    fn active_reports_prunes_pane_candidates_when_tmux_snapshot_fails() -> Result<()> {
        use std::time::{SystemTime, UNIX_EPOCH};

        let temp = TempDir::new()?;
        let store = StateStore::test_with_path(temp.path())?;
        let nanos = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
        let socket_name = format!("kmux-missing-test-{}-{nanos}", std::process::id());
        let tmux = Tmux::with_socket_name(&socket_name);
        let mut report = report_state("%1", Some("@1"));
        report.key.instance.clone_from(&socket_name);
        report.target.tmux_instance = Some(socket_name);
        store.upsert_report(&report)?;

        let active = active_reports(&store, &tmux)?;

        assert!(active.is_empty());
        assert!(store.get_report(&report.key)?.is_none());
        Ok(())
    }

    fn report_state(pane_id: &str, window_id: Option<&str>) -> AgentReportState {
        AgentReportState {
            key: AgentReportKey::tmux_pane("test", pane_id),
            session_id: None,
            status: AgentStatus::Working,
            status_changed_at: 100,
            observed_at: 100,
            title: None,
            context: None,
            target: AgentTargetHints {
                tmux_instance: Some("test".to_owned()),
                pane_id: Some(pane_id.to_owned()),
                window_id: window_id.map(str::to_owned),
                session_name: Some("old-session".to_owned()),
                window_name: Some("old-window".to_owned()),
                pane_title: Some("old-title".to_owned()),
                pane_current_command: Some("old-command".to_owned()),
                worktree_handle: Some("feature".to_owned()),
                worktree_path: Some("/repo__worktrees/feature".to_owned()),
                branch: Some("feature".to_owned()),
                directory: None,
            },
        }
    }

    fn pane_snapshot(
        pane_id: &str,
        window_id: &str,
        session_name: &str,
        window_name: &str,
        title: Option<&str>,
        current_command: Option<&str>,
    ) -> TmuxPaneSnapshot {
        TmuxPaneSnapshot {
            session_name: session_name.to_owned(),
            window_id: window_id.to_owned(),
            window_name: window_name.to_owned(),
            pane_id: pane_id.to_owned(),
            pane_left: 0,
            pane_width: 80,
            window_width: 160,
            title: title.map(str::to_owned),
            current_command: current_command.map(str::to_owned),
            pane_active: false,
            window_active: false,
            session_attached: false,
            kmux_role: None,
        }
    }

    fn window_snapshot(
        session_name: &str,
        window_id: &str,
        window_name: &str,
        worktree_handle: Option<&str>,
        worktree_path: Option<&str>,
        branch: Option<&str>,
    ) -> TmuxWindow {
        TmuxWindow {
            session_name: session_name.to_owned(),
            window_id: window_id.to_owned(),
            window_index: "1".to_owned(),
            window_name: window_name.to_owned(),
            active: false,
            kmux_worktree_handle: worktree_handle.map(str::to_owned),
            kmux_worktree_path: worktree_path.map(str::to_owned),
            kmux_worktree_branch: branch.map(str::to_owned),
        }
    }
}
