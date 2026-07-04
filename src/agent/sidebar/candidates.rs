//! Sidebar pane candidate detection and de-duplication.
//!
//! This module recognizes panes that can safely be treated as sidebar panes:
//! panes explicitly marked during the current tmux server lifetime, and inactive
//! left-edge panes whose geometry exactly matches the sidebar we would create.

use std::collections::HashMap;

use crate::tmux::{TmuxPaneSnapshot, TmuxSplitSize};

/// Default sidebar width in cells when config does not specify one.
pub(super) const DEFAULT_WIDTH: u16 = 42;
/// tmux pane role value used to mark kmux sidebar panes.
pub(super) const SIDEBAR_ROLE: &str = "sidebar";

/// Recognizes live and restored panes that belong to the kmux sidebar.
pub(super) struct SidebarCandidateMatcher {
    size: Option<TmuxSplitSize>,
}

impl SidebarCandidateMatcher {
    /// Build the matcher used to recognize live and restored sidebar panes.
    pub(super) fn new(size: Option<TmuxSplitSize>) -> Self {
        Self { size }
    }

    /// Return whether a pane should be treated as kmux sidebar state during reconciliation.
    pub(super) fn is_sidebar_candidate(&self, pane: &TmuxPaneSnapshot) -> bool {
        self.is_marked_sidebar(pane) || self.is_restored_sidebar_shape(pane)
    }

    fn is_marked_sidebar(&self, pane: &TmuxPaneSnapshot) -> bool {
        pane.kmux_role.as_deref() == Some(SIDEBAR_ROLE)
    }

    fn is_restored_sidebar_shape(&self, pane: &TmuxPaneSnapshot) -> bool {
        pane.kmux_role.is_none()
            && !pane.pane_active
            && pane.pane_left == 0
            && self
                .size
                .is_some_and(|size| pane.pane_width == sidebar_width_cells(size, pane.window_width))
    }
}

/// Convert an absolute or percentage sidebar size into cell width for a window.
pub(super) fn sidebar_width_cells(size: TmuxSplitSize, window_width: u16) -> u16 {
    match size {
        TmuxSplitSize::Cells(width) => width,
        TmuxSplitSize::Percent(percent) => ((u32::from(window_width) * u32::from(percent)) / 100)
            .try_into()
            .unwrap_or(u16::MAX),
    }
    .max(1)
}

/// Iterate panes that look like kmux sidebar panes.
pub(super) fn sidebar_candidate_snapshots<'a>(
    panes: &'a [TmuxPaneSnapshot],
    matcher: &'a SidebarCandidateMatcher,
) -> impl Iterator<Item = &'a TmuxPaneSnapshot> {
    panes
        .iter()
        .filter(move |pane| matcher.is_sidebar_candidate(pane))
}

/// Return the sidebar candidate for each window id.
///
/// Marked panes are authoritative for the current tmux server lifetime. Without a
/// marked pane, an unmarked shape match is used only when it is the sole match in
/// that window.
pub(super) fn sidebar_candidates_by_window<'a>(
    panes: &'a [TmuxPaneSnapshot],
    matcher: &'a SidebarCandidateMatcher,
) -> HashMap<String, &'a TmuxPaneSnapshot> {
    let mut matches = HashMap::<String, Vec<&TmuxPaneSnapshot>>::new();
    for pane in sidebar_candidate_snapshots(panes, matcher) {
        matches
            .entry(pane.window_id.clone())
            .or_default()
            .push(pane);
    }
    matches
        .into_iter()
        .filter_map(|(window_id, panes)| {
            sidebar_candidate_for_window(panes, matcher).map(|pane| (window_id, pane))
        })
        .collect()
}

fn sidebar_candidate_for_window<'a>(
    panes: Vec<&'a TmuxPaneSnapshot>,
    matcher: &SidebarCandidateMatcher,
) -> Option<&'a TmuxPaneSnapshot> {
    let mut marked = panes
        .iter()
        .copied()
        .filter(|pane| matcher.is_marked_sidebar(pane));
    if let Some(pane) = marked.next() {
        return Some(pane);
    }

    (panes.len() == 1).then(|| panes[0])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_candidates_are_marked_or_exact_left_panes() {
        let restored = pane_snapshot("%1", "@1", 0, 30, Some("fish"), None, None);
        let wide = pane_snapshot("%2", "@1", 0, 31, Some("kmux"), None, None);
        let not_left = pane_snapshot("%3", "@1", 10, 30, Some("kmux"), None, None);
        let mut active = pane_snapshot("%5", "@1", 0, 30, Some("kmux"), None, None);
        active.pane_active = true;
        let tagged = pane_snapshot("%4", "@1", 10, 90, Some("shell"), None, Some(SIDEBAR_ROLE));
        let matcher = test_matcher(Some(TmuxSplitSize::Cells(30)));

        assert!(matcher.is_sidebar_candidate(&restored));
        assert!(!matcher.is_sidebar_candidate(&wide));
        assert!(!matcher.is_sidebar_candidate(&not_left));
        assert!(!matcher.is_sidebar_candidate(&active));
        assert!(matcher.is_sidebar_candidate(&tagged));
    }

    #[test]
    fn sidebar_candidates_prefer_marked_panes() {
        let restored = pane_snapshot("%1", "@1", 0, 30, Some("kmux"), Some("sh"), None);
        let marked = pane_snapshot(
            "%2",
            "@1",
            0,
            30,
            Some("kmux"),
            Some("kmux"),
            Some(SIDEBAR_ROLE),
        );
        let panes = vec![restored, marked];
        let matcher = test_matcher(Some(TmuxSplitSize::Cells(30)));

        let sidebar = sidebar_candidates_by_window(&panes, &matcher)
            .remove("@1")
            .expect("window should have a sidebar candidate");

        assert_eq!(sidebar.pane_id, "%2");
    }

    #[test]
    fn sidebar_candidates_use_single_unmarked_geometry_match() {
        let restored = pane_snapshot("%1", "@1", 0, 30, Some("fish"), Some("fish"), None);
        let unrelated = pane_snapshot("%2", "@1", 30, 90, Some("fish"), Some("fish"), None);
        let panes = vec![unrelated, restored];
        let matcher = test_matcher(Some(TmuxSplitSize::Cells(30)));

        let sidebar = sidebar_candidates_by_window(&panes, &matcher)
            .remove("@1")
            .expect("window should have a sidebar candidate");

        assert_eq!(sidebar.pane_id, "%1");
    }

    #[test]
    fn sidebar_candidates_ignore_ambiguous_unmarked_geometry_matches() {
        let first = pane_snapshot("%1", "@1", 0, 30, Some("fish"), Some("fish"), None);
        let second = pane_snapshot("%2", "@1", 0, 30, Some("sh"), Some("sh"), None);
        let panes = vec![first, second];
        let matcher = test_matcher(Some(TmuxSplitSize::Cells(30)));

        assert!(
            sidebar_candidates_by_window(&panes, &matcher)
                .remove("@1")
                .is_none()
        );
    }

    #[test]
    fn sidebar_candidates_need_size_to_match_unmarked_geometry() {
        let restored = pane_snapshot("%1", "@1", 0, 30, Some("fish"), Some("fish"), None);
        let marked = pane_snapshot(
            "%2",
            "@1",
            10,
            90,
            Some("sh"),
            Some("sh"),
            Some(SIDEBAR_ROLE),
        );
        let matcher = test_matcher(None);

        assert!(!matcher.is_sidebar_candidate(&restored));
        assert!(matcher.is_sidebar_candidate(&marked));
    }

    fn pane_snapshot(
        pane_id: &str,
        window_id: &str,
        pane_left: u16,
        pane_width: u16,
        title: Option<&str>,
        current_command: Option<&str>,
        kmux_role: Option<&str>,
    ) -> TmuxPaneSnapshot {
        TmuxPaneSnapshot {
            session_name: "project".to_owned(),
            window_id: window_id.to_owned(),
            window_index: "1".to_owned(),
            window_name: "main".to_owned(),
            pane_id: pane_id.to_owned(),
            pane_index: pane_id.trim_start_matches('%').to_owned(),
            pane_left,
            pane_width,
            window_width: 120,
            title: title.map(str::to_owned),
            current_command: current_command.map(str::to_owned),
            current_path: None,
            pane_active: false,
            window_active: false,
            session_attached: false,
            kmux_role: kmux_role.map(str::to_owned),
        }
    }

    fn test_matcher(size: Option<TmuxSplitSize>) -> SidebarCandidateMatcher {
        SidebarCandidateMatcher { size }
    }
}
