//! Tmux subprocess construction, environment isolation, and error translation.

use std::ffi::OsString;
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};

use crate::telemetry;

#[derive(Debug, Clone, Default)]
/// Thin adapter for running tmux commands, optionally against a specific socket.
pub struct Tmux {
    socket_name: Option<OsString>,
    clear_environment: bool,
    clear_client_env: bool,
    env: Vec<(OsString, OsString)>,
}

#[derive(Debug)]
/// Raw tmux subprocess output with UTF-8-lossy stdout and stderr text.
pub struct TmuxOutput {
    pub status: ExitStatus,
    pub stdout: String,
    pub stderr: String,
}

impl Tmux {
    /// Create an adapter for the default tmux socket and current process environment.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an adapter from kmux-specific environment overrides.
    pub fn from_env() -> Self {
        let mut tmux = if let Some(socket_name) = std::env::var_os("KMUX_TMUX_SOCKET_NAME") {
            Self::with_socket_name(socket_name)
        } else {
            Self::new()
        };

        if let Some(tmux_tmpdir) = std::env::var_os("KMUX_TMUX_TMPDIR") {
            tmux = tmux.with_env("TMUX_TMPDIR", tmux_tmpdir);
        }

        tmux
    }

    /// Return a stable identifier for the tmux instance observed by this adapter.
    pub fn instance_id(&self) -> String {
        self.socket_name
            .as_ref()
            .map(|socket_name| socket_name.to_string_lossy().into_owned())
            .filter(|socket_name| !socket_name.is_empty())
            .unwrap_or_else(|| "default".to_owned())
    }

    /// Run a tmux command and return raw output without requiring a successful exit status.
    pub fn output<I, S>(&self, args: I) -> Result<TmuxOutput>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        if cfg!(test) {
            bail!(
                "tmux subprocesses are unavailable in library unit tests; use typed pane/window facts or an adapter contract target"
            );
        }
        let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        let display_args = display_args(&args);
        let command_name = command_name(&args);
        let output = telemetry::timed_result_event!(
            "subprocess",
            {
                program = "tmux",
                command = %command_name,
            },
            || {
                let mut command = Command::new("tmux");
                if self.clear_environment {
                    command.env_clear();
                }
                // Tmux otherwise sanitizes control separators in format output when
                // the caller's locale is unset or non-UTF-8.
                command.arg("-u");
                if let Some(socket_name) = &self.socket_name {
                    command.arg("-L").arg(socket_name);
                }
                if self.clear_client_env {
                    command.env_remove("TMUX");
                    command.env_remove("TMUX_PANE");
                }
                for (key, value) in &self.env {
                    command.env(key, value);
                }
                command
                    .args(&args)
                    .output()
                    .with_context(|| format!("failed to run tmux {display_args}"))
            },
            ok |output| {
                status_code = output.status.code().unwrap_or(-1),
                success = output.status.success(),
                stdout_bytes = output.stdout.len(),
                stderr_bytes = output.stderr.len(),
            },
        )?;

        Ok(TmuxOutput {
            status: output.status,
            stdout: String::from_utf8_lossy(&output.stdout).into_owned(),
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Run a tmux command, require success, and return trimmed stdout.
    pub fn stdout<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let output = self.output(args)?;
        if !output.status.success() {
            return bail_tmux(output);
        }
        Ok(output.stdout.trim_end().to_owned())
    }

    /// Create an adapter pinned to a named tmux socket.
    ///
    /// The ambient `TMUX` variables are cleared so commands target that socket rather
    /// than the caller's attached client.
    pub(super) fn with_socket_name(socket_name: impl Into<OsString>) -> Self {
        Self {
            socket_name: Some(socket_name.into()),
            clear_environment: false,
            clear_client_env: true,
            env: Vec::new(),
        }
    }

    /// Add one environment override to every tmux subprocess.
    pub(super) fn with_env(mut self, key: impl Into<OsString>, value: impl Into<OsString>) -> Self {
        self.env.push((key.into(), value.into()));
        self
    }

    #[cfg(feature = "internal-adapter-contract-tests")]
    pub(super) fn with_clean_environment(mut self) -> Self {
        self.clear_environment = true;
        self
    }
}

pub(super) fn tmux_server_is_absent(stderr: &str) -> bool {
    let stderr = stderr.trim();
    stderr.starts_with("no server running on ")
        || (stderr.starts_with("error connecting to ")
            && (stderr.contains("No such file or directory")
                || stderr.contains("Connection refused")))
}

pub(super) fn bail_tmux<T>(output: TmuxOutput) -> Result<T> {
    let stderr = output.stderr.trim();
    if stderr.is_empty() {
        bail!("tmux command failed with status {}", output.status);
    }
    bail!(
        "tmux command failed with status {}: {stderr}",
        output.status
    )
}

fn display_args(args: &[OsString]) -> String {
    let mut display = String::new();
    for arg in args {
        if !display.is_empty() {
            display.push(' ');
        }
        display.push_str(&arg.to_string_lossy());
    }
    display
}

fn command_name(args: &[OsString]) -> String {
    args.first()
        .map(|arg| arg.to_string_lossy().into_owned())
        .unwrap_or_default()
}
