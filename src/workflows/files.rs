use std::fs;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::Config;

pub(super) fn startup_command(config: &Config) -> Option<&str> {
    let panes = config.panes.as_deref()?;
    panes
        .iter()
        .find(|pane| pane.focus && pane.command.is_some())
        .or_else(|| panes.iter().find(|pane| pane.command.is_some()))
        .and_then(|pane| pane.command.as_deref())
}

pub(super) fn apply_file_operations(
    config: &Config,
    repo_root: &Path,
    worktree_path: &Path,
) -> Result<()> {
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

pub(super) fn run_post_create(
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

fn config_relative_path(entry: &str) -> Result<PathBuf> {
    let path = Path::new(entry);
    if entry.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                Component::CurDir | Component::ParentDir | Component::Prefix(_)
            )
        })
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
