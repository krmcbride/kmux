use std::collections::HashSet;
use std::ffi::OsString;
use std::io::IsTerminal;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use crate::agents::active_agents;
use crate::cli::{SidebarArgs, SidebarCommand};
use crate::config::{Config, SidebarSize};
use crate::state::{AgentState, StateStore};
use crate::tmux::{Tmux, TmuxPane, TmuxSplitSize, TmuxWindow};

const DEFAULT_WIDTH: u16 = 42;
const SIDEBAR_ROLE_OPTION: &str = "@kmux_role";
const SIDEBAR_ROLE: &str = "sidebar";
const SIDEBAR_ENABLED_OPTION: &str = "@kmux_sidebar_enabled";
const SIDEBAR_WIDTH_OPTION: &str = "@kmux_sidebar_width";
const SIDEBAR_LOCK_CHANNEL: &str = "kmux-sidebar-reconcile";
const SIDEBAR_HOOKS: &[&str] = &["after-new-window[90]", "after-new-session[90]"];

pub(crate) fn run(args: SidebarArgs) -> Result<()> {
    match args.command {
        Some(SidebarCommand::On) => enable(),
        Some(SidebarCommand::Off) => disable(),
        Some(SidebarCommand::Refresh) => refresh(),
        Some(SidebarCommand::Render) => render(),
        None => toggle(),
    }
}

fn toggle() -> Result<()> {
    let tmux = Tmux::from_env();
    if sidebar_enabled(&tmux)? {
        disable()
    } else {
        enable()
    }
}

fn enable() -> Result<()> {
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

fn disable() -> Result<()> {
    let tmux = Tmux::from_env();
    let _lock = SidebarLock::acquire(&tmux)?;
    tmux.unset_global_option(SIDEBAR_ENABLED_OPTION)?;
    remove_hooks(&tmux)?;
    for pane in sidebar_panes(&tmux.list_panes()?) {
        let _ = tmux.kill_pane(&pane.pane_id);
    }
    tmux.unset_global_option(SIDEBAR_WIDTH_OPTION)?;
    print_user_message("sidebar disabled");
    Ok(())
}

fn refresh() -> Result<()> {
    let config = Config::load()?;
    let tmux = Tmux::from_env();
    let _lock = SidebarLock::acquire(&tmux)?;
    if sidebar_enabled(&tmux)? {
        reconcile_locked(&tmux, &config)?;
    }
    Ok(())
}

fn render() -> Result<()> {
    let config = Config::load()?;
    let tmux = Tmux::from_env();
    let store = StateStore::new()?;
    let agents = active_agents(&store, &tmux)?;
    let width = render_width(&config, &tmux);
    print!("{}", render_agents(&agents, width, unix_now()));
    Ok(())
}

fn reconcile_locked(tmux: &Tmux, config: &Config) -> Result<()> {
    let windows = unique_windows(tmux.list_windows(None)?);
    prune_extra_sidebars(tmux)?;
    let mut windows_with_sidebar = sidebar_window_ids(&tmux.list_panes()?);
    let command = sidebar_loop_command()?;
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

fn sidebar_loop_command() -> Result<String> {
    let render = sidebar_command(["sidebar", "render"])?;
    let script = format!("while :; do printf '\\033[2J\\033[H'; {render}; sleep 2; done");
    Ok(format!("sh -c {}", shell_quote(&script)))
}

fn sidebar_refresh_command() -> Result<String> {
    sidebar_command(["sidebar", "refresh"])
}

fn sidebar_command<const N: usize>(args: [&str; N]) -> Result<String> {
    let executable = std::env::current_exe().context("failed to determine current executable")?;
    let mut parts = vec!["env".to_owned()];
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

pub(crate) fn render_agents(agents: &[AgentState], width: usize, now: u64) -> String {
    let width = width.max(12);
    let mut lines = vec![fit("kmux agents", width), fit("-----------", width)];
    if agents.is_empty() {
        lines.push(fit("No active agents", width));
        return finish_lines(lines);
    }

    for (index, agent) in agents.iter().enumerate() {
        if index > 0 {
            lines.push(String::new());
        }
        let name = agent
            .worktree_handle
            .as_deref()
            .or(agent.branch.as_deref())
            .unwrap_or(&agent.window_name);
        lines.push(fit(&format!("{} {name}", agent.icon), width));
        lines.push(fit(
            &format!(
                "  {} {}",
                agent.status.as_str(),
                compact_elapsed(now.saturating_sub(agent.updated_at))
            ),
            width,
        ));
        if let Some(branch) = &agent.branch {
            lines.push(fit(&format!("  {branch}"), width));
        }
        if let Some(title) = &agent.pane_title {
            lines.push(fit(&format!("  {title}"), width));
        }
    }

    finish_lines(lines)
}

fn finish_lines(lines: Vec<String>) -> String {
    let mut output = lines.join("\n");
    output.push('\n');
    output
}

fn fit(value: &str, width: usize) -> String {
    if value.chars().count() <= width {
        return value.to_owned();
    }
    if width <= 1 {
        return "~".to_owned();
    }
    let mut fitted = value.chars().take(width - 1).collect::<String>();
    fitted.push('~');
    fitted
}

fn compact_elapsed(seconds: u64) -> String {
    if seconds < 60 {
        "<1m".to_owned()
    } else if seconds < 60 * 60 {
        format!("{}m", seconds / 60)
    } else if seconds < 60 * 60 * 24 {
        format!("{}h", seconds / (60 * 60))
    } else {
        format!("{}d", seconds / (60 * 60 * 24))
    }
}

fn unix_now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::{AgentStatus, PaneKey};

    #[test]
    fn render_agents_includes_status_elapsed_branch_and_title() {
        let agents = vec![AgentState {
            pane_key: PaneKey::new_tmux("test", "%1"),
            status: AgentStatus::Waiting,
            icon: "?".to_owned(),
            updated_at: 120,
            pane_title: Some("Implement sidebar".to_owned()),
            pane_current_command: Some("nvim".to_owned()),
            worktree_handle: Some("feature-sidebar".to_owned()),
            worktree_path: Some("/repo__worktrees/feature-sidebar".to_owned()),
            branch: Some("feature/sidebar".to_owned()),
            session_name: "project".to_owned(),
            window_name: "kmux-feature-sidebar".to_owned(),
            window_id: "@1".to_owned(),
        }];

        let output = render_agents(&agents, 24, 300);

        assert!(output.contains("kmux agents"));
        assert!(output.contains("? feature-sidebar"));
        assert!(output.contains("waiting 3m"));
        assert!(output.contains("feature/sidebar"));
        assert!(output.contains("Implement sidebar"));
    }

    #[test]
    fn render_agents_truncates_to_width() {
        let output = render_agents(&[], 12, 0);

        assert!(output.lines().all(|line| line.chars().count() <= 12));
        assert!(output.contains("No active a~"));
    }

    #[test]
    fn sidebar_loop_runs_under_sh_not_user_shell() -> Result<()> {
        let command = sidebar_loop_command()?;

        assert!(command.starts_with("sh -c "));
        assert!(command.contains("while :; do"));
        assert!(command.contains(" sidebar render"));
        Ok(())
    }
}
