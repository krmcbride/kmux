//! Sidebar lifecycle orchestration against tmux.
//!
//! This module enables, disables, refreshes, wakes, and runs the hidden sidebar
//! pane. It owns tmux hooks, global sidebar options, reconciliation locking, and
//! pane repair while keeping row modeling and rendering in sibling modules.

use std::collections::HashSet;
use std::io::IsTerminal;

use anyhow::Result;

use crate::agent::sidebar::app::SidebarApp;
use crate::agent::sidebar::candidates::{
    DEFAULT_WIDTH, SIDEBAR_ROLE, SidebarCandidateMatcher, sidebar_candidate_snapshots,
    sidebar_candidates_by_window, sidebar_width_cells,
};
use crate::agent::sidebar::commands::{
    shell_quote, sidebar_off_command, sidebar_refresh_command, sidebar_tui_command,
    sidebar_wake_hook_command,
};
use crate::agent::sidebar::model::SidebarIcons;
use crate::agent::sidebar::runtime::run_terminal_app;
use crate::config::{Config, SidebarSize};
use crate::state::StateStore;
use crate::tmux::{Tmux, TmuxPane, TmuxPaneSnapshot, TmuxSplitSize, TmuxWindow};

const SIDEBAR_ROLE_OPTION: &str = "@kmux_role";
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

/// Toggle the sidebar on or off based on the current tmux global option.
pub(super) fn toggle() -> Result<()> {
    let tmux = Tmux::from_env();
    if sidebar_enabled(&tmux)? {
        disable()
    } else {
        enable()
    }
}

/// Enable the sidebar, install hooks, and reconcile panes for existing windows.
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

/// Disable the sidebar, remove hooks, and kill recognized sidebar panes.
pub(super) fn disable() -> Result<()> {
    let tmux = Tmux::from_env();
    let _lock = SidebarLock::acquire(&tmux)?;
    tmux.unset_global_option(SIDEBAR_ENABLED_OPTION)?;
    remove_hooks(&tmux)?;
    tmux.unset_global_option(SIDEBAR_WIDTH_OPTION)?;
    let panes = tmux.list_pane_snapshots()?;
    let matcher = SidebarCandidateMatcher::new(None);
    for pane in sidebar_candidate_snapshots(&panes, &matcher) {
        let _ = tmux.kill_pane(&pane.pane_id);
    }
    print_user_message("sidebar disabled");
    Ok(())
}

/// Reconcile sidebar panes when the sidebar is currently enabled.
pub(super) fn refresh() -> Result<()> {
    let config = Config::load()?;
    let tmux = Tmux::from_env();
    let _lock = SidebarLock::acquire(&tmux)?;
    if sidebar_enabled(&tmux)? {
        reconcile_locked(&tmux, &config)?;
    }
    Ok(())
}

/// Wake the sidebar pane associated with a tmux window id.
pub(super) fn wake(window_id: &str) -> Result<()> {
    let tmux = Tmux::from_env();
    let panes = tmux.list_panes()?;
    if let Some(pane_id) = sidebar_pane_for_window(&panes, window_id) {
        let _ = tmux.send_key(pane_id, SIDEBAR_WAKE_KEY);
    }
    Ok(())
}

/// Notify all live sidebar panes that agent observations changed.
pub(super) fn notify_observation_changed(tmux: &Tmux) -> Result<()> {
    for pane in sidebar_panes(&tmux.list_panes()?) {
        let _ = tmux.send_key(&pane.pane_id, SIDEBAR_WAKE_KEY);
    }
    Ok(())
}

/// Run the hidden sidebar TUI process inside a tmux pane.
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
        config.status_icons.clone(),
        icons,
        working_frames,
        config.sidebar.idle_after_seconds(),
        config.sidebar.selection_hooks,
    );
    let disable_requested = run_terminal_app(&mut app)?;
    if disable_requested {
        request_disable_async()?;
    }
    Ok(())
}

// Give the hidden sidebar pane a stable title in tmux UI surfaces.
fn set_current_sidebar_pane_title(tmux: &Tmux) {
    if let Ok(pane_id) = std::env::var("TMUX_PANE") {
        let _ = tmux.set_pane_title(&pane_id, "kmux");
    }
}

// Disable from outside the current TUI process so terminal cleanup can finish first.
fn request_disable_async() -> Result<()> {
    let tmux = Tmux::from_env();
    let command = sidebar_off_command()?;
    tmux.stdout(["run-shell", "-b", &command])?;
    Ok(())
}

// Reconcile under the global sidebar lock: one sidebar pane per window, marked
// with kmux role metadata and running the current sidebar command.
fn reconcile_locked(tmux: &Tmux, config: &Config) -> Result<()> {
    let windows = unique_windows(tmux.list_windows(None)?);
    let command = sidebar_tui_command()?;
    let size = split_size(config);
    prune_extra_sidebars(tmux)?;
    let panes = tmux.list_pane_snapshots()?;
    let matcher = SidebarCandidateMatcher::new(Some(size));
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

// Remove duplicate sidebars only when kmux marked them in this tmux server lifetime.
fn prune_extra_sidebars(tmux: &Tmux) -> Result<()> {
    let panes = tmux.list_pane_snapshots()?;
    let matcher = SidebarCandidateMatcher::new(None);
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

// Repair an existing sidebar candidate after tmux restore or config changes.
fn heal_sidebar_pane(
    tmux: &Tmux,
    pane: &TmuxPaneSnapshot,
    size: TmuxSplitSize,
    command: &str,
) -> Result<()> {
    let width = sidebar_width_cells(size, pane.window_width);
    if width > 0 && pane.pane_width != width {
        let _ = tmux.resize_pane_width(&pane.pane_id, width);
    }
    if should_respawn_sidebar_pane(pane) {
        tmux.respawn_pane(&pane.pane_id, command)?;
    }
    tmux.set_pane_option(&pane.pane_id, SIDEBAR_ROLE_OPTION, SIDEBAR_ROLE)?;
    Ok(())
}

// Geometry-only matches are just claimable space; respawn before marking so an
// unrelated `kmux` process cannot become a sidebar by command name alone.
fn should_respawn_sidebar_pane(pane: &TmuxPaneSnapshot) -> bool {
    pane.kmux_role.as_deref() != Some(SIDEBAR_ROLE)
        || pane.current_command.as_deref() != Some("kmux")
}

// Install hooks that create sidebars for new windows and wake hidden panes when
// users switch focus.
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

// Only print lifecycle messages for interactive commands, not background hooks.
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

fn sidebar_pane_for_window<'a>(panes: &'a [TmuxPane], window_id: &str) -> Option<&'a str> {
    sidebar_panes(panes)
        .find(|pane| pane.window_id == window_id)
        .map(|pane| pane.pane_id.as_str())
}

// RAII wrapper for the tmux wait-for lock used around sidebar reconciliation.
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

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn unmarked_geometry_candidate_respawns_even_when_command_is_kmux() {
        let marked_running = pane_snapshot(Some(SIDEBAR_ROLE), Some("kmux"));
        let marked_stale = pane_snapshot(Some(SIDEBAR_ROLE), Some("fish"));
        let unmarked_running_kmux = pane_snapshot(None, Some("kmux"));

        assert!(!should_respawn_sidebar_pane(&marked_running));
        assert!(should_respawn_sidebar_pane(&marked_stale));
        assert!(should_respawn_sidebar_pane(&unmarked_running_kmux));
    }

    fn pane_snapshot(kmux_role: Option<&str>, current_command: Option<&str>) -> TmuxPaneSnapshot {
        TmuxPaneSnapshot {
            session_name: "project".to_owned(),
            window_id: "@1".to_owned(),
            window_index: "1".to_owned(),
            window_name: "main".to_owned(),
            pane_id: "%1".to_owned(),
            pane_index: "1".to_owned(),
            pane_left: 0,
            pane_width: DEFAULT_WIDTH,
            window_width: 120,
            title: None,
            current_command: current_command.map(str::to_owned),
            current_path: None,
            pane_active: false,
            window_active: false,
            session_attached: false,
            kmux_role: kmux_role.map(str::to_owned),
        }
    }
}
