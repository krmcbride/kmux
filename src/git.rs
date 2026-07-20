//! Git subprocess adapter for repository and worktree operations.
//!
//! This module is kmux's boundary to the Git CLI. It keeps porcelain parsing,
//! ref checks, branch/worktree mutations, and Git common-dir discovery out of
//! workflow code so command use cases can reason in kmux terms.

use std::collections::HashSet;
use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, anyhow, bail};

use crate::{LIFECYCLE_ACTIVE_ENV, telemetry};

#[derive(Debug, Clone)]
/// Thin adapter for running Git commands from a fixed working directory.
pub struct Git {
    cwd: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Git repository paths needed by kmux to locate worktrees and shared metadata.
pub struct RepoInfo {
    pub current_worktree: PathBuf,
    pub git_common_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
/// Outcome of ensuring a local branch exists.
pub enum BranchAction {
    Existing,
    Created,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// One entry from `git worktree list --porcelain`.
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked: Option<String>,
    pub prunable: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Remote-tracking branch split into remote name, local branch name, and full ref.
pub struct RemoteBranch {
    pub remote: String,
    pub branch: String,
    pub ref_name: String,
}

#[derive(Debug)]
/// Raw Git subprocess output with UTF-8-lossy stdout and stderr text.
struct GitOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl Git {
    /// Create a Git adapter rooted at `cwd`.
    pub fn new(cwd: impl AsRef<Path>) -> Self {
        Self {
            cwd: cwd.as_ref().to_path_buf(),
        }
    }

    /// Run a Git command, require success, and return trimmed stdout.
    pub fn stdout<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let output = self.output(args)?;
        if !output.status.success() {
            return bail_git(output);
        }
        Ok(output.stdout.trim_end().to_owned())
    }

    /// Resolve the current worktree root and Git common dir for this adapter's cwd.
    pub fn repo_info(&self) -> Result<RepoInfo> {
        let current_worktree = self.worktree_root()?;

        // `--git-common-dir` is the shared metadata directory for all worktrees
        // in a repo. In a primary checkout this is `<repo>/.git`; in a linked
        // worktree it still points back to the primary checkout's `.git`.
        let common_dir_raw = self
            .stdout(["rev-parse", "--git-common-dir"])
            .context("failed to locate git common dir")?;
        let git_common_dir = resolve_existing_path(&self.cwd, common_dir_raw.trim())?;

        Ok(RepoInfo {
            current_worktree,
            git_common_dir,
        })
    }

    /// Return the canonical root for the Git worktree containing this adapter's cwd.
    pub fn worktree_root(&self) -> Result<PathBuf> {
        let current_worktree_raw = self
            .stdout(["rev-parse", "--show-toplevel"])
            .context("failed to locate git worktree root")?;
        resolve_existing_path(&self.cwd, current_worktree_raw.trim())
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

    /// List all Git worktrees using porcelain output.
    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let output = self.stdout(["worktree", "list", "--porcelain"])?;
        parse_worktree_list(&output)
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

    /// Return the first worktree from Git's worktree list, which Git reports as the main one.
    pub fn main_worktree_from_list(&self) -> Result<Option<PathBuf>> {
        Ok(self
            .worktrees()?
            .into_iter()
            .next()
            .map(|worktree| worktree.path))
    }

    /// Find a worktree currently checked out on `branch`.
    pub fn find_worktree_by_branch(&self, branch: &str) -> Result<Option<WorktreeInfo>> {
        Ok(self
            .worktrees()?
            .into_iter()
            .find(|worktree| worktree.branch.as_deref() == Some(branch)))
    }

    /// Ensure a candidate worktree path is absent or an empty directory.
    pub fn ensure_available_worktree_path(&self, path: &Path) -> Result<()> {
        if !path.exists() {
            return Ok(());
        }

        if !path.is_dir() {
            bail!(
                "worktree path {} already exists and is not a directory",
                path.display()
            );
        }

        let mut entries = fs::read_dir(path)
            .with_context(|| format!("failed to read worktree path {}", path.display()))?;
        if entries
            .next()
            .transpose()
            .with_context(|| format!("failed to inspect worktree path {}", path.display()))?
            .is_none()
        {
            return Ok(());
        }

        bail!(
            "worktree path {} already exists and is not empty",
            path.display()
        );
    }

    /// Add a linked worktree at `path` for an existing local branch.
    pub fn add_worktree(&self, path: &Path, branch: &str) -> Result<()> {
        self.ensure_available_worktree_path(path)?;
        self.stdout(vec![
            OsString::from("worktree"),
            OsString::from("add"),
            path.as_os_str().to_os_string(),
            OsString::from(branch),
        ])?;
        Ok(())
    }

    /// Return whether this worktree has staged changes.
    pub fn has_staged_changes(&self) -> Result<bool> {
        self.diff_has_changes(["--no-optional-locks", "diff", "--cached", "--quiet"])
    }

    /// Return whether this worktree has unstaged changes.
    pub fn has_unstaged_changes(&self) -> Result<bool> {
        self.diff_has_changes(["--no-optional-locks", "diff", "--quiet"])
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

    /// Remove a linked worktree, requiring a clean tree unless `force` is true.
    pub fn remove_worktree(&self, path: &Path, force: bool) -> Result<()> {
        if !force && self.worktree_is_dirty(path)? {
            bail!("worktree {} has uncommitted changes", path.display());
        }

        let mut args = vec![OsString::from("worktree"), OsString::from("remove")];
        if force {
            args.push(OsString::from("--force"));
        }
        args.push(path.as_os_str().to_os_string());
        self.stdout(args)?;
        Ok(())
    }

    /// Delete a local branch using `-d` or `-D` depending on `force`.
    pub fn delete_local_branch(&self, branch: &str, force: bool) -> Result<()> {
        let flag = if force { "-D" } else { "-d" };
        self.stdout(["branch", flag, branch])?;
        Ok(())
    }

    /// Run a Git command and return raw output without requiring a successful exit status.
    fn output<I, S>(&self, args: I) -> Result<GitOutput>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        let display_args = display_args(&args);
        let output = telemetry::timed_result_event!(
            "subprocess",
            {
                program = "git",
                args = %display_args,
                cwd = %self.cwd.display(),
            },
            || {
                Command::new("git")
                    .args(&args)
                    .current_dir(&self.cwd)
                    // Git hooks and checkout filters run synchronously inside
                    // this child. Prevent them from recursively waiting on a
                    // lifecycle lock held by their parent kmux process.
                    .env(LIFECYCLE_ACTIVE_ENV, "1")
                    .output()
                    .with_context(|| format!("failed to run git {display_args}"))
            },
            ok |output| {
                status_code = output.status.code().unwrap_or(-1),
                success = output.status.success(),
                stdout_bytes = output.stdout.len(),
                stderr_bytes = output.stderr.len(),
            },
        )?;

        Ok(GitOutput {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Return whether a reference resolves to a commit object.
    fn commit_ref_exists(&self, reference: &str) -> Result<bool> {
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

    /// Return whether a worktree contains staged, unstaged, or untracked changes.
    fn worktree_is_dirty(&self, path: &Path) -> Result<bool> {
        if !path.is_dir() {
            bail!("worktree path {} does not exist", path.display());
        }

        let output = Git::new(path).stdout(["status", "--porcelain", "--untracked-files=all"])?;
        Ok(!output.trim().is_empty())
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

    // `git diff --quiet` returns 1 for differences and >1 for real command failures.
    fn diff_has_changes<const N: usize>(&self, args: [&str; N]) -> Result<bool> {
        let output = self.output(args)?;
        if output.status.success() {
            Ok(false)
        } else if output.status.code() == Some(1) {
            Ok(true)
        } else {
            bail_git(output)
        }
    }
}

// Parse `git worktree list --porcelain` output into structured worktree records.
fn parse_worktree_list(output: &str) -> Result<Vec<WorktreeInfo>> {
    let mut worktrees = Vec::new();
    let mut current = WorktreeBuilder::default();

    for line in output.lines() {
        if line.is_empty() {
            push_worktree(&mut worktrees, &mut current);
            continue;
        }

        if let Some(path) = line.strip_prefix("worktree ") {
            push_worktree(&mut worktrees, &mut current);
            current.path = Some(PathBuf::from(path));
        } else if let Some(head) = line.strip_prefix("HEAD ") {
            current.head = Some(head.to_owned());
        } else if let Some(branch) = line.strip_prefix("branch ") {
            current.branch = Some(local_branch_name(branch).to_owned());
        } else if line == "detached" {
            current.detached = true;
        } else if line == "bare" {
            current.bare = true;
        } else if let Some(reason) = line.strip_prefix("locked") {
            current.locked = Some(trim_porcelain_reason(reason));
        } else if let Some(reason) = line.strip_prefix("prunable") {
            current.prunable = Some(trim_porcelain_reason(reason));
        }
    }

    push_worktree(&mut worktrees, &mut current);
    Ok(worktrees)
}

#[derive(Debug, Default)]
struct WorktreeBuilder {
    path: Option<PathBuf>,
    head: Option<String>,
    branch: Option<String>,
    detached: bool,
    bare: bool,
    locked: Option<String>,
    prunable: Option<String>,
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

fn push_worktree(worktrees: &mut Vec<WorktreeInfo>, current: &mut WorktreeBuilder) {
    let Some(path) = current.path.take() else {
        return;
    };

    worktrees.push(WorktreeInfo {
        path,
        head: current.head.take(),
        branch: current.branch.take(),
        detached: current.detached,
        bare: current.bare,
        locked: current.locked.take(),
        prunable: current.prunable.take(),
    });
    current.detached = false;
    current.bare = false;
}

// Git emits `refs/heads/<name>` for worktree branches; kmux stores/display names
// in the normal short local-branch form.
fn local_branch_name(branch_ref: &str) -> &str {
    branch_ref.strip_prefix("refs/heads/").unwrap_or(branch_ref)
}

fn trim_porcelain_reason(reason: &str) -> String {
    reason.strip_prefix(' ').unwrap_or(reason).to_owned()
}

fn resolve_existing_path(base: &Path, path: &str) -> Result<PathBuf> {
    base.join(path)
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {} from {}", path, base.display()))
}

fn display_args(args: &[OsString]) -> String {
    let mut display = String::new();
    for arg in args {
        if !display.is_empty() {
            display.push(' ');
        }
        display.push_str(&arg.to_string_lossy());
    }
    display
}

fn bail_git<T>(output: GitOutput) -> Result<T> {
    let stderr = output.stderr.trim();
    if stderr.is_empty() {
        bail!("git command failed with status {}", output.status);
    }
    bail!("git command failed with status {}: {stderr}", output.status)
}

#[cfg(test)]
pub mod test_support {
    use std::process::Command;

    use tempfile::TempDir;

    use super::*;

    /// Owned Git repository fixture for crate-local adapter and policy tests.
    pub struct GitRepoFixture {
        temp: TempDir,
        path: PathBuf,
    }

    impl GitRepoFixture {
        /// Initialize a repository with one commit and neutral test identity.
        pub fn new() -> Result<Self> {
            let temp = TempDir::new()?;
            let path = temp.path().join("project-alpha");
            fs::create_dir(&path)?;
            let fixture = Self { temp, path };
            fixture.git(&["init", "--initial-branch", "main"])?;
            fixture.git(&["config", "user.email", "test@example.invalid"])?;
            fixture.git(&["config", "user.name", "Test User"])?;
            fixture.commit_file("README.md", "test\n", "initial")?;
            Ok(fixture)
        }

        /// Return the repository path while retaining its temporary owner.
        pub fn path(&self) -> &Path {
            &self.path
        }

        /// Return the fixture root for sibling worktree paths.
        pub fn root(&self) -> &Path {
            self.temp.path()
        }

        /// Run a Git command in the fixture repository and require success.
        pub fn git(&self, args: &[&str]) -> Result<()> {
            let output = Command::new("git")
                .args(args)
                .current_dir(&self.path)
                .output()
                .with_context(|| format!("failed to run git {}", args.join(" ")))?;
            assert!(
                output.status.success(),
                "git {} failed\nstdout: {}\nstderr: {}",
                args.join(" "),
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
            Ok(())
        }

        /// Write and commit one file in the fixture repository.
        pub fn commit_file(&self, file_name: &str, content: &str, message: &str) -> Result<()> {
            fs::write(self.path.join(file_name), content)?;
            self.git(&["add", file_name])?;
            self.git(&["commit", "-m", message])
        }
    }
}

#[cfg(test)]
mod tests {
    use super::test_support::GitRepoFixture;
    use super::*;
    use std::collections::HashSet;

    #[test]
    fn discovers_repo_info_from_primary_worktree() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let repo = fixture.path();

        let info = Git::new(repo).repo_info()?;

        assert_eq!(info.current_worktree, repo.canonicalize()?);
        assert_eq!(info.git_common_dir, repo.join(".git").canonicalize()?);
        Ok(())
    }

    #[test]
    fn discovers_repo_info_from_linked_worktree() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let repo = fixture.path();
        let worktree_base = fixture.root().join("project-alpha__worktrees");
        let linked = worktree_base.join("feature-auth");
        fs::create_dir(&worktree_base)?;
        fixture.git(&[
            "worktree",
            "add",
            "-b",
            "feature/auth",
            linked.to_string_lossy().as_ref(),
        ])?;

        let info = Git::new(&linked).repo_info()?;

        assert_eq!(info.current_worktree, linked.canonicalize()?);
        assert_eq!(info.git_common_dir, repo.join(".git").canonicalize()?);
        Ok(())
    }

    #[test]
    fn parses_porcelain_worktree_records() -> Result<()> {
        let output = "\
worktree /tmp/project\n\
HEAD 1111111111111111111111111111111111111111\n\
branch refs/heads/main\n\
\n\
worktree /tmp/project__worktrees/feature auth\n\
HEAD 2222222222222222222222222222222222222222\n\
detached\n\
locked awaiting review\n\
prunable gitdir file points to non-existent location\n";

        let worktrees = parse_worktree_list(output)?;

        assert_eq!(worktrees.len(), 2);
        assert_eq!(worktrees[0].path, PathBuf::from("/tmp/project"));
        assert_eq!(worktrees[0].branch.as_deref(), Some("main"));
        assert!(!worktrees[0].detached);
        assert_eq!(
            worktrees[1].path,
            PathBuf::from("/tmp/project__worktrees/feature auth")
        );
        assert_eq!(worktrees[1].branch, None);
        assert!(worktrees[1].detached);
        assert_eq!(worktrees[1].locked.as_deref(), Some("awaiting review"));
        assert_eq!(
            worktrees[1].prunable.as_deref(),
            Some("gitdir file points to non-existent location")
        );
        Ok(())
    }

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

    #[test]
    fn detects_current_branch_and_detached_head() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let git_repo = Git::new(fixture.path());

        assert_eq!(git_repo.current_branch()?.as_deref(), Some("main"));

        let head = git_repo.stdout(["rev-parse", "HEAD"])?;
        fixture.git(&["checkout", "--detach", &head])?;

        assert_eq!(git_repo.current_branch()?, None);
        let error = git_repo.require_current_branch().unwrap_err();
        assert!(error.to_string().contains("detached HEAD"));
        Ok(())
    }

    #[test]
    fn creates_branch_from_current_branch_and_reuses_without_moving() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let git_repo = Git::new(fixture.path());

        assert_eq!(
            git_repo.ensure_local_branch("feature/auth", None)?,
            BranchAction::Created
        );
        let feature_rev = git_repo.stdout(["rev-parse", "feature/auth"])?;

        fixture.commit_file("after.txt", "after\n", "after feature branch")?;
        let main_rev = git_repo.stdout(["rev-parse", "main"])?;
        assert_ne!(feature_rev, main_rev);

        assert_eq!(
            git_repo.ensure_local_branch("feature/auth", None)?,
            BranchAction::Existing
        );
        assert_eq!(git_repo.stdout(["rev-parse", "feature/auth"])?, feature_rev);
        Ok(())
    }

    #[test]
    fn creates_branch_from_explicit_start_point() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let git_repo = Git::new(fixture.path());
        let initial_rev = git_repo.stdout(["rev-parse", "main"])?;

        fixture.commit_file("later.txt", "later\n", "later")?;

        assert_eq!(
            git_repo.ensure_local_branch("from-initial", Some(&initial_rev))?,
            BranchAction::Created
        );
        assert_eq!(git_repo.stdout(["rev-parse", "from-initial"])?, initial_rev);
        assert!(!git_repo.commit_ref_exists("missing-ref")?);
        Ok(())
    }

    #[test]
    fn returns_merge_base_when_branches_share_history() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let git_repo = Git::new(fixture.path());
        let initial_rev = git_repo.stdout(["rev-parse", "main"])?;
        git_repo.ensure_local_branch("feature/auth", Some("main"))?;
        fixture.commit_file("later.txt", "later\n", "later")?;

        assert_eq!(
            git_repo.merge_base("feature/auth", "main")?.as_deref(),
            Some(initial_rev.as_str())
        );
        Ok(())
    }

    #[test]
    fn returns_none_when_branches_have_no_merge_base() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let repo = fixture.path();
        let git_repo = Git::new(repo);
        git_repo.ensure_local_branch("feature/auth", Some("main"))?;
        fixture.git(&["checkout", "--orphan", "orphan-parent"])?;
        fs::remove_file(repo.join("README.md"))?;
        fixture.commit_file("orphan.txt", "orphan\n", "orphan")?;
        fixture.git(&["checkout", "main"])?;

        assert_eq!(git_repo.merge_base("feature/auth", "orphan-parent")?, None);
        Ok(())
    }

    #[test]
    fn safe_deletion_prefers_configured_upstream_over_local_head() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let remote = fixture.root().join("remote.git");
        let remote = remote.to_string_lossy();
        fixture.git(&["init", "--bare", remote.as_ref()])?;
        fixture.git(&["remote", "add", "origin", remote.as_ref()])?;
        fixture.git(&["push", "-u", "origin", "main"])?;
        fixture.git(&["checkout", "-b", "feature/safety"])?;
        fixture.commit_file("feature.txt", "feature\n", "feature change")?;
        fixture.git(&[
            "branch",
            "--set-upstream-to",
            "origin/main",
            "feature/safety",
        ])?;
        fixture.git(&["checkout", "main"])?;
        fixture.git(&[
            "merge",
            "--no-ff",
            "feature/safety",
            "-m",
            "merge feature locally",
        ])?;

        let git = Git::new(fixture.path());

        assert!(!git.branch_is_safely_deletable("feature/safety")?);
        Ok(())
    }

    #[test]
    fn adds_and_finds_worktree_by_branch() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let git_repo = Git::new(fixture.path());
        let worktree_base = fixture.root().join("project-alpha__worktrees");
        let linked = worktree_base.join("feature-auth");
        fs::create_dir(&worktree_base)?;

        git_repo.ensure_local_branch("feature/auth", None)?;
        git_repo.add_worktree(&linked, "feature/auth")?;

        assert_eq!(
            git_repo
                .find_worktree_by_branch("feature/auth")?
                .map(|worktree| worktree.path),
            Some(linked)
        );
        Ok(())
    }

    #[test]
    fn rejects_non_empty_worktree_path() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let git_repo = Git::new(fixture.path());
        let conflicting = fixture.root().join("project-alpha__worktrees/conflict");
        fs::create_dir_all(&conflicting)?;
        fs::write(conflicting.join("file.txt"), "occupied\n")?;

        let error = git_repo
            .add_worktree(&conflicting, "feature/auth")
            .unwrap_err();

        assert!(error.to_string().contains("not empty"));
        Ok(())
    }

    #[test]
    fn remove_worktree_guards_dirty_paths_unless_forced() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let git_repo = Git::new(fixture.path());
        let worktree_base = fixture.root().join("project-alpha__worktrees");
        let linked = worktree_base.join("feature-auth");
        fs::create_dir(&worktree_base)?;
        git_repo.ensure_local_branch("feature/auth", None)?;
        git_repo.add_worktree(&linked, "feature/auth")?;
        fs::write(linked.join("untracked.txt"), "dirty\n")?;

        assert!(git_repo.worktree_is_dirty(&linked)?);
        let error = git_repo.remove_worktree(&linked, false).unwrap_err();
        assert!(error.to_string().contains("uncommitted changes"));

        git_repo.remove_worktree(&linked, true)?;

        assert!(!linked.exists());
        Ok(())
    }
}
