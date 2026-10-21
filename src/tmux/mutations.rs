//! Tmux window/pane lifecycle, navigation, and synchronization operations.

use std::ffi::OsString;
use std::fs;
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

use super::context::{exact_session_target, validate_session_id, validate_window_id};
use super::process::Tmux;
use super::queries::TMUX_FIELD_SEPARATOR;

const PANE_START_TIMEOUT: Duration = Duration::from_secs(5);
const PANE_START_POLL_INTERVAL: Duration = Duration::from_millis(10);

impl Tmux {
    /// Create a detached shell-hosted window in an opaque session id.
    pub fn create_window_by_id(
        &self,
        session_id: &str,
        window_name: &str,
        cwd: &Path,
    ) -> Result<String> {
        validate_session_id(session_id)?;
        self.create_window_for_target(session_id, window_name, cwd)
    }

    /// Send controlled literal command text to a shell-hosted pane, followed by Enter.
    ///
    /// Launcher callers must pass only the hidden kmux ingress and its opaque
    /// capability path. Launcher argv and input do not belong in tmux arguments.
    pub fn send_literal_command(&self, pane_id: &str, command: &str) -> Result<()> {
        self.stdout(["send-keys", "-t", pane_id, "-l", command])?;
        self.stdout(["send-keys", "-t", pane_id, "Enter"])?;
        Ok(())
    }

    /// Send one tmux key token to a pane.
    pub fn send_key(&self, pane_id: &str, key: &str) -> Result<()> {
        self.stdout(["send-keys", "-t", pane_id, key])?;
        Ok(())
    }

    /// Start a shell command through tmux without waiting for it to finish.
    pub fn run_shell_background(&self, command: &str) -> Result<()> {
        self.stdout(["run-shell", "-b", command])?;
        Ok(())
    }

    /// Select a physical window by id within one exact tmux session.
    pub fn select_window_id_in_session(&self, session_target: &str, window_id: &str) -> Result<()> {
        let target = format!("{}:{window_id}", exact_session_target(session_target));
        self.stdout(["select-window", "-t", &target])?;
        Ok(())
    }

    /// Select a pane by tmux pane id.
    pub fn select_pane(&self, pane_id: &str) -> Result<()> {
        self.stdout(["select-pane", "-t", pane_id])?;
        Ok(())
    }

    /// Set the tmux pane title displayed by clients that expose pane titles.
    pub fn set_pane_title(&self, pane_id: &str, title: &str) -> Result<()> {
        self.stdout(["select-pane", "-t", pane_id, "-T", title])?;
        Ok(())
    }

    /// Switch the attached tmux client to a session.
    pub fn switch_client_to_session(&self, session_name: &str) -> Result<()> {
        let target = exact_session_target(session_name);
        self.stdout(["switch-client", "-t", &target])?;
        Ok(())
    }

    /// Kill one physical window by opaque session and window IDs.
    pub fn kill_window_id_in_session(&self, session_id: &str, window_id: &str) -> Result<()> {
        validate_session_id(session_id)?;
        validate_window_id(window_id)?;
        self.stdout(["kill-window", "-t", &format!("{session_id}:{window_id}")])?;
        Ok(())
    }

    /// Rename a physical window without changing its panes or running processes.
    pub fn rename_window(&self, window_id: &str, name: &str) -> Result<()> {
        validate_window_id(window_id)?;
        self.stdout(["rename-window", "-t", window_id, name])?;
        Ok(())
    }

    /// Create a detached full-height split with a concrete cell width at the window's left edge.
    pub fn split_window_left(
        &self,
        target_window: &str,
        width: u16,
        command: &str,
    ) -> Result<String> {
        let args = vec![
            OsString::from("split-window"),
            OsString::from("-d"),
            OsString::from("-h"),
            OsString::from("-b"),
            OsString::from("-f"),
            OsString::from("-t"),
            OsString::from(target_window),
            OsString::from("-l"),
            OsString::from(width.to_string()),
            OsString::from("-P"),
            OsString::from("-F"),
            OsString::from("#{pane_id}"),
            OsString::from(command),
        ];
        self.stdout(args)
    }

    /// Kill a tmux pane by pane id.
    pub fn kill_pane(&self, pane_id: &str) -> Result<()> {
        self.stdout(["kill-pane", "-t", pane_id])?;
        Ok(())
    }

    /// Resize a pane to an absolute cell width.
    pub fn resize_pane_width(&self, pane_id: &str, width: u16) -> Result<()> {
        self.stdout(["resize-pane", "-t", pane_id, "-x", &width.to_string()])?;
        Ok(())
    }

    /// Replace a pane's running command, killing the existing process if needed.
    pub fn respawn_pane(&self, pane_id: &str, command: &str) -> Result<()> {
        self.stdout(["respawn-pane", "-k", "-t", pane_id, command])?;
        Ok(())
    }

    /// Acquire a tmux wait-for lock channel.
    pub fn wait_for_lock(&self, channel: &str) -> Result<()> {
        self.stdout(["wait-for", "-L", channel])?;
        Ok(())
    }

    /// Release a tmux wait-for lock channel.
    pub fn wait_for_unlock(&self, channel: &str) -> Result<()> {
        self.stdout(["wait-for", "-U", channel])?;
        Ok(())
    }

    // `new-window` can return before the pane child has changed into `-c`.
    // Do not expose that transient parent cwd to topology snapshots or launcher handoff.
    pub(super) fn wait_for_pane_current_path(&self, pane_id: &str, expected: &Path) -> Result<()> {
        let started = Instant::now();
        let format = format!("#{{pane_current_path}}{TMUX_FIELD_SEPARATOR}#{{pane_dead}}");
        loop {
            let observed = self.stdout(["display-message", "-p", "-t", pane_id, &format])?;
            let (observed_path, pane_dead) = observed
                .split_once(TMUX_FIELD_SEPARATOR)
                .with_context(|| format!("invalid tmux pane readiness record {observed:?}"))?;
            if pane_dead == "1" {
                bail!(
                    "tmux pane {pane_id} exited before entering {}",
                    expected.display()
                );
            }
            if same_filesystem_path(Path::new(observed_path), expected) {
                return Ok(());
            }
            let elapsed = started.elapsed();
            if elapsed >= PANE_START_TIMEOUT {
                bail!(
                    "timed out after {elapsed:?} waiting for tmux pane {pane_id} to enter {}; last reported cwd was {observed_path:?}",
                    expected.display(),
                );
            }
            thread::sleep(PANE_START_POLL_INTERVAL.min(PANE_START_TIMEOUT - elapsed));
        }
    }

    fn create_window_for_target(
        &self,
        session_target: &str,
        window_name: &str,
        cwd: &Path,
    ) -> Result<String> {
        let target = format!("{session_target}:");
        let args = vec![
            OsString::from("new-window"),
            OsString::from("-d"),
            OsString::from("-t"),
            OsString::from(target),
            OsString::from("-n"),
            OsString::from(window_name),
            OsString::from("-c"),
            cwd.as_os_str().to_os_string(),
            OsString::from("-P"),
            OsString::from("-F"),
            OsString::from("#{pane_id}"),
        ];
        let pane_id = self.stdout(args)?;
        self.wait_for_pane_current_path(&pane_id, cwd)?;
        self.stdout([
            "set-option",
            "-w",
            "-t",
            &pane_id,
            "automatic-rename",
            "off",
        ])?;
        Ok(pane_id)
    }
}

fn same_filesystem_path(left: &Path, right: &Path) -> bool {
    left == right
        || fs::canonicalize(left)
            .and_then(|left| fs::canonicalize(right).map(|right| left == right))
            .unwrap_or(false)
}
