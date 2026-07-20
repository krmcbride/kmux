//! Filesystem path discovery and normalization for kmux-managed repositories.
//!
//! This module centralizes the rules for deriving the main worktree, Git common
//! dir, sibling kmux worktree base, and best-effort repo metadata from path
//! hints. Keep generic path helpers here only when they support those layout and
//! identity rules.

use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::git::Git;
use crate::project::ProjectIdentity;
use crate::telemetry;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Resolved filesystem layout for a Git repo and its kmux worktree area.
pub struct RepoPaths {
    pub current_worktree: PathBuf,
    pub main_worktree: PathBuf,
    pub git_common_dir: PathBuf,
    pub worktree_base_dir: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
/// Best-effort repo identity recovered from path hints in agent observations.
pub struct RepoMetadata {
    pub repo_name: Option<String>,
    pub repo_path: Option<String>,
    pub branch: Option<String>,
}

impl RepoPaths {
    /// Discover the current, main, common-Git, and kmux worktree-base paths for a repo.
    ///
    /// When called from a linked worktree, the main worktree is resolved from Git's
    /// common metadata so new kmux worktrees are still created beside the primary checkout.
    pub fn discover(cwd: impl AsRef<Path>) -> Result<Self> {
        let cwd = cwd.as_ref();
        telemetry::timed_result_event!(
            "repo_paths.discover",
            { cwd = %cwd.display(), },
            || discover_repo_paths(cwd),
        )
    }

    /// Return the filesystem path for a kmux workspace slug under this repo's worktree base.
    pub fn workspace_path(&self, workspace_slug: &str) -> PathBuf {
        self.worktree_base_dir.join(workspace_slug)
    }

    /// Return the repository-level identity shared by every one of its worktrees.
    pub fn project_identity(&self) -> Result<ProjectIdentity> {
        ProjectIdentity::from_canonical_paths(
            self.main_worktree.clone(),
            self.git_common_dir.clone(),
        )
    }
}

/// Resolve a filesystem path to its containing canonical Git project identity.
pub fn discover_project_identity(path: impl AsRef<Path>) -> Result<ProjectIdentity> {
    RepoPaths::discover(path)?.project_identity()
}

/// Compare paths after canonicalization when possible, falling back to literal comparison.
pub fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

/// Infer repo name, repo path, and branch from the first path that belongs to a Git repo.
pub fn infer_repo_metadata_from_paths(paths: &[Option<&str>]) -> RepoMetadata {
    paths
        .iter()
        .flatten()
        .find_map(|path| infer_repo_metadata(path))
        .unwrap_or_default()
}

/// Return the final path component as owned text, ignoring empty basenames.
pub fn path_basename(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

fn discover_repo_paths(cwd: &Path) -> Result<RepoPaths> {
    let git = Git::new(cwd);
    let repo_info = git.repo_info()?;
    let current_worktree = repo_info.current_worktree;
    let git_common_dir = repo_info.git_common_dir;

    // Resolve the main worktree before choosing the kmux worktree base, so
    // running inside a linked worktree still creates siblings under the
    // primary repo's `<repo>__worktrees/` directory instead of nesting
    // another worktree base.
    let main_worktree = if git_common_dir
        .file_name()
        .is_some_and(|name| name == ".git")
    {
        let parent = git_common_dir.parent().ok_or_else(|| {
            anyhow!(
                "could not determine parent directory for git common dir {}",
                git_common_dir.display()
            )
        })?;
        normalize_existing(parent)?
    } else {
        git.main_worktree_from_list()
            .context("failed to list git worktrees")?
            .map(|path| normalize_existing(&path))
            .transpose()?
            .ok_or_else(|| {
                anyhow!(
                    "could not determine main worktree from git common dir {}",
                    git_common_dir.display()
                )
            })?
    };
    let worktree_base_dir = default_worktree_base_dir(&main_worktree)?;

    Ok(RepoPaths {
        current_worktree,
        main_worktree,
        git_common_dir,
        worktree_base_dir,
    })
}

/// Return the sibling directory where kmux stores linked worktrees for a main worktree.
fn default_worktree_base_dir(main_worktree: &Path) -> Result<PathBuf> {
    let project_name = main_worktree
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "could not determine project name from {}",
                main_worktree.display()
            )
        })?;
    let parent = main_worktree.parent().ok_or_else(|| {
        anyhow!(
            "could not determine parent directory for {}",
            main_worktree.display()
        )
    })?;

    Ok(parent.join(format!("{project_name}__worktrees")))
}

// Observation paths may outlive their tmux pane or worktree. Treat failures here
// as absence of metadata so status/sidebar rendering can keep going.
fn infer_repo_metadata(path: &str) -> Option<RepoMetadata> {
    let paths = RepoPaths::discover(path).ok()?;
    let branch = Git::new(&paths.current_worktree)
        .current_branch()
        .ok()
        .flatten();

    Some(RepoMetadata {
        repo_name: path_name(&paths.main_worktree),
        repo_path: Some(paths.main_worktree.display().to_string()),
        branch,
    })
}

fn path_name(path: &Path) -> Option<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

fn normalize_existing(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("failed to canonicalize {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::git::test_support::GitRepoFixture;

    #[test]
    fn default_worktree_base_is_sibling_project_worktrees_dir() -> Result<()> {
        let main = PathBuf::from("/tmp/example/project");

        let base = default_worktree_base_dir(&main)?;

        assert_eq!(base, PathBuf::from("/tmp/example/project__worktrees"));
        Ok(())
    }

    #[test]
    fn discovers_paths_from_primary_worktree() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let repo = fixture.path();
        let paths = RepoPaths::discover(repo)?;
        let parent = paths
            .current_worktree
            .parent()
            .ok_or_else(|| anyhow::anyhow!("expected worktree to have a parent"))?;

        assert_eq!(paths.current_worktree, repo);
        assert_eq!(paths.main_worktree, paths.current_worktree);
        assert_eq!(
            paths.worktree_base_dir,
            parent.join("project-alpha__worktrees")
        );
        assert_eq!(
            paths.workspace_path("feature-auth"),
            paths.worktree_base_dir.join("feature-auth")
        );
        Ok(())
    }

    #[test]
    fn discovers_main_worktree_from_linked_worktree() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let repo = fixture.path();
        let worktree_base = fixture.root().join("project-alpha__worktrees");
        let linked = worktree_base.join("feature-auth");
        std::fs::create_dir(&worktree_base)?;
        let linked_str = linked
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("linked worktree path is not valid UTF-8"))?;
        fixture.git(&["worktree", "add", "-b", "feature/auth", linked_str])?;

        let paths = RepoPaths::discover(&linked)?;

        assert_eq!(paths.current_worktree, linked);
        assert_eq!(paths.main_worktree, repo);
        assert_eq!(paths.worktree_base_dir, worktree_base);
        assert_eq!(
            paths.project_identity()?,
            RepoPaths::discover(repo)?.project_identity()?
        );
        Ok(())
    }

    #[test]
    fn project_identity_matches_subdirectories_but_not_other_repositories() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let nested = fixture.path().join("src");
        std::fs::create_dir(&nested)?;
        let other = GitRepoFixture::new()?;
        let project = RepoPaths::discover(fixture.path())?.project_identity()?;

        assert_eq!(project.main_worktree(), fixture.path());
        assert_eq!(discover_project_identity(&nested)?, project);
        assert_ne!(discover_project_identity(other.path())?, project);
        Ok(())
    }
}
