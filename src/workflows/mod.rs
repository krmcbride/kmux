use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use serde::Serialize;

use crate::cli;
use crate::config::Config;
use crate::git::{Git, WorktreeInfo};
use crate::paths::RepoPaths;
use crate::slug::derive_handle;
use crate::tmux::{Tmux, kmux_worktree_option, window_target};

pub fn dispatch(command: cli::Command) -> Result<()> {
    match command {
        cli::Command::Add(args) => add(args),
        cli::Command::Open(args) => open(args),
        cli::Command::Close(args) => close(args),
        cli::Command::List(args) => list(args),
        cli::Command::Path(args) => path(args),
        cli::Command::Remove(args) => remove(args),
        command => bail!("{} is not implemented yet", command.display_name()),
    }
}

struct RepoContext {
    config: Config,
    paths: RepoPaths,
    git: Git,
}

struct TmuxContext {
    tmux: Tmux,
    session_name: String,
}

#[derive(Debug)]
struct ResolvedWorktree {
    handle: String,
    path: PathBuf,
    branch: Option<String>,
}

#[derive(Serialize)]
struct ListItem {
    handle: String,
    branch: Option<String>,
    path: String,
}

fn add(args: cli::AddArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let base = args.base.as_deref().or(repo.config.base_branch.as_deref());
    let handle = derive_handle(&args.branch, args.name.as_deref())?;
    let worktree_path = repo.paths.handle_path(&handle);

    if let Some(existing) = repo
        .git
        .find_worktree_by_name(&repo.paths.worktree_base_dir, &args.branch)?
    {
        if !args.open_if_exists {
            bail!(
                "worktree for '{}' already exists at {}",
                args.branch,
                existing.path.display()
            );
        }
        let resolved = resolved_from_worktree(existing)?;
        open_resolved(&repo, &tmux, &resolved, !args.background)?;
        println!("opened {}\t{}", resolved.handle, resolved.path.display());
        return Ok(());
    }

    if let Some(existing) = repo
        .git
        .find_worktree_by_handle(&repo.paths.worktree_base_dir, &handle)?
    {
        if !args.open_if_exists {
            bail!(
                "worktree handle '{}' already exists at {}",
                handle,
                existing.path.display()
            );
        }
        let resolved = resolved_from_worktree(existing)?;
        open_resolved(&repo, &tmux, &resolved, !args.background)?;
        println!("opened {}\t{}", resolved.handle, resolved.path.display());
        return Ok(());
    }

    repo.git.ensure_local_branch(&args.branch, base)?;
    repo.git.add_worktree(&worktree_path, &args.branch)?;
    apply_file_operations(&repo.config, &repo.paths.main_worktree, &worktree_path)?;
    run_post_create(
        &repo.config,
        &repo.paths.main_worktree,
        &worktree_path,
        &handle,
    )?;

    let resolved = ResolvedWorktree {
        handle,
        path: worktree_path,
        branch: Some(args.branch),
    };
    open_resolved(&repo, &tmux, &resolved, !args.background)?;
    println!("created {}\t{}", resolved.handle, resolved.path.display());
    Ok(())
}

fn open(args: cli::NameArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let resolved = resolve_worktree(&repo, &args.name)?;

    open_resolved(&repo, &tmux, &resolved, true)?;
    println!("opened {}\t{}", resolved.handle, resolved.path.display());
    Ok(())
}

fn close(args: cli::NameArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let resolved = resolve_worktree(&repo, &args.name)?;
    let window_name = repo.config.window_name(&resolved.handle);

    tmux.tmux.kill_window(&tmux.session_name, &window_name)?;
    println!("closed {}", resolved.handle);
    Ok(())
}

fn list(args: cli::JsonArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let items = list_items(&repo)?;

    if args.json {
        println!("{}", serde_json::to_string_pretty(&items)?);
    } else {
        for item in items {
            let branch = item.branch.as_deref().unwrap_or("-");
            println!("{}\t{}\t{}", item.handle, branch, item.path);
        }
    }
    Ok(())
}

fn path(args: cli::NameArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let resolved = resolve_worktree(&repo, &args.name)?;

    println!("{}", resolved.path.display());
    Ok(())
}

fn remove(args: cli::RemoveArgs) -> Result<()> {
    let repo = load_repo_context()?;
    let tmux = load_tmux_context()?;
    let resolved = resolve_worktree(&repo, &args.name)?;

    if same_path(&resolved.path, &repo.paths.main_worktree) {
        bail!(
            "cannot remove the main worktree at {}",
            resolved.path.display()
        );
    }
    if resolved.branch.is_none() && !args.keep_branch {
        bail!("worktree branch is unknown; use --keep-branch to remove only the worktree");
    }

    repo.git.remove_worktree(&resolved.path, args.force)?;
    if !args.keep_branch
        && let Some(branch) = &resolved.branch
    {
        repo.git.delete_local_branch(branch, args.force)?;
    }

    let window_name = repo.config.window_name(&resolved.handle);
    if tmux
        .tmux
        .window_exists_by_name(&tmux.session_name, &window_name)?
    {
        tmux.tmux.kill_window(&tmux.session_name, &window_name)?;
    }

    println!("removed {}", resolved.handle);
    Ok(())
}

fn load_repo_context() -> Result<RepoContext> {
    let cwd = std::env::current_dir().context("failed to read current directory")?;
    let paths = RepoPaths::discover(&cwd)?;
    let config = Config::load()?;
    let git = Git::new(&paths.main_worktree);

    Ok(RepoContext { config, paths, git })
}

fn load_tmux_context() -> Result<TmuxContext> {
    let tmux = Tmux::from_env();
    let context = tmux.current_context()?.ok_or_else(|| {
        anyhow!("tmux session could not be determined; run this command from inside tmux")
    })?;

    Ok(TmuxContext {
        tmux,
        session_name: context.session_name,
    })
}

fn open_resolved(
    repo: &RepoContext,
    tmux: &TmuxContext,
    resolved: &ResolvedWorktree,
    focus: bool,
) -> Result<()> {
    let window_name = repo.config.window_name(&resolved.handle);
    if tmux
        .tmux
        .window_exists_by_name(&tmux.session_name, &window_name)?
    {
        if focus {
            tmux.tmux.select_window(&tmux.session_name, &window_name)?;
        }
        return Ok(());
    }

    let command = startup_command(&repo.config);
    tmux.tmux.create_window_with_command(
        &tmux.session_name,
        &window_name,
        &resolved.path,
        command,
    )?;
    set_worktree_metadata(&tmux.tmux, &tmux.session_name, &window_name, resolved)?;
    if focus {
        tmux.tmux.select_window(&tmux.session_name, &window_name)?;
    }
    Ok(())
}

fn resolve_worktree(repo: &RepoContext, name: &str) -> Result<ResolvedWorktree> {
    for candidate in name_candidates(&repo.config, name) {
        if let Some(worktree) = repo
            .git
            .find_worktree_by_name(&repo.paths.worktree_base_dir, &candidate)?
        {
            return resolved_from_worktree(worktree);
        }
    }

    bail!("worktree '{}' not found", name)
}

fn resolved_from_worktree(worktree: WorktreeInfo) -> Result<ResolvedWorktree> {
    let handle = worktree
        .path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .filter(|name| !name.is_empty())
        .ok_or_else(|| {
            anyhow!(
                "could not determine handle from {}",
                worktree.path.display()
            )
        })?;

    Ok(ResolvedWorktree {
        handle,
        path: worktree.path,
        branch: worktree.branch,
    })
}

fn name_candidates(config: &Config, name: &str) -> Vec<String> {
    let mut candidates = vec![name.to_owned()];
    if let Some(stripped) = name.strip_prefix(config.window_prefix())
        && !stripped.is_empty()
    {
        candidates.push(stripped.to_owned());
    }
    candidates
}

fn list_items(repo: &RepoContext) -> Result<Vec<ListItem>> {
    let mut items = repo
        .git
        .worktrees()?
        .into_iter()
        .filter(|worktree| worktree.path.starts_with(&repo.paths.worktree_base_dir))
        .map(resolved_from_worktree)
        .collect::<Result<Vec<_>>>()?
        .into_iter()
        .map(|worktree| ListItem {
            handle: worktree.handle,
            branch: worktree.branch,
            path: worktree.path.display().to_string(),
        })
        .collect::<Vec<_>>();
    items.sort_by(|left, right| left.handle.cmp(&right.handle));
    Ok(items)
}

fn startup_command(config: &Config) -> Option<&str> {
    let panes = config.panes.as_deref()?;
    panes
        .iter()
        .find(|pane| pane.focus && pane.command.is_some())
        .or_else(|| panes.iter().find(|pane| pane.command.is_some()))
        .and_then(|pane| pane.command.as_deref())
}

fn set_worktree_metadata(
    tmux: &Tmux,
    session_name: &str,
    window_name: &str,
    resolved: &ResolvedWorktree,
) -> Result<()> {
    let target = window_target(session_name, window_name);
    tmux.set_window_option(
        &target,
        &kmux_worktree_option(&resolved.handle, "handle")?,
        &resolved.handle,
    )?;
    tmux.set_window_option(
        &target,
        &kmux_worktree_option(&resolved.handle, "path")?,
        &resolved.path.display().to_string(),
    )?;
    if let Some(branch) = &resolved.branch {
        tmux.set_window_option(
            &target,
            &kmux_worktree_option(&resolved.handle, "branch")?,
            branch,
        )?;
    }
    Ok(())
}

fn apply_file_operations(config: &Config, repo_root: &Path, worktree_path: &Path) -> Result<()> {
    for entry in config.files.copy_entries() {
        let relative = config_relative_path(entry)?;
        let source = repo_root.join(&relative);
        if source.symlink_metadata().is_err() {
            warn_missing_source("copy", &source);
            continue;
        }
        let destination = worktree_path.join(&relative);
        copy_path(&source, &destination)
            .with_context(|| format!("failed to copy {}", source.display()))?;
    }

    for entry in config.files.symlink_entries() {
        let relative = config_relative_path(entry)?;
        let source = repo_root.join(&relative);
        if source.symlink_metadata().is_err() {
            warn_missing_source("symlink", &source);
            continue;
        }
        let destination = worktree_path.join(&relative);
        symlink_path(&source, &destination)
            .with_context(|| format!("failed to symlink {}", source.display()))?;
    }

    Ok(())
}

fn config_relative_path(entry: &str) -> Result<PathBuf> {
    let path = Path::new(entry);
    if path.is_absolute()
        || path
            .components()
            .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        bail!("configured file path must be relative and stay inside the repo: {entry}");
    }
    Ok(path.to_path_buf())
}

fn warn_missing_source(operation: &str, source: &Path) {
    eprintln!(
        "kmux: warning: configured file source missing for {operation}: {}",
        source.display()
    );
}

fn copy_path(source: &Path, destination: &Path) -> Result<()> {
    let metadata = source.symlink_metadata()?;
    remove_destination(destination)?;
    if metadata.is_dir() {
        copy_dir_recursive(source, destination)?;
    } else {
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, destination)?;
    }
    Ok(())
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let entry_source = entry.path();
        let entry_destination = destination.join(entry.file_name());
        let file_type = entry.file_type()?;

        remove_destination(&entry_destination)?;
        if file_type.is_dir() {
            copy_dir_recursive(&entry_source, &entry_destination)?;
        } else if file_type.is_file() || file_type.is_symlink() {
            if let Some(parent) = entry_destination.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&entry_source, &entry_destination)?;
        }
    }
    Ok(())
}

fn symlink_path(source: &Path, destination: &Path) -> Result<()> {
    remove_destination(destination)?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }

    #[cfg(unix)]
    std::os::unix::fs::symlink(source, destination)?;

    #[cfg(windows)]
    {
        if source.is_dir() {
            std::os::windows::fs::symlink_dir(source, destination)?;
        } else {
            std::os::windows::fs::symlink_file(source, destination)?;
        }
    }

    Ok(())
}

fn remove_destination(destination: &Path) -> Result<()> {
    let Ok(metadata) = destination.symlink_metadata() else {
        return Ok(());
    };
    if metadata.is_dir() {
        fs::remove_dir_all(destination)?;
    } else {
        fs::remove_file(destination)?;
    }
    Ok(())
}

fn run_post_create(
    config: &Config,
    repo_root: &Path,
    worktree_path: &Path,
    handle: &str,
) -> Result<()> {
    for command in &config.post_create {
        let status = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(worktree_path)
            .env("KMUX_HANDLE", handle)
            .env("KMUX_WORKTREE_PATH", worktree_path)
            .env("KMUX_PROJECT_ROOT", repo_root)
            .status()
            .with_context(|| format!("failed to run post_create command: {command}"))?;

        if !status.success() {
            bail!("post_create command failed with status {status}: {command}");
        }
    }
    Ok(())
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}
