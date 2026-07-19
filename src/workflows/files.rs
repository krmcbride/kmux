use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{Context, Result, bail};

use crate::config::{Config, file_entry_relative_path};

/// Copy or symlink configured repo-relative files into a new worktree.
pub(super) fn apply_file_operations(
    config: &Config,
    repo_root: &Path,
    worktree_path: &Path,
) -> Result<()> {
    for entry in config.files.copy_entries() {
        let relative = file_entry_relative_path(entry)?;
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
        let relative = file_entry_relative_path(entry)?;
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

/// Run configured post-create shell commands in the new worktree with kmux env vars.
pub(super) fn run_post_create(
    config: &Config,
    repo_root: &Path,
    worktree_path: &Path,
    workspace_slug: &str,
) -> Result<()> {
    for command in &config.post_create {
        let status = Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(worktree_path)
            .env("KMUX_WORKSPACE_SLUG", workspace_slug)
            .env("KMUX_WORKSPACE_PATH", worktree_path)
            .env("KMUX_PROJECT_ROOT", repo_root)
            .status()
            .with_context(|| format!("failed to run post_create command: {command}"))?;

        if !status.success() {
            bail!("post_create command failed with status {status}: {command}");
        }
    }
    Ok(())
}

// Missing sources are warnings because optional local config files often differ
// between repos and machines.
fn warn_missing_source(operation: &str, source: &Path) {
    eprintln!(
        "kmux: warning: configured file source missing for {operation}: {}",
        source.display()
    );
}

// Copy files, symlinks, and directories, replacing any preexisting destination.
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

// Preserve directory structure recursively but skip special file types.
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

// Symlink configured paths using platform-specific directory/file APIs where required.
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

// Remove a destination path before copy/symlink so repeated setup is deterministic.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_operations_reject_invalid_config_relative_paths() {
        for entry in ["", ".", "./.envrc", "foo/./bar", "../secret", "/tmp/secret"] {
            let error = file_entry_relative_path(entry)
                .expect_err("invalid config file entry should fail before file operations");

            assert!(error.to_string().contains("configured file path"));
        }
    }
}
