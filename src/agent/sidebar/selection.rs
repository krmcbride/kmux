//! Selection policy and tmux option encoding for sidebar rows.
//!
//! `SidebarApp` owns tmux side effects and controller flow. This module keeps the
//! pure row-selection rules and selected-target option contract together.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::agent::sidebar::model::{
    SidebarRow, SidebarRowIdentity, row_index_by_identity, row_index_by_pane, row_index_by_window,
};

const CURRENT_VERSION: u32 = 2;

/// Tmux window option storing the preferred sidebar target row.
pub(super) const SELECTED_TARGET_OPTION: &str = "@kmux_sidebar_selected_target";

/// Selection behavior mode for the sidebar list.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum SelectionMode {
    FollowSidebarContext,
    Manual,
}

/// Data needed to roll back a selected-target option write after focus failure.
#[derive(Debug)]
pub(super) struct PersistedSelectionRollback {
    pub(super) window_id: String,
    pub(super) attempted: SidebarRowIdentity,
    pub(super) previous: PreviousSelectionOption,
}

/// Raw previous value for the selected-target tmux window option.
#[derive(Debug)]
pub(super) enum PreviousSelectionOption {
    Unset,
    Value(String),
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SelectedTargetOption {
    version: u32,
    target: SidebarRowIdentity,
}

impl From<Option<String>> for PreviousSelectionOption {
    fn from(value: Option<String>) -> Self {
        value.map_or(Self::Unset, Self::Value)
    }
}

/// Return the first row index associated with the sidebar tmux window.
pub(super) fn sidebar_window_index(
    rows: &[SidebarRow],
    sidebar_window_id: Option<&str>,
) -> Option<usize> {
    sidebar_window_id.and_then(|window_id| row_index_by_window(rows, window_id))
}

/// Return the first row associated with the sidebar window or, for session-level
/// targets, the sidebar session.
pub(super) fn sidebar_context_index(
    rows: &[SidebarRow],
    sidebar_window_id: Option<&str>,
    sidebar_session_name: Option<&str>,
) -> Option<usize> {
    sidebar_window_index(rows, sidebar_window_id).or_else(|| {
        let sidebar_session_name = sidebar_session_name?;
        rows.iter()
            .position(|row| row.window_id.is_empty() && row.session_name == sidebar_session_name)
    })
}

/// Return the remembered logical row index when it still belongs to the sidebar context.
pub(super) fn remembered_sidebar_context_identity_index(
    rows: &[SidebarRow],
    sidebar_window_id: Option<&str>,
    sidebar_session_name: Option<&str>,
    identity: Option<&SidebarRowIdentity>,
) -> Option<usize> {
    let identity = identity?;
    let index = row_index_by_identity(rows, identity)?;
    row_matches_sidebar_context(&rows[index], sidebar_window_id, sidebar_session_name)
        .then_some(index)
}

/// Return the persisted row identity index when it still belongs to the sidebar context.
pub(super) fn persisted_sidebar_context_identity_index(
    rows: &[SidebarRow],
    sidebar_window_id: Option<&str>,
    sidebar_session_name: Option<&str>,
    identity: &SidebarRowIdentity,
) -> Option<usize> {
    remembered_sidebar_context_identity_index(
        rows,
        sidebar_window_id,
        sidebar_session_name,
        Some(identity),
    )
}

fn row_matches_sidebar_context(
    row: &SidebarRow,
    sidebar_window_id: Option<&str>,
    sidebar_session_name: Option<&str>,
) -> bool {
    if !row.window_id.is_empty() {
        return sidebar_window_id.is_some_and(|window_id| row.window_id == window_id);
    }

    sidebar_session_name.is_some_and(|session_name| row.session_name == session_name)
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

/// Encode a sidebar row identity for storage in a tmux window option.
pub(super) fn encode_selected_target(identity: &SidebarRowIdentity) -> Result<String> {
    Ok(serde_json::to_string(&SelectedTargetOption {
        version: CURRENT_VERSION,
        target: identity.clone(),
    })?)
}

/// Decode a sidebar row identity from a tmux window option value.
pub(super) fn decode_selected_target(value: &str) -> Option<SidebarRowIdentity> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let option = serde_json::from_str::<SelectedTargetOption>(value).ok()?;
    if option.version != CURRENT_VERSION {
        return None;
    }
    if !option.target.is_valid() {
        return None;
    }
    Some(option.target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sidebar::test_support::{report_state, row_from_view};
    use crate::state::{AgentSessionKey, AgentStatus};

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
    fn sidebar_selection_helpers_require_sidebar_window_match() {
        let rows = vec![row("ses_a", "@1", "%1"), row("ses_b", "@2", "%2")];
        let other_window_identity = rows[1].identity.clone();

        assert_eq!(sidebar_window_index(&rows, Some("@1")), Some(0));
        assert_eq!(
            remembered_sidebar_context_identity_index(
                &rows,
                Some("@1"),
                Some("project"),
                Some(&other_window_identity)
            ),
            None
        );
        assert_eq!(
            persisted_sidebar_context_identity_index(
                &rows,
                Some("@1"),
                Some("project"),
                &rows[1].identity
            ),
            None
        );
        assert_eq!(
            persisted_sidebar_context_identity_index(
                &rows,
                Some("@2"),
                Some("project"),
                &rows[1].identity
            ),
            Some(1)
        );
    }

    #[test]
    fn sidebar_context_matches_session_target_rows_without_window_ids() {
        let mut session_row = row("ses_session", "", "%1");
        session_row.window_id.clear();
        session_row.pane_id = None;
        session_row.session_name = "project".to_owned();
        let rows = vec![row("ses_a", "@1", "%2"), session_row];

        assert_eq!(
            sidebar_context_index(&rows, Some("@missing"), Some("project")),
            Some(1)
        );
        assert_eq!(
            remembered_sidebar_context_identity_index(
                &rows,
                Some("@missing"),
                Some("project"),
                Some(&rows[1].identity)
            ),
            Some(1)
        );
        assert_eq!(
            remembered_sidebar_context_identity_index(
                &rows,
                Some("@missing"),
                Some("other"),
                Some(&rows[1].identity)
            ),
            None
        );
    }

    #[test]
    fn selected_target_option_round_trips_row_identity() -> Result<()> {
        let row = row("ses_123", "@1", "%1");

        let encoded = encode_selected_target(&row.identity)?;

        assert_eq!(decode_selected_target(&encoded), Some(row.identity));
        Ok(())
    }

    #[test]
    fn selected_target_option_shape_is_stable() -> Result<()> {
        let row = row("ses_123", "@1", "%1");

        let encoded = encode_selected_target(&row.identity)?;
        let json: serde_json::Value = serde_json::from_str(&encoded)?;

        assert_eq!(json["version"], CURRENT_VERSION);
        assert_eq!(
            json["target"]["key"],
            "workspace:/repo__worktrees/feature-sidebar/@1"
        );
        Ok(())
    }

    #[test]
    fn selected_target_option_rejects_empty_malformed_and_future_values() {
        assert_eq!(decode_selected_target(""), None);
        assert_eq!(decode_selected_target("not json"), None);
        assert_eq!(
            decode_selected_target(r#"{"version":999,"target":{"key":"directory:/repo"}}"#),
            None
        );
        assert_eq!(
            decode_selected_target(r#"{"version":2,"target":{"key":""}}"#),
            None
        );
    }
}
