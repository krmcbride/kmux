//! Shared workspace activity aggregates used by status, badges, and the sidebar.
//!
//! Logical agent sessions collapse here by canonical Git root. The aggregate
//! explicitly owns the chosen primary session and every member session key so
//! display, navigation, and workspace-wide actions use one application policy.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use anyhow::Result;

use crate::agent::sessions::{
    AgentTmuxTarget, ResolvedAgentSession, activity_status_priority, resolved_agent_sessions,
};
use crate::state::StateStore;
use crate::state::{AgentSessionKey, AgentStatus};
use crate::tmux::Tmux;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Application read model for all current agent activity in one Git workspace.
pub struct WorkspaceActivity {
    workspace_key: String,
    primary_session: ResolvedAgentSession,
    /// Sorted, deduplicated logical sessions currently represented by this workspace.
    member_session_keys: Vec<AgentSessionKey>,
    pub primary: String,
    pub secondary: String,
    pub display_title: String,
    pub display_context: String,
    workspace_slug: Option<String>,
}

impl WorkspaceActivity {
    fn from_primary(
        primary_session: ResolvedAgentSession,
        member_session_keys: Vec<AgentSessionKey>,
    ) -> Self {
        let workspace_key = primary_session.workspace_key().to_owned();
        let primary = repo_label(&primary_session);
        let secondary = branch_label(&primary_session, &primary);
        let workspace_slug = path_label(Some(primary_session.git_worktree_path()));
        let display_title = primary_session
            .title
            .as_deref()
            .filter(|title| *title != primary && *title != secondary)
            .or_else(|| {
                primary_session
                    .tmux_pane_title()
                    .filter(|title| *title != primary && *title != secondary)
            })
            .or_else(|| primary_session.tmux_pane_current_command())
            .map(str::to_owned)
            .or_else(|| fallback_session_title(&primary_session, &primary, &secondary))
            .unwrap_or_default();
        let display_context = primary_session
            .context
            .as_deref()
            .map(str::trim)
            .filter(|context| !context.is_empty())
            .unwrap_or_default()
            .to_owned();

        Self {
            workspace_key,
            primary_session,
            member_session_keys,
            primary,
            secondary,
            display_title,
            display_context,
            workspace_slug,
        }
    }

    /// Return the logical session selected to represent this workspace.
    pub fn primary_session_key(&self) -> &AgentSessionKey {
        &self.primary_session.key
    }

    /// Return the canonical Git root used as this activity's identity.
    pub fn workspace_key(&self) -> &str {
        &self.workspace_key
    }

    /// Return the sorted logical sessions represented by this workspace.
    pub fn member_session_keys(&self) -> &[AgentSessionKey] {
        &self.member_session_keys
    }

    /// Return the status selected by the shared workspace activity policy.
    pub fn status(&self) -> AgentStatus {
        self.primary_session.status
    }

    /// Return when the primary session was first observed.
    pub fn created_at(&self) -> u64 {
        self.primary_session.created_at
    }

    /// Return the age of the primary session's current status.
    pub fn status_age_secs(&self, now: u64) -> u64 {
        now.saturating_sub(self.primary_session.status_changed_at)
    }

    /// Return display-neutral elapsed seconds for the primary session at `now`.
    pub fn elapsed_secs(&self, now: u64) -> u64 {
        self.primary_session.elapsed_secs(now)
    }

    /// Return the settled live tmux candidates or unavailable reason.
    pub fn tmux_target(&self) -> &AgentTmuxTarget {
        &self.primary_session.tmux_target
    }

    /// Return whether this activity has an exact matching-window candidate set.
    pub fn has_window_tmux_target(&self) -> bool {
        matches!(self.tmux_target(), AgentTmuxTarget::Windows { .. })
    }

    /// Return the latest primary-session title reported by a producer.
    pub fn title(&self) -> Option<&str> {
        self.primary_session.title.as_deref()
    }

    /// Return the latest primary-session context reported by a producer.
    pub fn context(&self) -> Option<&str> {
        self.primary_session.context.as_deref()
    }

    /// Return whether this row matches a user-supplied status filter.
    pub fn matches_filter(&self, filter: &str) -> bool {
        self.primary_session.key.agent_kind == filter
            || self.primary_session.key.session_id == filter
            || self.workspace_key == filter
            || self.primary == filter
            || self.secondary == filter
            || self.workspace_slug() == Some(filter)
            || self.git_branch() == Some(filter)
            || self.tmux_window_name() == Some(filter)
            || self.git_worktree_path() == filter
            || self.directory() == Some(filter)
            || self.title() == Some(filter)
            || self.display_title == filter
    }

    /// Return the workspace slug derived from the canonical worktree path.
    pub fn workspace_slug(&self) -> Option<&str> {
        self.workspace_slug.as_deref()
    }

    /// Return the primary session's best known Git branch.
    pub fn git_branch(&self) -> Option<&str> {
        self.primary_session.git_branch()
    }

    /// Return the canonical Git worktree path.
    pub fn git_worktree_path(&self) -> &str {
        self.primary_session.git_worktree_path()
    }

    /// Return the latest directory reported by the primary session.
    pub fn directory(&self) -> Option<&str> {
        self.primary_session.directory()
    }

    /// Return the exact tmux session selected by reconciliation.
    pub fn tmux_session_name(&self) -> Option<&str> {
        self.primary_session.target.tmux_session_name.as_deref()
    }

    /// Return the preferred matching tmux window name.
    pub fn tmux_window_name(&self) -> Option<&str> {
        self.primary_session.tmux_window_name()
    }

    /// Return the preferred matching physical tmux window ID.
    pub fn tmux_window_id(&self) -> Option<&str> {
        self.primary_session.tmux_window_id()
    }

    /// Return the preferred matching non-sidebar pane ID.
    pub fn tmux_pane_id(&self) -> Option<&str> {
        self.primary_session.target.tmux_pane_id.as_deref()
    }

    /// Return the preferred pane title used as a display fallback.
    pub fn tmux_pane_title(&self) -> Option<&str> {
        self.primary_session.tmux_pane_title()
    }
}

/// Query the shared workspace activity read model from persisted and live state.
pub fn workspace_activities(store: &StateStore, tmux: &Tmux) -> Result<Vec<WorkspaceActivity>> {
    Ok(workspace_activities_from_sessions(resolved_agent_sessions(
        store, tmux,
    )?))
}

/// Collapse logical sessions into deterministic workspace activity aggregates.
pub fn workspace_activities_from_sessions(
    sessions: Vec<ResolvedAgentSession>,
) -> Vec<WorkspaceActivity> {
    let mut by_workspace = BTreeMap::<String, WorkspaceActivityAccumulator>::new();
    for session in sessions {
        let workspace_key = session.workspace_key().to_owned();
        let session_key = session.key.clone();
        match by_workspace.entry(workspace_key) {
            std::collections::btree_map::Entry::Vacant(entry) => {
                entry.insert(WorkspaceActivityAccumulator {
                    primary_session: session,
                    member_session_keys: BTreeSet::from([session_key]),
                });
            }
            std::collections::btree_map::Entry::Occupied(mut entry) => {
                let activity = entry.get_mut();
                activity.member_session_keys.insert(session_key);
                if primary_session_order(&session, &activity.primary_session).is_gt() {
                    activity.primary_session = session;
                }
            }
        }
    }

    let mut activities = by_workspace
        .into_values()
        .map(|activity| {
            WorkspaceActivity::from_primary(
                activity.primary_session,
                activity.member_session_keys.into_iter().collect(),
            )
        })
        .collect::<Vec<_>>();
    activities.sort_by(|left, right| {
        (
            &left.primary,
            &left.secondary,
            left.created_at(),
            &left.workspace_key,
            &left.primary_session.key.agent_kind,
            &left.primary_session.key.session_id,
        )
            .cmp(&(
                &right.primary,
                &right.secondary,
                right.created_at(),
                &right.workspace_key,
                &right.primary_session.key.agent_kind,
                &right.primary_session.key.session_id,
            ))
    });
    activities
}

struct WorkspaceActivityAccumulator {
    primary_session: ResolvedAgentSession,
    member_session_keys: BTreeSet<AgentSessionKey>,
}

// Higher ordering wins. A smaller stable session key wins the final tie so the
// result is deterministic regardless of observation or filesystem order.
fn primary_session_order(
    candidate: &ResolvedAgentSession,
    current: &ResolvedAgentSession,
) -> Ordering {
    activity_status_priority(candidate.status)
        .cmp(&activity_status_priority(current.status))
        .then_with(|| {
            candidate
                .status_observed_at
                .cmp(&current.status_observed_at)
        })
        .then_with(|| candidate.observed_at.cmp(&current.observed_at))
        .then_with(|| current.key.cmp(&candidate.key))
}

// Primary label should be stable and repo-oriented; fall back through paths,
// tmux window name, and finally session id.
fn repo_label(view: &ResolvedAgentSession) -> String {
    clean_label(view.git_repo_name())
        .or_else(|| path_label(view.git_repo_path()))
        .or_else(|| path_label(view.directory()))
        .or_else(|| path_label(Some(view.git_worktree_path())))
        .or_else(|| clean_label(view.tmux_window_name()))
        .unwrap_or_else(|| view.key.session_id.clone())
}

// Secondary label should add distinguishing context without repeating primary.
fn branch_label(view: &ResolvedAgentSession, primary: &str) -> String {
    clean_label(view.git_branch())
        .or_else(|| path_distinct_label(view.directory(), primary))
        .or_else(|| path_distinct_label(Some(view.git_worktree_path()), primary))
        .or_else(|| distinct_label(view.tmux_window_name(), primary))
        .or_else(|| fallback_session_label(view, primary))
        .unwrap_or_default()
}

fn clean_label(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
}

fn distinct_label(value: Option<&str>, primary: &str) -> Option<String> {
    clean_label(value).filter(|value| value != primary)
}

fn path_label(value: Option<&str>) -> Option<String> {
    clean_label(value).and_then(|value| {
        Path::new(&value)
            .file_name()
            .map(|name| name.to_string_lossy().into_owned())
            .filter(|name| !name.is_empty())
    })
}

fn path_distinct_label(value: Option<&str>, primary: &str) -> Option<String> {
    path_label(value).filter(|value| value != primary)
}

fn fallback_session_label(view: &ResolvedAgentSession, primary: &str) -> Option<String> {
    let label = compact_session_id(&view.key.session_id).to_owned();
    (label != primary).then_some(label)
}

fn fallback_session_title(
    view: &ResolvedAgentSession,
    primary: &str,
    secondary: &str,
) -> Option<String> {
    let label = format!("session {}", compact_session_id(&view.key.session_id));
    (label != primary && label != secondary).then_some(label)
}

fn compact_session_id(session_id: &str) -> &str {
    session_id.get(..12).unwrap_or(session_id)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::process::Command;

    use anyhow::Result;
    use tempfile::tempdir;

    use super::*;
    use crate::agent::sessions::{
        AgentTmuxTarget, AgentTmuxUnavailableReason, ResolvedAgentTarget, ResolvedAgentWorkspace,
    };
    use crate::state::{AgentLocationHints, AgentObservationKey, AgentObservationState};

    #[test]
    fn workspace_activity_uses_repo_branch_title_and_context_labels() {
        let view = session_view();

        let rows = workspace_activities_from_sessions(vec![view]);

        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].primary, "kmux");
        assert_eq!(rows[0].secondary, "feature/sidebar");
        assert_eq!(rows[0].display_title, "Implement shared rows");
        assert_eq!(rows[0].display_context, "55.2K");
        assert!(rows[0].matches_filter("feature/sidebar"));
        assert!(rows[0].matches_filter("/repo/project-alpha"));
    }

    #[test]
    fn primary_session_is_explicit_and_distinct_from_sorted_members() {
        let mut working = session_view();
        working.key.session_id = "ses_a_working".to_owned();
        working.status = AgentStatus::Working;
        working.status_observed_at = 300;
        working.observed_at = 300;
        let mut waiting = session_view();
        waiting.key.session_id = "ses_z_waiting".to_owned();
        waiting.status = AgentStatus::Waiting;
        waiting.status_observed_at = 100;
        waiting.observed_at = 100;

        let activities = workspace_activities_from_sessions(vec![working, waiting]);

        assert_eq!(activities.len(), 1);
        assert_eq!(
            activities[0].primary_session_key().session_id,
            "ses_z_waiting"
        );
        assert_eq!(
            activities[0]
                .member_session_keys()
                .iter()
                .map(|key| key.session_id.as_str())
                .collect::<Vec<_>>(),
            ["ses_a_working", "ses_z_waiting"]
        );
    }

    #[test]
    fn equal_time_primary_selection_uses_stable_session_key() {
        let mut later_key = session_view();
        later_key.key.session_id = "ses_z".to_owned();
        let mut earlier_key = session_view();
        earlier_key.key.session_id = "ses_a".to_owned();

        let forward =
            workspace_activities_from_sessions(vec![later_key.clone(), earlier_key.clone()]);
        let reverse = workspace_activities_from_sessions(vec![earlier_key, later_key]);

        assert_eq!(forward[0].primary_session_key().session_id, "ses_a");
        assert_eq!(reverse[0].primary_session_key().session_id, "ses_a");
    }

    #[test]
    fn primary_selection_uses_each_observation_recency_tier() {
        let cases = [
            ("newer status observation", 200, 100, 100, 300),
            ("newer overall observation", 100, 200, 100, 100),
        ];
        for (
            label,
            candidate_status_at,
            candidate_observed_at,
            current_status_at,
            current_observed_at,
        ) in cases
        {
            let mut candidate = session_view();
            candidate.key.session_id = "ses_z_candidate".to_owned();
            candidate.status_observed_at = candidate_status_at;
            candidate.observed_at = candidate_observed_at;
            let mut current = session_view();
            current.key.session_id = "ses_a_current".to_owned();
            current.status_observed_at = current_status_at;
            current.observed_at = current_observed_at;

            for sessions in [
                vec![current.clone(), candidate.clone()],
                vec![candidate.clone(), current.clone()],
            ] {
                let activities = workspace_activities_from_sessions(sessions);
                assert_eq!(
                    activities[0].primary_session_key().session_id,
                    "ses_z_candidate",
                    "{label} should win regardless of input order"
                );
            }
        }
    }

    #[test]
    fn tmux_snapshot_failure_keeps_workspace_activity_visible() -> Result<()> {
        let temp = tempdir()?;
        let repo = temp.path().join("project-alpha");
        fs::create_dir(&repo)?;
        run_git(&repo, &["init", "--initial-branch", "main"]);
        let store = crate::state::test_support::store_with_path(temp.path().join("state"))?;
        let repo_path = repo.to_string_lossy().into_owned();
        store.upsert_observation(&AgentObservationState {
            key: AgentObservationKey {
                session: AgentSessionKey {
                    agent_kind: "example-agent".to_owned(),
                    session_id: "ses_project_alpha".to_owned(),
                },
                producer_kind: "example-producer".to_owned(),
                producer_instance: "instance-1".to_owned(),
            },
            created_at: 100,
            status: Some(AgentStatus::Working),
            status_observed_at: Some(100),
            status_changed_at: Some(100),
            working_elapsed_secs: 0,
            observed_at: 100,
            title: Some("Example task".to_owned()),
            context: None,
            target: AgentLocationHints {
                directory: Some(repo_path),
                ..AgentLocationHints::default()
            },
        })?;
        let tmux = crate::tmux::test_support::disconnected_adapter();

        let activities = workspace_activities(&store, &tmux)?;

        assert_eq!(activities.len(), 1);
        assert_eq!(
            activities[0].workspace_key(),
            fs::canonicalize(&repo)?.to_string_lossy()
        );
        assert!(matches!(
            activities[0].tmux_target(),
            AgentTmuxTarget::Unavailable(AgentTmuxUnavailableReason::Missing)
        ));
        Ok(())
    }

    fn session_view() -> ResolvedAgentSession {
        let key = AgentSessionKey {
            agent_kind: "opencode".to_owned(),
            session_id: "ses_feature_sidebar".to_owned(),
        };
        ResolvedAgentSession {
            key,
            workspace: ResolvedAgentWorkspace::from_canonical_root(
                "/repo/project-alpha".into(),
                "/repo/project-alpha".to_owned(),
            )
            .expect("test workspace should be valid"),
            tmux_target: AgentTmuxTarget::Windows {
                session_name: "project-alpha".to_owned(),
                candidates: vec![crate::agent::sessions::AgentTmuxWindowCandidate {
                    window_id: "@1".to_owned(),
                    pane_ids: vec!["%1".to_owned()],
                }],
            },
            created_at: 120,
            status: AgentStatus::Working,
            status_observed_at: 120,
            status_changed_at: 120,
            working_elapsed_secs: 0,
            observed_at: 120,
            title: Some("Implement shared rows".to_owned()),
            context: Some("55.2K".to_owned()),
            target: ResolvedAgentTarget {
                git_repo_name: Some("kmux".to_owned()),
                git_repo_path: Some("/repo/kmux".to_owned()),
                git_branch: Some("feature/sidebar".to_owned()),
                tmux_session_name: Some("project-alpha".to_owned()),
                tmux_window_id: Some("@1".to_owned()),
                tmux_window_name: Some("kmux-feature-sidebar".to_owned()),
                tmux_pane_id: Some("%1".to_owned()),
                tmux_pane_title: Some("Implement sidebar".to_owned()),
                ..ResolvedAgentTarget::default()
            },
        }
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
}
