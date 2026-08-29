//! Pane-side launcher ingress and child-process ownership.
//!
//! Ingress runs as the pane shell's foreground job and starts the configured
//! launcher with that pane's TTY. It acknowledges spawn so the original workflow
//! can continue, but remains alive until it reaps the child. That ordering keeps
//! the shell from resuming beside a launcher that still owns the same TTY, even
//! when acknowledgment delivery fails.

use std::path::Path;
use std::process::{Command, ExitStatus, Stdio};

use anyhow::{Context, Result, bail};

use super::protocol::{
    SpawnResult, consume_request, validate_protocol_path, validate_request, write_acknowledgment,
};

#[cfg(unix)]
mod signals;

#[cfg(unix)]
use signals::{IngressSignalGuard, configure_child_signal_defaults};

/// Consume a private request, spawn its foreground child, and return the child's exit code.
pub fn run_ingress(request_path: &Path) -> Result<i32> {
    run_ingress_inner(request_path, true)
}

fn run_ingress_inner(request_path: &Path, own_signals: bool) -> Result<i32> {
    let directory = validate_protocol_path(request_path)?;
    let request = match consume_request(request_path) {
        Ok(request) => request,
        Err(_) => {
            let _ = write_acknowledgment(&directory, SpawnResult::Failed);
            bail!("private launcher request is invalid");
        }
    };
    if validate_request(&request).is_err() {
        let _ = write_acknowledgment(&directory, SpawnResult::Failed);
        bail!("private launcher request is invalid");
    }

    #[cfg(unix)]
    let _signal_guard = own_signals
        .then(IngressSignalGuard::install)
        .transpose()
        .context("failed to retain launcher ingress signal ownership")?;
    #[cfg(not(unix))]
    let _ = own_signals;

    if cfg!(test) {
        bail!(
            "launcher child processes are unavailable in library unit tests; use an adapter contract target"
        );
    }
    let mut command = Command::new(&request.executable);
    command
        .args(&request.static_args)
        .current_dir(&request.cwd)
        .stdin(Stdio::inherit())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());
    if let Some(input) = &request.input {
        command.arg(input);
    }
    #[cfg(unix)]
    if own_signals {
        configure_child_signal_defaults(&mut command);
    }

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(_) => {
            let _ = write_acknowledgment(&directory, SpawnResult::Failed);
            bail!("configured launcher process could not be started");
        }
    };

    // A failed acknowledgment must not release the shell while its launcher is
    // still using the same foreground TTY. Always reap first, then report it.
    let acknowledgment = write_acknowledgment(&directory, SpawnResult::Spawned);
    let status = child
        .wait()
        .context("failed while waiting for configured launcher process")?;
    acknowledgment.context("failed to deliver launcher process spawn acknowledgment")?;

    Ok(shell_exit_code(status))
}

#[cfg(any(test, feature = "internal-adapter-contract-tests"))]
pub(super) fn run_ingress_for_test(request_path: &Path) -> Result<i32> {
    run_ingress_inner(request_path, false)
}

fn shell_exit_code(status: ExitStatus) -> i32 {
    if let Some(code) = status.code() {
        return code;
    }

    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;

        status.signal().map_or(1, |signal| 128 + signal)
    }

    #[cfg(not(unix))]
    {
        1
    }
}
