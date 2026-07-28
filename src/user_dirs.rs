//! User-level config, state, and cache directory discovery.
//!
//! Unix platforms use the XDG defaults under the user's home directory. Other
//! platforms retain the native defaults supplied by the `directories` crate.
//! Absolute XDG environment variables override either policy.

use std::ffi::{OsStr, OsString};
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use directories::BaseDirs;

const UNIX_CONFIG_HOME: &str = ".config";
const UNIX_STATE_HOME: &str = ".local/state";
const UNIX_CACHE_HOME: &str = ".cache";

/// Return the user configuration directory.
///
/// An absolute `XDG_CONFIG_HOME` wins. Otherwise Unix uses `$HOME/.config`,
/// while non-Unix platforms retain their native configuration directory.
pub fn config_dir() -> Result<PathBuf> {
    let override_value = std::env::var_os("XDG_CONFIG_HOME");
    if let Some(path) = absolute_env_path(override_value.as_deref()) {
        return Ok(path);
    }

    let base_dirs = BaseDirs::new().context("could not determine config directory")?;
    Ok(resolve_user_dir(
        override_value,
        base_dirs.home_dir(),
        Path::new(UNIX_CONFIG_HOME),
        base_dirs.config_dir(),
    ))
}

/// Return the user state directory.
///
/// An absolute `XDG_STATE_HOME` wins. Otherwise Unix uses
/// `$HOME/.local/state`, while non-Unix platforms retain their native state or
/// local-data directory.
pub fn state_dir() -> Result<PathBuf> {
    let override_value = std::env::var_os("XDG_STATE_HOME");
    if let Some(path) = absolute_env_path(override_value.as_deref()) {
        return Ok(path);
    }

    let base_dirs = BaseDirs::new().context("could not determine state directory")?;
    let platform_default = base_dirs
        .state_dir()
        .unwrap_or_else(|| base_dirs.data_local_dir())
        .to_owned();
    Ok(resolve_user_dir(
        override_value,
        base_dirs.home_dir(),
        Path::new(UNIX_STATE_HOME),
        &platform_default,
    ))
}

/// Return the user cache directory.
///
/// An absolute `XDG_CACHE_HOME` wins. Otherwise Unix uses `$HOME/.cache`,
/// while non-Unix platforms retain their native cache directory.
pub fn cache_dir() -> Result<PathBuf> {
    let override_value = std::env::var_os("XDG_CACHE_HOME");
    if let Some(path) = absolute_env_path(override_value.as_deref()) {
        return Ok(path);
    }

    let base_dirs = BaseDirs::new().context("could not determine cache directory")?;
    Ok(resolve_user_dir(
        override_value,
        base_dirs.home_dir(),
        Path::new(UNIX_CACHE_HOME),
        base_dirs.cache_dir(),
    ))
}

fn resolve_user_dir(
    override_value: Option<OsString>,
    home_dir: &Path,
    unix_relative_default: &Path,
    platform_default: &Path,
) -> PathBuf {
    absolute_env_path(override_value.as_deref()).unwrap_or_else(|| {
        if cfg!(unix) {
            home_dir.join(unix_relative_default)
        } else {
            platform_default.to_owned()
        }
    })
}

// XDG base directories must be absolute. Treat empty or relative values as
// unset so fallback paths remain deterministic and safe to use from any cwd.
fn absolute_env_path(value: Option<&OsStr>) -> Option<PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_xdg_override_wins() {
        let override_path = if cfg!(windows) {
            PathBuf::from(r"C:\override\config")
        } else {
            PathBuf::from("/override/config")
        };
        assert_eq!(
            resolve_user_dir(
                Some(override_path.clone().into_os_string()),
                Path::new("/home/example"),
                Path::new(UNIX_CONFIG_HOME),
                Path::new("/native/config"),
            ),
            override_path
        );
    }

    #[test]
    fn unset_empty_and_relative_overrides_use_the_fallback() {
        let expected = if cfg!(unix) {
            PathBuf::from("/home/example/.config")
        } else {
            PathBuf::from("/native/config")
        };

        for override_value in [
            None,
            Some(OsString::new()),
            Some(OsString::from("relative/config")),
        ] {
            assert_eq!(
                resolve_user_dir(
                    override_value,
                    Path::new("/home/example"),
                    Path::new(UNIX_CONFIG_HOME),
                    Path::new("/native/config"),
                ),
                expected
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_defaults_are_xdg_directories_under_home() {
        assert_eq!(
            resolve_user_dir(
                None,
                Path::new("/home/example"),
                Path::new(UNIX_CONFIG_HOME),
                Path::new("/native/config"),
            ),
            PathBuf::from("/home/example/.config")
        );
        assert_eq!(
            resolve_user_dir(
                None,
                Path::new("/home/example"),
                Path::new(UNIX_STATE_HOME),
                Path::new("/native/state"),
            ),
            PathBuf::from("/home/example/.local/state")
        );
        assert_eq!(
            resolve_user_dir(
                None,
                Path::new("/home/example"),
                Path::new(UNIX_CACHE_HOME),
                Path::new("/native/cache"),
            ),
            PathBuf::from("/home/example/.cache")
        );
    }
}
