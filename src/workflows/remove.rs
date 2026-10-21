//! Explicit removal protects committed HEAD before changing files, registration, or presentation.

use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::cli;
use crate::git::WorktreeInfo;
use crate::paths::same_path;
use crate::state::workspace::{WorkspaceState, WorkspaceStateStore};
use crate::workspace::{Authority, Retention, WorkspacePolicy, WorkspaceRecord};

use super::context::{RepoContext, load_repo_context};
use super::project_session::{self, ProjectSessionResolution};
use super::resolve::{load_workspace_state, resolve_current_kmux_workspace, resolve_workspace};

/// Remove an explicitly owned checkout while protecting detached commits and publication branches.
pub(super) fn run(args: cli::RemoveArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let resolution = project_session::resolve(&repo.paths)?;
    let workspace = resolve_remove_target(&repo, args.name.as_deref())?;
    if same_path(workspace.path(), &repo.paths.main_worktree) {
        bail!(
            "cannot remove the main worktree at {}",
            workspace.path().display()
        );
    }
    if workspace.policy().authority() != Authority::Kmux {
        bail!(
            "kmux has no lifecycle authority for this external worktree; use 'kmux workspace close' to close its presentation"
        );
    }
    if !workspace.is_live() {
        bail!("workspace registration is stale; refusing to remove a replacement path");
    }
    let entry = workspace
        .git()
        .context("workspace has no live registration")?;
    let plan = RemovalPlan::new(workspace.policy(), entry)?;
    let dirty = repo.git.worktree_is_dirty(workspace.path())?;
    let branch_safe = if args.force {
        None
    } else {
        plan.branch_to_delete()
            .map(|branch| repo.git.branch_is_safely_deletable(branch))
            .transpose()?
    };
    validate_removal_safety(dirty, args.force, branch_safe)?;
    let (state, _) = load_workspace_state(&repo)?;
    let remaining_children = state.children_of(workspace.policy().id());
    // Validate live panes before protecting refs and refresh Git evidence immediately afterward.
    let window_id = resolution.prepare_workspace_removal(&workspace, &repo.config)?;
    leave_worktree_before_removal(&repo.paths.main_worktree)?;
    let mut effects = LiveRemoval {
        repo: &repo,
        resolution: &resolution,
        workspace: &workspace,
        plan: &plan,
        state,
        window_id,
        force: args.force,
    };
    let recovery = execute_removal(&mut effects)?;
    if !remaining_children.is_empty() {
        eprintln!(
            "warning: parent links still reference removed workspace '{}': {}",
            workspace.policy().label(),
            remaining_children.join(", ")
        );
    }
    println!("removed {}", workspace.policy().label());
    if let Some(reference) = recovery {
        println!("recovery ref: {reference}");
        println!(
            "recreate: git worktree add --detach {} {reference}",
            quote_path(workspace.path())
        );
    }
    Ok(())
}

#[derive(Debug, PartialEq, Eq)]
enum CommitProtection {
    KnownCreationAnchor,
    RecoveryRef,
    PreservedBranch(String),
    OwnedBranch(String),
}

#[derive(Debug)]
struct RemovalPlan {
    head: String,
    protection: CommitProtection,
}

impl RemovalPlan {
    // Keep authority, retention, and checkout state independent in deletion policy.
    fn new(policy: &WorkspacePolicy, entry: &WorktreeInfo) -> Result<Self> {
        if policy.authority() != Authority::Kmux || !policy.matches_registration(entry) {
            bail!("removal requires authority over the original worktree registration");
        }
        if entry.locked.is_some() {
            bail!("worktree is locked; unlock it explicitly before removal");
        }
        let head = entry
            .head
            .clone()
            .context("workspace has no committed HEAD to protect")?;
        let protection = match entry.branch.as_deref() {
            Some(branch)
                if policy.retention() == Some(Retention::Persistent)
                    && policy.owned_branch() == Some(branch) =>
            {
                CommitProtection::OwnedBranch(branch.to_owned())
            }
            Some(branch) => CommitProtection::PreservedBranch(branch.to_owned()),
            None if policy.creation_anchor() == Some(head.as_str()) => {
                CommitProtection::KnownCreationAnchor
            }
            None => CommitProtection::RecoveryRef,
        };
        Ok(Self { head, protection })
    }

    fn branch_to_delete(&self) -> Option<&str> {
        match &self.protection {
            CommitProtection::OwnedBranch(branch) => Some(branch),
            _ => None,
        }
    }
}

// Force authorizes uncommitted-file loss and the existing owned-branch unmerged boundary.
// It never bypasses ownership, registration, lock, presentation, or recovery-ref checks.
fn validate_removal_safety(
    dirty: bool,
    force: bool,
    owned_branch_safe: Option<bool>,
) -> Result<()> {
    if dirty && !force {
        bail!(
            "worktree has uncommitted changes; --force discards those files while detached committed HEAD is protected separately"
        );
    }
    if owned_branch_safe == Some(false) && !force {
        bail!("owned branch is not safely merged; use --force to delete the workspace anyway");
    }
    Ok(())
}

// This seam expresses destructive ordering and permits process-free failure tests.
trait RemovalEffects {
    /// Return only after committed HEAD protection has been verified.
    fn protect_commits(&mut self) -> Result<Option<String>>;
    fn revalidate_checkout(&mut self) -> Result<()>;
    fn remove_worktree(&mut self) -> Result<()>;
    fn finish_removal(&mut self) -> Result<()>;
}

fn execute_removal(effects: &mut impl RemovalEffects) -> Result<Option<String>> {
    let recovery = effects.protect_commits()?;
    let result = (|| {
        effects.revalidate_checkout()?;
        effects.remove_worktree()?;
        effects.finish_removal()
    })();
    result.with_context(|| match &recovery {
        Some(reference) => {
            format!("removal did not finish; committed work remains protected by {reference}")
        }
        None => "removal did not finish; inspect the workspace before retrying".to_owned(),
    })?;
    Ok(recovery)
}

struct LiveRemoval<'a> {
    repo: &'a RepoContext,
    resolution: &'a ProjectSessionResolution,
    workspace: &'a WorkspaceRecord,
    plan: &'a RemovalPlan,
    state: WorkspaceState,
    window_id: Option<String>,
    force: bool,
}

impl RemovalEffects for LiveRemoval<'_> {
    fn protect_commits(&mut self) -> Result<Option<String>> {
        match &self.plan.protection {
            CommitProtection::RecoveryRef => self
                .repo
                .git
                .create_recovery_ref(self.workspace.policy().id(), &self.plan.head)
                .map(Some),
            CommitProtection::PreservedBranch(branch) | CommitProtection::OwnedBranch(branch) => {
                self.repo
                    .git
                    .verify_preserved_branch(branch, &self.plan.head)?;
                Ok(None)
            }
            CommitProtection::KnownCreationAnchor => Ok(None),
        }
    }

    fn revalidate_checkout(&mut self) -> Result<()> {
        // A hook or another client may have changed panes while recovery refs were written.
        let window_id = self
            .resolution
            .prepare_workspace_removal(self.workspace, &self.repo.config)?;
        if window_id != self.window_id {
            bail!(
                "workspace presentation changed during removal; retry after checking its windows"
            );
        }
        let entries = self.repo.git.worktrees()?;
        let current = entries
            .iter()
            .find(|entry| self.workspace.policy().matches_registration(entry));
        if current != self.workspace.git() {
            bail!("Git worktree changed during removal; retry with its current checkout state");
        }
        Ok(())
    }

    fn remove_worktree(&mut self) -> Result<()> {
        self.repo
            .git
            .remove_worktree(self.workspace.path(), self.force)
    }

    fn finish_removal(&mut self) -> Result<()> {
        if let Some(branch) = self.plan.branch_to_delete() {
            self.repo
                .git
                .verify_preserved_branch(branch, &self.plan.head)?;
            self.repo.git.delete_local_branch(branch, true)?;
        }
        if let Some(directory) = self.workspace.policy().allocation_directory()
            && let Err(error) = std::fs::remove_dir(directory)
        {
            eprintln!(
                "workspace removed; allocation directory {} remains: {error}",
                directory.display()
            );
        }
        self.state.remove_policy(self.workspace.policy().id());
        WorkspaceStateStore::new(&self.repo.paths.git_common_dir).save(&self.state)?;
        if let Some(window_id) = &self.window_id {
            self.resolution.kill_prepared_window(window_id)?;
        }
        Ok(())
    }
}

// Leave the checkout so later subprocesses inherit an existing directory after deletion.
fn leave_worktree_before_removal(main_worktree: &Path) -> Result<()> {
    std::env::set_current_dir(main_worktree).with_context(|| {
        format!(
            "failed to leave worktree before removal for {}",
            main_worktree.display()
        )
    })
}

fn resolve_remove_target(repo: &RepoContext, name: Option<&str>) -> Result<WorkspaceRecord> {
    if let Some(name) = name {
        return resolve_workspace(repo, name);
    }
    resolve_current_kmux_workspace(repo, "workspace remove")
}

// The printed command is POSIX-shell-ready even for whitespace and single quotes in paths.
fn quote_path(path: &Path) -> String {
    format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn removal_policy_separates_retention_checkout_and_branch_authority() -> Result<()> {
        for retention in [Retention::Ephemeral, Retention::Persistent] {
            let policy = WorkspacePolicy::owned(
                "ws-example".to_owned(),
                PathBuf::from("/repo/workspace"),
                "example".to_owned(),
                retention,
                Some("anchor".to_owned()),
                None,
            )?;
            let mut entry = registration(&policy);
            assert_eq!(
                RemovalPlan::new(&policy, &entry)?.protection,
                CommitProtection::KnownCreationAnchor
            );
            entry.head = Some("advanced".to_owned());
            assert_eq!(
                RemovalPlan::new(&policy, &entry)?.protection,
                CommitProtection::RecoveryRef
            );
            entry.branch = Some("publication/one".to_owned());
            entry.detached = false;
            assert_eq!(
                RemovalPlan::new(&policy, &entry)?.protection,
                CommitProtection::PreservedBranch("publication/one".to_owned())
            );
            entry.kmux_binding = None;
            assert!(RemovalPlan::new(&policy, &entry).is_err());
        }
        let policy = WorkspacePolicy::owned(
            "ws-owned".to_owned(),
            PathBuf::from("/repo/legacy"),
            "legacy".to_owned(),
            Retention::Persistent,
            None,
            Some("feature/owned".to_owned()),
        )?;
        let mut entry = registration(&policy);
        assert_eq!(
            RemovalPlan::new(&policy, &entry)?.protection,
            CommitProtection::RecoveryRef
        );
        entry.branch = Some("feature/owned".to_owned());
        entry.detached = false;
        assert_eq!(
            RemovalPlan::new(&policy, &entry)?.branch_to_delete(),
            Some("feature/owned")
        );
        for primary in [true, false] {
            let observed = WorkspacePolicy::observed(PathBuf::from("/repo/external"), primary);
            assert!(RemovalPlan::new(&observed, &registration(&observed)).is_err());
        }
        assert!(validate_removal_safety(true, false, None).is_err());
        assert!(validate_removal_safety(false, false, Some(false)).is_err());
        validate_removal_safety(true, true, Some(false))?;
        Ok(())
    }

    #[test]
    fn failed_snapshot_creation_or_verification_stops_all_destructive_effects() {
        for failure in ["create", "verify", "changed"] {
            let mut effects = FakeRemoval {
                failure: Some(failure),
                events: Vec::new(),
            };
            assert!(execute_removal(&mut effects).is_err());
            assert!(!effects.events.contains(&"remove worktree"));
            assert!(!effects.events.contains(&"branch policy window"));
        }
        let mut effects = FakeRemoval {
            failure: None,
            events: Vec::new(),
        };
        assert_eq!(
            execute_removal(&mut effects).expect("successful removal"),
            Some("refs/kmux/recovery/example".to_owned())
        );
        assert_eq!(
            effects.events,
            [
                "create",
                "verify",
                "revalidate",
                "remove worktree",
                "branch policy window"
            ]
        );
    }

    fn registration(policy: &WorkspacePolicy) -> WorktreeInfo {
        WorktreeInfo {
            path: policy.path().to_path_buf(),
            head: Some("anchor".to_owned()),
            branch: None,
            detached: true,
            bare: false,
            locked: None,
            prunable: None,
            kmux_binding: Some(policy.id().to_owned()),
        }
    }

    struct FakeRemoval {
        failure: Option<&'static str>,
        events: Vec<&'static str>,
    }

    impl RemovalEffects for FakeRemoval {
        fn protect_commits(&mut self) -> Result<Option<String>> {
            self.events.push("create");
            if self.failure == Some("create") {
                bail!("ref creation failed")
            }
            self.events.push("verify");
            if self.failure == Some("verify") {
                bail!("ref verification failed")
            }
            Ok(Some("refs/kmux/recovery/example".to_owned()))
        }
        fn revalidate_checkout(&mut self) -> Result<()> {
            self.events.push("revalidate");
            if self.failure == Some("changed") {
                bail!("checkout changed")
            }
            Ok(())
        }
        fn remove_worktree(&mut self) -> Result<()> {
            self.events.push("remove worktree");
            Ok(())
        }
        fn finish_removal(&mut self) -> Result<()> {
            self.events.push("branch policy window");
            Ok(())
        }
    }
}
