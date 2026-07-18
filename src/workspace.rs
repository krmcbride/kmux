//! Workspace identity and inventory read-model types.
//!
//! A kmux workspace is identified most strongly by its canonical Git worktree
//! root. Branch names and slugs are useful routing and display hints, but they
//! must be validated against the worktree path before strict workspace commands
//! rely on them.

use std::path::{Path, PathBuf};

use anyhow::{Result, anyhow, bail};
use serde::Serialize;

use crate::git::WorktreeInfo;
use crate::paths::RepoPaths;
use crate::slug::workspace_slug_from_branch;

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
/// Canonical Git worktree root used as kmux's strongest workspace identity.
pub struct WorkspaceIdentity {
    canonical_worktree_root: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
/// Resolved workspace record derived from Git worktree state.
pub struct WorkspaceRecord {
    identity: WorkspaceIdentity,
    workspace_slug: String,
    branch: Option<String>,
    is_main: bool,
}

#[derive(Clone, Debug, Serialize)]
/// Workspace inventory row shared by human list output and JSON output.
pub struct WorkspaceInventoryItem {
    workspace_slug: String,
    git_branch: Option<String>,
    git_parent_branch: Option<String>,
    git_anchor_commit: Option<String>,
    git_worktree_path: String,
    is_main: bool,
    created_at: Option<u64>,
    #[serde(skip)]
    tree_depth: usize,
}

impl WorkspaceIdentity {
    /// Build an identity from a Git/paths-adapter-normalized worktree root.
    ///
    /// This constructor does not touch the filesystem. Callers are responsible
    /// for passing the canonical root reported by Git or `RepoPaths`.
    pub fn from_canonical_root(canonical_worktree_root: PathBuf) -> Result<Self> {
        if canonical_worktree_root.as_os_str().is_empty() {
            bail!("workspace identity path cannot be empty");
        }

        Ok(Self {
            canonical_worktree_root,
        })
    }

    /// Return the canonical Git worktree root for this workspace.
    pub fn root(&self) -> &Path {
        &self.canonical_worktree_root
    }
}

impl WorkspaceRecord {
    /// Convert a Git worktree record into kmux's resolved workspace shape.
    pub fn from_worktree(worktree: WorktreeInfo, is_main: bool) -> Result<Self> {
        let workspace_slug = workspace_slug_from_path(&worktree.path)?;

        Self::new(workspace_slug, worktree.path, worktree.branch, is_main)
    }

    /// Build a record for a newly-created strict kmux workspace.
    ///
    /// The path basename and branch-derived slug must both match the supplied
    /// workspace slug, so command workflows cannot construct inconsistent
    /// workspace identity facts after creating a worktree.
    pub fn from_created_kmux_workspace(
        workspace_slug: String,
        path: PathBuf,
        branch: String,
    ) -> Result<Self> {
        validate_path_slug(&path, &workspace_slug)?;
        let expected_slug = workspace_slug_from_branch(&branch)?;
        if workspace_slug != expected_slug {
            bail!(
                "workspace slug '{}' does not match branch-derived slug '{}' for branch '{}'",
                workspace_slug,
                expected_slug,
                branch
            );
        }

        Self::new(workspace_slug, path, Some(branch), false)
    }

    /// Return the branch/path-derived workspace slug.
    pub fn workspace_slug(&self) -> &str {
        &self.workspace_slug
    }

    /// Return the canonical Git worktree root path.
    pub fn path(&self) -> &Path {
        self.identity.root()
    }

    /// Return the checked-out local branch, if Git reports one.
    pub fn branch(&self) -> Option<&str> {
        self.branch.as_deref()
    }

    fn new(
        workspace_slug: String,
        path: PathBuf,
        branch: Option<String>,
        is_main: bool,
    ) -> Result<Self> {
        if workspace_slug.is_empty() {
            bail!("workspace slug cannot be empty");
        }

        Ok(Self {
            identity: WorkspaceIdentity::from_canonical_root(path)?,
            workspace_slug,
            branch,
            is_main,
        })
    }
}

impl WorkspaceInventoryItem {
    /// Build a list/read-model row from a resolved workspace record.
    pub fn from_record(record: WorkspaceRecord, created_at: Option<u64>) -> Self {
        Self {
            workspace_slug: record.workspace_slug,
            git_branch: record.branch,
            git_parent_branch: None,
            git_anchor_commit: None,
            git_worktree_path: record.identity.root().display().to_string(),
            is_main: record.is_main,
            created_at,
            tree_depth: 0,
        }
    }

    /// Return the workspace slug serialized in `list --json` output.
    pub fn workspace_slug(&self) -> &str {
        &self.workspace_slug
    }

    /// Return the Git branch serialized in `list --json` output.
    pub fn git_branch(&self) -> Option<&str> {
        self.git_branch.as_deref()
    }

    /// Return the parent branch serialized in `list --json` output.
    pub fn git_parent_branch(&self) -> Option<&str> {
        self.git_parent_branch.as_deref()
    }

    /// Return the display string for the Git worktree path.
    pub fn git_worktree_path(&self) -> &str {
        &self.git_worktree_path
    }

    /// Return whether this row represents the main Git worktree.
    pub fn is_main(&self) -> bool {
        self.is_main
    }

    /// Return best-effort filesystem creation time for this row.
    pub fn created_at(&self) -> Option<u64> {
        self.created_at
    }

    /// Return the parent-tree display depth for human list output.
    pub fn tree_depth(&self) -> usize {
        self.tree_depth
    }

    /// Attach parent graph metadata loaded by the workflow layer.
    pub fn set_parent_state(&mut self, parent: String, anchor: String) {
        self.git_parent_branch = Some(parent);
        self.git_anchor_commit = Some(anchor);
    }

    /// Set the display depth computed by parent-tree ordering.
    pub fn set_tree_depth(&mut self, tree_depth: usize) {
        self.tree_depth = tree_depth;
    }
}

/// Return whether `path` is a direct kmux-managed child of the repo worktree base.
pub fn is_kmux_worktree(paths: &RepoPaths, path: &Path) -> bool {
    path.parent() == Some(paths.worktree_base_dir.as_path())
}

/// Return whether a worktree has a strict branch-derived kmux workspace identity.
pub fn is_strict_kmux_workspace(paths: &RepoPaths, worktree: &WorktreeInfo) -> bool {
    if !is_kmux_worktree(paths, &worktree.path) {
        return false;
    }
    let Some(branch) = worktree.branch.as_deref() else {
        return false;
    };
    let Ok(expected_slug) = workspace_slug_from_branch(branch) else {
        return false;
    };
    worktree
        .path
        .file_name()
        .is_some_and(|file_name| file_name == expected_slug.as_str())
}

/// Convert a kmux worktree and reject branch/path slug mismatches.
pub fn validated_kmux_record(
    paths: &RepoPaths,
    worktree: WorktreeInfo,
    is_main: bool,
) -> Result<WorkspaceRecord> {
    let record = WorkspaceRecord::from_worktree(worktree, is_main)?;
    validate_branch_derived_workspace_slug(paths, &record)?;
    Ok(record)
}

/// Convert strict kmux worktrees into reusable workspace records.
pub fn strict_kmux_workspace_records(
    paths: &RepoPaths,
    worktrees: impl IntoIterator<Item = WorktreeInfo>,
) -> Result<Vec<WorkspaceRecord>> {
    worktrees
        .into_iter()
        .filter(|worktree| is_strict_kmux_workspace(paths, worktree))
        .map(|worktree| WorkspaceRecord::from_worktree(worktree, false))
        .collect()
}

/// Reject branch/path mismatches so strict commands do not operate on ambiguous worktrees.
pub fn validate_branch_derived_workspace_slug(
    paths: &RepoPaths,
    record: &WorkspaceRecord,
) -> Result<()> {
    if !is_kmux_worktree(paths, record.path()) {
        return Ok(());
    }
    let Some(branch) = record.branch() else {
        return Ok(());
    };
    let expected_slug = workspace_slug_from_branch(branch)?;
    if record.workspace_slug() != expected_slug {
        bail!(
            "branch '{}' is checked out at non-derived kmux workspace path '{}'; expected '{}'",
            branch,
            record.workspace_slug(),
            expected_slug
        );
    }
    Ok(())
}

fn workspace_slug_from_path(path: &Path) -> Result<String> {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| anyhow!("could not determine workspace slug from {}", path.display()))
}

fn validate_path_slug(path: &Path, workspace_slug: &str) -> Result<()> {
    let actual_slug = workspace_slug_from_path(path)?;
    if actual_slug != workspace_slug {
        bail!(
            "workspace path '{}' does not match workspace slug '{}'",
            actual_slug,
            workspace_slug
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn repo_paths() -> RepoPaths {
        RepoPaths {
            current_worktree: PathBuf::from("/repo/project"),
            main_worktree: PathBuf::from("/repo/project"),
            git_common_dir: PathBuf::from("/repo/project/.git"),
            worktree_base_dir: PathBuf::from("/repo/project__worktrees"),
        }
    }

    fn worktree(path: &str, branch: Option<&str>) -> WorktreeInfo {
        WorktreeInfo {
            path: PathBuf::from(path),
            head: None,
            branch: branch.map(ToOwned::to_owned),
            detached: branch.is_none(),
            bare: false,
            locked: None,
            prunable: None,
        }
    }

    #[test]
    fn identity_equality_uses_canonical_worktree_root() -> Result<()> {
        let left = WorkspaceIdentity::from_canonical_root(PathBuf::from("/repo/worktree"))?;
        let same = WorkspaceIdentity::from_canonical_root(PathBuf::from("/repo/worktree"))?;
        let different = WorkspaceIdentity::from_canonical_root(PathBuf::from("/repo/other"))?;

        assert_eq!(left, same);
        assert_ne!(left, different);
        assert_eq!(left.root(), Path::new("/repo/worktree"));
        Ok(())
    }

    #[test]
    fn identity_rejects_empty_root() {
        let error = WorkspaceIdentity::from_canonical_root(PathBuf::new())
            .expect_err("empty identity path should fail");

        assert!(error.to_string().contains("identity path cannot be empty"));
    }

    #[test]
    fn record_from_worktree_uses_path_basename_without_canonicalizing() -> Result<()> {
        let record = WorkspaceRecord::from_worktree(
            worktree(
                "/repo/project__worktrees/feature-auth",
                Some("feature/auth"),
            ),
            false,
        )?;

        assert_eq!(record.workspace_slug(), "feature-auth");
        assert_eq!(record.branch(), Some("feature/auth"));
        assert_eq!(
            record.path(),
            Path::new("/repo/project__worktrees/feature-auth")
        );
        Ok(())
    }

    #[test]
    fn created_kmux_record_requires_path_slug_to_match() {
        let error = WorkspaceRecord::from_created_kmux_workspace(
            "feature-auth".to_owned(),
            PathBuf::from("/repo/project__worktrees/custom-auth"),
            "feature/auth".to_owned(),
        )
        .expect_err("mismatched path basename should fail");

        assert!(error.to_string().contains("does not match workspace slug"));
    }

    #[test]
    fn created_kmux_record_requires_branch_derived_slug_to_match() {
        let error = WorkspaceRecord::from_created_kmux_workspace(
            "feature-auth".to_owned(),
            PathBuf::from("/repo/project__worktrees/feature-auth"),
            "feature/other".to_owned(),
        )
        .expect_err("mismatched branch slug should fail");

        assert!(
            error
                .to_string()
                .contains("does not match branch-derived slug")
        );
    }

    #[test]
    fn kmux_worktree_requires_direct_child_of_worktree_base() {
        let paths = repo_paths();

        assert!(is_kmux_worktree(
            &paths,
            Path::new("/repo/project__worktrees/feature-auth")
        ));
        assert!(!is_kmux_worktree(&paths, Path::new("/repo/external-auth")));
        assert!(!is_kmux_worktree(
            &paths,
            Path::new("/repo/project__worktrees/archive/nested-auth")
        ));
    }

    #[test]
    fn strict_workspace_requires_branch_derived_direct_child_path() {
        let paths = repo_paths();

        assert!(is_strict_kmux_workspace(
            &paths,
            &worktree(
                "/repo/project__worktrees/feature-auth",
                Some("feature/auth")
            )
        ));
        assert!(!is_strict_kmux_workspace(
            &paths,
            &worktree("/repo/external-auth", Some("feature/auth"))
        ));
        assert!(!is_strict_kmux_workspace(
            &paths,
            &worktree(
                "/repo/project__worktrees/archive/nested-auth",
                Some("feature/nested")
            )
        ));
        assert!(!is_strict_kmux_workspace(
            &paths,
            &worktree("/repo/project__worktrees/custom-auth", Some("feature/auth"))
        ));
        assert!(!is_strict_kmux_workspace(
            &paths,
            &worktree("/repo/project__worktrees/detached", None)
        ));
    }

    #[test]
    fn validated_record_rejects_non_branch_derived_kmux_path() {
        let paths = repo_paths();
        let error = validated_kmux_record(
            &paths,
            worktree(
                "/repo/project__worktrees/custom-auth",
                Some("feature/legacy-auth"),
            ),
            false,
        )
        .expect_err("non-derived path should fail");

        assert!(
            error
                .to_string()
                .contains("non-derived kmux workspace path")
        );
        assert!(error.to_string().contains("expected 'feature-legacy-auth'"));
    }

    #[test]
    fn strict_workspace_records_return_reusable_records() -> Result<()> {
        let paths = repo_paths();
        let records = strict_kmux_workspace_records(
            &paths,
            [
                worktree("/repo/project", Some("main")),
                worktree(
                    "/repo/project__worktrees/feature-auth",
                    Some("feature/auth"),
                ),
                worktree(
                    "/repo/project__worktrees/custom-auth",
                    Some("feature/custom"),
                ),
                worktree("/repo/project__worktrees/detached", None),
            ],
        )?;

        assert_eq!(records.len(), 1);
        assert_eq!(records[0].workspace_slug(), "feature-auth");
        assert_eq!(records[0].branch(), Some("feature/auth"));
        Ok(())
    }

    #[test]
    fn inventory_json_preserves_parent_and_anchor_field_names() -> Result<()> {
        let record = WorkspaceRecord::from_worktree(
            worktree(
                "/repo/project__worktrees/feature-auth",
                Some("feature/auth"),
            ),
            false,
        )?;
        let mut item = WorkspaceInventoryItem::from_record(record, Some(100));
        item.set_parent_state("main".to_owned(), "anchor-commit".to_owned());
        item.set_tree_depth(1);

        let json = serde_json::to_value(item)?;

        assert_eq!(json["git_parent_branch"], "main");
        assert_eq!(json["git_anchor_commit"], "anchor-commit");
        assert!(json.get("tree_depth").is_none());
        Ok(())
    }
}
