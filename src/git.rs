use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, anyhow, bail};

#[derive(Debug, Clone)]
pub struct Git {
    cwd: PathBuf,
}

#[derive(Debug)]
pub struct GitOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoInfo {
    pub current_worktree: PathBuf,
    pub git_common_dir: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BranchAction {
    Existing,
    Created,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorktreeInfo {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub locked: Option<String>,
    pub prunable: Option<String>,
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

impl Git {
    pub fn new(cwd: impl AsRef<Path>) -> Self {
        Self {
            cwd: cwd.as_ref().to_path_buf(),
        }
    }

    pub fn output<I, S>(&self, args: I) -> Result<GitOutput>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        let display_args = display_args(&args);
        let output = Command::new("git")
            .args(&args)
            .current_dir(&self.cwd)
            .output()
            .with_context(|| format!("failed to run git {display_args}"))?;

        Ok(GitOutput {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

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

    pub fn succeeds<I, S>(&self, args: I) -> Result<bool>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        Ok(self.output(args)?.status.success())
    }

    pub fn repo_info(&self) -> Result<RepoInfo> {
        let current_worktree_raw = self
            .stdout(["rev-parse", "--show-toplevel"])
            .context("failed to locate git worktree root")?;
        let current_worktree = resolve_existing_path(&self.cwd, current_worktree_raw.trim())?;

        // `--git-common-dir` is the shared metadata directory for all worktrees
        // in a repo. In a primary checkout this is `<repo>/.git`; in a linked
        // worktree it still points back to the primary checkout's `.git`.
        let common_dir_raw = self
            .stdout(["rev-parse", "--git-common-dir"])
            .context("failed to locate git common dir")?;
        let git_common_dir = resolve_existing_path(&current_worktree, common_dir_raw.trim())?;

        Ok(RepoInfo {
            current_worktree,
            git_common_dir,
        })
    }

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

    pub fn require_current_branch(&self) -> Result<String> {
        self.current_branch()?
            .ok_or_else(|| anyhow!("cannot create a branch from detached HEAD without --base"))
    }

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

    pub fn commit_ref_exists(&self, reference: &str) -> Result<bool> {
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

    pub fn ensure_local_branch(&self, branch: &str, base: Option<&str>) -> Result<BranchAction> {
        if self.local_branch_exists(branch)? {
            return Ok(BranchAction::Existing);
        }

        let base = if let Some(base) = base {
            base.to_owned()
        } else {
            self.require_current_branch()?
        };

        if !self.commit_ref_exists(&base)? {
            bail!("base ref '{base}' does not resolve to a commit");
        }

        self.stdout(vec![
            OsString::from("branch"),
            OsString::from(branch),
            OsString::from(base),
        ])?;
        Ok(BranchAction::Created)
    }

    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let output = self.stdout(["worktree", "list", "--porcelain"])?;
        parse_worktree_list(&output)
    }

    pub fn main_worktree_from_list(&self) -> Result<Option<PathBuf>> {
        Ok(self
            .worktrees()?
            .into_iter()
            .next()
            .map(|worktree| worktree.path))
    }

    pub fn find_worktree_by_branch(&self, branch: &str) -> Result<Option<WorktreeInfo>> {
        Ok(self
            .worktrees()?
            .into_iter()
            .find(|worktree| worktree.branch.as_deref() == Some(branch)))
    }

    pub fn find_worktree_by_handle(
        &self,
        worktree_base_dir: &Path,
        handle: &str,
    ) -> Result<Option<WorktreeInfo>> {
        let expected_path = worktree_base_dir.join(handle);
        Ok(self.worktrees()?.into_iter().find(|worktree| {
            worktree.path == expected_path
                || worktree.path.file_name().is_some_and(|name| name == handle)
        }))
    }

    pub fn find_worktree_by_name(
        &self,
        worktree_base_dir: &Path,
        name: &str,
    ) -> Result<Option<WorktreeInfo>> {
        if let Some(worktree) = self.find_worktree_by_branch(name)? {
            return Ok(Some(worktree));
        }
        self.find_worktree_by_handle(worktree_base_dir, name)
    }

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

    pub fn worktree_is_dirty(&self, path: &Path) -> Result<bool> {
        if !path.is_dir() {
            bail!("worktree path {} does not exist", path.display());
        }

        let output = Git::new(path).stdout(["status", "--porcelain", "--untracked-files=all"])?;
        Ok(!output.trim().is_empty())
    }

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

    pub fn delete_local_branch(&self, branch: &str, force: bool) -> Result<()> {
        let flag = if force { "-D" } else { "-d" };
        self.stdout(["branch", flag, branch])?;
        Ok(())
    }
}

pub fn parse_worktree_list(output: &str) -> Result<Vec<WorktreeInfo>> {
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
mod tests {
    use super::*;
    use std::process::Command;

    use tempfile::TempDir;

    fn run(cwd: &Path, program: &str, args: &[&str]) -> Result<()> {
        let output = Command::new(program)
            .args(args)
            .current_dir(cwd)
            .output()
            .with_context(|| format!("failed to run {} {}", program, args.join(" ")))?;
        assert!(
            output.status.success(),
            "{} {} failed\nstdout: {}\nstderr: {}",
            program,
            args.join(" "),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    fn git(cwd: &Path, args: &[&str]) -> Result<()> {
        run(cwd, "git", args)
    }

    fn commit_file(repo: &Path, file_name: &str, content: &str, message: &str) -> Result<()> {
        fs::write(repo.join(file_name), content)?;
        git(repo, &["add", file_name])?;
        git(repo, &["commit", "-m", message])?;
        Ok(())
    }

    fn init_repo() -> Result<(TempDir, PathBuf)> {
        let temp = TempDir::new()?;
        let repo = temp.path().join("project");
        fs::create_dir(&repo)?;
        git(&repo, &["init", "--initial-branch", "main"])?;
        git(&repo, &["config", "user.email", "test@example.invalid"])?;
        git(&repo, &["config", "user.name", "Test User"])?;
        commit_file(&repo, "README.md", "test\n", "initial")?;
        Ok((temp, repo))
    }

    #[test]
    fn discovers_repo_info_from_primary_worktree() -> Result<()> {
        let (_temp, repo) = init_repo()?;

        let info = Git::new(&repo).repo_info()?;

        assert_eq!(info.current_worktree, repo.canonicalize()?);
        assert_eq!(info.git_common_dir, repo.join(".git").canonicalize()?);
        Ok(())
    }

    #[test]
    fn discovers_repo_info_from_linked_worktree() -> Result<()> {
        let (temp, repo) = init_repo()?;
        let worktree_base = temp.path().join("project__worktrees");
        let linked = worktree_base.join("feature-auth");
        fs::create_dir(&worktree_base)?;
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feature/auth",
                linked.to_string_lossy().as_ref(),
            ],
        )?;

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
    fn detects_current_branch_and_detached_head() -> Result<()> {
        let (_temp, repo) = init_repo()?;
        let git_repo = Git::new(&repo);

        assert_eq!(git_repo.current_branch()?.as_deref(), Some("main"));

        let head = git_repo.stdout(["rev-parse", "HEAD"])?;
        git(&repo, &["checkout", "--detach", &head])?;

        assert_eq!(git_repo.current_branch()?, None);
        let error = git_repo.require_current_branch().unwrap_err();
        assert!(error.to_string().contains("detached HEAD"));
        Ok(())
    }

    #[test]
    fn creates_branch_from_current_branch_and_reuses_without_moving() -> Result<()> {
        let (_temp, repo) = init_repo()?;
        let git_repo = Git::new(&repo);

        assert_eq!(
            git_repo.ensure_local_branch("feature/auth", None)?,
            BranchAction::Created
        );
        let feature_rev = git_repo.stdout(["rev-parse", "feature/auth"])?;

        commit_file(&repo, "after.txt", "after\n", "after feature branch")?;
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
    fn creates_branch_from_explicit_base() -> Result<()> {
        let (_temp, repo) = init_repo()?;
        let git_repo = Git::new(&repo);
        let initial_rev = git_repo.stdout(["rev-parse", "main"])?;

        commit_file(&repo, "later.txt", "later\n", "later")?;

        assert_eq!(
            git_repo.ensure_local_branch("from-initial", Some(&initial_rev))?,
            BranchAction::Created
        );
        assert_eq!(git_repo.stdout(["rev-parse", "from-initial"])?, initial_rev);
        assert!(!git_repo.commit_ref_exists("missing-ref")?);
        Ok(())
    }

    #[test]
    fn adds_and_finds_worktree_by_branch_or_handle() -> Result<()> {
        let (temp, repo) = init_repo()?;
        let git_repo = Git::new(&repo);
        let worktree_base = temp.path().join("project__worktrees");
        let linked = worktree_base.join("feature-auth");
        fs::create_dir(&worktree_base)?;

        git_repo.ensure_local_branch("feature/auth", None)?;
        git_repo.add_worktree(&linked, "feature/auth")?;

        assert_eq!(
            git_repo
                .find_worktree_by_branch("feature/auth")?
                .map(|worktree| worktree.path),
            Some(linked.clone())
        );
        assert_eq!(
            git_repo
                .find_worktree_by_handle(&worktree_base, "feature-auth")?
                .map(|worktree| worktree.path),
            Some(linked.clone())
        );
        assert_eq!(
            git_repo
                .find_worktree_by_name(&worktree_base, "feature/auth")?
                .map(|worktree| worktree.path),
            Some(linked)
        );
        Ok(())
    }

    #[test]
    fn rejects_non_empty_worktree_path() -> Result<()> {
        let (temp, repo) = init_repo()?;
        let git_repo = Git::new(&repo);
        let conflicting = temp.path().join("project__worktrees/conflict");
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
        let (temp, repo) = init_repo()?;
        let git_repo = Git::new(&repo);
        let worktree_base = temp.path().join("project__worktrees");
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
