//! Isolated tmux server support shared by crate-level adapter contracts.
//!
//! This is a crate-wide visibility exception because sidebar contracts and tmux
//! adapter contracts need the same owned server fixture, and no narrower module
//! boundary spans those contract suites.

use std::ffi::OsString;
use std::fs;
use std::path::Path;

use anyhow::Result;
use tempfile::{Builder, TempDir};

use super::Tmux;

/// Isolated tmux server and process environment for adapter contracts.
pub struct TmuxFixture {
    pub tmux: Tmux,
    _environment: TempDir,
    _socket_root: TempDir,
}

impl TmuxFixture {
    /// Create a fixture with owned HOME/XDG/TMP state and a private named socket.
    pub fn new() -> Result<Self> {
        let environment = TempDir::new()?;
        // Darwin's per-user TMPDIR is long enough that tmux's additional
        // `tmux-<uid>/<label>` components can exceed the Unix socket limit.
        // A unique POSIX /tmp child keeps the complete socket path short
        // while the owned TempDir preserves parallel-test isolation.
        let socket_root = Builder::new().prefix("kmt-").tempdir_in("/tmp")?;
        let root = environment.path();
        let home = root.join("home");
        let config_home = root.join("config-home");
        let state_home = root.join("state-home");
        let cache_home = root.join("cache-home");
        let data_home = root.join("data-home");
        let runtime_dir = root.join("runtime-dir");
        let tmp = root.join("tmp");
        for directory in [
            &home,
            &config_home,
            &state_home,
            &cache_home,
            &data_home,
            &runtime_dir,
            &tmp,
        ] {
            fs::create_dir_all(directory)?;
        }

        let tmux = Tmux::with_socket_name("s")
            .with_clean_environment()
            .with_env("HOME", home.as_os_str())
            .with_env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .with_env("SHELL", "/bin/sh")
            .with_env("LANG", "C")
            .with_env("LC_ALL", "C")
            .with_env("XDG_CONFIG_HOME", config_home.as_os_str())
            .with_env("XDG_STATE_HOME", state_home.as_os_str())
            .with_env("XDG_CACHE_HOME", cache_home.as_os_str())
            .with_env("XDG_DATA_HOME", data_home.as_os_str())
            .with_env("XDG_RUNTIME_DIR", runtime_dir.as_os_str())
            .with_env("TMPDIR", tmp.as_os_str())
            .with_env("TMUX_TMPDIR", socket_root.path().as_os_str());

        Ok(Self {
            tmux,
            _environment: environment,
            _socket_root: socket_root,
        })
    }
}

impl Drop for TmuxFixture {
    fn drop(&mut self) {
        let _ = self.tmux.output(["kill-server"]);
    }
}

/// Create a detached test session and wait until its first pane reaches `cwd`.
pub fn create_test_session(tmux: &Tmux, session_name: &str, cwd: &Path) -> Result<String> {
    let pane_id = tmux.stdout(vec![
        OsString::from("-f"),
        OsString::from("/dev/null"),
        OsString::from("new-session"),
        OsString::from("-d"),
        OsString::from("-s"),
        OsString::from(session_name),
        OsString::from("-c"),
        cwd.as_os_str().to_os_string(),
        OsString::from("-P"),
        OsString::from("-F"),
        OsString::from("#{pane_id}"),
    ])?;
    tmux.wait_for_pane_current_path(&pane_id, cwd)?;
    Ok(pane_id)
}
