//! Branch, remote, ref, and ancestry operations.

use std::collections::HashSet;
use std::ffi::OsString;

use anyhow::{Result, anyhow, bail};

use super::Git;
use super::process::bail_git;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Outcome of ensuring a local branch exists.
pub enum BranchAction {
    Existing,
    Created,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Remote-tracking branch split into remote name, local branch name, and full ref.
pub struct RemoteBranch {
    pub remote: String,
    pub branch: String,
    pub ref_name: String,
}

impl Git {
    /// Resolve an explicit ref to a commit, rejecting option-like input at the Git boundary.
    pub fn resolve_commit(&self, reference: &str) -> Result<String> {
        self.stdout([
            "rev-parse",
            "--verify",
            "--end-of-options",
            &format!("{reference}^{{commit}}"),
        ])
    }
    /// Return the current branch name, or `None` when HEAD is detached.
    pub fn current_branch(&self) -> Result<Option<String>> {
        let output = self.output(["symbolic-ref", "--quiet", "--short", "HEAD"])?;
        if output.status.success() {
            let branch = output.stdout.trim();
            if branch.is_empty() {
                Ok(None)
            } else {
                Ok(Some(branch.to_owned()))
            }
        } else if output.status.code() == Some(1) {
            Ok(None)
        } else {
            bail_git(output)
        }
    }

    /// Return the current branch or fail with a user-facing detached-HEAD message.
    pub fn require_current_branch(&self) -> Result<String> {
        self.current_branch()?
            .ok_or_else(|| anyhow!("cannot create a branch from detached HEAD without --parent"))
    }

    /// Return whether a local branch exists under `refs/heads`.
    pub fn local_branch_exists(&self, branch: &str) -> Result<bool> {
        let output = self.output(vec![
            OsString::from("show-ref"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from(format!("refs/heads/{branch}")),
        ])?;
        if output.status.success() {
            Ok(true)
        } else if output.status.code() == Some(1) {
            Ok(false)
        } else {
            bail_git(output)
        }
    }

    /// Ensure `branch` exists locally, creating it from `start_point` or the current branch.
    ///
    /// When the start point is a remote-tracking ref, the new branch is configured to
    /// track that upstream.
    pub fn ensure_local_branch(
        &self,
        branch: &str,
        start_point: Option<&str>,
    ) -> Result<BranchAction> {
        if self.local_branch_exists(branch)? {
            return Ok(BranchAction::Existing);
        }

        let start_point = if let Some(start_point) = start_point {
            start_point.to_owned()
        } else {
            self.require_current_branch()?
        };

        if !self.commit_ref_exists(&start_point)? {
            bail!("start point '{start_point}' does not resolve to a commit");
        }

        self.stdout(vec![
            OsString::from("branch"),
            OsString::from(branch),
            OsString::from(&start_point),
        ])?;
        if self.remote_tracking_branch_exists(&start_point)? {
            self.set_branch_upstream(branch, &start_point)?;
        }
        Ok(BranchAction::Created)
    }

    /// Return the merge-base commit for two refs, or `None` when Git finds no common base.
    pub fn merge_base(&self, left: &str, right: &str) -> Result<Option<String>> {
        let output = self.output(["merge-base", left, right])?;
        if output.status.success() {
            let anchor = output.stdout.trim();
            if anchor.is_empty() {
                Ok(None)
            } else {
                Ok(Some(anchor.to_owned()))
            }
        } else if output.status.code() == Some(1) {
            Ok(None)
        } else {
            bail_git(output)
        }
    }

    /// Return sorted local branch names.
    pub fn local_branch_refs(&self) -> Result<Vec<String>> {
        let output = self.stdout([
            "for-each-ref",
            "--format=%(refname:short)",
            "--sort=refname",
            "refs/heads/",
        ])?;
        Ok(non_empty_branch_refs(&output))
    }

    /// Return branch refs that are not already checked out in any worktree.
    pub fn checkoutable_branch_refs(&self) -> Result<Vec<String>> {
        // This feeds shell tab completion, so keep it to a fixed number of Git
        // subprocesses. Per-ref validation belongs in command execution paths
        // like `known_remote_branch`, where the user has actually selected a ref.
        let checked_out = self
            .worktrees()?
            .into_iter()
            .filter_map(|worktree| worktree.branch)
            .collect::<HashSet<_>>();
        let remotes = self.sorted_remotes_by_length()?;

        Ok(checkoutable_branch_refs_from(
            self.branch_refs()?,
            &checked_out,
            &remotes,
        ))
    }

    /// Resolve a branch ref to a known remote-tracking branch, if it names one.
    pub fn known_remote_branch(&self, branch: &str) -> Result<Option<RemoteBranch>> {
        let remotes = self.sorted_remotes_by_length()?;
        let Some(remote_branch) = remote_branch_from_ref(&remotes, branch) else {
            return Ok(None);
        };

        if self.remote_tracking_branch_exists(branch)? {
            return Ok(Some(remote_branch));
        }

        Ok(None)
    }

    /// Return whether `branch` is merged into its upstream or, without upstream, HEAD.
    pub fn branch_is_safely_deletable(&self, branch: &str) -> Result<bool> {
        let target = self
            .branch_upstream(branch)?
            .unwrap_or_else(|| "HEAD".to_owned());
        let output = self.output(["merge-base", "--is-ancestor", branch, &target])?;
        if output.status.success() {
            Ok(true)
        } else if output.status.code() == Some(1) {
            Ok(false)
        } else {
            bail_git(output)
        }
    }

    /// Delete a local branch using `-d` or `-D` depending on `force`.
    pub fn delete_local_branch(&self, branch: &str, force: bool) -> Result<()> {
        let flag = if force { "-D" } else { "-d" };
        self.stdout(["branch", flag, branch])?;
        Ok(())
    }

    /// Return whether a reference resolves to a commit object.
    pub(super) fn commit_ref_exists(&self, reference: &str) -> Result<bool> {
        let output = self.output(vec![
            OsString::from("rev-parse"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from(format!("{reference}^{{commit}}")),
        ])?;
        if output.status.success() {
            Ok(true)
        } else if output.status.code() == Some(1) {
            Ok(false)
        } else {
            bail_git(output)
        }
    }

    /// Configure a local branch to track an upstream branch.
    fn set_branch_upstream(&self, branch: &str, upstream: &str) -> Result<()> {
        self.stdout(vec![
            OsString::from("branch"),
            OsString::from("--set-upstream-to"),
            OsString::from(upstream),
            OsString::from(branch),
        ])?;
        Ok(())
    }

    /// Return sorted local and remote branch refs suitable for display or completion.
    fn branch_refs(&self) -> Result<Vec<String>> {
        let output = self.stdout([
            "for-each-ref",
            "--format=%(refname:short)",
            "--sort=refname",
            "refs/heads/",
            "refs/remotes/",
        ])?;
        Ok(non_empty_branch_refs(&output))
    }

    /// Return configured Git remote names.
    fn remotes(&self) -> Result<Vec<String>> {
        let output = self.stdout(["remote"])?;
        Ok(non_empty_lines(&output))
    }

    /// Return whether a remote-tracking branch exists under `refs/remotes`.
    fn remote_tracking_branch_exists(&self, branch: &str) -> Result<bool> {
        let output = self.output(vec![
            OsString::from("show-ref"),
            OsString::from("--verify"),
            OsString::from("--quiet"),
            OsString::from(format!("refs/remotes/{branch}")),
        ])?;
        if output.status.success() {
            Ok(true)
        } else if output.status.code() == Some(1) {
            Ok(false)
        } else {
            bail_git(output)
        }
    }

    // Sort longest first so a remote named `origin/private` wins before `origin`.
    fn sorted_remotes_by_length(&self) -> Result<Vec<String>> {
        let mut remotes = self.remotes()?;
        remotes.sort_by_key(|remote| std::cmp::Reverse(remote.len()));
        Ok(remotes)
    }

    // Empty upstream output means the branch exists but has no upstream.
    fn branch_upstream(&self, branch: &str) -> Result<Option<String>> {
        let output = self.stdout(vec![
            OsString::from("for-each-ref"),
            OsString::from("--format=%(upstream:short)"),
            OsString::from(format!("refs/heads/{branch}")),
        ])?;
        Ok(Some(output).filter(|upstream| !upstream.is_empty()))
    }
}

fn non_empty_lines(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn non_empty_branch_refs(output: &str) -> Vec<String> {
    output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && *line != "HEAD" && !line.ends_with("/HEAD"))
        .map(ToOwned::to_owned)
        .collect()
}

fn remote_branch_from_ref(remotes: &[String], branch: &str) -> Option<RemoteBranch> {
    for remote in remotes {
        let prefix = format!("{remote}/");
        let Some(local_branch) = branch.strip_prefix(&prefix) else {
            continue;
        };
        if local_branch.is_empty() {
            continue;
        }

        return Some(RemoteBranch {
            remote: remote.clone(),
            branch: local_branch.to_owned(),
            ref_name: branch.to_owned(),
        });
    }

    None
}

fn checkoutable_branch_refs_from(
    branch_refs: Vec<String>,
    checked_out: &HashSet<String>,
    remotes: &[String],
) -> Vec<String> {
    branch_refs
        .into_iter()
        .filter(|branch| {
            !checked_out.contains(branch)
                && remote_branch_from_ref(remotes, branch)
                    .is_none_or(|remote| !checked_out.contains(&remote.branch))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;

    use super::*;

    #[test]
    fn checkoutable_branch_refs_filter_remotes_in_memory() {
        let branch_refs = [
            "main",
            "origin/main",
            "feature/new",
            "origin/feature/new",
            "feature/active",
            "origin/feature/active",
            "upstream/release",
        ]
        .into_iter()
        .map(ToOwned::to_owned)
        .collect();
        let checked_out = ["main", "feature/active"]
            .into_iter()
            .map(ToOwned::to_owned)
            .collect::<HashSet<_>>();
        let remotes = ["origin", "upstream"]
            .into_iter()
            .map(ToOwned::to_owned)
            .collect::<Vec<_>>();

        let refs = checkoutable_branch_refs_from(branch_refs, &checked_out, &remotes);

        assert_eq!(
            refs,
            vec![
                "feature/new".to_owned(),
                "origin/feature/new".to_owned(),
                "upstream/release".to_owned(),
            ]
        );
    }
}
