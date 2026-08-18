//! Process-backed contracts for launcher transport and child lifecycle.

use std::fs;
use std::path::Path;
use std::thread;
use std::time::Duration;

use anyhow::{Context, Result, bail};

use super::ingress::run_ingress_for_test;
use super::pending::shell_quote;
use super::protocol::ACK_TEMP_FILE;
use super::{PendingLaunch, ResolvedLauncher};

// Process-backed contract tests may run while Nix is saturating the build
// host. Keep production handoff deadlines strict, but give test helper
// threads enough scheduling headroom to observe the same behavior reliably.
const CONTRACT_HANDOFF_TIMEOUT: Duration = Duration::from_secs(30);

fn create_pending(launcher: &ResolvedLauncher, cwd: &Path) -> Result<PendingLaunch> {
    PendingLaunch::create_under(launcher, cwd, &cwd.join("launcher-state"))
}

fn wait_for_contract_spawn(pending: PendingLaunch) -> Result<()> {
    pending.wait_for_spawn_timeouts(CONTRACT_HANDOFF_TIMEOUT, CONTRACT_HANDOFF_TIMEOUT)
}

fn join_ingress(ingress: thread::JoinHandle<Result<i32>>) -> Result<i32> {
    ingress
        .join()
        .map_err(|_| anyhow::anyhow!("launcher ingress thread panicked"))?
}

fn expected_error<T>(result: Result<T>, expectation: &str) -> Result<anyhow::Error> {
    match result {
        Ok(_) => bail!("{expectation}"),
        Err(error) => Ok(error),
    }
}

#[cfg(unix)]
pub fn request_round_trip_preserves_exact_argv_and_cleans_up() -> Result<()> {
    let cwd = tempfile::tempdir()?;
    let output = cwd.path().join("argv");
    let output_arg = output.display().to_string();
    let input = " spaces ' quotes \" Unicode λ\n--leading ;$() * > sentinel ";
    let launcher = ResolvedLauncher::for_test(
        "/bin/sh",
        &[
            "-c",
            "output=$1; shift; printf '%s\\0' \"$@\" > \"$output\"",
            "launcher",
            &output_arg,
            "static two words",
            "",
            "--static",
        ],
        Some(input),
    );
    let pending = create_pending(&launcher, cwd.path())?;
    let request_path = pending.request_path().to_path_buf();
    let directory = request_path
        .parent()
        .context("launcher request should have a parent")?
        .to_path_buf();
    let ingress = thread::spawn(move || run_ingress_for_test(&request_path));

    wait_for_contract_spawn(pending)?;
    assert_eq!(join_ingress(ingress)?, 0);
    assert!(!directory.exists());
    let bytes = fs::read(output)?;
    let mut argv = bytes
        .split(|byte| *byte == 0)
        .map(|argument| String::from_utf8(argument.to_vec()))
        .collect::<std::result::Result<Vec<_>, _>>()?;
    if argv.last().is_some_and(String::is_empty) {
        argv.pop();
    }
    assert_eq!(argv, ["static two words", "", "--static", input]);
    Ok(())
}

#[cfg(unix)]
pub fn absent_and_empty_input_remain_distinct() -> Result<()> {
    for (input, expected_count) in [(None, "0"), (Some(""), "1")] {
        let cwd = tempfile::tempdir()?;
        let output = cwd.path().join("count");
        let script = format!(
            "printf '%s' \"$#\" > {}",
            shell_quote(&output.display().to_string())
        );
        let launcher = ResolvedLauncher::for_test("/bin/sh", &["-c", &script, "launcher"], input);
        let pending = create_pending(&launcher, cwd.path())?;
        let request_path = pending.request_path().to_path_buf();
        let ingress = thread::spawn(move || run_ingress_for_test(&request_path));

        wait_for_contract_spawn(pending)?;
        assert_eq!(join_ingress(ingress)?, 0);
        assert_eq!(fs::read_to_string(output)?, expected_count);
    }
    Ok(())
}

#[cfg(unix)]
pub fn concurrent_requests_do_not_collide_or_cross_acknowledgments() -> Result<()> {
    let cwd = tempfile::tempdir()?;
    let launcher = ResolvedLauncher::for_test("/bin/sh", &["-c", "exit 0"], None);
    let pending = (0..8)
        .map(|_| create_pending(&launcher, cwd.path()))
        .collect::<Result<Vec<_>>>()?;
    let ingress = pending
        .iter()
        .map(|pending| {
            let path = pending.request_path().to_path_buf();
            thread::spawn(move || run_ingress_for_test(&path))
        })
        .collect::<Vec<_>>();

    for launch in pending {
        wait_for_contract_spawn(launch)?;
    }
    for ingress in ingress {
        assert_eq!(join_ingress(ingress)?, 0);
    }
    Ok(())
}

pub fn spawn_failure_acknowledgment_and_diagnostics_are_sanitized() -> Result<()> {
    let cwd = tempfile::tempdir()?;
    let missing = cwd.path().join("missing-launcher");
    let command_sentinel = missing.display().to_string();
    let input_sentinel = "opaque-input-sentinel";
    let pending = create_pending(
        &ResolvedLauncher::for_test(&command_sentinel, &[], Some(input_sentinel)),
        cwd.path(),
    )?;
    let request_path = pending.request_path().to_path_buf();
    let ingress = thread::spawn(move || run_ingress_for_test(&request_path));

    let parent_error = expected_error(
        wait_for_contract_spawn(pending),
        "spawn failure should be acknowledged",
    )?
    .to_string();
    let ingress_error = expected_error(
        ingress
            .join()
            .map_err(|_| anyhow::anyhow!("launcher ingress thread panicked"))?,
        "ingress should fail",
    )?
    .to_string();
    for message in [parent_error, ingress_error] {
        assert!(!message.contains(&command_sentinel));
        assert!(!message.contains(input_sentinel));
    }
    Ok(())
}

#[cfg(unix)]
pub fn acknowledgment_delivery_failure_keeps_waiting_and_leaves_spawn_state_unknown() -> Result<()>
{
    let cwd = tempfile::tempdir()?;
    let marker = cwd.path().join("launcher-ran");
    let script = format!("touch {}", shell_quote(&marker.display().to_string()));
    let pending = create_pending(
        &ResolvedLauncher::for_test("/bin/sh", &["-c", &script], None),
        cwd.path(),
    )?;
    let request_parent = pending
        .request_path()
        .parent()
        .context("launcher request should have a parent")?;
    fs::create_dir(request_parent.join(ACK_TEMP_FILE))?;
    let request_path = pending.request_path().to_path_buf();
    let ingress = thread::spawn(move || run_ingress_for_test(&request_path));

    let error = expected_error(
        pending.wait_for_spawn_timeouts(Duration::from_millis(200), Duration::from_millis(50)),
        "missing acknowledgment should time out",
    )?;
    assert!(
        error
            .to_string()
            .contains("waiting for launcher process spawn acknowledgment")
    );
    let ingress_error = expected_error(
        ingress
            .join()
            .map_err(|_| anyhow::anyhow!("launcher ingress thread panicked"))?,
        "acknowledgment delivery should fail after child reaping",
    )?;
    assert!(
        ingress_error
            .to_string()
            .contains("failed to deliver launcher process spawn acknowledgment")
    );
    assert!(marker.exists());
    Ok(())
}

#[cfg(unix)]
pub fn relative_executable_paths_resolve_from_launcher_cwd() -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    let cwd = tempfile::tempdir()?;
    let marker = cwd.path().join("relative-ran");
    let executable = cwd.path().join("relative-launcher");
    fs::write(&executable, "#!/bin/sh\ntouch relative-ran\n")?;
    fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
    let pending = create_pending(
        &ResolvedLauncher::for_test("./relative-launcher", &[], None),
        cwd.path(),
    )?;
    let request_path = pending.request_path().to_path_buf();
    let ingress = thread::spawn(move || run_ingress_for_test(&request_path));

    wait_for_contract_spawn(pending)?;
    assert_eq!(join_ingress(ingress)?, 0);
    assert!(marker.exists());
    Ok(())
}
