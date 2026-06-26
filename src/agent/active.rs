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
        } else if report.target.window_id.is_none()
            && let Some(window) = resolve_report_window(&report, windows)
        {
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

        if report.target.window_id.is_some() {
            active.push(report);
        }
    }
    Ok(active)
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

        let active = reconcile_active_reports(&store, vec![report], &HashMap::new(), &[], "test")?;

        assert_eq!(active.len(), 1);
        assert_eq!(active[0].target.window_id.as_deref(), Some("@1"));
        assert_eq!(active[0].target.pane_id, None);
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
