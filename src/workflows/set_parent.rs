//! Source selection and explicit workspace lineage changes.

use anyhow::{Context, Result, bail};

use crate::cli;
use crate::git::Git;
use crate::state::workspace::WorkspaceStateStore;
use crate::workspace::{LineageParent, WorkspaceLineage};

use super::context::{RepoContext, load_repo_context};
use super::project_session;
use super::resolve::{
    find_workspace, load_workspace_state, resolve_current_kmux_workspace, resolve_workspace,
};

/// A source selected before creation or reparenting, with its commit resolved now.
pub(super) struct ResolvedSource {
    pub parent: LineageParent,
    pub commit: String,
}

/// Set lineage for a detached or attached child without changing its lifecycle authority.
pub(super) fn run(args: cli::SetParentArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let _lock = project_session::lock_project_lifecycle(&repo.paths)?;
    let child = match args.child.as_deref() {
        Some(target) => resolve_workspace(&repo, target)?,
        None => resolve_current_kmux_workspace(&repo, "workspace set-parent")?,
    };
    if !child.is_live() {
        bail!("child workspace is unavailable");
    }
    let source = resolve_source(&repo, Some(&args.parent), args.git_ref)?;
    let head = child
        .git()
        .and_then(|entry| entry.head.as_deref())
        .context("child workspace has no HEAD commit")?;
    let lineage = lineage_at(&repo, &source, head)?;
    let (mut state, _) = load_workspace_state(&repo)?;
    state.set_lineage(child.policy().id(), lineage.clone())?;
    WorkspaceStateStore::new(&repo.paths.git_common_dir).save(&state)?;
    println!(
        "set parent of {} to {} @ {}",
        child.branch().unwrap_or(child.policy().label()),
        source.parent.git_ref().unwrap_or(source.parent.label()),
        lineage.anchor.chars().take(12).collect::<String>()
    );
    Ok(())
}

/// Select a known workspace by ID/path/label/branch, or a Git ref when no workspace matches.
/// Explicit ref mode avoids selector ambiguity and records no workspace-to-workspace edge.
pub(super) fn resolve_source(
    repo: &RepoContext,
    target: Option<&str>,
    git_ref: bool,
) -> Result<ResolvedSource> {
    let current = repo.paths.current_worktree.to_string_lossy();
    let selector = target.unwrap_or(&current);
    if !git_ref && let Some(workspace) = find_workspace(repo, selector)? {
        if !workspace.is_live() {
            bail!(
                "source workspace '{}' is unavailable",
                workspace.policy().label()
            );
        }
        return Ok(ResolvedSource {
            parent: LineageParent::workspace(workspace.policy(), workspace.branch()),
            commit: workspace
                .git()
                .and_then(|entry| entry.head.clone())
                .context("source workspace has no HEAD commit")?,
        });
    }
    let reference = target.unwrap_or("HEAD");
    Ok(ResolvedSource {
        parent: LineageParent::GitRef { reference: reference.to_owned() },
        commit: Git::new(&repo.paths.current_worktree).resolve_commit(reference)
            .with_context(|| format!("source '{reference}' is neither an available workspace nor a valid Git commit ref"))?,
    })
}

/// Record the shared commit while leaving the immutable creation anchor independent.
pub(super) fn lineage_at(
    repo: &RepoContext,
    source: &ResolvedSource,
    child_commit: &str,
) -> Result<WorkspaceLineage> {
    let anchor = repo
        .git
        .merge_base(child_commit, &source.commit)?
        .ok_or_else(|| {
            anyhow::anyhow!(
                "workspace and source '{}' have no merge base",
                source.parent.label()
            )
        })?;
    Ok(WorkspaceLineage::new(source.parent.clone(), anchor))
}
