//! Git subprocess construction, environment isolation, and error translation.

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus};

use anyhow::{Context, Result, bail};

use crate::{LIFECYCLE_ACTIVE_ENV, telemetry};

#[derive(Debug, Clone)]
/// Thin adapter for running Git commands from a fixed working directory.
pub struct Git {
    cwd: PathBuf,
    clear_environment: bool,
    env: Vec<(OsString, OsString)>,
}

#[derive(Debug)]
/// Raw Git subprocess output with validated UTF-8 stdout and diagnostic stderr.
pub(super) struct GitOutput {
    pub(super) status: ExitStatus,
    pub(super) stdout: String,
    pub(super) stderr: String,
}

impl Git {
    /// Create a Git adapter rooted at `cwd`.
    pub fn new(cwd: impl AsRef<Path>) -> Self {
        Self {
            cwd: cwd.as_ref().to_path_buf(),
            clear_environment: false,
            env: Vec::new(),
        }
    }

    /// Run a Git command and return raw output without requiring a successful exit status.
    pub(super) fn output<I, S>(&self, args: I) -> Result<GitOutput>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        if cfg!(test) {
            bail!(
                "Git subprocesses are unavailable in library unit tests; use a pure policy seam or an adapter contract target"
            );
        }
        let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
        let display_args = display_args(&args);
        let output = telemetry::timed_result_event!(
            "subprocess",
            {
                program = "git",
                args = %display_args,
                cwd = %self.cwd.display(),
            },
            || {
                let mut command = Command::new("git");
                if self.clear_environment {
                    command.env_clear();
                }
                for (key, value) in &self.env {
                    command.env(key, value);
                }
                command
                    .args(&args)
                    .current_dir(&self.cwd)
                    // Git hooks and checkout filters run synchronously inside
                    // this child. Prevent them from recursively waiting on a
                    // lifecycle lock held by their parent kmux process.
                    .env(LIFECYCLE_ACTIVE_ENV, "1")
                    .output()
                    .with_context(|| format!("failed to run git {display_args}"))
            },
            ok |output| {
                status_code = output.status.code().unwrap_or(-1),
                success = output.status.success(),
                stdout_bytes = output.stdout.len(),
                stderr_bytes = output.stderr.len(),
            },
        )?;

        Ok(GitOutput {
            status: output.status,
            // Never turn an undecodable filesystem path into a different path.
            stdout: String::from_utf8(output.stdout).context(
                "git output contains a non-UTF-8 path or value; refusing lossy identity",
            )?,
            stderr: String::from_utf8_lossy(&output.stderr).into_owned(),
        })
    }

    /// Run a Git command, require success, and return trimmed stdout.
    pub(super) fn stdout<I, S>(&self, args: I) -> Result<String>
    where
        I: IntoIterator<Item = S>,
        S: Into<OsString>,
    {
        let output = self.output(args)?;
        if !output.status.success() {
            return bail_git(output);
        }
        Ok(output.stdout.trim_end().to_owned())
    }

    pub(super) fn cwd(&self) -> &Path {
        &self.cwd
    }

    pub(super) fn with_cwd(&self, cwd: impl AsRef<Path>) -> Self {
        Self {
            cwd: cwd.as_ref().to_path_buf(),
            clear_environment: self.clear_environment,
            env: self.env.clone(),
        }
    }

    #[cfg(feature = "internal-adapter-contract-tests")]
    pub(super) fn with_command_environment(mut self, env: Vec<(OsString, OsString)>) -> Self {
        self.clear_environment = true;
        self.env = env;
        self
    }
}

pub(super) fn bail_git<T>(output: GitOutput) -> Result<T> {
    let stderr = output.stderr.trim();
    if stderr.is_empty() {
        bail!("git command failed with status {}", output.status);
    }
    bail!("git command failed with status {}: {stderr}", output.status)
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
