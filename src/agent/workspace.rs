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
    pub(super) fn for_test(path: impl ToString) -> Self {
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
}

fn resolve_path(path: &str) -> Option<AgentWorkspaceAttachment> {
    resolve_path_with(path, |resolved| Git::new(resolved).worktree_root())
}

fn resolve_path_with(
    path: &str,
    worktree_root: impl FnOnce(&Path) -> anyhow::Result<PathBuf>,
) -> Option<AgentWorkspaceAttachment> {
    let (result, elapsed_ms) = telemetry::timed(|| {
        let Some(resolved) = normalize_existing(Path::new(path)) else {
            return WorkspaceResolveTelemetry::unattached("missing");
        };
        if !resolved.is_dir() {
            return WorkspaceResolveTelemetry::unattached("not_dir");
        }

        match worktree_root(&resolved) {
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
    use anyhow::Result;
    use std::fs;
    use tempfile::TempDir;

    #[test]
    fn missing_path_does_not_attach() -> Result<()> {
        let temp = TempDir::new()?;
        let missing = temp.path().join("missing");
        let mut resolver = AgentWorkspaceResolver::default();

        assert!(
            resolver
                .attachment_for_path(&missing.display().to_string())
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
}

#[cfg(feature = "internal-adapter-contract-tests")]
pub mod contract_tests {
    use std::fs;

    use anyhow::Result;

    use crate::git::contract_support::GitRepoFixture;

    use super::*;

    pub fn repo_root_resolves_to_canonical_git_worktree_root() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let repo = fixture.path();
        let reported = repo.display().to_string();
        let attachment =
            resolve_path_with(&reported, |path| fixture.adapter_at(path).worktree_root())
                .ok_or_else(|| anyhow::anyhow!("repo root should resolve"))?;

        assert_eq!(
            attachment.path(),
            repo.canonicalize()?.display().to_string()
        );
        assert_eq!(attachment.reported_path(), reported);
        Ok(())
    }

    pub fn subdirectory_resolves_to_git_worktree_root() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let repo = fixture.path();
        let nested = repo.join("src/bin");
        fs::create_dir_all(&nested)?;
        let attachment = resolve_path_with(&nested.display().to_string(), |path| {
            fixture.adapter_at(path).worktree_root()
        })
        .ok_or_else(|| anyhow::anyhow!("repo subdirectory should resolve"))?;

        assert_eq!(
            attachment.path(),
            repo.canonicalize()?.display().to_string()
        );
        Ok(())
    }

    pub fn linked_worktree_root_is_distinct_from_main_root() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let repo = fixture.path();
        let worktree = fixture.root().join("project-alpha__worktrees/feature");
        let worktree_parent = worktree
            .parent()
            .ok_or_else(|| anyhow::anyhow!("worktree should have a parent"))?;
        fs::create_dir_all(worktree_parent)?;
        let worktree_text = worktree
            .to_str()
            .ok_or_else(|| anyhow::anyhow!("test worktree path should be UTF-8"))?;
        fixture.git(&["worktree", "add", "-b", "feature", worktree_text])?;

        let main = resolve_path_with(&repo.display().to_string(), |path| {
            fixture.adapter_at(path).worktree_root()
        })
        .ok_or_else(|| anyhow::anyhow!("main root should resolve"))?;
        let linked = resolve_path_with(&worktree.display().to_string(), |path| {
            fixture.adapter_at(path).worktree_root()
        })
        .ok_or_else(|| anyhow::anyhow!("linked worktree should resolve"))?;

        assert_ne!(main.key(), linked.key());
        assert_eq!(
            linked.path(),
            worktree.canonicalize()?.display().to_string()
        );
        Ok(())
    }

    pub fn non_git_directory_does_not_attach() -> Result<()> {
        let fixture = GitRepoFixture::new()?;
        let plain = fixture.root().join("plain");
        fs::create_dir(&plain)?;

        let attachment = resolve_path_with(&plain.display().to_string(), |path| {
            fixture.adapter_at(path).worktree_root()
        });

        assert!(attachment.is_none());
        Ok(())
    }
}
