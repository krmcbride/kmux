//! Historical workspace ancestry, independent of checkout branches and retention.

use anyhow::{Result, bail};
use serde::{Deserialize, Serialize};

use super::WorkspacePolicy;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
/// One source relationship with a historical shared commit.
pub struct WorkspaceLineage {
    pub parent: LineageParent,
    pub anchor: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
/// A workspace identity or an explicitly ref-based source, retaining historical labels.
pub enum LineageParent {
    Workspace {
        workspace_id: String,
        label: String,
        git_ref: Option<String>,
    },
    GitRef {
        reference: String,
    },
}

impl WorkspaceLineage {
    /// Keep the anchor separate from the original creation anchor used for recovery.
    pub fn new(parent: LineageParent, anchor: String) -> Self {
        Self { parent, anchor }
    }

    /// Reject empty historical data before persistence.
    pub fn validate(&self) -> Result<()> {
        if self.anchor.is_empty()
            || self.parent.label().is_empty()
            || self.parent.workspace_id().is_some_and(str::is_empty)
        {
            bail!("workspace lineage requires a parent and anchor commit");
        }
        Ok(())
    }
}

impl LineageParent {
    /// Snapshot readable labels while binding ancestry to the stable policy ID.
    pub fn workspace(policy: &WorkspacePolicy, git_ref: Option<&str>) -> Self {
        Self::Workspace {
            workspace_id: policy.id().to_owned(),
            label: policy.label().to_owned(),
            git_ref: git_ref.map(ToOwned::to_owned),
        }
    }

    /// Return a parent ID only for workspace-to-workspace ancestry.
    pub fn workspace_id(&self) -> Option<&str> {
        match self {
            Self::Workspace { workspace_id, .. } => Some(workspace_id),
            Self::GitRef { .. } => None,
        }
    }

    /// Return the historical display label, including when the parent is absent.
    pub fn label(&self) -> &str {
        match self {
            Self::Workspace { label, .. } => label,
            Self::GitRef { reference } => reference,
        }
    }

    /// Return the historical branch/ref label for compatibility consumers.
    pub fn git_ref(&self) -> Option<&str> {
        match self {
            Self::Workspace { git_ref, .. } => git_ref.as_deref(),
            Self::GitRef { reference } => Some(reference),
        }
    }
}
