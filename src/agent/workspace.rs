//! Git-root workspace attachment for external agent observations and live tmux panes.
//!
//! A kmux workspace is keyed by the canonical Git worktree root for a local path.
//! Paths outside Git intentionally do not attach; kmux is a Git + tmux + agent
//! workflow tool rather than a generic directory tracker.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::git::Git;
use crate::state::AgentLocationHints;
use crate::telemetry;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Resolved Git worktree root used to attach agent sessions to live tmux state.
pub struct AgentWorkspaceAttachment {
    key: String,
    path: String,
    reported_path: String,
}

#[derive(Debug, Default)]
/// Per-reconciliation cache for path-to-Git-workspace resolution.
pub struct AgentWorkspaceResolver {
    cache: HashMap<String, Option<AgentWorkspaceAttachment>>,
}

impl AgentWorkspaceAttachment {
    /// Return the normalized key used for grouping attached agent sessions.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Return the resolved local Git worktree root path used for matching.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Return the path exactly as reported before resolution.
    pub fn reported_path(&self) -> &str {
        &self.reported_path
    }

    #[cfg(test)]
    pub(crate) fn for_test(path: impl ToString) -> Self {
        let path = path.to_string();
        attachment(path.clone(), PathBuf::from(path))
    }
}

impl AgentWorkspaceResolver {
    /// Resolve the reported agent directory into a Git-root attachment identity.
    pub fn attachment_for_hints(
        &mut self,
        target: &AgentLocationHints,
    ) -> Option<AgentWorkspaceAttachment> {
        self.attachment_for_path(target.directory.as_deref()?)
    }

    /// Resolve one path into an attachment only when it belongs to a local Git worktree.
    pub fn attachment_for_path(&mut self, path: &str) -> Option<AgentWorkspaceAttachment> {
        let path = clean_path(path)?;
        if let Some(attachment) = self.cache.get(path) {
            return attachment.clone();
        }

        let attachment = resolve_path(path);
        self.cache.insert(path.to_owned(), attachment.clone());
        attachment
    }

    /// Return whether an attachment matches a candidate path's Git worktree root.
    pub fn attachment_matches_path(
        &mut self,
        attachment: &AgentWorkspaceAttachment,
        candidate: Option<&str>,
    ) -> bool {
        let Some(candidate) = candidate.and_then(clean_path) else {
            return false;
        };

        self.attachment_for_path(candidate)
            .is_some_and(|candidate_attachment| candidate_attachment.key == attachment.key)
    }
}

fn resolve_path(path: &str) -> Option<AgentWorkspaceAttachment> {
    let (result, elapsed_ms) = telemetry::timed(|| {
        let Some(resolved) = normalize_existing(Path::new(path)) else {
            return WorkspaceResolveTelemetry::unattached("missing");
        };
        if !resolved.is_dir() {
            return WorkspaceResolveTelemetry::unattached("not_dir");
        }

        match Git::new(&resolved).worktree_root() {
            Ok(root) => WorkspaceResolveTelemetry::attached(attachment(path, root)),
            Err(_) => WorkspaceResolveTelemetry::unattached("not_git"),
        }
    });

    match &result.attachment {
        Some(attachment) => tracing::debug!(
            event = "workspace.resolve",
            elapsed_ms,
            path,
            attached = true,
            workspace = %attachment.key(),
        ),
        None => tracing::debug!(
            event = "workspace.resolve",
            elapsed_ms,
            path,
            attached = false,
            reason = result.reason.unwrap_or("unknown"),
        ),
    };
    result.attachment
}

struct WorkspaceResolveTelemetry {
    attachment: Option<AgentWorkspaceAttachment>,
    reason: Option<&'static str>,
}

impl WorkspaceResolveTelemetry {
    fn attached(attachment: AgentWorkspaceAttachment) -> Self {
        Self {
            attachment: Some(attachment),
            reason: None,
        }
    }

    fn unattached(reason: &'static str) -> Self {
        Self {
            attachment: None,
            reason: Some(reason),
        }
    }
}

fn attachment(reported_path: impl ToString, path: PathBuf) -> AgentWorkspaceAttachment {
    let path = path.display().to_string();
    AgentWorkspaceAttachment {
        key: path.clone(),
        path,
        reported_path: reported_path.to_string(),
    }
}

fn normalize_existing(path: &Path) -> Option<PathBuf> {
    path.canonicalize().ok()
}

fn clean_path(path: &str) -> Option<&str> {
    let path = path.trim();
    (!path.is_empty()).then_some(path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use anyhow::{Context, Result};
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
    fn repo_root_resolves_to_canonical_git_worktree_root() -> Result<()> {
        let (_temp, repo) = init_repo()?;
        let mut resolver = AgentWorkspaceResolver::default();

        let attachment = resolver
            .attachment_for_path(&repo.display().to_string())
            .expect("repo root should resolve");

        assert_eq!(
            attachment.path(),
            repo.canonicalize()?.display().to_string()
        );
        assert_eq!(attachment.reported_path(), repo.display().to_string());
        Ok(())
    }

    #[test]
    fn subdirectory_resolves_to_git_worktree_root() -> Result<()> {
        let (_temp, repo) = init_repo()?;
        let nested = repo.join("src/bin");
        fs::create_dir_all(&nested)?;
        let mut resolver = AgentWorkspaceResolver::default();

        let attachment = resolver
            .attachment_for_path(&nested.display().to_string())
            .expect("repo subdirectory should resolve");

        assert_eq!(
            attachment.path(),
            repo.canonicalize()?.display().to_string()
        );
        Ok(())
    }

    #[test]
    fn linked_worktree_root_is_distinct_from_main_root() -> Result<()> {
        let (temp, repo) = init_repo()?;
        let worktree = temp.path().join("project__worktrees/feature");
        fs::create_dir_all(worktree.parent().expect("worktree should have parent"))?;
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-b",
                "feature",
                worktree.to_str().unwrap(),
            ],
        )?;
        let mut resolver = AgentWorkspaceResolver::default();

        let main = resolver
            .attachment_for_path(&repo.display().to_string())
            .expect("main root should resolve");
        let linked = resolver
            .attachment_for_path(&worktree.display().to_string())
            .expect("linked worktree should resolve");

        assert_ne!(main.key(), linked.key());
        assert_eq!(
            linked.path(),
            worktree.canonicalize()?.display().to_string()
        );
        Ok(())
    }

    #[test]
    fn missing_and_non_git_directories_do_not_attach() -> Result<()> {
        let temp = TempDir::new()?;
        let non_git = temp.path().join("plain");
        fs::create_dir(&non_git)?;
        let mut resolver = AgentWorkspaceResolver::default();

        assert!(
            resolver
                .attachment_for_path(&non_git.display().to_string())
                .is_none()
        );
        assert!(
            resolver
                .attachment_for_path("/tmp/does-not-exist/kmux-agent")
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn non_directory_path_does_not_attach() -> Result<()> {
        let temp = TempDir::new()?;
        let file = temp.path().join("plain-file");
        fs::write(&file, "not a directory\n")?;
        let mut resolver = AgentWorkspaceResolver::default();

        assert!(
            resolver
                .attachment_for_path(&file.display().to_string())
                .is_none()
        );
        Ok(())
    }

    #[test]
    fn git_worktree_path_without_directory_does_not_attach() -> Result<()> {
        let (_temp, repo) = init_repo()?;
        let mut resolver = AgentWorkspaceResolver::default();
        let target = AgentLocationHints {
            git_worktree_path: Some(repo.display().to_string()),
            ..AgentLocationHints::default()
        };

        assert!(resolver.attachment_for_hints(&target).is_none());
        Ok(())
    }
}
