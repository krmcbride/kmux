//! Repository root and shared Git metadata discovery.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result};

use super::Git;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Git repository paths needed by kmux to locate worktrees and shared metadata.
pub struct RepoInfo {
    pub current_worktree: PathBuf,
    pub git_common_dir: PathBuf,
}

impl Git {
    /// Resolve the current worktree root and Git common dir for this adapter's cwd.
    pub fn repo_info(&self) -> Result<RepoInfo> {
        let current_worktree = self.worktree_root()?;

        // `--git-common-dir` is the shared metadata directory for all worktrees
        // in a repo. In a primary checkout this is `<repo>/.git`; in a linked
        // worktree it still points back to the primary checkout's `.git`.
        let common_dir_raw = self
            .path_stdout("--git-common-dir")
            .context("failed to locate git common dir")?;
        let git_common_dir = resolve_existing_path(self.cwd(), &common_dir_raw)?;

        Ok(RepoInfo {
            current_worktree,
            git_common_dir,
        })
    }

    /// Return the canonical root for the Git worktree containing this adapter's cwd.
    pub fn worktree_root(&self) -> Result<PathBuf> {
        let current_worktree_raw = self
            .path_stdout("--show-toplevel")
            .context("failed to locate git worktree root")?;
        resolve_existing_path(self.cwd(), &current_worktree_raw)
    }

    /// Return the first worktree from Git's worktree list, which Git reports as the main one.
    pub fn main_worktree_from_list(&self) -> Result<Option<PathBuf>> {
        Ok(self
            .worktrees()?
            .into_iter()
            .next()
            .map(|worktree| worktree.path))
    }

    /// Preserve path whitespace while removing only Git's output terminator.
    pub(super) fn path_stdout(&self, option: &str) -> Result<String> {
        let output = self.output(["rev-parse", option])?;
        if !output.status.success() {
            return super::process::bail_git(output);
        }
        Ok(output
            .stdout
            .strip_suffix('\n')
            .unwrap_or(&output.stdout)
            .to_owned())
    }
}

fn resolve_existing_path(base: &Path, path: &str) -> Result<PathBuf> {
    base.join(path)
        .canonicalize()
        .with_context(|| format!("failed to canonicalize {} from {}", path, base.display()))
}
