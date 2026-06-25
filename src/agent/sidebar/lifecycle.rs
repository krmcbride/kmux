use std::collections::HashSet;
use std::ffi::OsString;
use std::io::IsTerminal;

use anyhow::{Context, Result};

use crate::agent::active::active_agents;
use crate::agent::sidebar::app::SidebarApp;
use crate::agent::sidebar::render::render_agents;
use crate::agent::sidebar::runtime::run_terminal_app;
use crate::config::{Config, SidebarSize};
use crate::state::{StateStore, now_unix_seconds};
use crate::tmux::{Tmux, TmuxPane, TmuxSplitSize, TmuxWindow};

const DEFAULT_WIDTH: u16 = 42;
const SIDEBAR_ROLE_OPTION: &str = "@kmux_role";
const SIDEBAR_ROLE: &str = "sidebar";
const SIDEBAR_ENABLED_OPTION: &str = "@kmux_sidebar_enabled";
const SIDEBAR_WIDTH_OPTION: &str = "@kmux_sidebar_width";
const SIDEBAR_LOCK_CHANNEL: &str = "kmux-sidebar-reconcile";
const SIDEBAR_HOOKS: &[&str] = &["after-new-window[90]", "after-new-session[90]"];

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
    tmux.unset_global_option(SIDEBAR_ENABLED_OPTION)?;
    remove_hooks(&tmux)?;
    tmux.unset_global_option(SIDEBAR_WIDTH_OPTION)?;
    for pane in sidebar_panes(&tmux.list_panes()?) {
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

pub(super) fn render() -> Result<()> {
    let config = Config::load()?;
    let tmux = Tmux::from_env();
    let store = StateStore::new()?;
    let agents = active_agents(&store, &tmux)?;
    let width = render_width(&config, &tmux);
    print!("{}", render_agents(&agents, width, now_unix_seconds()));
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
    let mut app = SidebarApp::new(
        tmux,
        store,
        config.status_icons.sleeping().to_owned(),
        working_frames,
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
    prune_extra_sidebars(tmux)?;
    let mut windows_with_sidebar = sidebar_window_ids(&tmux.list_panes()?);
    let command = sidebar_tui_command()?;
    let size = split_size(config);

    for window in windows {
        if windows_with_sidebar.contains(&window.window_id) {
            continue;
        }
        let pane_id = tmux.split_window_left(&window.window_id, size, &command)?;
        tmux.set_pane_option(&pane_id, SIDEBAR_ROLE_OPTION, SIDEBAR_ROLE)?;
        windows_with_sidebar.insert(window.window_id);
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

fn prune_extra_sidebars(tmux: &Tmux) -> Result<()> {
    let mut seen_panes = HashSet::new();
    let mut seen_windows = HashSet::new();
    for pane in sidebar_panes(&tmux.list_panes()?) {
        if !seen_panes.insert(pane.pane_id.clone()) {
            continue;
        }
        if !seen_windows.insert(pane.window_id.clone()) {
            let _ = tmux.kill_pane(&pane.pane_id);
        }
    }
    Ok(())
}

fn sidebar_window_ids(panes: &[TmuxPane]) -> HashSet<String> {
    sidebar_panes(panes)
        .map(|pane| pane.window_id.clone())
        .collect()
}

fn install_hooks(tmux: &Tmux) -> Result<()> {
    let command = format!("run-shell -b {}", shell_quote(&sidebar_refresh_command()?));
    for hook in SIDEBAR_HOOKS {
        tmux.set_hook(hook, &command)?;
    }
    Ok(())
}

fn remove_hooks(tmux: &Tmux) -> Result<()> {
    for hook in SIDEBAR_HOOKS {
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
    match config
        .sidebar
        .width
        .unwrap_or(SidebarSize::Absolute(DEFAULT_WIDTH))
    {
        SidebarSize::Absolute(width) => TmuxSplitSize::Cells(width),
        SidebarSize::Percent(percent) => TmuxSplitSize::Percent(percent),
    }
}

fn render_width(config: &Config, tmux: &Tmux) -> usize {
    if let Some(width) = std::env::var("TMUX_PANE")
        .ok()
        .and_then(|pane_id| tmux.pane_width(&pane_id).ok().flatten())
    {
        return usize::from(width);
    }

    match config
        .sidebar
        .width
        .unwrap_or(SidebarSize::Absolute(DEFAULT_WIDTH))
    {
        SidebarSize::Absolute(width) => usize::from(width),
        SidebarSize::Percent(_) => usize::from(DEFAULT_WIDTH),
    }
}

fn configured_width_label(config: &Config) -> String {
    match config
        .sidebar
        .width
        .unwrap_or(SidebarSize::Absolute(DEFAULT_WIDTH))
    {
        SidebarSize::Absolute(width) => width.to_string(),
        SidebarSize::Percent(percent) => format!("{percent}%"),
    }
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
}
