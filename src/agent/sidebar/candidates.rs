//! Sidebar pane candidate detection and de-duplication.
//!
//! This module recognizes panes that can safely be treated as sidebar panes:
//! panes explicitly marked during the current tmux server lifetime, and inactive
//! left-edge panes whose geometry exactly matches the sidebar we would create.
//! For an unmarked restored pane, the candidate itself is excluded from the
//! recursive layout minimum before applying the shared sizing policy. This asks
//! whether the remaining content would have produced that exact sidebar width.

use std::collections::HashMap;

use crate::config::SidebarWidth;
use crate::tmux::TmuxPaneSnapshot;

use super::sizing::target_width;

/// tmux pane role value used to mark kmux sidebar panes.
pub(super) const SIDEBAR_ROLE: &str = "sidebar";

/// Recognizes live and restored panes that belong to the kmux sidebar.
pub(super) struct SidebarCandidateMatcher {
    width: Option<SidebarWidth>,
}

impl SidebarCandidateMatcher {
    /// Build the matcher used to recognize live and restored sidebar panes.
    pub(super) fn new(width: Option<SidebarWidth>) -> Self {
        Self { width }
    }

    /// Return whether a pane should be treated as kmux sidebar state during reconciliation.
    pub(super) fn is_sidebar_candidate(&self, pane: &TmuxPaneSnapshot) -> bool {
        self.is_marked_sidebar(pane) || self.is_restored_sidebar_shape(pane)
    }

    fn is_marked_sidebar(&self, pane: &TmuxPaneSnapshot) -> bool {
        pane.kmux_role.as_deref() == Some(SIDEBAR_ROLE)
    }

    fn is_restored_sidebar_shape(&self, pane: &TmuxPaneSnapshot) -> bool {
        if pane.kmux_role.is_some() || pane.pane_active || pane.pane_left != 0 {
            return false;
        }
        let Some(width) = self.width else {
            return false;
        };
        // Use the same content-only geometry and target calculation as lifecycle
        // reconciliation so restoration cannot recognize a width we would not create.
        let minimum_content_width = pane.window_layout.minimum_width(Some(&pane.pane_id));
        pane.pane_width == target_width(width, pane.window_width, minimum_content_width)
    }
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
        let window_matches = matches.entry(pane.window_id.clone()).or_default();
        if window_matches
            .iter()
            .all(|existing| existing.pane_id != pane.pane_id)
        {
            window_matches.push(pane);
        }
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
        let matcher = test_matcher(Some(fixed_width(30)));

        assert!(is_candidate(&matcher, &restored));
        assert!(!is_candidate(&matcher, &wide));
        assert!(!is_candidate(&matcher, &not_left));
        assert!(!is_candidate(&matcher, &active));
        assert!(is_candidate(&matcher, &tagged));
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
        let matcher = test_matcher(Some(fixed_width(30)));

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
        let matcher = test_matcher(Some(fixed_width(30)));

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
        let matcher = test_matcher(Some(fixed_width(30)));

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

        assert!(!is_candidate(&matcher, &restored));
        assert!(is_candidate(&matcher, &marked));
    }

    #[test]
    fn sidebar_candidates_match_policy_width_for_each_window() {
        let policy = SidebarWidth {
            min: 30,
            percent: 25,
            max: 50,
        };
        let narrow = pane_snapshot("%1", "@1", 0, 30, Some("fish"), Some("fish"), None);
        let mut proportional = pane_snapshot("%2", "@2", 0, 40, Some("fish"), Some("fish"), None);
        proportional.window_width = 160;
        let panes = vec![narrow, proportional];
        let matcher = test_matcher(Some(policy));

        let sidebars = sidebar_candidates_by_window(&panes, &matcher);

        assert_eq!(sidebars.len(), 2);
        assert_eq!(sidebars["@1"].pane_width, 30);
        assert_eq!(sidebars["@2"].pane_width, 40);
    }

    #[test]
    fn sidebar_candidates_match_narrow_multi_pane_layout_cap() {
        let policy = SidebarWidth::default();
        let mut sidebar = pane_snapshot("%1", "@1", 0, 16, Some("kmux"), Some("kmux"), None);
        sidebar.window_width = 20;
        sidebar.window_layout = crate::tmux::test_support::test_window_layout(&["%1", "%2", "%3"]);
        let mut first_content = pane_snapshot("%2", "@1", 17, 1, Some("fish"), Some("fish"), None);
        first_content.window_width = 20;
        let mut second_content = pane_snapshot("%3", "@1", 19, 1, Some("fish"), Some("fish"), None);
        second_content.window_width = 20;
        let panes = vec![sidebar, first_content, second_content];
        let matcher = test_matcher(Some(policy));

        let matched = sidebar_candidates_by_window(&panes, &matcher)
            .remove("@1")
            .expect("narrow sidebar should match layout-aware cap");

        assert_eq!(matched.pane_id, "%1");
    }

    #[test]
    fn sidebar_candidates_deduplicate_linked_window_snapshots() {
        let restored = pane_snapshot("%1", "@1", 0, 30, Some("fish"), Some("fish"), None);
        let mut linked = restored.clone();
        linked.session_name = "linked-project".to_owned();
        let panes = vec![restored, linked];
        let matcher = test_matcher(Some(fixed_width(30)));

        let matched = sidebar_candidates_by_window(&panes, &matcher)
            .remove("@1")
            .expect("linked snapshot should reuse one physical sidebar");

        assert_eq!(matched.pane_id, "%1");
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
            window_layout: crate::tmux::test_support::test_window_layout(&[pane_id]),
            title: title.map(str::to_owned),
            current_command: current_command.map(str::to_owned),
            current_path: None,
            pane_active: false,
            pane_last: false,
            window_active: false,
            window_last: false,
            session_attached: false,
            kmux_role: kmux_role.map(str::to_owned),
        }
    }

    fn fixed_width(width: u16) -> SidebarWidth {
        SidebarWidth {
            min: width,
            percent: 20,
            max: width,
        }
    }

    fn test_matcher(width: Option<SidebarWidth>) -> SidebarCandidateMatcher {
        SidebarCandidateMatcher { width }
    }

    fn is_candidate(matcher: &SidebarCandidateMatcher, pane: &TmuxPaneSnapshot) -> bool {
        matcher.is_sidebar_candidate(pane)
    }
}
