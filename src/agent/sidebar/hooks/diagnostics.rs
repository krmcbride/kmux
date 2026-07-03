//! Persistent diagnostics for sidebar hook attempts.

use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result};
use directories::BaseDirs;
use serde::Serialize;

/// One hook attempt to append to the sidebar hook diagnostic log.
pub(super) struct HookAttemptLog<'a> {
    pub(super) event: &'static str,
    pub(super) command: &'a str,
    pub(super) agent_kind: &'a str,
    pub(super) agent_session_id: &'a str,
    pub(super) status: &'a str,
    pub(super) duration: Duration,
    pub(super) exit_status: Option<String>,
    pub(super) error: Option<String>,
    pub(super) cwd: Option<String>,
    pub(super) stdout: &'a str,
    pub(super) stderr: &'a str,
}

#[derive(Debug, Serialize)]
struct HookAttemptLogEntry<'a> {
    timestamp: u64,
    event: &'static str,
    command: &'a str,
    agent_kind: &'a str,
    agent_session_id: &'a str,
    status: &'a str,
    duration_ms: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    exit_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cwd: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stdout: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stderr: Option<String>,
}

/// Return the default JSONL diagnostics file for sidebar hook attempts.
pub(super) fn default_log_path() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not determine state directory")?;
    let state_root = base_dirs
        .state_dir()
        .unwrap_or_else(|| base_dirs.data_local_dir());
    Ok(state_root.join("kmux/sidebar-hooks.jsonl"))
}

/// Append one hook attempt to the configured diagnostics file.
pub(super) fn log_attempt(log_path: Option<&Path>, attempt: HookAttemptLog<'_>) {
    let Some(log_path) = log_path else {
        return;
    };
    let entry = HookAttemptLogEntry {
        timestamp: crate::state::now_unix_seconds(),
        event: attempt.event,
        command: attempt.command,
        agent_kind: attempt.agent_kind,
        agent_session_id: attempt.agent_session_id,
        status: attempt.status,
        duration_ms: attempt.duration.as_millis().try_into().unwrap_or(u64::MAX),
        exit_status: attempt.exit_status,
        error: attempt.error,
        cwd: attempt.cwd,
        stdout: non_empty_output(attempt.stdout),
        stderr: non_empty_output(attempt.stderr),
    };
    let _ = append_hook_log(log_path, &entry);
}

fn append_hook_log(log_path: &Path, entry: &HookAttemptLogEntry<'_>) -> Result<()> {
    if let Some(parent) = log_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create hook log directory {}", parent.display()))?;
    }
    let mut options = OpenOptions::new();
    options.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(log_path)
        .with_context(|| format!("failed to open hook log {}", log_path.display()))?;
    serde_json::to_writer(&mut file, entry)?;
    file.write_all(b"\n")?;
    Ok(())
}

fn non_empty_output(output: &str) -> Option<String> {
    let output = output.trim();
    if output.is_empty() {
        None
    } else {
        Some(output.to_owned())
    }
}
