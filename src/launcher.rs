//! One-shot launcher model, private transport, and pane-side process ownership.
//!
//! The module has three related roles:
//!
//! - [`ResolvedLauncher`] is the validated in-memory executable, static argv, and
//!   optional final input selected by a workflow. It contains no tmux or agent
//!   concepts.
//! - [`PendingLaunch`] is the caller-side capability. It creates a private
//!   one-shot directory, writes a versioned request, builds the controlled hidden
//!   shell command, waits bounded intervals for ingress claim and spawn
//!   acknowledgment, and owns cleanup even on failure.
//! - [`run_ingress`] is the pane-side adapter invoked by that hidden command. It
//!   consumes the request before spawn, validates it again, launches exact argv in
//!   the worktree with inherited TTY streams, acknowledges spawn, and waits/reaps
//!   the child before returning control to the pane shell.
//!
//! Only the current kmux executable and an opaque capability path pass through
//! tmux/shell command text; launcher argv and input stay in mode-restricted
//! transient storage and are removed before the child lifetime. On Unix, ingress
//! also retains foreground-job ownership across catchable terminal signals so a
//! launcher that handles Ctrl-C cannot outlive its parent and race the resumed
//! shell.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime};

use anyhow::{Context, Result, bail};
use directories::BaseDirs;
use serde::{Deserialize, Serialize};
use tempfile::{Builder, TempDir};

use crate::config::LauncherConfig;

const PROTOCOL_VERSION: u32 = 1;
const DIRECTORY_PREFIX: &str = ".kmux-launch-v1-";
const REQUEST_FILE: &str = "request.json";
const REQUEST_TEMP_FILE: &str = "request.tmp";
const ACK_FILE: &str = "ack.json";
const ACK_TEMP_FILE: &str = "ack.tmp";
const LAUNCH_RUNTIME_DIRECTORY: &str = "launcher-runtime";
// A newly-created pane may run shell hooks or a cold direnv/Nix evaluation
// before it can execute command text already queued by tmux. Once ingress
// consumes the request, process spawn should remain a short local operation.
const INGRESS_CLAIM_TIMEOUT: Duration = Duration::from_secs(3);
const SPAWN_ACK_TIMEOUT: Duration = Duration::from_secs(3);
const ACK_POLL_INTERVAL: Duration = Duration::from_millis(10);
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

/// A validated, in-memory launcher choice ready for one workspace window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedLauncher {
    name: String,
    executable: String,
    static_args: Vec<String>,
    input: Option<String>,
}

impl ResolvedLauncher {
    /// Resolve one validated config record while preserving its exact argv data.
    pub fn from_config(name: &str, config: &LauncherConfig, input: Option<String>) -> Self {
        Self {
            name: name.to_owned(),
            executable: config.command().to_owned(),
            static_args: config.args().to_vec(),
            input,
        }
    }

    /// Return the user-facing launcher name used in sanitized workflow errors.
    pub fn name(&self) -> &str {
        &self.name
    }
}

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

    #[cfg(test)]
    fn create_under(launcher: &ResolvedLauncher, cwd: &Path, base: &Path) -> Result<Self> {
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
            executable: launcher.executable.clone(),
            static_args: launcher.static_args.clone(),
            input: launcher.input.clone(),
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

    fn wait_for_spawn_timeouts(
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
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchRequest {
    version: u32,
    cwd: PathBuf,
    executable: String,
    static_args: Vec<String>,
    input: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct LaunchAcknowledgment {
    version: u32,
    result: SpawnResult,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SpawnResult {
    Spawned,
    Failed,
}

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

#[cfg(test)]
fn run_ingress_for_test(request_path: &Path) -> Result<i32> {
    run_ingress_inner(request_path, false)
}

#[cfg(unix)]
const INGRESS_SIGNALS: [libc::c_int; 3] = [libc::SIGINT, libc::SIGHUP, libc::SIGTERM];

#[cfg(unix)]
struct IngressSignalGuard {
    previous: Vec<(libc::c_int, libc::sigaction)>,
}

#[cfg(unix)]
impl IngressSignalGuard {
    // The shell waits for ingress, not its child. Keep ingress alive for signals
    // sent to their shared foreground process group so it remains the sole owner
    // that reaps the launcher before the shell can resume.
    fn install() -> Result<Self> {
        let mut guard = Self {
            previous: Vec::with_capacity(INGRESS_SIGNALS.len()),
        };
        for signal in INGRESS_SIGNALS {
            let mut action = unsafe { std::mem::zeroed::<libc::sigaction>() };
            action.sa_sigaction = retain_ingress_ownership as *const () as usize;
            action.sa_flags = 0;
            unsafe {
                libc::sigemptyset(&mut action.sa_mask);
            }

            let mut previous = unsafe { std::mem::zeroed::<libc::sigaction>() };
            if unsafe { libc::sigaction(signal, &action, &mut previous) } != 0 {
                return Err(std::io::Error::last_os_error().into());
            }
            guard.previous.push((signal, previous));
        }
        Ok(guard)
    }
}

#[cfg(unix)]
impl Drop for IngressSignalGuard {
    fn drop(&mut self) {
        for (signal, action) in self.previous.iter().rev() {
            unsafe {
                libc::sigaction(*signal, action, std::ptr::null_mut());
            }
        }
    }
}

#[cfg(unix)]
extern "C" fn retain_ingress_ownership(_signal: libc::c_int) {}

#[cfg(unix)]
fn configure_child_signal_defaults(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    unsafe {
        command.pre_exec(|| {
            for signal in INGRESS_SIGNALS {
                let mut action = std::mem::zeroed::<libc::sigaction>();
                action.sa_sigaction = libc::SIG_DFL;
                action.sa_flags = 0;
                libc::sigemptyset(&mut action.sa_mask);
                if libc::sigaction(signal, &action, std::ptr::null_mut()) != 0 {
                    return Err(std::io::Error::last_os_error());
                }
            }
            Ok(())
        });
    }
}

fn validate_resolved_launcher(launcher: &ResolvedLauncher, cwd: &Path) -> Result<()> {
    if launcher.executable.trim().is_empty() || launcher.executable.contains('\0') {
        bail!("resolved launcher executable is invalid");
    }
    if launcher
        .static_args
        .iter()
        .any(|argument| argument.contains('\0'))
        || launcher
            .input
            .as_ref()
            .is_some_and(|input| input.contains('\0'))
    {
        bail!("resolved launcher arguments contain unsupported NUL data");
    }
    validate_cwd(cwd)
}

fn validate_request(request: &LaunchRequest) -> Result<()> {
    if request.version != PROTOCOL_VERSION {
        bail!("unsupported private launcher request version");
    }
    if request.executable.trim().is_empty() || request.executable.contains('\0') {
        bail!("private launcher request executable is invalid");
    }
    if request
        .static_args
        .iter()
        .any(|argument| argument.contains('\0'))
        || request
            .input
            .as_ref()
            .is_some_and(|input| input.contains('\0'))
    {
        bail!("private launcher request arguments are invalid");
    }
    validate_cwd(&request.cwd)
}

fn validate_cwd(cwd: &Path) -> Result<()> {
    if !cwd.is_absolute() {
        bail!("launcher working directory must be absolute");
    }
    let metadata = fs::metadata(cwd).context("launcher working directory is unavailable")?;
    if !metadata.is_dir() {
        bail!("launcher working directory is not a directory");
    }
    Ok(())
}

fn create_request_directory() -> Result<TempDir> {
    let base = shared_launcher_runtime_directory()?;
    create_request_directory_under(&base)
}

fn create_request_directory_under(base: &Path) -> Result<TempDir> {
    create_private_base_directory(base)?;
    let base = fs::canonicalize(base).with_context(|| {
        format!(
            "failed to resolve launcher runtime directory {}",
            base.display()
        )
    })?;
    prune_stale_directories(&base, STALE_AFTER);
    create_private_tempdir(&base).with_context(|| {
        format!(
            "failed to create private launcher request directory under {}",
            base.display()
        )
    })
}

fn create_private_tempdir(base: &Path) -> Result<TempDir> {
    let directory = Builder::new().prefix(DIRECTORY_PREFIX).tempdir_in(base)?;
    set_private_directory_permissions(directory.path())?;
    validate_private_directory(directory.path())?;
    Ok(directory)
}

// Launcher requests cross from the invoking process into an existing tmux
// server. Sandboxes may give those processes different /tmp and runtime mounts,
// so use the shared user-state filesystem rather than namespace-local temp.
fn shared_launcher_runtime_directory() -> Result<PathBuf> {
    let base_dirs = BaseDirs::new().context("could not determine launcher state directory")?;
    let state_root = base_dirs
        .state_dir()
        .unwrap_or_else(|| base_dirs.data_local_dir());
    Ok(state_root.join("kmux").join(LAUNCH_RUNTIME_DIRECTORY))
}

fn create_private_base_directory(path: &Path) -> Result<()> {
    let parent = path.parent().context("launcher runtime has no parent")?;
    fs::create_dir_all(parent)
        .with_context(|| format!("failed to create kmux state directory {}", parent.display()))?;
    let mut builder = fs::DirBuilder::new();
    configure_private_directory_create(&mut builder);
    match builder.create(path) {
        Ok(()) => set_private_directory_permissions(path)?,
        Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(error) => return Err(error).context("failed to create launcher runtime directory"),
    }
    validate_private_directory(path)
}

fn validate_protocol_path(request_path: &Path) -> Result<PathBuf> {
    if !request_path.is_absolute() || request_path.file_name() != Some(REQUEST_FILE.as_ref()) {
        bail!("private launcher request path is invalid");
    }
    let directory = request_path
        .parent()
        .context("private launcher request has no parent directory")?;
    let name = directory
        .file_name()
        .and_then(|name| name.to_str())
        .context("private launcher request directory name is invalid")?;
    if !name.starts_with(DIRECTORY_PREFIX) {
        bail!("private launcher request directory is not owned by this protocol");
    }
    validate_private_directory(directory)?;
    let canonical = fs::canonicalize(directory)
        .context("failed to resolve private launcher request directory")?;
    if canonical != directory {
        bail!("private launcher request directory must not use symlinks");
    }
    Ok(canonical)
}

fn consume_request(path: &Path) -> Result<LaunchRequest> {
    let result = (|| {
        let mut file = open_private_file(path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        serde_json::from_slice(&bytes).context("failed to decode private launcher request")
    })();
    finish_request_claim(path, result)
}

// Request removal is the atomic claim. If deadline cancellation removes the
// file first, ingress must reject even a request it already opened and decoded.
fn finish_request_claim(path: &Path, result: Result<LaunchRequest>) -> Result<LaunchRequest> {
    let removal = fs::remove_file(path).context("failed to consume private launcher request");
    match (result, removal) {
        (Ok(request), Ok(())) => Ok(request),
        (Err(error), _) | (_, Err(error)) => Err(error),
    }
}

// Compete with ingress's required request removal at the claim deadline. A
// successful cancellation prevents late spawn; NotFound means ingress won.
fn cancel_unclaimed_request(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("failed to cancel private launcher request"),
    }
}

fn write_acknowledgment(directory: &Path, result: SpawnResult) -> Result<()> {
    write_json_atomically(
        directory,
        ACK_TEMP_FILE,
        ACK_FILE,
        &LaunchAcknowledgment {
            version: PROTOCOL_VERSION,
            result,
        },
    )
}

fn read_acknowledgment(path: &Path) -> Result<Option<LaunchAcknowledgment>> {
    let mut file = match open_private_file(path) {
        Ok(file) => file,
        Err(error)
            if error
                .downcast_ref::<std::io::Error>()
                .is_some_and(|error| error.kind() == std::io::ErrorKind::NotFound) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error),
    };
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)?;
    let acknowledgment = serde_json::from_slice(&bytes)?;
    fs::remove_file(path).context("failed to consume launcher acknowledgment")?;
    Ok(Some(acknowledgment))
}

fn write_json_atomically(
    directory: &Path,
    temporary_name: &str,
    final_name: &str,
    value: &impl Serialize,
) -> Result<()> {
    let temporary_path = directory.join(temporary_name);
    let final_path = directory.join(final_name);
    let bytes = serde_json::to_vec(value)?;
    let result = (|| {
        let mut file = create_private_file(&temporary_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        fs::rename(&temporary_path, &final_path)?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary_path);
    }
    result
}

fn create_private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    configure_private_create(&mut options);
    let file = options.open(path)?;
    set_private_file_permissions(path)?;
    validate_private_file(&file)?;
    Ok(file)
}

fn open_private_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.read(true);
    configure_no_follow(&mut options);
    let file = options.open(path)?;
    validate_private_file(&file)?;
    Ok(file)
}

#[cfg(unix)]
fn configure_private_create(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options
        .mode(0o600)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
}

#[cfg(not(unix))]
fn configure_private_create(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn configure_no_follow(options: &mut OpenOptions) {
    use std::os::unix::fs::OpenOptionsExt;

    options.custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW);
}

#[cfg(not(unix))]
fn configure_no_follow(_options: &mut OpenOptions) {}

#[cfg(unix)]
fn configure_private_directory_create(builder: &mut fs::DirBuilder) {
    use std::os::unix::fs::DirBuilderExt;

    builder.mode(0o700);
}

#[cfg(not(unix))]
fn configure_private_directory_create(_builder: &mut fs::DirBuilder) {}

#[cfg(unix)]
fn set_private_directory_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_directory_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn set_private_file_permissions(path: &Path) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))?;
    Ok(())
}

#[cfg(not(unix))]
fn set_private_file_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

#[cfg(unix)]
fn validate_private_directory(path: &Path) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("launcher request directory must be a real directory");
    }
    if metadata.mode() & 0o777 != 0o700 || metadata.uid() != effective_user_id() {
        bail!("launcher request directory must be owned by the current user with mode 0700");
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_directory(path: &Path) -> Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        bail!("launcher request directory must be a real directory");
    }
    Ok(())
}

#[cfg(unix)]
fn validate_private_file(file: &File) -> Result<()> {
    use std::os::unix::fs::MetadataExt;

    let metadata = file.metadata()?;
    if !metadata.is_file()
        || metadata.mode() & 0o777 != 0o600
        || metadata.uid() != effective_user_id()
    {
        bail!("launcher protocol file must be owned by the current user with mode 0600");
    }
    Ok(())
}

#[cfg(not(unix))]
fn validate_private_file(file: &File) -> Result<()> {
    if !file.metadata()?.is_file() {
        bail!("launcher protocol path must be a file");
    }
    Ok(())
}

#[cfg(unix)]
fn effective_user_id() -> u32 {
    // SAFETY: `geteuid` has no preconditions and does not dereference memory.
    unsafe { libc::geteuid() }
}

fn prune_stale_directories(base: &Path, stale_after: Duration) {
    let Ok(entries) = fs::read_dir(base) else {
        return;
    };
    for entry in entries.flatten() {
        let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
            continue;
        };
        if !name.starts_with(DIRECTORY_PREFIX) {
            continue;
        }
        let path = entry.path();
        if validate_private_directory(&path).is_err()
            || !directory_is_stale(&path, stale_after)
            || !directory_contains_only_protocol_files(&path)
        {
            continue;
        }
        let _ = fs::remove_dir_all(path);
    }
}

fn directory_is_stale(path: &Path, stale_after: Duration) -> bool {
    fs::symlink_metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| SystemTime::now().duration_since(modified).ok())
        .is_some_and(|age| age >= stale_after)
}

fn directory_contains_only_protocol_files(path: &Path) -> bool {
    let Ok(entries) = fs::read_dir(path) else {
        return false;
    };
    entries.flatten().all(|entry| {
        entry.file_name().to_str().is_some_and(|name| {
            matches!(
                name,
                REQUEST_FILE | REQUEST_TEMP_FILE | ACK_FILE | ACK_TEMP_FILE
            )
        })
    })
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn resolved(
        executable: impl Into<String>,
        args: &[&str],
        input: Option<&str>,
    ) -> ResolvedLauncher {
        ResolvedLauncher {
            name: "example-launcher".to_owned(),
            executable: executable.into(),
            static_args: args.iter().map(|argument| (*argument).to_owned()).collect(),
            input: input.map(str::to_owned),
        }
    }

    fn create_pending(launcher: &ResolvedLauncher, cwd: &Path) -> Result<PendingLaunch> {
        PendingLaunch::create_under(launcher, cwd, &cwd.join("launcher-state"))
    }

    #[cfg(unix)]
    #[test]
    fn request_round_trip_preserves_exact_argv_and_cleans_up() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let output = cwd.path().join("argv");
        let output_arg = output.display().to_string();
        let input = " spaces ' quotes \" Unicode λ\n--leading ;$() * > sentinel ";
        let launcher = resolved(
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
        let request_path = pending.request_path.clone();
        let directory = request_path.parent().expect("request parent").to_path_buf();
        let ingress_path = request_path;
        let ingress = thread::spawn(move || run_ingress_for_test(&ingress_path));

        pending.wait_for_spawn()?;
        assert_eq!(ingress.join().expect("ingress thread")?, 0);
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
    #[test]
    fn absent_and_empty_input_remain_distinct() -> Result<()> {
        for (input, expected_count) in [(None, "0"), (Some(""), "1")] {
            let cwd = tempfile::tempdir()?;
            let output = cwd.path().join("count");
            let script = format!(
                "printf '%s' \"$#\" > {}",
                shell_quote(&output.display().to_string())
            );
            let launcher = resolved("/bin/sh", &["-c", &script, "launcher"], input);
            let pending = create_pending(&launcher, cwd.path())?;
            let ingress_path = pending.request_path.clone();
            let ingress = thread::spawn(move || run_ingress_for_test(&ingress_path));

            pending.wait_for_spawn()?;
            assert_eq!(ingress.join().expect("ingress thread")?, 0);
            assert_eq!(fs::read_to_string(output)?, expected_count);
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn request_paths_are_private_atomic_and_unique() -> Result<()> {
        use std::os::unix::fs::MetadataExt;

        let cwd = tempfile::tempdir()?;
        let launcher = resolved("/bin/true", &[], None);
        let first = create_pending(&launcher, cwd.path())?;
        let second = create_pending(&launcher, cwd.path())?;
        let runtime = cwd.path().join("launcher-state");

        assert_ne!(first.request_path, second.request_path);
        for pending in [&first, &second] {
            let directory = pending.request_path.parent().expect("request parent");
            assert_eq!(directory.parent(), Some(runtime.as_path()));
            assert_eq!(fs::metadata(&runtime)?.mode() & 0o777, 0o700);
            assert_eq!(fs::metadata(directory)?.mode() & 0o777, 0o700);
            assert_eq!(fs::metadata(&pending.request_path)?.mode() & 0o777, 0o600);
            assert!(!directory.join(REQUEST_TEMP_FILE).exists());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn request_base_rejects_symlinks_and_non_private_permissions() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let cwd = tempfile::tempdir()?;
        let launcher = resolved("/bin/true", &[], None);
        let public_base = cwd.path().join("public-base");
        fs::create_dir(&public_base)?;
        fs::set_permissions(&public_base, fs::Permissions::from_mode(0o755))?;

        assert!(PendingLaunch::create_under(&launcher, cwd.path(), &public_base).is_err());

        let private_target = cwd.path().join("private-target");
        fs::create_dir(&private_target)?;
        fs::set_permissions(&private_target, fs::Permissions::from_mode(0o700))?;
        let symlink_base = cwd.path().join("symlink-base");
        symlink(&private_target, &symlink_base)?;

        assert!(PendingLaunch::create_under(&launcher, cwd.path(), &symlink_base).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn concurrent_requests_do_not_collide_or_cross_acknowledgments() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let launcher = resolved("/bin/sh", &["-c", "exit 0"], None);
        let pending = (0..8)
            .map(|_| create_pending(&launcher, cwd.path()))
            .collect::<Result<Vec<_>>>()?;
        let ingress = pending
            .iter()
            .map(|pending| {
                let path = pending.request_path.clone();
                thread::spawn(move || run_ingress_for_test(&path))
            })
            .collect::<Vec<_>>();

        for launch in pending {
            launch.wait_for_spawn()?;
        }
        for ingress in ingress {
            assert_eq!(ingress.join().expect("ingress thread")?, 0);
        }
        Ok(())
    }

    #[test]
    fn ingress_command_contains_only_controlled_capability_data() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let sentinel = "opaque-input-sentinel";
        let pending = create_pending(
            &resolved("example-command", &["static-sentinel"], Some(sentinel)),
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
    fn spawn_failure_acknowledgment_and_diagnostics_are_sanitized() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let command_sentinel = "missing-command-sentinel";
        let input_sentinel = "opaque-input-sentinel";
        let pending = create_pending(
            &resolved(command_sentinel, &[], Some(input_sentinel)),
            cwd.path(),
        )?;
        let ingress_path = pending.request_path.clone();
        let ingress = thread::spawn(move || run_ingress_for_test(&ingress_path));

        let parent_error = pending
            .wait_for_spawn()
            .expect_err("spawn failure should be acknowledged")
            .to_string();
        let ingress_error = ingress
            .join()
            .expect("ingress thread")
            .expect_err("ingress should fail")
            .to_string();
        for message in [parent_error, ingress_error] {
            assert!(!message.contains(command_sentinel));
            assert!(!message.contains(input_sentinel));
        }
        Ok(())
    }

    #[test]
    fn malformed_version_is_consumed_and_acknowledged_as_failure() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let launcher = resolved("example-command", &[], None);
        let pending = create_pending(&launcher, cwd.path())?;
        fs::remove_file(&pending.request_path)?;
        write_json_atomically(
            pending.request_path.parent().expect("request parent"),
            REQUEST_TEMP_FILE,
            REQUEST_FILE,
            &LaunchRequest {
                version: PROTOCOL_VERSION + 1,
                cwd: cwd.path().to_path_buf(),
                executable: "example-command".to_owned(),
                static_args: Vec::new(),
                input: None,
            },
        )?;
        let ingress_path = pending.request_path.clone();
        let ingress = thread::spawn(move || run_ingress_for_test(&ingress_path));

        pending
            .wait_for_spawn()
            .expect_err("malformed request should acknowledge failure");
        ingress
            .join()
            .expect("ingress thread")
            .expect_err("malformed request should fail ingress");
        Ok(())
    }

    #[test]
    fn ingress_claim_timeout_drops_all_transient_files() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let pending = create_pending(&resolved("example-command", &[], None), cwd.path())?;
        let directory = pending
            .request_path
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
        let pending = create_pending(&resolved("example-command", &[], None), cwd.path())?;
        let directory = pending
            .request_path
            .parent()
            .expect("request parent")
            .to_path_buf();
        fs::remove_file(&pending.request_path)?;
        let acknowledgment = thread::spawn(move || {
            thread::sleep(Duration::from_millis(50));
            write_acknowledgment(&directory, SpawnResult::Spawned)
        });

        pending.wait_for_spawn_timeouts(Duration::from_millis(10), Duration::from_millis(250))?;
        acknowledgment.join().expect("acknowledgment thread")?;
        Ok(())
    }

    #[test]
    fn claim_timeout_cancellation_prevents_late_request_consumption() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let pending = create_pending(&resolved("example-command", &[], None), cwd.path())?;
        let mut file = open_private_file(&pending.request_path)?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let decoded = serde_json::from_slice(&bytes)
            .context("test request should decode before cancellation")?;

        assert!(cancel_unclaimed_request(&pending.request_path)?);
        let error = finish_request_claim(&pending.request_path, Ok(decoded))
            .expect_err("ingress must lose after deadline cancellation removes the request");

        assert!(
            error
                .to_string()
                .contains("failed to consume private launcher request")
        );
        Ok(())
    }

    #[test]
    fn spawn_acknowledgment_timeout_starts_after_request_consumption() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let pending = create_pending(&resolved("example-command", &[], None), cwd.path())?;
        fs::remove_file(&pending.request_path)?;

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

    #[cfg(unix)]
    #[test]
    fn acknowledgment_delivery_failure_keeps_waiting_and_leaves_spawn_state_unknown() -> Result<()>
    {
        let cwd = tempfile::tempdir()?;
        let marker = cwd.path().join("launcher-ran");
        let script = format!("touch {}", shell_quote(&marker.display().to_string()));
        let pending = create_pending(&resolved("/bin/sh", &["-c", &script], None), cwd.path())?;
        fs::create_dir(
            pending
                .request_path
                .parent()
                .expect("request parent")
                .join(ACK_TEMP_FILE),
        )?;
        let ingress_path = pending.request_path.clone();
        let ingress = thread::spawn(move || run_ingress_for_test(&ingress_path));

        let error = pending
            .wait_for_spawn_timeouts(Duration::from_millis(200), Duration::from_millis(50))
            .expect_err("missing acknowledgment should time out");
        assert!(
            error
                .to_string()
                .contains("waiting for launcher process spawn acknowledgment")
        );
        let ingress_error = ingress
            .join()
            .expect("ingress thread")
            .expect_err("acknowledgment delivery should fail after child reaping");
        assert!(
            ingress_error
                .to_string()
                .contains("failed to deliver launcher process spawn acknowledgment")
        );
        assert!(marker.exists());
        Ok(())
    }

    #[test]
    fn malformed_acknowledgment_is_rejected_and_cleaned_up() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let pending = create_pending(&resolved("example-command", &[], None), cwd.path())?;
        let directory = pending
            .request_path
            .parent()
            .expect("request parent")
            .to_path_buf();
        let mut acknowledgment = create_private_file(&pending.ack_path)?;
        acknowledgment.write_all(b"{malformed")?;
        drop(acknowledgment);

        pending
            .wait_for_spawn_timeouts(Duration::from_millis(25), Duration::from_millis(25))
            .expect_err("malformed acknowledgment should fail");
        drop(pending);
        assert!(!directory.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn relative_executable_paths_resolve_from_launcher_cwd() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let cwd = tempfile::tempdir()?;
        let marker = cwd.path().join("relative-ran");
        let executable = cwd.path().join("relative-launcher");
        fs::write(&executable, "#!/bin/sh\ntouch relative-ran\n")?;
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700))?;
        let pending = create_pending(&resolved("./relative-launcher", &[], None), cwd.path())?;
        let ingress_path = pending.request_path.clone();
        let ingress = thread::spawn(move || run_ingress_for_test(&ingress_path));

        pending.wait_for_spawn()?;
        assert_eq!(ingress.join().expect("ingress thread")?, 0);
        assert!(marker.exists());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn ingress_refuses_a_symlinked_request() -> Result<()> {
        use std::os::unix::fs::symlink;

        let cwd = tempfile::tempdir()?;
        let external = cwd.path().join("external-request");
        fs::write(&external, "not a request")?;
        let pending = create_pending(&resolved("/bin/true", &[], None), cwd.path())?;
        fs::remove_file(&pending.request_path)?;
        symlink(&external, &pending.request_path)?;

        run_ingress_for_test(&pending.request_path).expect_err("symlinked request must fail");
        assert_eq!(fs::read_to_string(external)?, "not a request");
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn stale_pruning_removes_only_owned_protocol_directories() -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let base = tempfile::tempdir()?;
        let stale = base.path().join(format!("{DIRECTORY_PREFIX}stale"));
        fs::create_dir(&stale)?;
        fs::set_permissions(&stale, fs::Permissions::from_mode(0o700))?;
        fs::write(stale.join(REQUEST_FILE), "stale")?;
        let foreign = base.path().join(format!("{DIRECTORY_PREFIX}foreign"));
        fs::create_dir(&foreign)?;
        fs::set_permissions(&foreign, fs::Permissions::from_mode(0o700))?;
        fs::write(foreign.join("unrelated"), "keep")?;

        prune_stale_directories(base.path(), Duration::ZERO);

        assert!(!stale.exists());
        assert!(foreign.exists());
        Ok(())
    }
}
