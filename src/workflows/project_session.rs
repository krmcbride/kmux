//! Project-to-tmux-session topology and selection policy.
//!
//! Git paths establish project identity and tmux supplies live pane evidence.
//! This application-layer resolver treats that evidence as a strict partial
//! one-to-one topology: a project may appear in at most one tmux session, and a
//! session selected for that project may contain at most one discoverable Git
//! project. Split projects and mixed-project sessions fail before mutation.
//!
//! Sidebar panes and paths that do not resolve to a live Git project are neutral.
//! Ambient tmux context affects only whether later presentation may focus the
//! selected session; attached and detached callers otherwise use the same
//! topology decision. The resolver deliberately does not create sessions,
//! persist ownership metadata, infer identity from session names, or use agent
//! observation/sidebar state as a topology source.

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use anyhow::{Result, bail};

use crate::LIFECYCLE_ACTIVE_ENV;
use crate::paths::{RepoPaths, discover_project_identity, same_path};
use crate::project::ProjectIdentity;
use crate::state::workspace::{WorkspaceLifecycleLock, WorkspaceStateStore};
use crate::tmux::{Tmux, TmuxPane};

/// Tmux target and held lifecycle lock shared by window-mutating workflows.
pub(super) struct TmuxContext {
    pub(super) tmux: Tmux,
    pub(super) session_name: String,
    pub(super) session_id: String,
    pub(super) is_ambient: bool,
    _lifecycle_lock: WorkspaceLifecycleLock,
}

/// An existing project-session selection or a verified missing-session result.
pub(super) struct ProjectSessionResolution {
    tmux: Tmux,
    project: ProjectIdentity,
    selected: Option<SelectedSession>,
    lifecycle_lock: WorkspaceLifecycleLock,
}

#[derive(Clone, Debug)]
struct SelectedSession {
    session_name: String,
    session_id: String,
    is_ambient: bool,
}

#[derive(Debug)]
struct SessionEvidence {
    session_name: String,
    session_id: String,
    projects: Vec<ProjectIdentity>,
}

#[derive(Clone)]
struct WorkspacePaneFacts {
    pane: TmuxPane,
    project: Option<ProjectIdentity>,
    matches_workspace: bool,
}

trait WorkspaceRemovalSnapshotSource {
    fn snapshot_workspace_panes(&self, workspace: &Path) -> Result<Vec<WorkspacePaneFacts>>;
}

impl WorkspaceRemovalSnapshotSource for Tmux {
    fn snapshot_workspace_panes(&self, workspace: &Path) -> Result<Vec<WorkspacePaneFacts>> {
        // The adapter read belongs here so every removal evaluation receives a
        // snapshot taken after project-session resolution and lifecycle locking.
        Ok(inspect_workspace_panes(self.list_panes()?, workspace))
    }
}

/// Resolve the current Git project to one existing tmux session.
pub(super) fn resolve(paths: &RepoPaths) -> Result<ProjectSessionResolution> {
    let tmux = Tmux::from_env();
    let project = paths.project_identity()?;
    let lifecycle_lock = lock_project_lifecycle(paths)?;
    let panes = tmux.list_panes()?;
    let ambient = tmux.current_context_for_session_resolution()?;
    let evidence = collect_live_evidence(&panes)?;
    let selected = select_session(
        &project,
        &evidence,
        ambient.as_ref().map(|context| context.session_id.as_str()),
    )?;

    Ok(ProjectSessionResolution {
        tmux,
        project,
        selected,
        lifecycle_lock,
    })
}

/// Acquire the repository lifecycle lock and reject recursive hook invocation.
pub(super) fn lock_project_lifecycle(paths: &RepoPaths) -> Result<WorkspaceLifecycleLock> {
    if std::env::var_os(LIFECYCLE_ACTIVE_ENV).is_some() {
        bail!(
            "kmux lifecycle commands cannot run recursively from post_create; move the nested lifecycle operation outside the hook"
        );
    }
    WorkspaceStateStore::new(&paths.git_common_dir).lock_lifecycle()
}

impl ProjectSessionResolution {
    /// Require a resolved existing session for a window-creating operation.
    pub(super) fn require(self, operation: &str) -> Result<TmuxContext> {
        let selected = self.selected.ok_or_else(|| {
            anyhow::anyhow!(
                "{operation} requires an existing tmux session containing a live pane for project {}; open the project in tmux before retrying",
                self.project.main_worktree().display()
            )
        })?;
        Ok(TmuxContext {
            tmux: self.tmux,
            session_name: selected.session_name,
            session_id: selected.session_id,
            is_ambient: selected.is_ambient,
            _lifecycle_lock: self.lifecycle_lock,
        })
    }

    /// Reject removal when a matching workspace pane is live outside its managed window.
    ///
    /// With no selected project session, every live match blocks removal. With a
    /// selected session, only panes in its expected managed window are removed by
    /// the ordinary workflow; scratch or linked windows remain external evidence.
    pub(super) fn prepare_workspace_removal(
        &self,
        workspace: &Path,
        expected_window_name: &str,
    ) -> Result<Option<String>> {
        prepare_workspace_removal_from_source(
            &self.tmux,
            &self.project,
            self.selected.as_ref(),
            workspace,
            expected_window_name,
        )
    }

    /// Kill one previously validated physical window in the resolved project session.
    pub(super) fn kill_prepared_window(&self, window_id: &str) -> Result<()> {
        if let Some(selected) = &self.selected {
            self.tmux
                .kill_window_id_in_session(&selected.session_id, window_id)?;
        }
        Ok(())
    }
}

fn collect_live_evidence(panes: &[TmuxPane]) -> Result<Vec<SessionEvidence>> {
    collect_evidence(panes.iter().map(|pane| {
        let project = if pane.kmux_role.as_deref() == Some("sidebar") {
            None
        } else {
            pane.placement
                .current_path
                .as_deref()
                .and_then(|path| discover_project_identity(path).ok())
        };
        (pane, project)
    }))
}

/// Aggregate topology from pane records whose project identities were resolved at the edge.
fn collect_evidence<'a>(
    pane_projects: impl IntoIterator<Item = (&'a TmuxPane, Option<ProjectIdentity>)>,
) -> Result<Vec<SessionEvidence>> {
    let mut sessions = HashMap::<String, SessionEvidence>::new();
    for (pane, project) in pane_projects {
        let session = sessions
            .entry(pane.identity.session_id.clone())
            .or_insert_with(|| SessionEvidence {
                session_name: pane.placement.session_name.clone(),
                session_id: pane.identity.session_id.clone(),
                projects: Vec::new(),
            });
        if session.session_name != pane.placement.session_name {
            bail!(
                "inconsistent tmux pane records for session id {:?}",
                pane.identity.session_id
            );
        }
        if pane.kmux_role.as_deref() == Some("sidebar") {
            continue;
        }
        if let Some(candidate) = project
            && !session.projects.contains(&candidate)
        {
            session.projects.push(candidate);
        }
    }

    let mut sessions = sessions.into_values().collect::<Vec<_>>();
    for session in &mut sessions {
        session
            .projects
            .sort_by(|left, right| left.main_worktree().cmp(right.main_worktree()));
    }
    sessions.sort_by(|left, right| {
        left.session_name
            .cmp(&right.session_name)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });
    Ok(sessions)
}

fn inspect_workspace_panes(panes: Vec<TmuxPane>, workspace: &Path) -> Vec<WorkspacePaneFacts> {
    panes
        .into_iter()
        .map(|pane| {
            if pane.kmux_role.as_deref() == Some("sidebar") {
                return WorkspacePaneFacts {
                    pane,
                    project: None,
                    matches_workspace: false,
                };
            }

            let paths = pane
                .placement
                .current_path
                .as_deref()
                .and_then(|path| RepoPaths::discover(path).ok());
            let matches_workspace = paths
                .as_ref()
                .is_some_and(|paths| same_path(&paths.current_worktree, workspace));
            let project = paths.and_then(|paths| paths.project_identity().ok());
            WorkspacePaneFacts {
                pane,
                project,
                matches_workspace,
            }
        })
        .collect()
}

/// Decide removal safety from one already-refreshed pane snapshot and its resolved facts.
fn evaluate_workspace_removal(
    project: &ProjectIdentity,
    original_selected: Option<&SelectedSession>,
    workspace: &Path,
    expected_window_name: &str,
    panes: &[WorkspacePaneFacts],
) -> Result<Option<String>> {
    let evidence = collect_evidence(
        panes
            .iter()
            .map(|facts| (&facts.pane, facts.project.clone())),
    )?;
    let fresh_selected = select_session(project, &evidence, None)?;
    let original_id = original_selected.map(|session| session.session_id.as_str());
    let selected_id = fresh_selected
        .as_ref()
        .map(|session| session.session_id.as_str());
    if original_id != selected_id {
        bail!(
            "project session topology changed while preparing to remove workspace at {} (original selected session id: {original_id:?}, fresh selected session id: {selected_id:?}); retry after tmux settles",
            workspace.display()
        );
    }

    let mut matching_sessions = BTreeSet::new();
    let expected_window_ids = panes
        .iter()
        .map(|facts| &facts.pane)
        .filter(|pane| selected_id == Some(pane.identity.session_id.as_str()))
        .filter(|pane| pane.placement.window_name == expected_window_name)
        .map(|pane| pane.identity.window_id.clone())
        .collect::<BTreeSet<_>>();
    if expected_window_ids.len() > 1 {
        let selected_name = fresh_selected
            .as_ref()
            .map(|session| session.session_name.as_str())
            .unwrap_or("<none>");
        bail!(
            "tmux session {selected_name:?} has multiple windows named {expected_window_name:?}; close duplicate windows before removing the workspace"
        );
    }
    let expected_window_id = expected_window_ids.into_iter().next();

    for session in &evidence {
        let has_external_match = panes
            .iter()
            .filter(|facts| facts.pane.identity.session_id == session.session_id)
            .any(|facts| {
                let pane = &facts.pane;
                if pane.kmux_role.as_deref() == Some("sidebar") || !facts.matches_workspace {
                    return false;
                }
                selected_id != Some(session.session_id.as_str())
                    || expected_window_id.as_deref() != Some(pane.identity.window_id.as_str())
            });
        if has_external_match {
            matching_sessions.insert(session.session_name.clone());
        }
    }

    if matching_sessions.is_empty() {
        return Ok(expected_window_id);
    }
    bail!(
        "workspace at {} still has a live tmux pane outside its managed window in: {}; close or move those windows before removing it",
        workspace.display(),
        display_session_names(matching_sessions.iter().map(String::as_str))
    )
}

fn prepare_workspace_removal_from_source(
    source: &impl WorkspaceRemovalSnapshotSource,
    project: &ProjectIdentity,
    original_selected: Option<&SelectedSession>,
    workspace: &Path,
    expected_window_name: &str,
) -> Result<Option<String>> {
    let facts = source.snapshot_workspace_panes(workspace)?;
    evaluate_workspace_removal(
        project,
        original_selected,
        workspace,
        expected_window_name,
        &facts,
    )
}

fn select_session(
    project: &ProjectIdentity,
    sessions: &[SessionEvidence],
    ambient_session_id: Option<&str>,
) -> Result<Option<SelectedSession>> {
    let mut matching = sessions
        .iter()
        .filter(|session| session.projects.contains(project))
        .collect::<Vec<_>>();
    matching.sort_by(|left, right| {
        left.session_name
            .cmp(&right.session_name)
            .then_with(|| left.session_id.cmp(&right.session_id))
    });

    let session = match matching.as_slice() {
        [] => return Ok(None),
        [session] => *session,
        _ => {
            bail!(
                "project {} has live panes in multiple tmux sessions: {}; move, unlink, or close project windows until it appears in exactly one session",
                project.main_worktree().display(),
                display_session_names(matching.iter().map(|session| session.session_name.as_str()))
            )
        }
    };

    if session.projects.len() > 1 {
        bail!(
            "tmux session {:?} contains panes from multiple Git projects: {}; move or close windows until the session contains exactly one project",
            session.session_name,
            display_project_roots(&session.projects)
        );
    }

    Ok(Some(SelectedSession {
        session_name: session.session_name.clone(),
        session_id: session.session_id.clone(),
        is_ambient: ambient_session_id == Some(session.session_id.as_str()),
    }))
}

fn display_session_names<'a>(names: impl IntoIterator<Item = &'a str>) -> String {
    names
        .into_iter()
        .map(|name| format!("{name:?}"))
        .collect::<Vec<_>>()
        .join(", ")
}

fn display_project_roots(projects: &[ProjectIdentity]) -> String {
    let mut roots = projects
        .iter()
        .map(|project| project.main_worktree().display().to_string())
        .collect::<Vec<_>>();
    roots.sort();
    roots.join(", ")
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use super::*;
    use crate::tmux::{TmuxPaneIdentity, TmuxPanePlacement};

    fn project(root: &str) -> ProjectIdentity {
        let root = std::path::PathBuf::from(root);
        ProjectIdentity::from_canonical_paths(root.clone(), root.join(".git"))
            .expect("test project identity should be valid")
    }

    fn evidence(name: &str, id: &str, projects: &[ProjectIdentity]) -> SessionEvidence {
        SessionEvidence {
            session_name: name.to_owned(),
            session_id: id.to_owned(),
            projects: projects.to_vec(),
        }
    }

    fn pane(
        session_name: &str,
        session_id: &str,
        window_name: &str,
        window_id: &str,
        pane_id: &str,
        path: Option<&str>,
        role: Option<&str>,
    ) -> TmuxPane {
        TmuxPane {
            identity: TmuxPaneIdentity {
                session_id: session_id.to_owned(),
                window_id: window_id.to_owned(),
                pane_id: pane_id.to_owned(),
            },
            placement: TmuxPanePlacement {
                session_name: session_name.to_owned(),
                window_name: window_name.to_owned(),
                window_index: "1".to_owned(),
                pane_index: pane_id.trim_start_matches('%').to_owned(),
                current_path: path.map(str::to_owned),
            },
            kmux_role: role.map(str::to_owned),
        }
    }

    struct TestRemovalSnapshotSource {
        calls: Cell<usize>,
        facts: Vec<WorkspacePaneFacts>,
    }

    impl WorkspaceRemovalSnapshotSource for TestRemovalSnapshotSource {
        fn snapshot_workspace_panes(&self, _workspace: &Path) -> Result<Vec<WorkspacePaneFacts>> {
            self.calls.set(self.calls.get() + 1);
            Ok(self.facts.clone())
        }
    }

    #[test]
    fn unique_project_bucket_resolves_equally_for_attached_and_detached_callers() -> Result<()> {
        let target = project("/repo/project-alpha");
        let sessions = [evidence(
            "project-alpha",
            "$1",
            std::slice::from_ref(&target),
        )];

        let detached = select_session(&target, &sessions, None)?
            .expect("detached caller should resolve the project bucket");
        let attached = select_session(&target, &sessions, Some("$1"))?
            .expect("attached caller should resolve the same project bucket");
        let other_ambient = select_session(&target, &sessions, Some("$9"))?
            .expect("unrelated ambient context should not change resolution");

        assert_eq!(detached.session_id, "$1", "detached: {detached:?}");
        assert!(!detached.is_ambient, "detached: {detached:?}");
        assert_eq!(
            attached.session_id, detached.session_id,
            "attached: {attached:?}, detached: {detached:?}"
        );
        assert!(attached.is_ambient, "attached: {attached:?}");
        assert_eq!(
            other_ambient.session_id, detached.session_id,
            "other ambient: {other_ambient:?}, detached: {detached:?}"
        );
        assert!(
            !other_ambient.is_ambient,
            "other ambient: {other_ambient:?}"
        );
        Ok(())
    }

    #[test]
    fn split_project_reports_sessions_in_deterministic_order() {
        let target = project("/repo/project-alpha");
        let sessions = [
            evidence("zeta", "$2", std::slice::from_ref(&target)),
            evidence("alpha", "$1", std::slice::from_ref(&target)),
        ];

        let error = select_session(&target, &sessions, Some("$2"))
            .expect_err("ambient context must not override split topology");
        let message = error.to_string();
        assert!(
            message.contains("live panes in multiple tmux sessions"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("\"alpha\", \"zeta\""),
            "unexpected session ordering: {message}"
        );
    }

    #[test]
    fn mixed_project_session_reports_every_project_root() {
        let target = project("/repo/project-alpha");
        let other = project("/repo/project-beta");
        let sessions = [evidence("mixed", "$1", &[target.clone(), other])];

        let error = select_session(&target, &sessions, None)
            .expect_err("a mixed-project session should fail closed");
        let message = error.to_string();
        assert!(
            message.contains("contains panes from multiple Git projects"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("/repo/project-alpha, /repo/project-beta"),
            "unexpected project roots: {message}"
        );
    }

    #[test]
    fn unrelated_inconsistent_session_does_not_block_target_project() -> Result<()> {
        let target = project("/repo/project-alpha");
        let other = project("/repo/project-beta");
        let third = project("/repo/project-gamma");
        let sessions = [
            evidence("project-alpha", "$1", std::slice::from_ref(&target)),
            evidence("unrelated-mixed", "$2", &[other, third]),
        ];

        let selected = select_session(&target, &sessions, None)?
            .expect("unrelated inconsistency should not block the target");
        assert_eq!(selected.session_id, "$1", "selected: {selected:?}");
        Ok(())
    }

    #[test]
    fn missing_project_evidence_returns_no_session() -> Result<()> {
        let target = project("/repo/project-alpha");
        let other = project("/repo/project-beta");
        let sessions = [evidence("project-beta", "$1", &[other])];

        let selected = select_session(&target, &sessions, None)?;
        assert!(
            selected.is_none(),
            "unexpected selected session: {selected:?}"
        );
        Ok(())
    }

    #[test]
    fn topology_collapses_linked_worktrees_and_ignores_neutral_and_sidebar_panes() -> Result<()> {
        let target = project("/repo/project-alpha");
        let other = project("/repo/project-beta");
        let main = pane(
            "project-alpha",
            "$1",
            "main",
            "@1",
            "%1",
            Some("/repo/project-alpha"),
            None,
        );
        let linked = pane(
            "project-alpha",
            "$1",
            "feature-sidebar",
            "@2",
            "%2",
            Some("/repo/project-alpha__worktrees/feature-sidebar"),
            None,
        );
        let neutral = pane(
            "project-alpha",
            "$1",
            "notes",
            "@3",
            "%3",
            Some("/scratch/notes"),
            None,
        );
        let sidebar = pane(
            "project-alpha",
            "$1",
            "sidebar",
            "@4",
            "%4",
            Some("/repo/project-beta"),
            Some("sidebar"),
        );

        let topology = collect_evidence([
            (&main, Some(target.clone())),
            (&linked, Some(target.clone())),
            (&neutral, None),
            (&sidebar, Some(other)),
        ])?;

        assert_eq!(topology.len(), 1, "unexpected topology: {topology:#?}");
        assert_eq!(
            topology[0].projects,
            vec![target],
            "unexpected topology: {topology:#?}"
        );
        Ok(())
    }

    #[test]
    fn topology_detects_two_projects_in_one_session() -> Result<()> {
        let first_project = project("/repo/project-alpha");
        let second_project = project("/repo/project-beta");
        let first = pane(
            "mixed",
            "$1",
            "alpha",
            "@1",
            "%1",
            Some("/repo/project-alpha"),
            None,
        );
        let second = pane(
            "mixed",
            "$1",
            "beta",
            "@2",
            "%2",
            Some("/repo/project-beta"),
            None,
        );

        let topology = collect_evidence([
            (&first, Some(first_project)),
            (&second, Some(second_project)),
        ])?;

        assert_eq!(topology.len(), 1, "unexpected topology: {topology:#?}");
        assert_eq!(
            topology[0].projects.len(),
            2,
            "unexpected topology: {topology:#?}"
        );
        Ok(())
    }

    #[test]
    fn topology_preserves_linked_window_rows_as_distinct_sessions() -> Result<()> {
        let target = project("/repo/project-alpha");
        let first = pane(
            "project-alpha",
            "$1",
            "main",
            "@1",
            "%1",
            Some("/repo/project-alpha"),
            None,
        );
        let mut linked = first.clone();
        linked.identity.session_id = "$2".to_owned();
        linked.placement.session_name = "linked-project".to_owned();

        let topology = collect_evidence([
            (&first, Some(target.clone())),
            (&linked, Some(target.clone())),
        ])?;
        let error = select_session(&target, &topology, None)
            .expect_err("one physical window linked into two sessions must remain ambiguous");
        let message = error.to_string();

        assert!(
            message.contains("live panes in multiple tmux sessions"),
            "unexpected error for topology {topology:#?}: {message}"
        );
        Ok(())
    }

    #[test]
    fn removal_preparation_reads_fresh_snapshot_after_resolution() -> Result<()> {
        let target = project("/repo/project-alpha");
        let workspace = Path::new("/repo/project-alpha__worktrees/feature-sidebar");
        let managed = pane(
            "project-alpha",
            "$1",
            "kmux-feature-sidebar",
            "@1",
            "%1",
            Some("/repo/project-alpha__worktrees/feature-sidebar"),
            None,
        );
        let late_external = pane(
            "project-alpha",
            "$1",
            "scratch",
            "@2",
            "%2",
            Some("/repo/project-alpha__worktrees/feature-sidebar"),
            None,
        );
        let source = TestRemovalSnapshotSource {
            calls: Cell::new(0),
            facts: vec![
                WorkspacePaneFacts {
                    pane: managed,
                    project: Some(target.clone()),
                    matches_workspace: true,
                },
                WorkspacePaneFacts {
                    pane: late_external,
                    project: Some(target.clone()),
                    matches_workspace: true,
                },
            ],
        };
        let original_selected = SelectedSession {
            session_name: "project-alpha".to_owned(),
            session_id: "$1".to_owned(),
            is_ambient: false,
        };

        let error = prepare_workspace_removal_from_source(
            &source,
            &target,
            Some(&original_selected),
            workspace,
            "kmux-feature-sidebar",
        )
        .expect_err("the fresh snapshot should expose the late external pane");
        assert_eq!(source.calls.get(), 1, "fresh snapshot call count");
        let message = error.to_string();
        assert!(
            message.contains("outside its managed window"),
            "unexpected error: {message}"
        );
        assert!(
            message.contains("\"project-alpha\""),
            "missing observed session in error: {message}"
        );
        assert!(
            message.contains(&workspace.display().to_string()),
            "missing workspace path in error: {message}"
        );
        Ok(())
    }

    #[test]
    fn removal_policy_reports_original_and_fresh_selected_session_ids() -> Result<()> {
        let target = project("/repo/project-alpha");
        let workspace = Path::new("/repo/project-alpha__worktrees/feature-sidebar");
        let fresh = pane(
            "replacement",
            "$2",
            "kmux-feature-sidebar",
            "@2",
            "%2",
            Some("/repo/project-alpha__worktrees/feature-sidebar"),
            None,
        );
        let fresh_facts = [WorkspacePaneFacts {
            pane: fresh,
            project: Some(target.clone()),
            matches_workspace: true,
        }];
        let original_selected = SelectedSession {
            session_name: "original".to_owned(),
            session_id: "$1".to_owned(),
            is_ambient: false,
        };

        let error = evaluate_workspace_removal(
            &target,
            Some(&original_selected),
            workspace,
            "kmux-feature-sidebar",
            &fresh_facts,
        )
        .expect_err("a changed selected session should fail closed");
        let message = error.to_string();
        assert!(
            message.contains("original selected session id: Some(\"$1\")"),
            "missing original selection in error: {message}"
        );
        assert!(
            message.contains("fresh selected session id: Some(\"$2\")"),
            "missing fresh selection in error: {message}"
        );
        Ok(())
    }
}
