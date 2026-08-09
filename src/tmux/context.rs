//! Current pane discovery and exact tmux target syntax.

use anyhow::{Result, bail};

use super::process::{Tmux, bail_tmux, tmux_server_is_absent};

#[derive(Debug, Clone, PartialEq, Eq)]
/// Current tmux session, window, and pane identity for command workflows.
pub struct TmuxContext {
    pub session_name: String,
    pub session_id: String,
    pub window_name: String,
    pub window_id: String,
    pub pane_id: String,
}

impl Tmux {
    /// Return context for the current pane when running inside tmux, otherwise `None`.
    pub fn current_context(&self) -> Result<Option<TmuxContext>> {
        let pane_id = std::env::var("TMUX_PANE").ok();
        if let Some(pane_id) = pane_id {
            return self.pane_context(&pane_id).map(Some);
        }

        if std::env::var_os("TMUX").is_none() {
            return Ok(None);
        }

        self.query_pane_context(None).map(Some)
    }

    /// Return current pane context for lifecycle targeting, tolerating stale pane state.
    ///
    /// `TMUX_PANE` is the only reliable evidence that this process belongs to a
    /// client pane. A stale or missing pane/server is treated as detached, while
    /// permission failures and malformed successful output remain errors.
    pub fn current_context_for_session_resolution(&self) -> Result<Option<TmuxContext>> {
        let Some(pane_id) = std::env::var("TMUX_PANE")
            .ok()
            .filter(|pane_id| !pane_id.is_empty())
        else {
            return Ok(None);
        };
        if !is_tmux_pane_id(&pane_id) {
            return Ok(None);
        }
        let output = self.output(["display-message", "-p", "-t", &pane_id, TMUX_CONTEXT_FORMAT])?;
        if !output.status.success() {
            if tmux_server_is_absent(&output.stderr) || tmux_pane_is_absent(&output.stderr) {
                return Ok(None);
            }
            return bail_tmux(output);
        }
        // Some tmux versions return a successful record of empty fields for a
        // stale pane id instead of reporting a target error.
        if output
            .stdout
            .trim_matches(|ch| matches!(ch, '\t' | '\r' | '\n'))
            .is_empty()
        {
            return Ok(None);
        }
        parse_context(&output.stdout).map(Some)
    }

    /// Return session/window/pane context for a specific pane id.
    pub(super) fn pane_context(&self, pane_id: &str) -> Result<TmuxContext> {
        self.query_pane_context(Some(pane_id))
    }

    // Use tmux format expansion so callers get IDs from tmux itself rather than
    // reconstructing context from environment variables. Keep the format and
    // parsing together so tmux format changes fail near the adapter boundary.
    fn query_pane_context(&self, target_pane: Option<&str>) -> Result<TmuxContext> {
        let output = if let Some(target_pane) = target_pane {
            self.stdout([
                "display-message",
                "-p",
                "-t",
                target_pane,
                TMUX_CONTEXT_FORMAT,
            ])?
        } else {
            self.stdout(["display-message", "-p", TMUX_CONTEXT_FORMAT])?
        };
        parse_context(&output)
    }
}

pub(super) fn exact_session_target(session_target: &str) -> String {
    format!("={session_target}")
}

pub(super) fn validate_session_id(session_id: &str) -> Result<()> {
    if !session_id
        .strip_prefix('$')
        .is_some_and(|value| !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit()))
    {
        bail!("invalid tmux session id {session_id:?}");
    }
    Ok(())
}

pub(super) fn validate_window_id(window_id: &str) -> Result<()> {
    if !window_id
        .strip_prefix('@')
        .is_some_and(|value| !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit()))
    {
        bail!("invalid tmux window id {window_id:?}");
    }
    Ok(())
}

pub(super) fn is_tmux_pane_id(pane_id: &str) -> bool {
    pane_id
        .strip_prefix('%')
        .is_some_and(|value| !value.is_empty() && value.chars().all(|ch| ch.is_ascii_digit()))
}

const TMUX_CONTEXT_FORMAT: &str =
    "#{session_name}\t#{session_id}\t#{window_name}\t#{window_id}\t#{pane_id}";

fn parse_context(output: &str) -> Result<TmuxContext> {
    let fields = output.trim_end().split('\t').collect::<Vec<_>>();
    if fields.len() != 5 {
        bail!("unexpected tmux context format: {output:?}");
    }
    validate_session_id(fields[1])?;

    Ok(TmuxContext {
        session_name: fields[0].to_owned(),
        session_id: fields[1].to_owned(),
        window_name: fields[2].to_owned(),
        window_id: fields[3].to_owned(),
        pane_id: fields[4].to_owned(),
    })
}

fn tmux_pane_is_absent(stderr: &str) -> bool {
    let stderr = stderr.trim();
    stderr.starts_with("can't find pane:") || stderr.starts_with("no such pane:")
}

#[cfg(test)]
mod tests {
    use anyhow::Result;

    use super::{is_tmux_pane_id, validate_session_id, validate_window_id};

    #[test]
    fn validates_opaque_tmux_ids() -> Result<()> {
        assert!(validate_session_id("$3").is_ok());
        assert!(validate_session_id("$project").is_err());
        assert!(validate_window_id("@42").is_ok());
        assert!(validate_window_id("feature-auth").is_err());
        assert!(is_tmux_pane_id("%42"));
        assert!(!is_tmux_pane_id("project:main"));
        assert!(!is_tmux_pane_id("%pane"));
        Ok(())
    }
}
