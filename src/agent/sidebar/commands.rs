//! Shell command builders for tmux sidebar hooks and panes.
//!
//! Commands preserve the current kmux executable and relevant environment so
//! background tmux hooks run against the same binary, socket, and XDG roots as
//! the command that installed them.

use std::ffi::OsString;
use std::path::Path;

use anyhow::{Context, Result};

/// Build the shell command tmux uses to run the sidebar TUI pane.
pub(super) fn sidebar_tui_command() -> Result<String> {
    sidebar_command(["sidebar", "_run"])
}

/// Build the shell command tmux hooks use to reconcile sidebar panes.
pub(super) fn sidebar_refresh_command() -> Result<String> {
    sidebar_command(["sidebar", "_refresh"])
}

/// Build the shell command used to disable the sidebar from inside tmux.
pub(super) fn sidebar_off_command() -> Result<String> {
    sidebar_command(["sidebar", "off"])
}

/// Build the shell command that wakes the sidebar pane for one window.
pub(super) fn sidebar_wake_command(window_id: &str) -> Result<String> {
    sidebar_command(["sidebar", "_wake", window_id])
}

/// Build the tmux hook command that expands `#{window_id}` at hook runtime.
pub(super) fn sidebar_wake_hook_command() -> Result<String> {
    Ok(sidebar_wake_hook_command_from(&sidebar_wake_command(
        "#{window_id}",
    )?))
}

fn sidebar_wake_hook_command_from(wake_command: &str) -> String {
    format!("run-shell -b {}", shell_quote(wake_command))
}

/// Quote a shell word for the simple POSIX shell commands kmux installs in tmux.
pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

// Preserve the executable path and relevant XDG/tmux environment so hooks run
// against the same kmux binary and tmux socket as the user command.
fn sidebar_command<const N: usize>(args: [&str; N]) -> Result<String> {
    let executable = std::env::current_exe().context("failed to determine current executable")?;
    let environment = sidebar_environment();
    Ok(sidebar_command_from(&executable, &environment, args))
}

fn sidebar_environment() -> Vec<(&'static str, OsString)> {
    [
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "KMUX_TMUX_SOCKET_NAME",
        "KMUX_TMUX_TMPDIR",
    ]
    .into_iter()
    .filter_map(|key| std::env::var_os(key).map(|value| (key, value)))
    .collect()
}

fn sidebar_command_from<const N: usize>(
    executable: &Path,
    environment: &[(&str, OsString)],
    args: [&str; N],
) -> String {
    let mut parts = vec!["exec".to_owned(), "env".to_owned()];
    for (key, value) in environment {
        parts.push(format_env_assignment(key, value));
    }
    parts.push(shell_quote(&executable.to_string_lossy()));
    parts.extend(args.into_iter().map(str::to_owned));
    parts.join(" ")
}

fn format_env_assignment(key: &str, value: &OsString) -> String {
    format!("{key}={}", shell_quote(&value.to_string_lossy()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn command<const N: usize>(args: [&str; N]) -> String {
        sidebar_command_from(
            Path::new("/opt/example/bin/kmux"),
            &[("XDG_CONFIG_HOME", OsString::from("/config/example"))],
            args,
        )
    }

    #[test]
    fn sidebar_pane_command_runs_hidden_tui() {
        let command = command(["sidebar", "_run"]);

        assert!(command.starts_with("exec env "));
        assert!(command.contains(" sidebar _run"));
        assert!(!command.contains("while :; do"));
    }

    #[test]
    fn sidebar_off_command_runs_visible_disable_path() {
        let command = command(["sidebar", "off"]);

        assert!(command.starts_with("exec env "));
        assert!(command.contains(" sidebar off"));
    }

    #[test]
    fn sidebar_wake_command_targets_window_id() {
        let command = command(["sidebar", "_wake", "@42"]);

        assert!(command.starts_with("exec env "));
        assert!(command.contains(" sidebar _wake "));
        assert!(command.ends_with(" sidebar _wake @42"));
    }

    #[test]
    fn sidebar_wake_hook_preserves_tmux_window_format() {
        let wake = command(["sidebar", "_wake", "#{window_id}"]);
        let command = sidebar_wake_hook_command_from(&wake);

        assert!(command.starts_with("run-shell -b "));
        assert!(command.contains("sidebar _wake"));
        assert!(command.contains("#{window_id}"));
    }
}
