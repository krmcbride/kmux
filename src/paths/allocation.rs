//! Exclusive opaque directories for new ephemeral worktrees across projects.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};
use tempfile::TempDir;

/// An empty reserved directory, automatically cleaned until Git creation begins.
pub struct EphemeralAllocation {
    directory: TempDir,
    path: PathBuf,
    id: String,
}

impl EphemeralAllocation {
    /// Atomically reserve an unpredictable directory without reusing an existing allocation.
    pub fn reserve(root: &Path, main_worktree: &Path) -> Result<Self> {
        if !root.is_absolute() {
            bail!("worktree root must be absolute");
        }
        let basename = main_worktree
            .file_name()
            .context("main worktree has no basename")?;
        fs::create_dir_all(root)
            .with_context(|| format!("failed to create worktree root {}", root.display()))?;
        let root = root.canonicalize()?;
        let directory = tempfile::Builder::new()
            .prefix("")
            .rand_bytes(12)
            .tempdir_in(root)?;
        let id = directory
            .path()
            .file_name()
            .context("allocation has no ID")?
            .to_string_lossy()
            .into_owned();
        let path = directory.path().join(basename);
        Ok(Self {
            directory,
            path,
            id,
        })
    }

    /// Return the opaque storage ID, also used as the default display label.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Return the reserved future worktree path.
    pub fn path(&self) -> &Path {
        &self.path
    }

    /// Preserve this allocation before invoking Git, including any partial Git failure.
    pub fn keep(self) -> PathBuf {
        let _ = self.directory.keep();
        self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeSet;

    #[test]
    fn concurrent_projects_with_same_basename_receive_exclusive_allocations() -> Result<()> {
        let root = TempDir::new()?;
        let gate = std::sync::Barrier::new(16);
        let allocations = std::thread::scope(|scope| {
            let mut handles = Vec::new();
            // Start every creator before joining so the fixture exercises contention.
            for index in 0..16 {
                let root = root.path();
                let gate = &gate;
                handles.push(scope.spawn(move || {
                    gate.wait();
                    EphemeralAllocation::reserve(
                        root,
                        &PathBuf::from(format!("/repo/group-{index}/project-alpha")),
                    )
                }));
            }
            handles
                .into_iter()
                .map(|handle| {
                    handle
                        .join()
                        .map_err(|_| anyhow::anyhow!("allocation thread failed"))?
                })
                .collect::<Result<Vec<_>>>()
        })?;
        let ids = allocations
            .iter()
            .map(EphemeralAllocation::id)
            .collect::<BTreeSet<_>>();
        assert_eq!(ids.len(), 16);
        for allocation in &allocations {
            assert_eq!(
                allocation.path(),
                root.path().join(allocation.id()).join("project-alpha")
            );
            assert_eq!(allocation.id().len(), 12);
        }
        Ok(())
    }
}
