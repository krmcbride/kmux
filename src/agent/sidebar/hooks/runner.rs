//! Bounded shell command execution for sidebar hooks.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus, Stdio};
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const MAX_HOOK_OUTPUT_BYTES: u64 = 8 * 1024;
const MAX_ERROR_OUTPUT_CHARS: usize = 800;
static HOOK_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Shell command invocation details for one matching hook.
pub(super) struct HookCommand<'a> {
    pub(super) command: &'a str,
    pub(super) stdin: &'a [u8],
    pub(super) timeout: Duration,
    pub(super) env: Vec<(OsString, OsString)>,
    pub(super) cwd: Option<PathBuf>,
}

#[derive(Debug)]
/// Captured result from a spawned hook command.
pub(super) struct HookCommandOutcome {
    pub(super) duration: Duration,
    pub(super) stdout: String,
    pub(super) stderr: String,
    status: HookCommandStatus,
    timeout: Duration,
}

#[derive(Debug, Clone, Copy)]
enum HookCommandStatus {
    Exited(ExitStatus),
    TimedOut,
}

#[derive(Debug)]
struct HookOutputFiles {
    stdout_file: File,
    stdout_path: PathBuf,
    stderr_file: File,
    stderr_path: PathBuf,
}

#[derive(Debug)]
pub(super) struct HookCommandError {
    status_label: &'static str,
    error: anyhow::Error,
}

#[derive(Debug)]
struct TemporaryPath {
    path: Option<PathBuf>,
}

/// Run one hook command, returning bounded stdout/stderr and completion status.
pub(super) fn run(
    command: HookCommand<'_>,
) -> std::result::Result<HookCommandOutcome, HookCommandError> {
    let (stdin_file, stdin_path) =
        payload_stdin_file(command.stdin).map_err(HookCommandError::runner)?;
    let mut stdin_path = TemporaryPath::new(stdin_path);
    let mut output_files = hook_output_files().map_err(HookCommandError::runner)?;
    let mut process = Command::new("sh");
    process
        .arg("-c")
        .arg(command.command)
        .stdin(Stdio::from(stdin_file))
        .stdout(Stdio::from(output_files.stdout_file.try_clone().map_err(
            |error| HookCommandError::runner(anyhow::Error::new(error)),
        )?))
        .stderr(Stdio::from(output_files.stderr_file.try_clone().map_err(
            |error| HookCommandError::runner(anyhow::Error::new(error)),
        )?));
    for (key, value) in command.env {
        process.env(key, value);
    }
    if let Some(current_dir) = &command.cwd {
        process.current_dir(current_dir);
    }

    let started = Instant::now();
    let mut child = match process.spawn() {
        Ok(child) => child,
        Err(error) => return Err(HookCommandError::spawn(command.command, error)),
    };
    stdin_path.remove_now();

    let status =
        wait_with_timeout(&mut child, command.timeout).map_err(HookCommandError::runner)?;
    let duration = started.elapsed();
    let stdout = output_files
        .stdout_tail()
        .map_err(HookCommandError::runner)?;
    let stderr = output_files
        .stderr_tail()
        .map_err(HookCommandError::runner)?;

    Ok(HookCommandOutcome {
        duration,
        stdout,
        stderr,
        status,
        timeout: command.timeout,
    })
}

impl HookCommandOutcome {
    pub(super) fn status_label(&self) -> &'static str {
        match self.status {
            HookCommandStatus::Exited(status) if status.success() => "ok",
            HookCommandStatus::Exited(_) => "failed",
            HookCommandStatus::TimedOut => "timed_out",
        }
    }

    pub(super) fn exit_status(&self) -> Option<String> {
        match self.status {
            HookCommandStatus::Exited(status) => Some(status.to_string()),
            HookCommandStatus::TimedOut => None,
        }
    }

    pub(super) fn failure_message(&self) -> Option<String> {
        match self.status {
            HookCommandStatus::Exited(status) if status.success() => None,
            HookCommandStatus::Exited(status) => Some(with_output_summary(
                format!("exited with status {status}"),
                &self.stdout,
                &self.stderr,
            )),
            HookCommandStatus::TimedOut => Some(with_output_summary(
                format!("timed out after {}ms", self.timeout.as_millis()),
                &self.stdout,
                &self.stderr,
            )),
        }
    }
}

impl HookCommandError {
    fn runner(error: anyhow::Error) -> Self {
        Self {
            status_label: "runner_failed",
            error,
        }
    }

    fn spawn(command: &str, error: std::io::Error) -> Self {
        Self {
            status_label: "spawn_failed",
            error: anyhow::Error::new(error)
                .context(format!("failed to start hook command: {command}")),
        }
    }

    pub(super) fn status_label(&self) -> &'static str {
        self.status_label
    }

    pub(super) fn message(&self) -> String {
        format!("{:#}", self.error)
    }

    pub(super) fn into_error(self) -> anyhow::Error {
        self.error
    }
}

impl TemporaryPath {
    fn new(path: PathBuf) -> Self {
        Self { path: Some(path) }
    }

    fn remove_now(&mut self) {
        if let Some(path) = self.path.take() {
            let _ = fs::remove_file(path);
        }
    }
}

impl Drop for TemporaryPath {
    fn drop(&mut self) {
        self.remove_now();
    }
}

fn payload_stdin_file(payload_json: &[u8]) -> Result<(File, PathBuf)> {
    for _ in 0..16 {
        let path = temporary_hook_path("stdin", "json");

        match open_temporary_file(&path) {
            Ok(mut file) => {
                if let Err(error) = write_payload_file(&mut file, payload_json) {
                    let _ = fs::remove_file(&path);
                    return Err(error);
                }
                return Ok((file, path));
            }
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create selection hook stdin file at {}",
                        path.display()
                    )
                });
            }
        }
    }

    bail!("failed to create unique selection hook stdin file")
}

fn hook_output_files() -> Result<HookOutputFiles> {
    let (stdout_file, stdout_path) = temporary_output_file("stdout")?;
    let (stderr_file, stderr_path) = match temporary_output_file("stderr") {
        Ok(file) => file,
        Err(error) => {
            let _ = fs::remove_file(&stdout_path);
            return Err(error);
        }
    };
    Ok(HookOutputFiles {
        stdout_file,
        stdout_path,
        stderr_file,
        stderr_path,
    })
}

fn temporary_output_file(kind: &str) -> Result<(File, PathBuf)> {
    for _ in 0..16 {
        let path = temporary_hook_path(kind, "log");

        match open_temporary_file(&path) {
            Ok(file) => return Ok((file, path)),
            Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "failed to create selection hook {kind} file at {}",
                        path.display()
                    )
                });
            }
        }
    }

    bail!("failed to create unique selection hook {kind} file")
}

fn temporary_hook_path(kind: &str, extension: &str) -> PathBuf {
    let counter = HOOK_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "kmux-sidebar-hook-{kind}-{}-{counter}.{extension}",
        std::process::id()
    ))
}

fn open_temporary_file(path: &Path) -> std::io::Result<File> {
    let mut options = OpenOptions::new();
    options.read(true).write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)
}

fn write_payload_file(file: &mut File, payload_json: &[u8]) -> Result<()> {
    match file.write_all(payload_json) {
        Ok(()) => {
            file.seek(SeekFrom::Start(0))?;
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error).context("failed to write selection hook payload"),
    }
}

fn wait_with_timeout(child: &mut Child, timeout: Duration) -> Result<HookCommandStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(HookCommandStatus::Exited(status));
        }
        if started.elapsed() >= timeout {
            let _ = child.kill();
            let _ = child.wait();
            return Ok(HookCommandStatus::TimedOut);
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn with_output_summary(message: String, stdout: &str, stderr: &str) -> String {
    let stderr = stderr.trim();
    if !stderr.is_empty() {
        return format!("{message}: stderr: {}", error_output_summary(stderr));
    }
    let stdout = stdout.trim();
    if !stdout.is_empty() {
        return format!("{message}: stdout: {}", error_output_summary(stdout));
    }
    message
}

fn error_output_summary(output: &str) -> String {
    let mut summary = String::new();
    for (index, character) in output.chars().enumerate() {
        if index == MAX_ERROR_OUTPUT_CHARS {
            summary.push_str("...");
            break;
        }
        summary.push(character);
    }
    summary
}

impl HookOutputFiles {
    fn stdout_tail(&mut self) -> Result<String> {
        read_file_tail(&mut self.stdout_file)
    }

    fn stderr_tail(&mut self) -> Result<String> {
        read_file_tail(&mut self.stderr_file)
    }
}

impl Drop for HookOutputFiles {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.stdout_path);
        let _ = fs::remove_file(&self.stderr_path);
    }
}

fn read_file_tail(file: &mut File) -> Result<String> {
    let len = file.metadata()?.len();
    let start = len.saturating_sub(MAX_HOOK_OUTPUT_BYTES);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::new();
    file.take(MAX_HOOK_OUTPUT_BYTES).read_to_end(&mut bytes)?;
    let output = String::from_utf8_lossy(&bytes);
    if start > 0 {
        Ok(format!("[truncated]\n{output}"))
    } else {
        Ok(output.into_owned())
    }
}
