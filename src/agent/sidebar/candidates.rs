use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;

use anyhow::Result;

use crate::tmux::{Tmux, TmuxPaneSnapshot, TmuxSplitSize};

/// Default sidebar width in cells when config does not specify one.
pub(super) const DEFAULT_WIDTH: u16 = 42;
/// tmux pane role value used to mark kmux sidebar panes.
pub(super) const SIDEBAR_ROLE: &str = "sidebar";

const RESURRECT_DIR_OPTION: &str = "@resurrect-dir";

/// Recognizes live and restored panes that belong to the kmux sidebar.
pub(super) struct SidebarCandidateMatcher {
    size: Option<TmuxSplitSize>,
    resurrect_panes: HashSet<ResurrectPaneKey>,
    geometry_panes: HashSet<String>,
}

impl SidebarCandidateMatcher {
    /// Build the matcher used to recognize live and tmux-resurrect-restored sidebar panes.
    pub(super) fn new(
        tmux: &Tmux,
        panes: &[TmuxPaneSnapshot],
        size: Option<TmuxSplitSize>,
        include_geometry: bool,
    ) -> Self {
        let resurrect_panes = resurrect_sidebar_panes(tmux).unwrap_or_default();
        let resurrect_sessions = resurrect_panes
            .iter()
            .map(|pane| pane.session_name.clone())
            .collect::<HashSet<_>>();
        let geometry_panes = if include_geometry {
            size.map_or_else(HashSet::new, |size| {
                restored_sidebar_geometry_panes(panes, size, &resurrect_sessions)
            })
        } else {
            HashSet::new()
        };
        Self {
            size,
            resurrect_panes,
            geometry_panes,
        }
    }

    /// Return whether a pane should be treated as kmux sidebar state during reconciliation.
    pub(super) fn is_sidebar_candidate(&self, pane: &TmuxPaneSnapshot) -> bool {
        pane.kmux_role.as_deref() == Some(SIDEBAR_ROLE)
            || is_title_restored_sidebar_candidate(pane, self.size)
            || self.is_resurrect_restored_sidebar_candidate(pane)
            || self.geometry_panes.contains(&pane.pane_id)
    }

    fn is_resurrect_restored_sidebar_candidate(&self, pane: &TmuxPaneSnapshot) -> bool {
        self.resurrect_panes.contains(&ResurrectPaneKey {
            session_name: pane.session_name.clone(),
            window_index: pane.window_index.clone(),
            pane_index: pane.pane_index.clone(),
        }) && pane.pane_left == 0
            && pane.pane_width <= restored_sidebar_width_limit(pane, self.size)
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

/// Return the best sidebar candidate for each window id.
pub(super) fn sidebar_candidates_by_window<'a>(
    panes: &'a [TmuxPaneSnapshot],
    matcher: &'a SidebarCandidateMatcher,
) -> HashMap<String, &'a TmuxPaneSnapshot> {
    let mut sidebars = HashMap::<String, &TmuxPaneSnapshot>::new();
    for pane in sidebar_candidate_snapshots(panes, matcher) {
        sidebars
            .entry(pane.window_id.clone())
            .and_modify(|current| {
                if sidebar_candidate_score(pane) > sidebar_candidate_score(current) {
                    *current = pane;
                }
            })
            .or_insert(pane);
    }
    sidebars
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ResurrectPaneKey {
    session_name: String,
    window_index: String,
    pane_index: String,
}

// tmux-resurrect may restore pane titles before kmux user options are back; use
// title plus left-edge geometry as a recovery signal.
fn is_title_restored_sidebar_candidate(
    pane: &TmuxPaneSnapshot,
    size: Option<TmuxSplitSize>,
) -> bool {
    pane.title.as_deref() == Some("kmux")
        && pane.pane_left == 0
        && pane.pane_width <= restored_sidebar_width_limit(pane, size)
}

// Geometry matching is a fallback for resurrected sidebars that lost both role
// metadata and title, so keep it narrow and tied to the configured width.
fn looks_like_restored_sidebar_geometry(pane: &TmuxPaneSnapshot, size: TmuxSplitSize) -> bool {
    pane.kmux_role.is_none()
        && pane.pane_left == 0
        && pane.window_width > pane.pane_width.saturating_add(20)
        && pane
            .pane_width
            .abs_diff(sidebar_width_cells(size, pane.window_width))
            <= 2
}

fn restored_sidebar_width_limit(pane: &TmuxPaneSnapshot, size: Option<TmuxSplitSize>) -> u16 {
    let expected_width = size.map(|size| sidebar_width_cells(size, pane.window_width));
    expected_width
        .unwrap_or(DEFAULT_WIDTH)
        .max(DEFAULT_WIDTH)
        .saturating_add(8)
        .max(1)
}

// Prefer panes that are currently running kmux and already marked with the role option.
fn sidebar_candidate_score(pane: &TmuxPaneSnapshot) -> (u8, u8) {
    (
        u8::from(pane.current_command.as_deref() == Some("kmux")),
        u8::from(pane.kmux_role.as_deref() == Some(SIDEBAR_ROLE)),
    )
}

fn restored_sidebar_geometry_panes(
    panes: &[TmuxPaneSnapshot],
    size: TmuxSplitSize,
    resurrect_sessions: &HashSet<String>,
) -> HashSet<String> {
    if resurrect_sessions.is_empty() {
        return HashSet::new();
    }

    let candidates = panes
        .iter()
        .filter(|pane| {
            resurrect_sessions.contains(&pane.session_name)
                && looks_like_restored_sidebar_geometry(pane, size)
        })
        .collect::<Vec<_>>();

    let mut windows_by_session = HashMap::<&str, HashSet<&str>>::new();
    for pane in panes {
        if resurrect_sessions.contains(&pane.session_name) {
            windows_by_session
                .entry(&pane.session_name)
                .or_default()
                .insert(&pane.window_id);
        }
    }

    let mut candidate_windows_by_session = HashMap::<&str, HashSet<&str>>::new();
    for pane in &candidates {
        candidate_windows_by_session
            .entry(&pane.session_name)
            .or_default()
            .insert(&pane.window_id);
    }

    candidates
        .into_iter()
        .filter(|pane| {
            windows_by_session
                .get(pane.session_name.as_str())
                .is_some_and(|windows| windows.len() > 1)
                && candidate_windows_by_session
                    .get(pane.session_name.as_str())
                    .is_some_and(|windows| windows.len() >= 2)
        })
        .map(|pane| pane.pane_id.clone())
        .collect()
}

fn resurrect_sidebar_panes(tmux: &Tmux) -> Result<HashSet<ResurrectPaneKey>> {
    let Some(last_file) = resurrect_last_file(tmux)? else {
        return Ok(HashSet::new());
    };
    let contents = fs::read_to_string(last_file)?;
    Ok(parse_resurrect_sidebar_panes(&contents))
}

fn resurrect_last_file(tmux: &Tmux) -> Result<Option<PathBuf>> {
    let Some(dir) = resurrect_dir(tmux)? else {
        return Ok(None);
    };
    let last = dir.join("last");
    if !last.exists() {
        return Ok(None);
    }
    Ok(Some(fs::read_link(&last).map_or(last, |target| {
        if target.is_absolute() {
            target
        } else {
            dir.join(target)
        }
    })))
}

fn resurrect_dir(tmux: &Tmux) -> Result<Option<PathBuf>> {
    let output = tmux.output(["show-option", "-gqv", RESURRECT_DIR_OPTION])?;
    if output.status.success() {
        let value = output.stdout.trim_end();
        if !value.is_empty() {
            return Ok(Some(expand_resurrect_path(value)));
        }
    }

    let Some(home) = std::env::var_os("HOME") else {
        return Ok(None);
    };
    let home = PathBuf::from(home);
    let legacy_dir = home.join(".tmux/resurrect");
    if legacy_dir.is_dir() {
        return Ok(Some(legacy_dir));
    }

    Ok(Some(
        std::env::var_os("XDG_DATA_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|| home.join(".local/share"))
            .join("tmux/resurrect"),
    ))
}

fn expand_resurrect_path(value: &str) -> PathBuf {
    let home = std::env::var("HOME").unwrap_or_default();
    if value == "~" {
        return PathBuf::from(home);
    }
    if let Some(rest) = value.strip_prefix("~/") {
        return PathBuf::from(home).join(rest);
    }
    PathBuf::from(value.replace("$HOME", &home))
}

fn parse_resurrect_sidebar_panes(contents: &str) -> HashSet<ResurrectPaneKey> {
    contents
        .lines()
        .filter_map(parse_resurrect_sidebar_pane)
        .collect()
}

fn parse_resurrect_sidebar_pane(line: &str) -> Option<ResurrectPaneKey> {
    let fields = line.split('\t').collect::<Vec<_>>();
    if fields.first().copied() != Some("pane") || fields.len() < 10 {
        return None;
    }

    let pane_title = fields[6];
    let pane_command = fields[9];
    if pane_title != "kmux" && pane_command != "kmux" {
        return None;
    }

    Some(ResurrectPaneKey {
        session_name: fields[1].to_owned(),
        window_index: fields[2].to_owned(),
        pane_index: fields[5].to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn restored_sidebar_candidates_are_kmux_titled_left_panes() {
        let restored = pane_snapshot("%1", "@1", 0, 30, Some("kmux"), None, None);
        let wide = pane_snapshot("%2", "@1", 0, 90, Some("kmux"), None, None);
        let not_left = pane_snapshot("%3", "@1", 10, 30, Some("kmux"), None, None);
        let tagged = pane_snapshot("%4", "@1", 10, 90, Some("shell"), None, Some(SIDEBAR_ROLE));
        let matcher = test_matcher(Some(TmuxSplitSize::Cells(30)));

        assert!(matcher.is_sidebar_candidate(&restored));
        assert!(!matcher.is_sidebar_candidate(&wide));
        assert!(!matcher.is_sidebar_candidate(&not_left));
        assert!(matcher.is_sidebar_candidate(&tagged));
    }

    #[test]
    fn resurrect_save_rows_identify_sidebars_after_titles_are_lost() {
        let restored = pane_snapshot("%1", "@1", 0, 30, Some("fish"), Some("fish"), None);
        let unrelated = pane_snapshot("%2", "@1", 30, 90, Some("fish"), Some("fish"), None);
        let mut matcher = test_matcher(Some(TmuxSplitSize::Cells(30)));
        matcher.resurrect_panes.insert(ResurrectPaneKey {
            session_name: "project".to_owned(),
            window_index: "1".to_owned(),
            pane_index: "1".to_owned(),
        });

        assert!(matcher.is_sidebar_candidate(&restored));
        assert!(!matcher.is_sidebar_candidate(&unrelated));
    }

    #[test]
    fn parses_resurrect_sidebar_panes_from_saved_kmux_rows() {
        let keys = parse_resurrect_sidebar_panes(
            "pane\tproject\t1\t1\t:*\t1\tkmux\t:/repo\t0\tkmux\t:\n\
             pane\tproject\t1\t1\t:*\t2\tfish\t:/repo\t1\tfish\t:\n\
             window\tproject\t1\t:main\t1\t:*\tlayout\t:\n",
        );

        assert!(keys.contains(&ResurrectPaneKey {
            session_name: "project".to_owned(),
            window_index: "1".to_owned(),
            pane_index: "1".to_owned(),
        }));
        assert_eq!(keys.len(), 1);
    }

    #[test]
    fn repeated_left_geometry_is_restored_sidebar_fallback() {
        let panes = vec![
            pane_snapshot("%1", "@1", 0, 30, Some("fish"), Some("fish"), None),
            pane_snapshot("%2", "@1", 31, 89, Some("fish"), Some("fish"), None),
            pane_snapshot("%3", "@2", 0, 30, Some("fish"), Some("fish"), None),
            pane_snapshot("%4", "@2", 31, 89, Some("fish"), Some("fish"), None),
        ];
        let resurrect_sessions = HashSet::from(["project".to_owned()]);
        let geometry_panes =
            restored_sidebar_geometry_panes(&panes, TmuxSplitSize::Cells(30), &resurrect_sessions);

        assert!(geometry_panes.contains("%1"));
        assert!(geometry_panes.contains("%3"));
        assert!(!geometry_panes.contains("%2"));
        assert!(!geometry_panes.contains("%4"));
    }

    #[test]
    fn repeated_left_geometry_without_resurrect_evidence_is_ignored() {
        let panes = vec![
            pane_snapshot("%1", "@1", 0, 30, Some("fish"), Some("fish"), None),
            pane_snapshot("%2", "@1", 31, 89, Some("fish"), Some("fish"), None),
            pane_snapshot("%3", "@2", 0, 30, Some("fish"), Some("fish"), None),
            pane_snapshot("%4", "@2", 31, 89, Some("fish"), Some("fish"), None),
        ];

        assert!(
            restored_sidebar_geometry_panes(&panes, TmuxSplitSize::Cells(30), &HashSet::new())
                .is_empty()
        );
    }

    #[test]
    fn sidebar_candidates_prefer_running_kmux_panes() {
        let restored = pane_snapshot("%1", "@1", 0, 30, Some("kmux"), Some("sh"), None);
        let running = pane_snapshot(
            "%2",
            "@1",
            0,
            30,
            Some("kmux"),
            Some("kmux"),
            Some(SIDEBAR_ROLE),
        );
        let panes = vec![restored, running];
        let matcher = test_matcher(Some(TmuxSplitSize::Cells(30)));

        let sidebar = sidebar_candidates_by_window(&panes, &matcher)
            .remove("@1")
            .expect("window should have a sidebar candidate");

        assert_eq!(sidebar.pane_id, "%2");
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
        SidebarCandidateMatcher {
            size,
            resurrect_panes: HashSet::new(),
            geometry_panes: HashSet::new(),
        }
    }
}
