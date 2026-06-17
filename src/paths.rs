use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoPaths {
    pub current_worktree: PathBuf,
    pub main_worktree: PathBuf,
    pub git_common_dir: PathBuf,
    pub worktree_base_dir: PathBuf,
}

impl RepoPaths {
    pub fn discover(cwd: impl AsRef<Path>) -> Result<Self> {
        let cwd = cwd.as_ref();
        let current_worktree_raw = run_git(cwd, &["rev-parse", "--show-toplevel"])
            .context("failed to locate git worktree root")?;
        let current_worktree = resolve_path(cwd, current_worktree_raw.trim())?;
        let common_dir_raw = run_git(cwd, &["rev-parse", "--git-common-dir"])
            .context("failed to locate git common dir")?;
        let git_common_dir = resolve_path(&current_worktree, common_dir_raw.trim())?;

        // `git rev-parse --git-common-dir` returns the shared metadata directory
        // for all worktrees in a repository. In a normal checkout that is
        // `<repo>/.git`; in a linked worktree it still points back to the main
        // checkout's `.git`, while the linked worktree has its own per-worktree
        // git dir under `.git/worktrees/`.
        //
        // Resolve the main worktree before choosing the worktree base directory
        // so running kmux inside an existing linked worktree still creates
        // siblings under `<repo>__worktrees/` instead of nesting another
        // worktree base.
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
            let worktree_output = run_git(cwd, &["worktree", "list", "--porcelain"])
                .context("failed to list git worktrees")?;
            first_worktree_from_porcelain(&worktree_output)?.ok_or_else(|| {
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

fn first_worktree_from_porcelain(output: &str) -> Result<Option<PathBuf>> {
    for line in output.lines() {
        if let Some(path) = line.strip_prefix("worktree ") {
            return Ok(Some(normalize_existing(Path::new(path))?));
        }
    }
    Ok(None)
}

fn run_git(cwd: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(cwd)
        .output()
        .with_context(|| format!("failed to run git {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

fn resolve_path(base: &Path, path: &str) -> Result<PathBuf> {
    normalize_existing(&base.join(path))
}

fn normalize_existing(path: &Path) -> Result<PathBuf> {
    path.canonicalize()
        .with_context(|| format!("failed to canonicalize {}", path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

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
