//! Selection policy and tmux option encoding for sidebar rows.
//!
//! `SidebarApp` owns tmux side effects and controller flow. This module keeps the
//! pure row-selection rules and selected-session option contract together.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent::sidebar::model::{
    SidebarRow, SidebarRowIdentity, row_index_by_identity, row_index_by_pane, row_index_by_window,
};
use crate::state::AgentSessionKey;

const CURRENT_VERSION: u32 = 1;

/// Tmux window option storing the preferred logical sidebar session.
pub(super) const SELECTED_SESSION_OPTION: &str = "@kmux_sidebar_selected_session";

/// Selection behavior mode for the sidebar list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SelectionMode {
    FollowHost,
    Manual,
}

/// Data needed to roll back a selected-session option write after focus failure.
#[derive(Debug)]
pub(super) struct PersistedSelectionRollback {
    pub(super) window_id: String,
    pub(super) attempted: AgentSessionKey,
    pub(super) previous: PreviousSelectionOption,
}

/// Raw previous value for the selected-session tmux window option.
#[derive(Debug)]
pub(super) enum PreviousSelectionOption {
    Unset,
    Value(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedSessionOption {
    version: u32,
    session: AgentSessionKey,
}

impl From<Option<String>> for PreviousSelectionOption {
    fn from(value: Option<String>) -> Self {
        value.map_or(Self::Unset, Self::Value)
    }
}

/// Return the first row index associated with the host tmux window.
pub(super) fn host_window_index(
    rows: &[SidebarRow],
    host_window_id: Option<&str>,
) -> Option<usize> {
    host_window_id.and_then(|window_id| row_index_by_window(rows, window_id))
}

/// Return the remembered logical row index when it still belongs to the host window.
pub(super) fn remembered_host_identity_index(
    rows: &[SidebarRow],
    host_window_id: Option<&str>,
    identity: Option<&SidebarRowIdentity>,
) -> Option<usize> {
    let host_window_id = host_window_id?;
    let identity = identity?;
    let index = row_index_by_identity(rows, identity)?;
    (rows[index].window_id == host_window_id).then_some(index)
}

/// Return the persisted logical session row index when it still belongs to the host window.
pub(super) fn persisted_host_session_index(
    rows: &[SidebarRow],
    host_window_id: Option<&str>,
    session: &AgentSessionKey,
) -> Option<usize> {
    let host_window_id = host_window_id?;
    rows.iter()
        .position(|row| row.window_id == host_window_id && row.identity.session_key() == *session)
}

/// Return the best row index for manual selection after row refreshes.
pub(super) fn manual_selection_index(
    rows: &[SidebarRow],
    identity: Option<&SidebarRowIdentity>,
    pane_id: Option<&str>,
    window_id: Option<&str>,
    selected_index: Option<usize>,
) -> Option<usize> {
    identity
        .and_then(|identity| row_index_by_identity(rows, identity))
        .or_else(|| pane_id.and_then(|pane_id| row_index_by_pane(rows, pane_id)))
        .or_else(|| window_id.and_then(|window_id| row_index_by_window(rows, window_id)))
        .or_else(|| selected_index.filter(|index| *index < rows.len()))
}

/// Encode a logical agent session for storage in a tmux window option.
pub(super) fn encode_selected_session(session: &AgentSessionKey) -> Result<String> {
    Ok(serde_json::to_string(&SelectedSessionOption {
        version: CURRENT_VERSION,
        session: session.clone(),
    })?)
}

/// Decode a logical agent session from a tmux window option value.
pub(super) fn decode_selected_session(value: &str) -> Option<AgentSessionKey> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let option = serde_json::from_str::<SelectedSessionOption>(value).ok()?;
    if option.version != CURRENT_VERSION {
        return None;
    }
    if option.session.agent_kind.trim().is_empty() || option.session.session_id.trim().is_empty() {
        return None;
    }
    Some(option.session)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sidebar::test_support::{report_state, row_from_view};
    use crate::state::AgentStatus;

    fn session(agent_kind: &str, session_id: &str) -> AgentSessionKey {
        AgentSessionKey {
            agent_kind: agent_kind.to_owned(),
            session_id: session_id.to_owned(),
        }
    }

    fn row(session_id: &str, window_id: &str, pane_id: &str) -> SidebarRow {
        let mut view = report_state(AgentStatus::Waiting, 100, window_id, pane_id);
        view.key = session("opencode", session_id);
        row_from_view(&view, 100)
    }

    #[test]
    fn manual_selection_prefers_stable_identity_then_pane_then_window_then_current_index() {
        let rows = vec![row("ses_a", "@1", "%1"), row("ses_b", "@2", "%2")];
        let identity = rows[1].identity.clone();

        assert_eq!(
            manual_selection_index(&rows, Some(&identity), Some("%1"), Some("@1"), Some(0)),
            Some(1)
        );
        assert_eq!(
            manual_selection_index(&rows, None, Some("%2"), Some("@1"), Some(0)),
            Some(1)
        );
        assert_eq!(
            manual_selection_index(&rows, None, None, Some("@2"), Some(0)),
            Some(1)
        );
        assert_eq!(
            manual_selection_index(&rows, None, None, None, Some(1)),
            Some(1)
        );
    }

    #[test]
    fn host_selection_helpers_require_host_window_match() {
        let rows = vec![row("ses_a", "@1", "%1"), row("ses_b", "@2", "%2")];
        let other_window_identity = rows[1].identity.clone();

        assert_eq!(host_window_index(&rows, Some("@1")), Some(0));
        assert_eq!(
            remembered_host_identity_index(&rows, Some("@1"), Some(&other_window_identity)),
            None
        );
        assert_eq!(
            persisted_host_session_index(&rows, Some("@1"), &session("opencode", "ses_b")),
            None
        );
        assert_eq!(
            persisted_host_session_index(&rows, Some("@2"), &session("opencode", "ses_b")),
            Some(1)
        );
    }

    #[test]
    fn selected_session_option_round_trips_session_key() -> Result<()> {
        let key = session("other-agent", "ses_123");

        let encoded = encode_selected_session(&key)?;

        assert_eq!(decode_selected_session(&encoded), Some(key));
        Ok(())
    }

    #[test]
    fn selected_session_option_shape_is_stable() -> Result<()> {
        let encoded = encode_selected_session(&session("opencode", "ses_123"))?;
        let json: serde_json::Value = serde_json::from_str(&encoded)?;

        assert_eq!(json["version"], CURRENT_VERSION);
        assert_eq!(json["session"]["agent_kind"], "opencode");
        assert_eq!(json["session"]["session_id"], "ses_123");
        Ok(())
    }

    #[test]
    fn selected_session_option_rejects_empty_malformed_and_future_values() {
        assert_eq!(decode_selected_session(""), None);
        assert_eq!(decode_selected_session("not json"), None);
        assert_eq!(
            decode_selected_session(
                r#"{"version":999,"session":{"agent_kind":"opencode","session_id":"ses_123"}}"#
            ),
            None
        );
        assert_eq!(
            decode_selected_session(
                r#"{"version":1,"session":{"agent_kind":"","session_id":"ses_123"}}"#
            ),
            None
        );
        assert_eq!(
            decode_selected_session(
                r#"{"version":1,"session":{"agent_kind":"opencode","session_id":""}}"#
            ),
            None
        );
    }
}
