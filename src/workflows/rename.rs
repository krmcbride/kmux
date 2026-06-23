use anyhow::{Context, Result, bail};

use crate::cli;
use crate::slug::derive_handle;
use crate::state::StateStore;

use super::context::{load_repo_context, load_tmux_context};
use super::metadata::{clear_worktree_metadata, set_worktree_metadata};
use super::resolve::{ResolvedWorktree, find_kmux_worktree_by_handle, resolve_worktree};
use super::util::same_path;

pub(super) fn run(args: cli::RenameArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let resolved = resolve_worktree(&repo, &args.old)?;
    let new_handle = derive_handle(
        resolved.branch.as_deref().unwrap_or(&args.new),
        Some(&args.new),
    )?;

    if same_path(&resolved.path, &repo.paths.main_worktree) {
        bail!(
            "cannot rename the main worktree at {}",
            resolved.path.display()
        );
    }
    if new_handle == resolved.handle {
        bail!(
            "nothing to rename: '{}' is already the worktree handle",
            new_handle
        );
    }

    let new_path = repo.paths.handle_path(&new_handle);
    if let Some(existing) = find_kmux_worktree_by_handle(&repo, &new_handle)? {
        bail!(
            "worktree handle '{}' already exists at {}",
            new_handle,
            existing.path.display()
        );
    }

    let old_window_name = repo.config.window_name(&resolved.handle);
    let new_window_name = repo.config.window_name(&new_handle);
    if tmux
        .tmux
        .window_exists_by_name(&tmux.session_name, &new_window_name)?
    {
        bail!("tmux window '{}' already exists", new_window_name);
    }

    std::env::set_current_dir(&repo.paths.main_worktree).with_context(|| {
        format!(
            "failed to change directory to {} before moving worktree",
            repo.paths.main_worktree.display()
        )
    })?;
    repo.git.move_worktree(&resolved.path, &new_path)?;

    if tmux
        .tmux
        .window_exists_by_name(&tmux.session_name, &old_window_name)?
    {
        tmux.tmux
            .rename_window(&tmux.session_name, &old_window_name, &new_window_name)?;
        clear_worktree_metadata(&tmux.tmux, &tmux.session_name, &new_window_name, &resolved)?;
        let renamed = ResolvedWorktree {
            handle: new_handle.clone(),
            path: new_path.clone(),
            branch: resolved.branch.clone(),
        };
        set_worktree_metadata(&tmux.tmux, &tmux.session_name, &new_window_name, &renamed)?;
    }

    if let Ok(store) = StateStore::new() {
        store.migrate_worktree(
            &resolved.handle,
            &new_handle,
            &resolved.path,
            &new_path,
            &old_window_name,
            &new_window_name,
        )?;
    }

    println!("renamed {}\t{}", resolved.handle, new_path.display());
    Ok(())
}
