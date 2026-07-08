//! Opt-in local telemetry for kmux hot paths.
//!
//! Set `KMUX_TELEMETRY=1` to append JSONL tracing events. By default, events
//! are written to the cache directory; set `KMUX_TELEMETRY_PATH` to a file path,
//! or to a directory path to write `telemetry.jsonl` inside it.

use std::ffi::OsString;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Instant;

use directories::BaseDirs;
use tracing_subscriber::EnvFilter;
use tracing_subscriber::fmt::MakeWriter;

const TELEMETRY_FILENAME: &str = "telemetry.jsonl";

/// Initialize opt-in JSONL telemetry. Failures are ignored so telemetry never breaks kmux.
pub fn init() {
    if !telemetry_enabled() {
        return;
    }

    let Some(path) = telemetry_path() else {
        return;
    };
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        && fs::create_dir_all(parent).is_err()
    {
        return;
    }
    let Ok(file) = OpenOptions::new().create(true).append(true).open(&path) else {
        return;
    };
    let Some(filter) = telemetry_filter() else {
        return;
    };

    let writer = TelemetryWriter::new(file);
    let subscriber = tracing_subscriber::fmt()
        .json()
        .flatten_event(true)
        .with_current_span(false)
        .with_span_list(false)
        .with_target(true)
        .with_env_filter(filter)
        .with_writer(writer)
        .finish();

    let _ = tracing::subscriber::set_global_default(subscriber);
}

/// Return elapsed milliseconds as a tracing-friendly integer.
fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().min(u128::from(u64::MAX)) as u64
}

/// Run an operation and return its value with elapsed milliseconds.
pub fn timed<T>(operation: impl FnOnce() -> T) -> (T, u64) {
    let started = Instant::now();
    let value = operation();
    (value, elapsed_ms(started))
}

/// Run a fallible operation and return its result with elapsed milliseconds.
pub fn timed_result<T, E>(operation: impl FnOnce() -> Result<T, E>) -> (Result<T, E>, u64) {
    let started = Instant::now();
    let result = operation();
    (result, elapsed_ms(started))
}

/// Time a fallible operation and emit one summary event with static fields.
macro_rules! timed_result_event {
    ($event:expr, { $($fields:tt)* }, $operation:expr, ok |$value:ident| { $($ok_fields:tt)* } $(,)?) => {{
        let (result, elapsed_ms) = $crate::telemetry::timed_result($operation);
        match &result {
            Ok($value) => tracing::debug!(
                event = $event,
                elapsed_ms,
                ok = true,
                $($fields)*
                $($ok_fields)*
            ),
            Err(error) => tracing::debug!(
                event = $event,
                elapsed_ms,
                ok = false,
                error = %error,
                $($fields)*
            ),
        }
        result
    }};

    ($event:expr, { $($fields:tt)* }, $operation:expr $(,)?) => {{
        let (result, elapsed_ms) = $crate::telemetry::timed_result($operation);
        match &result {
            Ok(_) => tracing::debug!(
                event = $event,
                elapsed_ms,
                ok = true,
                $($fields)*
            ),
            Err(error) => tracing::debug!(
                event = $event,
                elapsed_ms,
                ok = false,
                error = %error,
                $($fields)*
            ),
        }
        result
    }};
}

pub(crate) use timed_result_event;

fn telemetry_path() -> Option<PathBuf> {
    telemetry_path_from_env(
        std::env::var_os("KMUX_TELEMETRY_PATH"),
        default_telemetry_path,
    )
}

fn default_telemetry_path() -> Option<PathBuf> {
    BaseDirs::new().map(|base_dirs| base_dirs.cache_dir().join("kmux").join(TELEMETRY_FILENAME))
}

fn telemetry_path_from_env(
    configured_path: Option<OsString>,
    default_path: impl FnOnce() -> Option<PathBuf>,
) -> Option<PathBuf> {
    configured_path
        .filter(|path| !path.is_empty())
        .map(configured_telemetry_path)
        .or_else(default_path)
}

fn configured_telemetry_path(path: OsString) -> PathBuf {
    let has_trailing_separator = path.to_string_lossy().ends_with(std::path::MAIN_SEPARATOR);
    let mut path = PathBuf::from(path);
    if has_trailing_separator || path.is_dir() {
        path.push(TELEMETRY_FILENAME);
    }
    path
}

fn telemetry_enabled() -> bool {
    std::env::var("KMUX_TELEMETRY").is_ok_and(|value| {
        matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "on"
        )
    })
}

fn telemetry_filter() -> Option<EnvFilter> {
    EnvFilter::try_from_env("KMUX_TELEMETRY_FILTER")
        .or_else(|_| EnvFilter::try_new("kmux=debug"))
        .ok()
}

#[derive(Clone)]
struct TelemetryWriter {
    file: Arc<Mutex<File>>,
}

impl TelemetryWriter {
    fn new(file: File) -> Self {
        Self {
            file: Arc::new(Mutex::new(file)),
        }
    }
}

impl<'writer> MakeWriter<'writer> for TelemetryWriter {
    type Writer = TelemetryWriteGuard<'writer>;

    fn make_writer(&'writer self) -> Self::Writer {
        let guard = match self.file.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        TelemetryWriteGuard { guard }
    }
}

struct TelemetryWriteGuard<'writer> {
    guard: MutexGuard<'writer, File>,
}

impl Write for TelemetryWriteGuard<'_> {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.guard.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.guard.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn telemetry_path_uses_configured_path() {
        let path = telemetry_path_from_env(Some(OsString::from("/tmp/custom.jsonl")), || {
            Some(PathBuf::from("/tmp/default.jsonl"))
        });

        assert_eq!(path, Some(PathBuf::from("/tmp/custom.jsonl")));
    }

    #[test]
    fn telemetry_path_uses_filename_inside_trailing_separator_path() {
        let configured_path = OsString::from(format!("/tmp/custom{}", std::path::MAIN_SEPARATOR));

        let path = telemetry_path_from_env(Some(configured_path), || {
            Some(PathBuf::from("/tmp/default.jsonl"))
        });

        assert_eq!(
            path,
            Some(PathBuf::from("/tmp/custom").join(TELEMETRY_FILENAME))
        );
    }

    #[test]
    fn telemetry_path_uses_filename_inside_existing_directory() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;

        let path = telemetry_path_from_env(Some(directory.path().as_os_str().to_owned()), || {
            Some(PathBuf::from("/tmp/default.jsonl"))
        });

        assert_eq!(path, Some(directory.path().join(TELEMETRY_FILENAME)));
        Ok(())
    }

    #[test]
    fn telemetry_path_uses_default_path_without_configured_path() {
        let path = telemetry_path_from_env(None, || Some(PathBuf::from("/tmp/default.jsonl")));

        assert_eq!(path, Some(PathBuf::from("/tmp/default.jsonl")));
    }

    #[test]
    fn telemetry_path_ignores_empty_configured_path() {
        let path = telemetry_path_from_env(Some(OsString::new()), || {
            Some(PathBuf::from("/tmp/default.jsonl"))
        });

        assert_eq!(path, Some(PathBuf::from("/tmp/default.jsonl")));
    }
}
