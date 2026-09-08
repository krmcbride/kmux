//! Persisted workspace intent, independent of live Git checkout facts.

use std::path::{Path, PathBuf};

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Authority {
    Primary,
    Kmux,
    External,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Retention {
    Ephemeral,
    Persistent,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// Policy associated with a registered worktree; branches are never its identity.
pub struct WorkspacePolicy {
    id: String,
    path: PathBuf,
    label: String,
    window_slug: String,
    authority: Authority,
    retention: Option<Retention>,
    presentation: bool,
    creation_anchor: Option<String>,
    owned_branch: Option<String>,
    #[serde(default)]
    retired: bool,
}

impl WorkspacePolicy {
    /// Observe a canonical path without claiming worktree or branch ownership.
    pub fn observed(path: PathBuf, primary: bool) -> Self {
        let digest = Sha256::digest(path.as_os_str().as_encoded_bytes());
        let id = format!("ws-{digest:x}");
        let label = if primary {
            "primary".to_owned()
        } else {
            format!("external-{}", &id[3..19])
        };
        Self {
            id,
            path,
            window_slug: label.clone(),
            label,
            authority: if primary {
                Authority::Primary
            } else {
                Authority::External
            },
            retention: None,
            presentation: false,
            creation_anchor: None,
            owned_branch: None,
            retired: false,
        }
    }

    /// Record explicit ownership after creation or the one-time legacy migration.
    pub fn owned(
        id: String,
        path: PathBuf,
        label: String,
        retention: Retention,
        creation_anchor: Option<String>,
        owned_branch: Option<String>,
    ) -> Result<Self> {
        validate_label(&label)?;
        let mut policy = Self::observed(path, false);
        policy.id = id;
        policy.window_slug = label.clone();
        policy.label = label;
        policy.authority = Authority::Kmux;
        policy.retention = Some(retention);
        policy.presentation = true;
        policy.creation_anchor = creation_anchor;
        policy.owned_branch = owned_branch;
        Ok(policy)
    }

    /// Return the stable selector, independent of branch or presentation labels.
    pub fn id(&self) -> &str {
        &self.id
    }
    /// Return the canonical worktree association.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// Return the human-readable workspace label.
    pub fn label(&self) -> &str {
        &self.label
    }
    /// Return the persisted tmux name component, preserving legacy window names.
    pub fn window_slug(&self) -> &str {
        &self.window_slug
    }
    /// Return explicit worktree lifecycle authority.
    pub fn authority(&self) -> Authority {
        self.authority
    }
    /// Return kmux retention policy, absent for external and primary worktrees.
    pub fn retention(&self) -> Option<Retention> {
        self.retention
    }
    /// Return whether a presentation is remembered for this workspace.
    pub fn presentation(&self) -> bool {
        self.presentation
    }
    /// Return the original creation commit, when known.
    pub fn creation_anchor(&self) -> Option<&str> {
        self.creation_anchor.as_deref()
    }
    /// Return the branch explicitly created or migrated with this workspace.
    pub fn owned_branch(&self) -> Option<&str> {
        self.owned_branch.as_deref()
    }

    /// Return whether this record is history for a registration that has ended.
    pub fn retired(&self) -> bool {
        self.retired
    }

    /// Retain identity and lineage after losing the original registration.
    pub fn retire(&mut self) {
        self.retired = true;
        self.presentation = false;
    }

    /// Match live facts only to this environment's original registration.
    pub fn matches_registration(&self, entry: &crate::git::WorktreeInfo) -> bool {
        !self.retired
            && self.path == entry.path
            && (self.authority != Authority::Kmux
                || entry.kmux_binding.as_deref() == Some(&self.id))
    }

    /// Validate persisted policy before it can authorize workflow effects.
    pub fn validate(&self) -> Result<()> {
        if !self.path.is_absolute() || self.id.is_empty() {
            bail!("workspace policy requires an absolute path and stable identity");
        }
        validate_label(&self.label)?;
        validate_label(&self.window_slug)?;
        if (self.authority == Authority::Kmux) != self.retention.is_some()
            || (self.authority != Authority::Kmux && self.owned_branch.is_some())
        {
            bail!(
                "workspace '{}' has inconsistent lifecycle authority",
                self.id
            );
        }
        Ok(())
    }
}

/// Keep selectors and tmux names unambiguous without giving labels branch semantics.
pub fn validate_label(label: &str) -> Result<()> {
    if label.is_empty()
        || !label
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("workspace label must contain ASCII letters, digits, '-' or '_' and cannot be empty");
    }
    Ok(())
}
