//! Tmux window option encoding for sidebar logical selection.
//!
//! Selection stickiness is scoped to a tmux window, so the serialized value lives
//! in that window's `@kmux_*` option rather than in durable kmux state.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::state::AgentSessionKey;

const CURRENT_VERSION: u32 = 1;

/// Tmux window option storing the preferred logical sidebar session.
pub(super) const SELECTED_SESSION_OPTION: &str = "@kmux_sidebar_selected_session";

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedSessionOption {
    version: u32,
    session: AgentSessionKey,
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

    fn session(agent_kind: &str, session_id: &str) -> AgentSessionKey {
        AgentSessionKey {
            agent_kind: agent_kind.to_owned(),
            session_id: session_id.to_owned(),
        }
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
