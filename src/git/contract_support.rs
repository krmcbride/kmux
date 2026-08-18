//! Isolated repository and process environment for Git adapter contracts.

use std::ffi::OsString;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Result;
use tempfile::TempDir;

use super::Git;

/// Owned repository and command environment for Git adapter contracts.
pub(crate) struct GitRepoFixture {
    _temp: TempDir,
    root: PathBuf,
    path: PathBuf,
    environment: Vec<(OsString, OsString)>,
}

impl GitRepoFixture {
    /// Initialize a repository with one commit under a private Git environment.
    pub(crate) fn new() -> Result<Self> {
        let temp = TempDir::new()?;
        let root = temp.path().canonicalize()?;
        let path = root.join("project-alpha");
        let home = root.join("home");
        let config_home = root.join("config-home");
        let state_home = root.join("state-home");
        let cache_home = root.join("cache-home");
        let data_home = root.join("data-home");
        let runtime_dir = root.join("runtime-dir");
        let tmp = root.join("tmp");
        let hooks = root.join("empty-hooks");
        for directory in [
            &path,
            &home,
            &config_home,
            &state_home,
            &cache_home,
            &data_home,
            &runtime_dir,
            &tmp,
            &hooks,
        ] {
            fs::create_dir_all(directory)?;
        }
        let gitconfig = root.join("gitconfig");
        fs::write(
            &gitconfig,
            format!(
                "[commit]\n\tgpgSign = false\n[core]\n\thooksPath = {}\n",
                hooks.display()
            ),
        )?;
        let environment = vec![
            (OsString::from("HOME"), home.into_os_string()),
            (
                OsString::from("PATH"),
                std::env::var_os("PATH").unwrap_or_default(),
            ),
            (OsString::from("SHELL"), OsString::from("/bin/sh")),
            (OsString::from("LANG"), OsString::from("C")),
            (OsString::from("LC_ALL"), OsString::from("C")),
            (
                OsString::from("XDG_CONFIG_HOME"),
                config_home.into_os_string(),
            ),
            (
                OsString::from("XDG_STATE_HOME"),
                state_home.into_os_string(),
            ),
            (
                OsString::from("XDG_CACHE_HOME"),
                cache_home.into_os_string(),
            ),
            (OsString::from("XDG_DATA_HOME"), data_home.into_os_string()),
            (
                OsString::from("XDG_RUNTIME_DIR"),
                runtime_dir.into_os_string(),
            ),
            (OsString::from("TMPDIR"), tmp.into_os_string()),
            (OsString::from("GIT_CONFIG_NOSYSTEM"), OsString::from("1")),
            (
                OsString::from("GIT_CONFIG_GLOBAL"),
                gitconfig.into_os_string(),
            ),
            (
                OsString::from("GIT_AUTHOR_NAME"),
                OsString::from("Example Author"),
            ),
            (
                OsString::from("GIT_AUTHOR_EMAIL"),
                OsString::from("author@example.invalid"),
            ),
            (
                OsString::from("GIT_COMMITTER_NAME"),
                OsString::from("Example Committer"),
            ),
            (
                OsString::from("GIT_COMMITTER_EMAIL"),
                OsString::from("committer@example.invalid"),
            ),
        ];
        let fixture = Self {
            _temp: temp,
            root,
            path,
            environment,
        };
        fixture.git(&["init", "--initial-branch", "main"])?;
        fixture.commit_file("README.md", "example\n", "initial")?;
        Ok(fixture)
    }

    pub(crate) fn path(&self) -> &Path {
        &self.path
    }

    pub(crate) fn root(&self) -> &Path {
        &self.root
    }

    pub(crate) fn adapter(&self) -> Git {
        self.adapter_at(&self.path)
    }

    pub(crate) fn adapter_at(&self, path: impl AsRef<Path>) -> Git {
        Git::new(path).with_command_environment(self.environment.clone())
    }

    pub(crate) fn git(&self, args: &[&str]) -> Result<()> {
        self.adapter().stdout(args.iter().copied()).map(|_| ())
    }

    pub(crate) fn commit_file(&self, file_name: &str, content: &str, message: &str) -> Result<()> {
        fs::write(self.path.join(file_name), content)?;
        self.git(&["add", file_name])?;
        self.git(&["commit", "-m", message])
    }
}
