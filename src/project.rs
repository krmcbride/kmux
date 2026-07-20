//! Git project identity and repository-level invariants.
//!
//! A project is the canonical Git common repository shared by a main worktree
//! and all of its linked worktrees. Filesystem and Git discovery belong to the
//! paths adapter; this module owns only the validated identity value produced by
//! that discovery.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Canonical identity shared by a repository's main and linked worktrees.
pub struct ProjectIdentity {
    main_worktree: PathBuf,
    git_common_dir: PathBuf,
}

impl ProjectIdentity {
    /// Build an identity from Git/paths-adapter-normalized project paths.
    ///
    /// This constructor does not touch the filesystem. Callers are responsible
    /// for passing the canonical main worktree and Git common directory reported
    /// by the repository adapter.
    pub fn from_canonical_paths(main_worktree: PathBuf, git_common_dir: PathBuf) -> Result<Self> {
        if main_worktree.as_os_str().is_empty() {
            bail!("project main worktree path cannot be empty");
        }
        if git_common_dir.as_os_str().is_empty() {
            bail!("project Git common directory cannot be empty");
        }

        Ok(Self {
            main_worktree,
            git_common_dir,
        })
    }

    /// Return the canonical main worktree used for project display and diagnostics.
    pub fn main_worktree(&self) -> &Path {
        &self.main_worktree
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_requires_both_canonical_project_paths() -> Result<()> {
        let identity = ProjectIdentity::from_canonical_paths(
            PathBuf::from("/repo/project-alpha"),
            PathBuf::from("/repo/project-alpha/.git"),
        )?;

        assert_eq!(identity.main_worktree(), Path::new("/repo/project-alpha"));
        assert!(
            ProjectIdentity::from_canonical_paths(
                PathBuf::new(),
                PathBuf::from("/repo/project-alpha/.git")
            )
            .is_err()
        );
        assert!(
            ProjectIdentity::from_canonical_paths(
                PathBuf::from("/repo/project-alpha"),
                PathBuf::new()
            )
            .is_err()
        );
        Ok(())
    }

    #[test]
    fn identity_equality_uses_main_worktree_and_git_common_dir() -> Result<()> {
        let first = ProjectIdentity::from_canonical_paths(
            PathBuf::from("/repo/project-alpha"),
            PathBuf::from("/repo/project-alpha/.git"),
        )?;
        let same = ProjectIdentity::from_canonical_paths(
            PathBuf::from("/repo/project-alpha"),
            PathBuf::from("/repo/project-alpha/.git"),
        )?;
        let other = ProjectIdentity::from_canonical_paths(
            PathBuf::from("/repo/project-beta"),
            PathBuf::from("/repo/project-beta/.git"),
        )?;

        assert_eq!(first, same);
        assert_ne!(first, other);
        Ok(())
    }
}
