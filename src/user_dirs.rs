//! User-level config, state, and cache directory discovery.
//!
//! The `directories` crate supplies platform-native defaults, but on Darwin it
//! intentionally does not consult XDG environment variables. Kmux documents
//! those variables as explicit overrides, so resolve them before falling back
//! to the platform directory policy.

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::{Context, Result};
use directories::BaseDirs;

/// Return the user configuration directory, honoring an absolute `XDG_CONFIG_HOME`.
pub fn config_dir() -> Result<PathBuf> {
    if let Some(path) = absolute_env_path(std::env::var_os("XDG_CONFIG_HOME")) {
        return Ok(path);
    }

    let base_dirs = BaseDirs::new().context("could not determine config directory")?;
    Ok(base_dirs.config_dir().to_owned())
}

/// Return the user state directory, honoring an absolute `XDG_STATE_HOME`.
pub fn state_dir() -> Result<PathBuf> {
    if let Some(path) = absolute_env_path(std::env::var_os("XDG_STATE_HOME")) {
        return Ok(path);
    }

    let base_dirs = BaseDirs::new().context("could not determine state directory")?;
    Ok(base_dirs
        .state_dir()
        .unwrap_or_else(|| base_dirs.data_local_dir())
        .to_owned())
}

/// Return the user cache directory, honoring an absolute `XDG_CACHE_HOME`.
pub fn cache_dir() -> Result<PathBuf> {
    if let Some(path) = absolute_env_path(std::env::var_os("XDG_CACHE_HOME")) {
        return Ok(path);
    }

    let base_dirs = BaseDirs::new().context("could not determine cache directory")?;
    Ok(base_dirs.cache_dir().to_owned())
}

// XDG base directories must be absolute. Treat empty or relative values as
// unset so fallback paths remain deterministic and safe to use from any cwd.
fn absolute_env_path(value: Option<OsString>) -> Option<PathBuf> {
    value
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .filter(|path| path.is_absolute())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn absolute_xdg_path_is_used() {
        assert_eq!(
            absolute_env_path(Some(OsString::from("/user/config"))),
            Some(PathBuf::from("/user/config"))
        );
    }

    #[test]
    fn empty_or_relative_xdg_path_is_ignored() {
        assert_eq!(absolute_env_path(Some(OsString::new())), None);
        assert_eq!(
            absolute_env_path(Some(OsString::from("relative/config"))),
            None
        );
    }
}
