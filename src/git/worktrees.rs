//! Worktree discovery, status, lifecycle, and porcelain parsing.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::Git;
use super::process::bail_git;

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
    pub kmux_binding: Option<String>,
}

impl Git {
    /// List all Git worktrees using NUL-delimited porcelain, preserving path whitespace.
    pub fn worktrees(&self) -> Result<Vec<WorktreeInfo>> {
        let output = self.output(["worktree", "list", "--porcelain", "-z"])?;
        if !output.status.success() {
            return bail_git(output);
        }
        let mut entries = parse_worktree_list(&output.stdout)?;
        let bindings = self.worktree_bindings()?;
        for entry in &mut entries {
            if let Ok(path) = entry.path.canonicalize() {
                entry.path = path;
            }
            entry.kmux_binding = bindings.get(&entry.path).cloned();
        }
        Ok(entries)
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

    /// Return whether a worktree contains staged, unstaged, or untracked changes.
    pub(super) fn worktree_is_dirty(&self, path: &Path) -> Result<bool> {
        if !path.is_dir() {
            bail!("worktree path {} does not exist", path.display());
        }

        let output =
            self.with_cwd(path)
                .stdout(["status", "--porcelain", "--untracked-files=all"])?;
        Ok(!output.trim().is_empty())
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

// NUL-delimited attributes allow newlines, quotes, and trailing whitespace in paths.
fn parse_worktree_list(output: &str) -> Result<Vec<WorktreeInfo>> {
    let mut worktrees = Vec::new();
    let mut current = WorktreeBuilder::default();

    for line in output.split('\0') {
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
        kmux_binding: None,
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

#[cfg(test)]
mod tests {
    use tempfile::TempDir;

    use super::*;

    #[test]
    fn nul_records_preserve_literal_path_and_reason_whitespace() -> Result<()> {
        let path = "/repo/quoted \"workspace\"\nwith trailing space ";
        let output = format!(
            "worktree {path}\0HEAD abc123\0detached\0locked review\nnotes \0\0worktree /repo/missing\0HEAD def456\0prunable\0\0"
        );
        let entries = parse_worktree_list(&output)?;
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].path, Path::new(path));
        assert_eq!(entries[0].locked.as_deref(), Some("review\nnotes "));
        assert_eq!(entries[0].head.as_deref(), Some("abc123"));
        assert!(entries[0].detached);
        assert_eq!(entries[1].prunable.as_deref(), Some(""));
        assert!(entries[1].locked.is_none());
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

        let worktrees = parse_worktree_list(&output.replace('\n', "\0"))?;

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
    fn rejects_non_empty_worktree_path() -> Result<()> {
        let temp = TempDir::new()?;
        let git_repo = Git::new(temp.path());
        let conflicting = temp.path().join("conflict");
        fs::create_dir_all(&conflicting)?;
        fs::write(conflicting.join("file.txt"), "occupied\n")?;

        let error = git_repo
            .ensure_available_worktree_path(&conflicting)
            .unwrap_err();

        assert!(error.to_string().contains("not empty"));
        Ok(())
    }
}
