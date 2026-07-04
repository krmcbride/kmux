//! Directory-centered attachment for external agent observations.
//!
//! Producers report their agent's current directory. kmux resolves that path into
//! a local directory identity and matches it against live kmux tmux windows.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use crate::state::AgentLocationHints;

#[derive(Debug, Clone, PartialEq, Eq)]
/// Resolved directory identity used to attach an agent session to tmux state.
pub struct AgentDirectoryAttachment {
    key: String,
    path: String,
    reported_path: String,
}

#[derive(Debug, Default)]
/// Per-reconciliation cache for path-to-directory attachment resolution.
pub struct AgentDirectoryResolver {
    cache: HashMap<String, AgentDirectoryAttachment>,
}

impl AgentDirectoryAttachment {
    /// Return the normalized key used for grouping attached agent sessions.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Return the resolved local directory path used for matching and enrichment.
    pub fn path(&self) -> &str {
        &self.path
    }

    /// Return the path exactly as reported before resolution.
    pub fn reported_path(&self) -> &str {
        &self.reported_path
    }
}

impl AgentDirectoryResolver {
    /// Resolve the reported agent directory into a local attachment identity.
    pub fn attachment_for_hints(
        &mut self,
        target: &AgentLocationHints,
    ) -> Option<AgentDirectoryAttachment> {
        self.attachment_for_path(target.directory.as_deref()?)
    }

    /// Resolve one path into an attachment only when it is an existing directory.
    pub fn attachment_for_path(&mut self, path: &str) -> Option<AgentDirectoryAttachment> {
        let path = clean_path(path)?;
        if let Some(attachment) = self.cache.get(path) {
            return Some(attachment.clone());
        }

        let attachment = resolve_path(path)?;
        self.cache.insert(path.to_owned(), attachment.clone());
        Some(attachment)
    }

    /// Return whether an attachment matches a candidate tmux workspace path.
    pub fn attachment_matches_path(
        &mut self,
        attachment: &AgentDirectoryAttachment,
        candidate: Option<&str>,
    ) -> bool {
        let Some(candidate) = candidate.and_then(clean_path) else {
            return false;
        };

        self.attachment_for_path(candidate)
            .is_some_and(|candidate_attachment| candidate_attachment.key == attachment.key)
    }
}

fn resolve_path(path: &str) -> Option<AgentDirectoryAttachment> {
    let resolved = normalize_existing(Path::new(path))?;
    resolved.is_dir().then(|| attachment(path, resolved))
}

fn attachment(reported_path: impl ToString, path: PathBuf) -> AgentDirectoryAttachment {
    let path = path.display().to_string();
    AgentDirectoryAttachment {
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
    fn existing_directory_resolves_to_canonical_directory() -> Result<()> {
        let temp = TempDir::new()?;
        let directory = temp.path().join("workspace");
        fs::create_dir(&directory)?;
        let mut resolver = AgentDirectoryResolver::default();

        let attachment = resolver
            .attachment_for_path(&directory.display().to_string())
            .expect("existing directory should resolve");

        assert_eq!(
            attachment.path(),
            directory.canonicalize()?.display().to_string()
        );
        assert_eq!(attachment.reported_path(), directory.display().to_string());
        Ok(())
    }

    #[test]
    fn missing_directory_does_not_attach() {
        let mut resolver = AgentDirectoryResolver::default();

        assert!(
            resolver
                .attachment_for_path("/tmp/does-not-exist/kmux-agent")
                .is_none()
        );
    }

    #[test]
    fn hints_require_reported_directory() -> Result<()> {
        let temp = TempDir::new()?;
        let worktree = temp.path().join("worktree");
        fs::create_dir(&worktree)?;
        let mut resolver = AgentDirectoryResolver::default();
        let target = AgentLocationHints {
            git_worktree_path: Some(worktree.display().to_string()),
            ..AgentLocationHints::default()
        };

        assert!(resolver.attachment_for_hints(&target).is_none());
        Ok(())
    }

    #[test]
    fn textual_paths_to_same_directory_share_key() -> Result<()> {
        let temp = TempDir::new()?;
        let directory = temp.path().join("workspace");
        fs::create_dir(&directory)?;
        let via_dot = directory.join(".");
        let mut resolver = AgentDirectoryResolver::default();

        let left = resolver
            .attachment_for_path(&directory.display().to_string())
            .expect("directory should resolve");
        let right = resolver
            .attachment_for_path(&via_dot.display().to_string())
            .expect("directory should resolve");

        assert_eq!(left.key(), right.key());
        Ok(())
    }
}
