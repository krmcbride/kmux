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

use std::collections::BTreeSet;
use std::path::Path;

use anyhow::{Result, bail};

use crate::LIFECYCLE_ACTIVE_ENV;
use crate::paths::{RepoPaths, discover_project_identity, same_path};
use crate::project::ProjectIdentity;
use crate::state::workspace::{WorkspaceLifecycleLock, WorkspaceStateStore};
use crate::tmux::{Tmux, TmuxSessionSnapshot};

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

struct SessionEvidence<'a> {
    snapshot: &'a TmuxSessionSnapshot,
    projects: Vec<ProjectIdentity>,
}

/// Resolve the current Git project to one existing tmux session.
pub(super) fn resolve(paths: &RepoPaths) -> Result<ProjectSessionResolution> {
    let tmux = Tmux::from_env();
    let project = paths.project_identity()?;
    let lifecycle_lock = lock_project_lifecycle(paths)?;
    let sessions = tmux.list_session_snapshots()?;
    let ambient = tmux.current_context_for_session_resolution()?;
    let evidence = collect_evidence(&sessions);
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
        let sessions = self.tmux.list_session_snapshots()?;
        let evidence = collect_evidence(&sessions);
        let fresh_selected = select_session(&self.project, &evidence, None)?;
        let original_id = self
            .selected
            .as_ref()
            .map(|session| session.session_id.as_str());
        let selected_id = fresh_selected
            .as_ref()
            .map(|session| session.session_id.as_str());
        if original_id != selected_id {
            bail!(
                "project session topology changed while preparing to remove workspace at {}; retry after tmux settles",
                workspace.display()
            );
        }
        let mut matching_sessions = BTreeSet::new();
        let expected_window_ids = sessions
            .iter()
            .filter(|session| selected_id == Some(session.session_id.as_str()))
            .flat_map(|session| &session.panes)
            .filter(|pane| pane.window_name == expected_window_name)
            .map(|pane| pane.window_id.clone())
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

        for session in &sessions {
            let has_external_match = session.panes.iter().any(|pane| {
                if pane.kmux_role.as_deref() == Some("sidebar") {
                    return false;
                }
                let matches_workspace = pane.current_path.as_deref().is_some_and(|path| {
                    RepoPaths::discover(path)
                        .is_ok_and(|paths| same_path(&paths.current_worktree, workspace))
                });
                if !matches_workspace {
                    return false;
                }
                selected_id != Some(session.session_id.as_str())
                    || expected_window_id.as_deref() != Some(pane.window_id.as_str())
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

    /// Kill one previously validated physical window in the resolved project session.
    pub(super) fn kill_prepared_window(&self, window_id: &str) -> Result<()> {
        if let Some(selected) = &self.selected {
            self.tmux
                .kill_window_id_in_session(&selected.session_id, window_id)?;
        }
        Ok(())
    }
}

fn collect_evidence(sessions: &[TmuxSessionSnapshot]) -> Vec<SessionEvidence<'_>> {
    sessions
        .iter()
        .map(|snapshot| {
            let mut projects = Vec::new();
            for path in snapshot
                .panes
                .iter()
                .filter(|pane| pane.kmux_role.as_deref() != Some("sidebar"))
                .filter_map(|pane| pane.current_path.as_deref())
            {
                if let Ok(candidate) = discover_project_identity(path)
                    && !projects.contains(&candidate)
                {
                    projects.push(candidate);
                }
            }
            projects.sort_by(|left, right| left.main_worktree().cmp(right.main_worktree()));
            SessionEvidence { snapshot, projects }
        })
        .collect()
}

fn select_session(
    project: &ProjectIdentity,
    sessions: &[SessionEvidence<'_>],
    ambient_session_id: Option<&str>,
) -> Result<Option<SelectedSession>> {
    let mut matching = sessions
        .iter()
        .filter(|session| session.projects.contains(project))
        .collect::<Vec<_>>();
    matching.sort_by(|left, right| {
        left.snapshot
            .session_name
            .cmp(&right.snapshot.session_name)
            .then_with(|| left.snapshot.session_id.cmp(&right.snapshot.session_id))
    });

    let session = match matching.as_slice() {
        [] => return Ok(None),
        [session] => *session,
        _ => {
            bail!(
                "project {} has live panes in multiple tmux sessions: {}; move, unlink, or close project windows until it appears in exactly one session",
                project.main_worktree().display(),
                display_session_names(
                    matching
                        .iter()
                        .map(|session| session.snapshot.session_name.as_str())
                )
            )
        }
    };

    if session.projects.len() > 1 {
        bail!(
            "tmux session {:?} contains panes from multiple Git projects: {}; move or close windows until the session contains exactly one project",
            session.snapshot.session_name,
            display_project_roots(&session.projects)
        );
    }

    Ok(Some(SelectedSession {
        session_name: session.snapshot.session_name.clone(),
        session_id: session.snapshot.session_id.clone(),
        is_ambient: ambient_session_id == Some(session.snapshot.session_id.as_str()),
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
    use super::*;
    use crate::git::test_support::GitRepoFixture;
    use crate::tmux::test_support::{TmuxFixture, create_test_session};
    use crate::tmux::{TmuxSessionPaneSnapshot, TmuxSessionSnapshot};

    fn project(root: &str) -> ProjectIdentity {
        let root = std::path::PathBuf::from(root);
        ProjectIdentity::from_canonical_paths(root.clone(), root.join(".git"))
            .expect("test project identity should be valid")
    }

    fn snapshot(name: &str, id: &str) -> TmuxSessionSnapshot {
        TmuxSessionSnapshot {
            session_name: name.to_owned(),
            session_id: id.to_owned(),
            panes: Vec::new(),
        }
    }

    fn pane(path: Option<&Path>, role: Option<&str>) -> TmuxSessionPaneSnapshot {
        TmuxSessionPaneSnapshot {
            window_id: "@1".to_owned(),
            window_name: "main".to_owned(),
            pane_id: "%1".to_owned(),
            current_path: path.map(|path| path.display().to_string()),
            kmux_role: role.map(str::to_owned),
        }
    }

    fn evidence<'a>(
        snapshot: &'a TmuxSessionSnapshot,
        projects: &[ProjectIdentity],
    ) -> SessionEvidence<'a> {
        SessionEvidence {
            snapshot,
            projects: projects.to_vec(),
        }
    }

    #[test]
    fn unique_project_bucket_resolves_equally_for_attached_and_detached_callers() -> Result<()> {
        let target = project("/repo/project-alpha");
        let session = snapshot("project-alpha", "$1");
        let sessions = [evidence(&session, std::slice::from_ref(&target))];

        let detached = select_session(&target, &sessions, None)?
            .expect("detached caller should resolve the project bucket");
        let attached = select_session(&target, &sessions, Some("$1"))?
            .expect("attached caller should resolve the same project bucket");
        let other_ambient = select_session(&target, &sessions, Some("$9"))?
            .expect("unrelated ambient context should not change resolution");

        assert_eq!(detached.session_id, "$1");
        assert!(!detached.is_ambient);
        assert_eq!(attached.session_id, detached.session_id);
        assert!(attached.is_ambient);
        assert_eq!(other_ambient.session_id, detached.session_id);
        assert!(!other_ambient.is_ambient);
        Ok(())
    }

    #[test]
    fn split_project_reports_sessions_in_deterministic_order() {
        let target = project("/repo/project-alpha");
        let later = snapshot("zeta", "$2");
        let earlier = snapshot("alpha", "$1");
        let sessions = [
            evidence(&later, std::slice::from_ref(&target)),
            evidence(&earlier, std::slice::from_ref(&target)),
        ];

        let error = select_session(&target, &sessions, Some("$2"))
            .expect_err("ambient context must not override split topology");
        let message = error.to_string();
        assert!(message.contains("live panes in multiple tmux sessions"));
        assert!(message.contains("\"alpha\", \"zeta\""));
    }

    #[test]
    fn mixed_project_session_reports_every_project_root() {
        let target = project("/repo/project-alpha");
        let other = project("/repo/project-beta");
        let session = snapshot("mixed", "$1");
        let sessions = [evidence(&session, &[target.clone(), other])];

        let error = select_session(&target, &sessions, None)
            .expect_err("a mixed-project session should fail closed");
        let message = error.to_string();
        assert!(message.contains("contains panes from multiple Git projects"));
        assert!(message.contains("/repo/project-alpha, /repo/project-beta"));
    }

    #[test]
    fn unrelated_inconsistent_session_does_not_block_target_project() -> Result<()> {
        let target = project("/repo/project-alpha");
        let other = project("/repo/project-beta");
        let third = project("/repo/project-gamma");
        let target_session = snapshot("project-alpha", "$1");
        let unrelated = snapshot("unrelated-mixed", "$2");
        let sessions = [
            evidence(&target_session, std::slice::from_ref(&target)),
            evidence(&unrelated, &[other, third]),
        ];

        let selected = select_session(&target, &sessions, None)?
            .expect("unrelated inconsistency should not block the target");
        assert_eq!(selected.session_id, "$1");
        Ok(())
    }

    #[test]
    fn missing_project_evidence_returns_no_session() -> Result<()> {
        let target = project("/repo/project-alpha");
        let other = project("/repo/project-beta");
        let session = snapshot("project-beta", "$1");
        let sessions = [evidence(&session, &[other])];

        assert!(select_session(&target, &sessions, None)?.is_none());
        Ok(())
    }

    #[test]
    fn topology_collapses_linked_worktrees_and_ignores_neutral_and_sidebar_panes() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let worktree_base = fixture.root().join("project-alpha__worktrees");
        let linked = worktree_base.join("feature-auth");
        std::fs::create_dir(&worktree_base)?;
        let linked_text = linked
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("linked worktree path should be UTF-8"))?;
        fixture.git(&["worktree", "add", "-b", "feature/auth", linked_text])?;
        let other = GitRepoFixture::new()?;
        let neutral = tempfile::tempdir()?;
        let mut session = snapshot("project-alpha", "$1");
        session.panes = vec![
            pane(Some(fixture.path()), None),
            pane(Some(&linked), None),
            pane(Some(neutral.path()), None),
            pane(Some(other.path()), Some("sidebar")),
        ];

        let topology = collect_evidence(std::slice::from_ref(&session));
        let expected = RepoPaths::discover(fixture.path())?.project_identity()?;

        assert_eq!(topology.len(), 1);
        assert_eq!(topology[0].projects, vec![expected]);
        Ok(())
    }

    #[test]
    fn topology_detects_two_projects_in_one_session() -> Result<()> {
        let first = GitRepoFixture::new()?;
        let second = GitRepoFixture::new()?;
        let mut session = snapshot("mixed", "$1");
        session.panes = vec![
            pane(Some(first.path()), None),
            pane(Some(second.path()), None),
        ];

        let topology = collect_evidence(std::slice::from_ref(&session));

        assert_eq!(topology[0].projects.len(), 2);
        Ok(())
    }

    #[test]
    fn removal_check_refreshes_panes_created_after_resolution() -> Result<()> {
        let repo = GitRepoFixture::new()?;
        let worktree_base = repo.root().join("project-alpha__worktrees");
        let workspace = worktree_base.join("feature-auth");
        std::fs::create_dir(&worktree_base)?;
        let workspace_text = workspace
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("test workspace path should be UTF-8"))?;
        repo.git(&["worktree", "add", "-b", "feature/auth", workspace_text])?;
        let Some(fixture) = TmuxFixture::new()? else {
            return Ok(());
        };
        create_test_session(&fixture.tmux, "primary", repo.path())?;
        let primary = fixture
            .tmux
            .list_session_snapshots()?
            .into_iter()
            .find(|session| session.session_name == "primary")
            .ok_or_else(|| anyhow::anyhow!("expected primary test session"))?;
        let resolution = ProjectSessionResolution {
            tmux: fixture.tmux.clone(),
            project: RepoPaths::discover(repo.path())?.project_identity()?,
            selected: Some(SelectedSession {
                session_name: primary.session_name,
                session_id: primary.session_id,
                is_ambient: false,
            }),
            lifecycle_lock: WorkspaceStateStore::new(
                &RepoPaths::discover(repo.path())?.git_common_dir,
            )
            .lock_lifecycle()?,
        };

        create_test_session(&fixture.tmux, "late-external", &workspace)?;

        let error = resolution
            .prepare_workspace_removal(&workspace, "kmux-feature-auth")
            .expect_err("fresh removal scan should detect a newly-created external pane");
        assert!(error.to_string().contains("late-external"));
        Ok(())
    }
}
