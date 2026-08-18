//! Private one-shot filesystem transport shared by launcher caller and ingress.
//!
//! The invoking kmux process and the tmux-hosted ingress may see different
//! namespace-local temporary directories. This protocol therefore uses private
//! user-state storage to transfer exact argv data through an opaque capability
//! path, with atomic request claims and spawn acknowledgments.

use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use tempfile::{Builder, TempDir};

use crate::user_dirs;

pub(super) const PROTOCOL_VERSION: u32 = 1;
pub(super) const DIRECTORY_PREFIX: &str = ".kmux-launch-v1-";
pub(super) const REQUEST_FILE: &str = "request.json";
pub(super) const REQUEST_TEMP_FILE: &str = "request.tmp";
pub(super) const ACK_FILE: &str = "ack.json";
pub(super) const ACK_TEMP_FILE: &str = "ack.tmp";
const LAUNCH_RUNTIME_DIRECTORY: &str = "launcher-runtime";
const STALE_AFTER: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LaunchRequest {
    pub(super) version: u32,
    pub(super) cwd: PathBuf,
    pub(super) executable: String,
    pub(super) static_args: Vec<String>,
    pub(super) input: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct LaunchAcknowledgment {
    pub(super) version: u32,
    pub(super) result: SpawnResult,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub(super) enum SpawnResult {
    Spawned,
    Failed,
}

pub(super) fn validate_request(request: &LaunchRequest) -> Result<()> {
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

pub(super) fn validate_cwd(cwd: &Path) -> Result<()> {
    if !cwd.is_absolute() {
        bail!("launcher working directory must be absolute");
    }
    let metadata = fs::metadata(cwd).context("launcher working directory is unavailable")?;
    if !metadata.is_dir() {
        bail!("launcher working directory is not a directory");
    }
    Ok(())
}

pub(super) fn create_request_directory() -> Result<TempDir> {
    let base = shared_launcher_runtime_directory()?;
    create_request_directory_under(&base)
}

pub(super) fn create_request_directory_under(base: &Path) -> Result<TempDir> {
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
    Ok(user_dirs::state_dir()?
        .join("kmux")
        .join(LAUNCH_RUNTIME_DIRECTORY))
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

pub(super) fn validate_protocol_path(request_path: &Path) -> Result<PathBuf> {
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

pub(super) fn consume_request(path: &Path) -> Result<LaunchRequest> {
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
pub(super) fn cancel_unclaimed_request(path: &Path) -> Result<bool> {
    match fs::remove_file(path) {
        Ok(()) => Ok(true),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error).context("failed to cancel private launcher request"),
    }
}

pub(super) fn write_acknowledgment(directory: &Path, result: SpawnResult) -> Result<()> {
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

pub(super) fn read_acknowledgment(path: &Path) -> Result<Option<LaunchAcknowledgment>> {
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

pub(super) fn write_json_atomically(
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

#[cfg(test)]
mod tests {
    use std::thread;

    use super::*;
    use crate::launcher::ingress::run_ingress_for_test;
    use crate::launcher::{PendingLaunch, ResolvedLauncher};

    fn resolved(executable: impl Into<String>) -> ResolvedLauncher {
        ResolvedLauncher::for_test(executable, &[], None)
    }

    fn create_pending(launcher: &ResolvedLauncher, cwd: &Path) -> Result<PendingLaunch> {
        PendingLaunch::create_under(launcher, cwd, &cwd.join("launcher-state"))
    }

    #[cfg(unix)]
    #[test]
    fn request_paths_are_private_atomic_and_unique() -> Result<()> {
        use std::os::unix::fs::MetadataExt;

        let cwd = tempfile::tempdir()?;
        let launcher = resolved("/bin/true");
        let first = create_pending(&launcher, cwd.path())?;
        let second = create_pending(&launcher, cwd.path())?;
        let runtime = fs::canonicalize(cwd.path().join("launcher-state"))?;

        assert_ne!(first.request_path(), second.request_path());
        for pending in [&first, &second] {
            let directory = pending.request_path().parent().expect("request parent");
            assert_eq!(directory.parent(), Some(runtime.as_path()));
            assert_eq!(fs::metadata(&runtime)?.mode() & 0o777, 0o700);
            assert_eq!(fs::metadata(directory)?.mode() & 0o777, 0o700);
            assert_eq!(fs::metadata(pending.request_path())?.mode() & 0o777, 0o600);
            assert!(!directory.join(REQUEST_TEMP_FILE).exists());
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn request_base_rejects_symlinks_and_non_private_permissions() -> Result<()> {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let cwd = tempfile::tempdir()?;
        let launcher = resolved("/bin/true");
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

    #[test]
    fn malformed_version_is_consumed_and_acknowledged_as_failure() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let launcher = resolved("example-command");
        let pending = create_pending(&launcher, cwd.path())?;
        fs::remove_file(pending.request_path())?;
        write_json_atomically(
            pending.request_path().parent().expect("request parent"),
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
        let ingress_path = pending.request_path().to_path_buf();
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
    fn claim_timeout_cancellation_prevents_late_request_consumption() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let pending = create_pending(&resolved("example-command"), cwd.path())?;
        let mut file = open_private_file(pending.request_path())?;
        let mut bytes = Vec::new();
        file.read_to_end(&mut bytes)?;
        let decoded = serde_json::from_slice(&bytes)
            .context("test request should decode before cancellation")?;

        assert!(cancel_unclaimed_request(pending.request_path())?);
        let error = finish_request_claim(pending.request_path(), Ok(decoded))
            .expect_err("ingress must lose after deadline cancellation removes the request");

        assert!(
            error
                .to_string()
                .contains("failed to consume private launcher request")
        );
        Ok(())
    }

    #[test]
    fn malformed_acknowledgment_is_rejected_and_cleaned_up() -> Result<()> {
        let cwd = tempfile::tempdir()?;
        let pending = create_pending(&resolved("example-command"), cwd.path())?;
        let directory = pending
            .request_path()
            .parent()
            .expect("request parent")
            .to_path_buf();
        let mut acknowledgment = create_private_file(pending.acknowledgment_path())?;
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
    fn ingress_refuses_a_symlinked_request() -> Result<()> {
        use std::os::unix::fs::symlink;

        let cwd = tempfile::tempdir()?;
        let external = cwd.path().join("external-request");
        fs::write(&external, "not a request")?;
        let pending = create_pending(&resolved("/bin/true"), cwd.path())?;
        fs::remove_file(pending.request_path())?;
        symlink(&external, pending.request_path())?;

        run_ingress_for_test(pending.request_path()).expect_err("symlinked request must fail");
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
