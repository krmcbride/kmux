//! Caller-side ownership of one private launcher handoff.
//!
//! The caller keeps the private request directory alive until the handoff
//! resolves. It waits separately for the pane shell to claim the request and for
//! ingress to spawn the child, so slow shell initialization does not consume the
//! spawn budget. At the claim deadline, request removal arbitrates cancellation:
//! the caller either prevents a late spawn or observes that ingress already owns
//! the request and grants it a fresh spawn-acknowledgment interval.

use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use tempfile::TempDir;

#[cfg(any(test, feature = "internal-adapter-contract-tests"))]
use super::protocol::create_request_directory_under;
use super::protocol::{
    ACK_FILE, LaunchRequest, PROTOCOL_VERSION, REQUEST_FILE, REQUEST_TEMP_FILE, SpawnResult,
    cancel_unclaimed_request, create_request_directory, read_acknowledgment, validate_cwd,
    write_json_atomically,
};
use super::resolved::ResolvedLauncher;

// A newly-created pane may run shell hooks or a cold direnv/Nix evaluation
// before it can execute command text already queued by tmux. Once ingress
// consumes the request, process spawn should remain a short local operation.
const INGRESS_CLAIM_TIMEOUT: Duration = Duration::from_secs(3);
const SPAWN_ACK_TIMEOUT: Duration = Duration::from_secs(3);
const ACK_POLL_INTERVAL: Duration = Duration::from_millis(10);

/// Outer cleanup owner for one private request and its spawn acknowledgment.
pub struct PendingLaunch {
    _directory: TempDir,
    request_path: PathBuf,
    ack_path: PathBuf,
}

impl PendingLaunch {
    /// Materialize a mode-restricted one-shot request for a resolved launcher.
    pub fn create(launcher: &ResolvedLauncher, cwd: &Path) -> Result<Self> {
        validate_resolved_launcher(launcher, cwd)?;
        let directory = create_request_directory()?;
        Self::create_in_directory(launcher, cwd, directory)
    }

    /// Build the literal shell command containing only kmux ingress data.
    pub fn ingress_command(&self) -> Result<String> {
        let executable =
            std::env::current_exe().context("failed to locate current kmux executable")?;
        let executable = executable
            .to_str()
            .context("current kmux executable path is not valid UTF-8")?;
        let request = self
            .request_path
            .to_str()
            .context("private launcher request path is not valid UTF-8")?;
        Ok(format!(
            "{} _launch {}",
            shell_quote(executable),
            shell_quote(request)
        ))
    }

    /// Wait for the pane shell to claim the request, then acknowledge child spawn.
    pub fn wait_for_spawn(self) -> Result<()> {
        self.wait_for_spawn_timeouts(INGRESS_CLAIM_TIMEOUT, SPAWN_ACK_TIMEOUT)
    }

    #[cfg(any(test, feature = "internal-adapter-contract-tests"))]
    pub(super) fn create_under(
        launcher: &ResolvedLauncher,
        cwd: &Path,
        base: &Path,
    ) -> Result<Self> {
        validate_resolved_launcher(launcher, cwd)?;
        let directory = create_request_directory_under(base)?;
        Self::create_in_directory(launcher, cwd, directory)
    }

    fn create_in_directory(
        launcher: &ResolvedLauncher,
        cwd: &Path,
        directory: TempDir,
    ) -> Result<Self> {
        let canonical_directory = fs::canonicalize(directory.path())
            .context("failed to resolve private launcher request directory")?;
        let request_path = canonical_directory.join(REQUEST_FILE);
        let ack_path = canonical_directory.join(ACK_FILE);
        let request = LaunchRequest {
            version: PROTOCOL_VERSION,
            cwd: cwd.to_path_buf(),
            executable: launcher.executable().to_owned(),
            static_args: launcher.static_args().to_vec(),
            input: launcher.input().map(str::to_owned),
        };
        write_json_atomically(
            &canonical_directory,
            REQUEST_TEMP_FILE,
            REQUEST_FILE,
            &request,
        )
        .context("failed to materialize private launcher request")?;

        Ok(Self {
            _directory: directory,
            request_path,
            ack_path,
        })
    }

    pub(super) fn wait_for_spawn_timeouts(
        &self,
        ingress_timeout: Duration,
        spawn_timeout: Duration,
    ) -> Result<()> {
        let ingress_started = Instant::now();
        let mut spawn_started = None;
        loop {
            match read_acknowledgment(&self.ack_path) {
                Ok(Some(acknowledgment)) => {
                    if acknowledgment.version != PROTOCOL_VERSION {
                        bail!("launcher ingress returned an unsupported acknowledgment version");
                    }
                    return match acknowledgment.result {
                        SpawnResult::Spawned => Ok(()),
                        SpawnResult::Failed => bail!("launcher process could not be started"),
                    };
                }
                Ok(None) => {}
                Err(error) => {
                    return Err(error)
                        .context("launcher ingress returned an invalid acknowledgment");
                }
            }

            if spawn_started.is_none()
                && !self
                    .request_path
                    .try_exists()
                    .context("failed to inspect private launcher request")?
            {
                spawn_started = Some(Instant::now());
            }
            let (started, timeout) = spawn_started
                .map(|started| (started, spawn_timeout))
                .unwrap_or((ingress_started, ingress_timeout));
            let elapsed = started.elapsed();
            if elapsed >= timeout {
                if spawn_started.is_some() {
                    bail!(
                        "timed out after {timeout:?} waiting for launcher process spawn acknowledgment"
                    );
                }
                if cancel_unclaimed_request(&self.request_path)? {
                    bail!(
                        "timed out after {timeout:?} waiting for launcher ingress to consume its request"
                    );
                }
                // Ingress won the atomic remove race at the claim deadline. It
                // owns the request and receives the full spawn-ack interval.
                spawn_started = Some(Instant::now());
                continue;
            }
            thread::sleep(ACK_POLL_INTERVAL.min(timeout.saturating_sub(elapsed)));
        }
    }

    #[cfg(any(test, feature = "internal-adapter-contract-tests"))]
    pub(super) fn request_path(&self) -> &Path {
        &self.request_path
    }

    #[cfg(test)]
    pub(super) fn acknowledgment_path(&self) -> &Path {
        &self.ack_path
    }
}

fn validate_resolved_launcher(launcher: &ResolvedLauncher, cwd: &Path) -> Result<()> {
    if launcher.executable().trim().is_empty() || launcher.executable().contains('\0') {
        bail!("resolved launcher executable is invalid");
    }
    if launcher
        .static_args()
        .iter()
        .any(|argument| argument.contains('\0'))
        || launcher.input().is_some_and(|input| input.contains('\0'))
    {
        bail!("resolved launcher arguments contain unsupported NUL data");
    }
    validate_cwd(cwd)
}

pub(super) fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::launcher::protocol::{DIRECTORY_PREFIX, SpawnResult, write_acknowledgment};

    fn create_pending(launcher: &ResolvedLauncher, cwd: &Path) -> Result<PendingLaunch> {
        PendingLaunch::create_under(launcher, cwd, &cwd.join("launcher-state"))
    }

    #[test]
    fn ingress_command_contains_only_controlled_capability_data() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let sentinel = "opaque-input-sentinel";
        let pending = create_pending(
            &ResolvedLauncher::for_test("example-command", &["static-sentinel"], Some(sentinel)),
            cwd.path(),
        )?;

        let command = pending.ingress_command()?;
        assert!(command.contains(" _launch "));
        assert!(command.contains(DIRECTORY_PREFIX));
        assert!(!command.contains(sentinel));
        assert!(!command.contains("static-sentinel"));
        assert!(!command.contains("example-command"));
        Ok(())
    }

    #[test]
    fn ingress_claim_timeout_drops_all_transient_files() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let pending = create_pending(
            &ResolvedLauncher::for_test("example-command", &[], None),
            cwd.path(),
        )?;
        let directory = pending
            .request_path()
            .parent()
            .expect("request parent")
            .to_path_buf();

        let error = pending
            .wait_for_spawn_timeouts(Duration::from_millis(25), Duration::from_millis(25))
            .expect_err("missing ingress should time out");
        assert!(
            error
                .to_string()
                .contains("waiting for launcher ingress to consume its request")
        );
        drop(pending);
        assert!(!directory.exists());
        Ok(())
    }

    #[test]
    fn consumed_request_gets_a_fresh_spawn_acknowledgment_window() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let pending = create_pending(
            &ResolvedLauncher::for_test("example-command", &[], None),
            cwd.path(),
        )?;
        let directory = pending
            .request_path()
            .parent()
            .expect("request parent")
            .to_path_buf();
        fs::remove_file(pending.request_path())?;
        let (started_tx, started_rx) = std::sync::mpsc::sync_channel(0);
        let acknowledgment = thread::spawn(move || {
            started_tx
                .send(())
                .map_err(|_| anyhow::anyhow!("acknowledgment test receiver dropped"))?;
            thread::sleep(Duration::from_millis(50));
            write_acknowledgment(&directory, SpawnResult::Spawned)
        });
        started_rx
            .recv()
            .context("acknowledgment test thread did not start")?;

        pending.wait_for_spawn_timeouts(Duration::from_millis(10), Duration::from_secs(1))?;
        acknowledgment
            .join()
            .map_err(|_| anyhow::anyhow!("acknowledgment thread panicked"))??;
        Ok(())
    }

    #[test]
    fn spawn_acknowledgment_timeout_starts_after_request_consumption() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let pending = create_pending(
            &ResolvedLauncher::for_test("example-command", &[], None),
            cwd.path(),
        )?;
        fs::remove_file(pending.request_path())?;

        let error = pending
            .wait_for_spawn_timeouts(Duration::from_secs(1), Duration::from_millis(25))
            .expect_err("consumed request without acknowledgment should time out");

        assert!(
            error
                .to_string()
                .contains("waiting for launcher process spawn acknowledgment")
        );
        Ok(())
    }
}
