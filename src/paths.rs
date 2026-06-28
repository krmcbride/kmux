use std::path::{Path, PathBuf};

use anyhow::{Context, Result, anyhow};

use crate::git::Git;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPaths {
    pub current_worktree: PathBuf,
    pub main_worktree: PathBuf,
    pub git_common_dir: PathBuf,
    pub worktree_base_dir: PathBuf,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RepoMetadata {
    pub repo_name: Option<String>,
    pub repo_path: Option<String>,
    pub branch: Option<String>,
}

impl RepoPaths {
    pub fn discover(cwd: impl AsRef<Path>) -> Result<Self> {
        let cwd = cwd.as_ref();
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

        Ok(Self {
            current_worktree,
            main_worktree,
            git_common_dir,
            worktree_base_dir,
        })
    }

    pub fn handle_path(&self, handle: &str) -> PathBuf {
        self.worktree_base_dir.join(handle)
    }
}

pub fn default_worktree_base_dir(main_worktree: &Path) -> Result<PathBuf> {
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

pub fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

pub fn infer_repo_metadata_from_paths(paths: &[Option<&str>]) -> RepoMetadata {
    paths
        .iter()
        .flatten()
        .find_map(|path| infer_repo_metadata(path))
        .unwrap_or_default()
}

pub fn path_basename(path: &str) -> Option<String> {
    Path::new(path)
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
}

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
    use std::fs;
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

    fn init_repo() -> Result<(TempDir, PathBuf)> {
        let temp = TempDir::new()?;
        let repo = temp.path().join("project");
        fs::create_dir(&repo)?;
        git(&repo, &["init", "--initial-branch", "main"])?;
        git(&repo, &["config", "user.email", "test@example.invalid"])?;
        git(&repo, &["config", "user.name", "Test User"])?;
        fs::write(repo.join("README.md"), "test\n")?;
        git(&repo, &["add", "README.md"])?;
        git(&repo, &["commit", "-m", "initial"])?;
        Ok((temp, repo))
    }

    #[test]
    fn default_worktree_base_is_sibling_project_worktrees_dir() -> Result<()> {
        let main = PathBuf::from("/tmp/example/project");

        let base = default_worktree_base_dir(&main)?;

        assert_eq!(base, PathBuf::from("/tmp/example/project__worktrees"));
        Ok(())
    }

    #[test]
    fn discovers_paths_from_primary_worktree() -> Result<()> {
        let (_temp, repo) = init_repo()?;
        let paths = RepoPaths::discover(&repo)?;
        let parent = paths
            .current_worktree
            .parent()
            .ok_or_else(|| anyhow::anyhow!("expected worktree to have a parent"))?;

        assert_eq!(paths.current_worktree, repo);
        assert_eq!(paths.main_worktree, paths.current_worktree);
        assert_eq!(paths.worktree_base_dir, parent.join("project__worktrees"));
        assert_eq!(
            paths.handle_path("feature-auth"),
            paths.worktree_base_dir.join("feature-auth")
        );
        Ok(())
    }

    #[test]
    fn discovers_main_worktree_from_linked_worktree() -> Result<()> {
        let (temp, repo) = init_repo()?;
        let worktree_base = temp.path().join("project__worktrees");
        let linked = worktree_base.join("feature-auth");
        fs::create_dir(&worktree_base)?;
        let linked_str = linked
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("linked worktree path is not valid UTF-8"))?;
        git(
            &repo,
            &["worktree", "add", "-b", "feature/auth", linked_str],
        )?;

        let paths = RepoPaths::discover(&linked)?;

        assert_eq!(paths.current_worktree, linked);
        assert_eq!(paths.main_worktree, repo);
        assert_eq!(paths.worktree_base_dir, worktree_base);
        Ok(())
    }
}
