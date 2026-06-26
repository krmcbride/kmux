use std::collections::{HashMap, HashSet};
use std::ffi::OsString;
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result};

use crate::agent::sidebar::app::SidebarApp;
use crate::agent::sidebar::model::SidebarIcons;
use crate::agent::sidebar::runtime::run_terminal_app;
use crate::config::{Config, SidebarSize};
use crate::state::StateStore;
use crate::tmux::{Tmux, TmuxPane, TmuxPaneSnapshot, TmuxSplitSize, TmuxWindow};

const DEFAULT_WIDTH: u16 = 42;
const SIDEBAR_ROLE_OPTION: &str = "@kmux_role";
const SIDEBAR_ROLE: &str = "sidebar";
const SIDEBAR_ENABLED_OPTION: &str = "@kmux_sidebar_enabled";
const SIDEBAR_WIDTH_OPTION: &str = "@kmux_sidebar_width";
const SIDEBAR_LOCK_CHANNEL: &str = "kmux-sidebar-reconcile";
const SIDEBAR_RECONCILE_HOOKS: &[&str] = &["after-new-window[90]", "after-new-session[90]"];
const SIDEBAR_WAKE_HOOKS: &[&str] = &[
    "after-select-window[90]",
    "after-select-pane[90]",
    "client-session-changed[90]",
];
const SIDEBAR_WAKE_KEY: &str = "F5";
const RESURRECT_DIR_OPTION: &str = "@resurrect-dir";

pub(super) fn toggle() -> Result<()> {
    let tmux = Tmux::from_env();
    if sidebar_enabled(&tmux)? {
        disable()
    } else {
        enable()
    }
}

pub(super) fn enable() -> Result<()> {
    let config = Config::load()?;
    let tmux = Tmux::from_env();
    let _lock = SidebarLock::acquire(&tmux)?;
    tmux.set_global_option(SIDEBAR_ENABLED_OPTION, "1")?;
    tmux.set_global_option(SIDEBAR_WIDTH_OPTION, &configured_width_label(&config))?;
    install_hooks(&tmux)?;
    reconcile_locked(&tmux, &config)?;
    print_user_message("sidebar enabled");
    Ok(())
}

pub(super) fn disable() -> Result<()> {
    let tmux = Tmux::from_env();
    let _lock = SidebarLock::acquire(&tmux)?;
    let size = Config::load().ok().map(|config| split_size(&config));
    tmux.unset_global_option(SIDEBAR_ENABLED_OPTION)?;
    remove_hooks(&tmux)?;
    tmux.unset_global_option(SIDEBAR_WIDTH_OPTION)?;
    let panes = tmux.list_pane_snapshots()?;
    let matcher = SidebarCandidateMatcher::new(&tmux, &panes, size, false);
    for pane in sidebar_candidate_snapshots(&panes, &matcher) {
        let _ = tmux.kill_pane(&pane.pane_id);
    }
    print_user_message("sidebar disabled");
    Ok(())
}

pub(super) fn refresh() -> Result<()> {
    let config = Config::load()?;
    let tmux = Tmux::from_env();
    let _lock = SidebarLock::acquire(&tmux)?;
    if sidebar_enabled(&tmux)? {
        reconcile_locked(&tmux, &config)?;
    }
    Ok(())
}

pub(super) fn wake(window_id: &str) -> Result<()> {
    let tmux = Tmux::from_env();
    let panes = tmux.list_panes()?;
    if let Some(pane_id) = sidebar_pane_for_window(&panes, window_id) {
        let _ = tmux.send_key(pane_id, SIDEBAR_WAKE_KEY);
    }
    Ok(())
}

pub(super) fn run_tui() -> Result<()> {
    let tmux = Tmux::from_env();
    set_current_sidebar_pane_title(&tmux);
    let config = Config::load()?;
    let store = StateStore::new()?;
    let working_frames = config
        .status_icons
        .working_frames()
        .map_or_else(Vec::new, <[String]>::to_vec);
    let icons = SidebarIcons::from_config(&config.status_icons);
    let mut app = SidebarApp::new(
        tmux,
        store,
        icons,
        working_frames,
        config.sidebar.idle_after_seconds(),
    );
    let disable_requested = run_terminal_app(&mut app)?;
    if disable_requested {
        request_disable_async()?;
    }
    Ok(())
}

fn set_current_sidebar_pane_title(tmux: &Tmux) {
    if let Ok(pane_id) = std::env::var("TMUX_PANE") {
        let _ = tmux.set_pane_title(&pane_id, "kmux");
    }
}

fn request_disable_async() -> Result<()> {
    let tmux = Tmux::from_env();
    let command = sidebar_off_command()?;
    tmux.stdout(["run-shell", "-b", &command])?;
    Ok(())
}

fn reconcile_locked(tmux: &Tmux, config: &Config) -> Result<()> {
    let windows = unique_windows(tmux.list_windows(None)?);
    let command = sidebar_tui_command()?;
    let size = split_size(config);
    prune_extra_sidebars(tmux, size)?;
    let panes = tmux.list_pane_snapshots()?;
    let matcher = SidebarCandidateMatcher::new(tmux, &panes, Some(size), true);
    let mut sidebars_by_window = sidebar_candidates_by_window(&panes, &matcher);

    for window in windows {
        if let Some(pane) = sidebars_by_window.remove(&window.window_id) {
            heal_sidebar_pane(tmux, pane, size, &command)?;
            continue;
        }
        let pane_id = tmux.split_window_left(&window.window_id, size, &command)?;
        tmux.set_pane_option(&pane_id, SIDEBAR_ROLE_OPTION, SIDEBAR_ROLE)?;
    }

    Ok(())
}

fn unique_windows(windows: Vec<TmuxWindow>) -> Vec<TmuxWindow> {
    let mut seen = HashSet::new();
    windows
        .into_iter()
        .filter(|window| seen.insert(window.window_id.clone()))
        .collect()
}

fn prune_extra_sidebars(tmux: &Tmux, size: TmuxSplitSize) -> Result<()> {
    let panes = tmux.list_pane_snapshots()?;
    let matcher = SidebarCandidateMatcher::new(tmux, &panes, Some(size), true);
    let keep = sidebar_candidates_by_window(&panes, &matcher)
        .into_values()
        .map(|pane| pane.pane_id.clone())
        .collect::<HashSet<_>>();
    for pane in sidebar_candidate_snapshots(&panes, &matcher) {
        if !keep.contains(&pane.pane_id) {
            let _ = tmux.kill_pane(&pane.pane_id);
        }
    }
    Ok(())
}

fn sidebar_candidates_by_window<'a>(
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

fn heal_sidebar_pane(
    tmux: &Tmux,
    pane: &TmuxPaneSnapshot,
    size: TmuxSplitSize,
    command: &str,
) -> Result<()> {
    tmux.set_pane_option(&pane.pane_id, SIDEBAR_ROLE_OPTION, SIDEBAR_ROLE)?;
    let width = sidebar_width_cells(size, pane.window_width);
    if width > 0 && pane.pane_width != width {
        let _ = tmux.resize_pane_width(&pane.pane_id, width);
    }
    if pane.current_command.as_deref() != Some("kmux") {
        tmux.respawn_pane(&pane.pane_id, command)?;
    }
    Ok(())
}

fn sidebar_width_cells(size: TmuxSplitSize, window_width: u16) -> u16 {
    match size {
        TmuxSplitSize::Cells(width) => width,
        TmuxSplitSize::Percent(percent) => ((u32::from(window_width) * u32::from(percent)) / 100)
            .try_into()
            .unwrap_or(u16::MAX),
    }
    .max(1)
}

fn install_hooks(tmux: &Tmux) -> Result<()> {
    let refresh_command = format!("run-shell -b {}", shell_quote(&sidebar_refresh_command()?));
    for hook in SIDEBAR_RECONCILE_HOOKS {
        tmux.set_hook(hook, &refresh_command)?;
    }

    let wake_command = sidebar_wake_hook_command()?;
    for hook in SIDEBAR_WAKE_HOOKS {
        tmux.set_hook(hook, &wake_command)?;
    }
    Ok(())
}

fn remove_hooks(tmux: &Tmux) -> Result<()> {
    for hook in SIDEBAR_RECONCILE_HOOKS
        .iter()
        .chain(SIDEBAR_WAKE_HOOKS.iter())
    {
        let _ = tmux.unset_hook(hook);
    }
    Ok(())
}

fn sidebar_enabled(tmux: &Tmux) -> Result<bool> {
    Ok(tmux.show_global_option(SIDEBAR_ENABLED_OPTION)?.as_deref() == Some("1"))
}

fn print_user_message(message: &str) {
    if std::io::stdout().is_terminal() {
        println!("{message}");
    }
}

fn sidebar_panes(panes: &[TmuxPane]) -> impl Iterator<Item = &TmuxPane> {
    panes
        .iter()
        .filter(|pane| pane.kmux_role.as_deref() == Some(SIDEBAR_ROLE))
}

fn sidebar_candidate_snapshots<'a>(
    panes: &'a [TmuxPaneSnapshot],
    matcher: &'a SidebarCandidateMatcher,
) -> impl Iterator<Item = &'a TmuxPaneSnapshot> {
    panes
        .iter()
        .filter(move |pane| matcher.is_sidebar_candidate(pane))
}

fn is_title_restored_sidebar_candidate(
    pane: &TmuxPaneSnapshot,
    size: Option<TmuxSplitSize>,
) -> bool {
    pane.title.as_deref() == Some("kmux")
        && pane.pane_left == 0
        && pane.pane_width <= restored_sidebar_width_limit(pane, size)
}

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

fn sidebar_candidate_score(pane: &TmuxPaneSnapshot) -> (u8, u8) {
    (
        u8::from(pane.current_command.as_deref() == Some("kmux")),
        u8::from(pane.kmux_role.as_deref() == Some(SIDEBAR_ROLE)),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct ResurrectPaneKey {
    session_name: String,
    window_index: String,
    pane_index: String,
}

struct SidebarCandidateMatcher {
    size: Option<TmuxSplitSize>,
    resurrect_panes: HashSet<ResurrectPaneKey>,
    geometry_panes: HashSet<String>,
}

impl SidebarCandidateMatcher {
    fn new(
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

    #[cfg(test)]
    fn test(size: Option<TmuxSplitSize>) -> Self {
        Self {
            size,
            resurrect_panes: HashSet::new(),
            geometry_panes: HashSet::new(),
        }
    }

    fn is_sidebar_candidate(&self, pane: &TmuxPaneSnapshot) -> bool {
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

fn sidebar_pane_for_window<'a>(panes: &'a [TmuxPane], window_id: &str) -> Option<&'a str> {
    sidebar_panes(panes)
        .find(|pane| pane.window_id == window_id)
        .map(|pane| pane.pane_id.as_str())
}

struct SidebarLock<'a> {
    tmux: &'a Tmux,
}

impl<'a> SidebarLock<'a> {
    fn acquire(tmux: &'a Tmux) -> Result<Self> {
        tmux.wait_for_lock(SIDEBAR_LOCK_CHANNEL)?;
        Ok(Self { tmux })
    }
}

impl Drop for SidebarLock<'_> {
    fn drop(&mut self) {
        let _ = self.tmux.wait_for_unlock(SIDEBAR_LOCK_CHANNEL);
    }
}

fn split_size(config: &Config) -> TmuxSplitSize {
    match configured_width(config) {
        SidebarSize::Absolute(width) => TmuxSplitSize::Cells(width),
        SidebarSize::Percent(percent) => TmuxSplitSize::Percent(percent),
    }
}

fn configured_width_label(config: &Config) -> String {
    match configured_width(config) {
        SidebarSize::Absolute(width) => width.to_string(),
        SidebarSize::Percent(percent) => format!("{percent}%"),
    }
}

fn configured_width(config: &Config) -> SidebarSize {
    config
        .sidebar
        .width
        .unwrap_or(SidebarSize::Absolute(DEFAULT_WIDTH))
}

fn sidebar_tui_command() -> Result<String> {
    sidebar_command(["sidebar", "run"])
}

fn sidebar_refresh_command() -> Result<String> {
    sidebar_command(["sidebar", "refresh"])
}

fn sidebar_off_command() -> Result<String> {
    sidebar_command(["sidebar", "off"])
}

fn sidebar_wake_command(window_id: &str) -> Result<String> {
    sidebar_command(["sidebar", "wake", window_id])
}

fn sidebar_wake_hook_command() -> Result<String> {
    Ok(format!(
        "run-shell -b {}",
        shell_quote(&sidebar_wake_command("#{window_id}")?)
    ))
}

fn sidebar_command<const N: usize>(args: [&str; N]) -> Result<String> {
    let executable = std::env::current_exe().context("failed to determine current executable")?;
    let mut parts = vec!["exec".to_owned(), "env".to_owned()];
    for key in [
        "XDG_CONFIG_HOME",
        "XDG_STATE_HOME",
        "KMUX_TMUX_SOCKET_NAME",
        "KMUX_TMUX_TMPDIR",
    ] {
        if let Some(value) = std::env::var_os(key) {
            parts.push(format_env_assignment(key, &value));
        }
    }
    parts.push(shell_quote(&executable.to_string_lossy()));
    parts.extend(args.into_iter().map(str::to_owned));
    Ok(parts.join(" "))
}

fn format_env_assignment(key: &str, value: &OsString) -> String {
    format!("{key}={}", shell_quote(&value.to_string_lossy()))
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\"'\"'"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_pane_command_runs_hidden_tui() -> Result<()> {
        let command = sidebar_tui_command()?;

        assert!(command.starts_with("exec env "));
        assert!(command.contains(" sidebar run"));
        assert!(!command.contains("while :; do"));
        Ok(())
    }

    #[test]
    fn sidebar_off_command_runs_visible_disable_path() -> Result<()> {
        let command = sidebar_off_command()?;

        assert!(command.starts_with("exec env "));
        assert!(command.contains(" sidebar off"));
        Ok(())
    }

    #[test]
    fn sidebar_wake_command_targets_window_id() -> Result<()> {
        let command = sidebar_wake_command("@42")?;

        assert!(command.starts_with("exec env "));
        assert!(command.contains(" sidebar wake "));
        assert!(command.ends_with(" sidebar wake @42"));
        Ok(())
    }

    #[test]
    fn sidebar_wake_hook_preserves_tmux_window_format() -> Result<()> {
        let command = sidebar_wake_hook_command()?;

        assert!(command.starts_with("run-shell -b "));
        assert!(command.contains("sidebar wake"));
        assert!(command.contains("#{window_id}"));
        Ok(())
    }

    #[test]
    fn restored_sidebar_candidates_are_kmux_titled_left_panes() {
        let restored = pane_snapshot("%1", "@1", 0, 30, Some("kmux"), None, None);
        let wide = pane_snapshot("%2", "@1", 0, 90, Some("kmux"), None, None);
        let not_left = pane_snapshot("%3", "@1", 10, 30, Some("kmux"), None, None);
        let tagged = pane_snapshot("%4", "@1", 10, 90, Some("shell"), None, Some(SIDEBAR_ROLE));
        let matcher = SidebarCandidateMatcher::test(Some(TmuxSplitSize::Cells(30)));

        assert!(matcher.is_sidebar_candidate(&restored));
        assert!(!matcher.is_sidebar_candidate(&wide));
        assert!(!matcher.is_sidebar_candidate(&not_left));
        assert!(matcher.is_sidebar_candidate(&tagged));
    }

    #[test]
    fn resurrect_save_rows_identify_sidebars_after_titles_are_lost() {
        let restored = pane_snapshot("%1", "@1", 0, 30, Some("fish"), Some("fish"), None);
        let unrelated = pane_snapshot("%2", "@1", 30, 90, Some("fish"), Some("fish"), None);
        let mut matcher = SidebarCandidateMatcher::test(Some(TmuxSplitSize::Cells(30)));
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
        let matcher = SidebarCandidateMatcher::test(Some(TmuxSplitSize::Cells(30)));

        let sidebar = sidebar_candidates_by_window(&panes, &matcher)
            .remove("@1")
            .expect("window should have a sidebar candidate");

        assert_eq!(sidebar.pane_id, "%2");
    }

    #[test]
    fn sidebar_pane_for_window_returns_target_window_sidebar() {
        let panes = vec![
            TmuxPane {
                session_name: "project".to_owned(),
                window_id: "@1".to_owned(),
                pane_id: "%1".to_owned(),
                kmux_role: Some(SIDEBAR_ROLE.to_owned()),
            },
            TmuxPane {
                session_name: "project".to_owned(),
                window_id: "@2".to_owned(),
                pane_id: "%2".to_owned(),
                kmux_role: None,
            },
            TmuxPane {
                session_name: "project".to_owned(),
                window_id: "@2".to_owned(),
                pane_id: "%3".to_owned(),
                kmux_role: Some(SIDEBAR_ROLE.to_owned()),
            },
        ];

        assert_eq!(sidebar_pane_for_window(&panes, "@2"), Some("%3"));
        assert_eq!(sidebar_pane_for_window(&panes, "@missing"), None);
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
            pane_active: false,
            window_active: false,
            session_attached: false,
            kmux_role: kmux_role.map(str::to_owned),
        }
    }
}
