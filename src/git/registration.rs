//! Ownership binding to one Git worktree registration, never to a reusable path.

use std::collections::HashMap;
use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result, bail};

use super::Git;

impl Git {
    /// Bind explicit kmux ownership to this registration's private Git directory.
    ///
    /// Only creation and the one-time legacy migration may call this. Git removes
    /// the marker with the registration, so reusing a path cannot inherit authority.
    pub fn claim_worktree(&self, path: &Path) -> Result<String> {
        let marker = self.registration_marker(path)?;
        if let Some(id) = read_marker(&marker)? {
            return Ok(id);
        }
        let mut temporary = tempfile::Builder::new()
            .prefix("ws-")
            .rand_bytes(16)
            .tempfile_in(
                marker
                    .parent()
                    .context("registration marker has no parent")?,
            )?;
        let id = temporary
            .path()
            .file_name()
            .context("missing temporary name")?
            .to_string_lossy()
            .into_owned();
        writeln!(temporary, "{id}")?;
        temporary.as_file().sync_all()?;
        temporary.persist_noclobber(&marker).with_context(|| {
            format!("failed to bind workspace ownership at {}", marker.display())
        })?;
        Ok(id)
    }

    /// Read bindings from Git's linked-worktree administration directories.
    ///
    /// Reading the registration also works when a checkout is temporarily missing
    /// or locked. The marker is kmux policy, never evidence of authority by itself.
    pub(super) fn worktree_bindings(&self) -> Result<HashMap<PathBuf, String>> {
        let common = self.cwd().join(self.path_stdout("--git-common-dir")?);
        let directories = match fs::read_dir(common.join("worktrees")) {
            Ok(directories) => directories,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(HashMap::new()),
            Err(error) => return Err(error).context("failed to read Git worktree registrations"),
        };
        let mut bindings = HashMap::new();
        for directory in directories {
            let directory = directory?;
            if !directory.file_type()?.is_dir() {
                continue;
            }
            let Some(id) = read_marker(&directory.path().join("kmux-workspace-id"))? else {
                continue;
            };
            let gitdir = fs::read_to_string(directory.path().join("gitdir"))?;
            let gitdir = Path::new(gitdir.strip_suffix('\n').unwrap_or(&gitdir));
            let path = gitdir
                .parent()
                .context("Git registration has no worktree path")?;
            let path = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
            bindings.insert(path, id);
        }
        Ok(bindings)
    }

    fn registration_marker(&self, path: &Path) -> Result<PathBuf> {
        let directory = self.with_cwd(path).path_stdout("--absolute-git-dir")?;
        Ok(PathBuf::from(directory).join("kmux-workspace-id"))
    }
}

fn read_marker(path: &Path) -> Result<Option<String>> {
    let content = match fs::read_to_string(path) {
        Ok(content) => content,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(error).with_context(|| format!("failed to read {}", path.display()));
        }
    };
    let id = content.trim_end_matches('\n');
    if !id.starts_with("ws-") || !id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-') {
        bail!(
            "invalid workspace registration binding at {}",
            path.display()
        );
    }
    Ok(Some(id.to_owned()))
}
