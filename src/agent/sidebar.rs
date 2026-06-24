use std::collections::HashSet;
use std::ffi::OsString;
use std::io::{self, IsTerminal};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{
    Frame, Terminal,
    backend::CrosstermBackend,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::agent::active::active_agents;
use crate::cli::{SidebarArgs, SidebarCommand};
use crate::config::{Config, SidebarSize};
use crate::state::{AgentState, AgentStatus, StateStore};
use crate::tmux::{Tmux, TmuxPane, TmuxSplitSize, TmuxWindow};

const DEFAULT_WIDTH: u16 = 42;
const SIDEBAR_ROLE_OPTION: &str = "@kmux_role";
const SIDEBAR_ROLE: &str = "sidebar";
const SIDEBAR_ENABLED_OPTION: &str = "@kmux_sidebar_enabled";
const SIDEBAR_WIDTH_OPTION: &str = "@kmux_sidebar_width";
const SIDEBAR_LOCK_CHANNEL: &str = "kmux-sidebar-reconcile";
const SIDEBAR_HOOKS: &[&str] = &["after-new-window[90]", "after-new-session[90]"];
const REFRESH_INTERVAL: Duration = Duration::from_millis(750);
const STALE_AFTER_SECONDS: u64 = 60 * 60;
const SELECTED_BG: Color = Color::Rgb(40, 48, 62);
const TEXT_FG: Color = Color::Rgb(205, 214, 244);
const DIM_FG: Color = Color::Rgb(108, 112, 134);
const BORDER_FG: Color = Color::Rgb(58, 74, 94);
const WORKING_FG: Color = Color::Rgb(120, 225, 213);
const WAITING_FG: Color = Color::Rgb(203, 166, 247);
const DONE_FG: Color = Color::Rgb(166, 218, 149);

pub fn run(args: SidebarArgs) -> Result<()> {
    match args.command {
        Some(SidebarCommand::On) => enable(),
        Some(SidebarCommand::Off) => disable(),
        Some(SidebarCommand::Refresh) => refresh(),
        Some(SidebarCommand::Render) => render(),
        Some(SidebarCommand::Run) => run_tui(),
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
    tmux.unset_global_option(SIDEBAR_WIDTH_OPTION)?;
    for pane in sidebar_panes(&tmux.list_panes()?) {
        let _ = tmux.kill_pane(&pane.pane_id);
    }
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

fn run_tui() -> Result<()> {
    let tmux = Tmux::from_env();
    set_current_sidebar_pane_title(&tmux);
    let config = Config::load()?;
    let store = StateStore::new()?;
    let mut app = SidebarApp::new(tmux, store, config.status_icons.sleeping().to_owned());
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

fn run_terminal_app(app: &mut SidebarApp) -> Result<bool> {
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    app.refresh_rows();
    loop {
        terminal.draw(|frame| render_sidebar_tui(frame, app))?;
        if app.should_quit {
            return Ok(app.disable_requested);
        }

        if event::poll(REFRESH_INTERVAL)? {
            process_tui_event(event::read()?, app);
        } else {
            app.refresh_rows();
        }
    }
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

fn process_tui_event(event: Event, app: &mut SidebarApp) {
    match event {
        Event::Key(key) if key.kind == KeyEventKind::Press => match key.code {
            KeyCode::Char('q') | KeyCode::Esc => app.request_disable(),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                app.request_disable();
            }
            KeyCode::Char('j') | KeyCode::Down => app.next(),
            KeyCode::Char('k') | KeyCode::Up => app.previous(),
            KeyCode::Char('g') => app.select_first(),
            KeyCode::Char('G') => app.select_last(),
            KeyCode::Enter => app.jump_to_selected(),
            _ => {}
        },
        Event::Resize(_, _) => app.refresh_rows(),
        _ => {}
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SelectionMode {
    FollowHost,
    Manual,
}

struct SidebarApp {
    tmux: Tmux,
    store: StateStore,
    sleeping_icon: String,
    rows: Vec<SidebarRow>,
    list_state: ListState,
    sidebar_pane_id: Option<String>,
    host_window_id: Option<String>,
    selection_mode: SelectionMode,
    selected_pane_id: Option<String>,
    selected_window_id: Option<String>,
    last_error: Option<String>,
    should_quit: bool,
    disable_requested: bool,
}

impl SidebarApp {
    fn new(tmux: Tmux, store: StateStore, sleeping_icon: String) -> Self {
        let context = tmux.current_context().ok().flatten();
        let host_window_id = context.as_ref().map(|context| context.window_id.clone());
        let sidebar_pane_id = context.map(|context| context.pane_id);
        Self {
            tmux,
            store,
            sleeping_icon,
            rows: Vec::new(),
            list_state: ListState::default(),
            sidebar_pane_id,
            host_window_id,
            selection_mode: SelectionMode::FollowHost,
            selected_pane_id: None,
            selected_window_id: None,
            last_error: None,
            should_quit: false,
            disable_requested: false,
        }
    }

    #[cfg(test)]
    fn test(host_window_id: Option<&str>, rows: Vec<SidebarRow>) -> Self {
        let mut app = Self {
            tmux: Tmux::new(),
            store: test_state_store(),
            sleeping_icon: TEST_SLEEPING_ICON.to_owned(),
            rows,
            list_state: ListState::default(),
            sidebar_pane_id: None,
            host_window_id: host_window_id.map(str::to_owned),
            selection_mode: SelectionMode::FollowHost,
            selected_pane_id: None,
            selected_window_id: None,
            last_error: None,
            should_quit: false,
            disable_requested: false,
        };
        app.sync_selection();
        app
    }

    fn refresh_rows(&mut self) {
        let sidebar_has_focus = self.sidebar_has_focus();
        match active_agents(&self.store, &self.tmux) {
            Ok(agents) => {
                self.rows = build_rows(&agents, unix_now(), &self.sleeping_icon);
                self.last_error = None;
                self.update_selection_mode_for_focus(sidebar_has_focus);
                self.sync_selection();
            }
            Err(error) => {
                self.last_error = Some(error.to_string());
            }
        }
    }

    fn sync_selection(&mut self) {
        if self.rows.is_empty() {
            self.list_state.select(None);
            return;
        }

        let selected = match self.selection_mode {
            SelectionMode::FollowHost => self
                .host_window_id
                .as_deref()
                .and_then(|window_id| row_index_by_window(&self.rows, window_id))
                .unwrap_or(0),
            SelectionMode::Manual => self
                .selected_pane_id
                .as_deref()
                .and_then(|pane_id| row_index_by_pane(&self.rows, pane_id))
                .or_else(|| {
                    self.selected_window_id
                        .as_deref()
                        .and_then(|window_id| row_index_by_window(&self.rows, window_id))
                })
                .or_else(|| {
                    self.list_state
                        .selected()
                        .filter(|idx| *idx < self.rows.len())
                })
                .unwrap_or(0),
        };
        self.select_index_internal(selected);
    }

    fn request_disable(&mut self) {
        self.disable_requested = true;
        self.should_quit = true;
    }

    fn next(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let selected = self.list_state.selected().unwrap_or(0);
        let next = (selected + 1).min(self.rows.len() - 1);
        self.select_index_manual(next);
    }

    fn previous(&mut self) {
        if self.rows.is_empty() {
            return;
        }
        let selected = self.list_state.selected().unwrap_or(0);
        self.select_index_manual(selected.saturating_sub(1));
    }

    fn select_first(&mut self) {
        if !self.rows.is_empty() {
            self.select_index_manual(0);
        }
    }

    fn select_last(&mut self) {
        if !self.rows.is_empty() {
            self.select_index_manual(self.rows.len() - 1);
        }
    }

    fn jump_to_selected(&mut self) {
        let Some(row) = self.selected_row().cloned() else {
            return;
        };
        if let Err(error) = self.select_row_target(&row) {
            self.refresh_rows();
            self.last_error = Some(format!("jump failed: {error}"));
        } else {
            self.selection_mode = SelectionMode::FollowHost;
        }
    }

    fn select_row_target(&self, row: &SidebarRow) -> Result<()> {
        self.tmux.select_window_id(&row.window_id)?;
        let _ = self.tmux.switch_client_to_session(&row.session_name);
        self.tmux.select_pane(&row.pane_id)
    }

    fn selected_row(&self) -> Option<&SidebarRow> {
        self.list_state
            .selected()
            .and_then(|index| self.rows.get(index))
    }

    fn select_index_manual(&mut self, index: usize) {
        self.selection_mode = SelectionMode::Manual;
        self.select_index_internal(index);
    }

    fn select_index_internal(&mut self, index: usize) {
        let index = index.min(self.rows.len().saturating_sub(1));
        self.list_state.select(Some(index));
        if let Some(row) = self.rows.get(index) {
            self.selected_pane_id = Some(row.pane_id.clone());
            self.selected_window_id = Some(row.window_id.clone());
        }
    }

    fn sidebar_has_focus(&self) -> bool {
        self.sidebar_pane_id
            .as_deref()
            .is_some_and(|pane_id| self.tmux.pane_has_focus(pane_id).unwrap_or(false))
    }

    fn update_selection_mode_for_focus(&mut self, sidebar_has_focus: bool) {
        if !sidebar_has_focus {
            self.selection_mode = SelectionMode::FollowHost;
        }
    }
}

#[cfg(test)]
const TEST_SLEEPING_ICON: &str = "z";

#[cfg(test)]
fn test_state_store() -> StateStore {
    StateStore::test_with_path(std::env::temp_dir().join(format!(
        "kmux-sidebar-test-empty-{}-{}",
        std::process::id(),
        unix_now()
    )))
    .expect("test state store should be created")
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SidebarRow {
    status: AgentStatus,
    icon: String,
    primary: String,
    secondary: String,
    title: String,
    elapsed: String,
    is_stale: bool,
    session_name: String,
    window_id: String,
    pane_id: String,
}

impl SidebarRow {
    fn from_agent(agent: &AgentState, now: u64, sleeping_icon: &str) -> Self {
        let primary = agent
            .worktree_handle
            .as_deref()
            .or(agent.branch.as_deref())
            .unwrap_or(&agent.window_name)
            .to_owned();
        let secondary = secondary_label(agent, &primary);
        let title = agent
            .pane_title
            .as_deref()
            .filter(|title| *title != primary && *title != secondary)
            .or(agent.pane_current_command.as_deref())
            .unwrap_or_default()
            .to_owned();
        let age = now.saturating_sub(agent.updated_at);
        let is_stale = agent.status == AgentStatus::Done && age >= STALE_AFTER_SECONDS;

        Self {
            status: agent.status,
            icon: if is_stale {
                sleeping_icon.to_owned()
            } else {
                agent.icon.clone()
            },
            primary,
            secondary,
            title,
            elapsed: compact_elapsed(age),
            is_stale,
            session_name: agent.session_name.clone(),
            window_id: agent.window_id.clone(),
            pane_id: agent.pane_key.pane_id.clone(),
        }
    }
}

fn secondary_label(agent: &AgentState, primary: &str) -> String {
    match agent.branch.as_deref().filter(|branch| *branch != primary) {
        Some(branch) => format!("{} / {branch}", agent.session_name),
        None => agent.session_name.clone(),
    }
}

fn build_rows(agents: &[AgentState], now: u64, sleeping_icon: &str) -> Vec<SidebarRow> {
    agents
        .iter()
        .map(|agent| SidebarRow::from_agent(agent, now, sleeping_icon))
        .collect()
}

fn row_index_by_window(rows: &[SidebarRow], window_id: &str) -> Option<usize> {
    rows.iter().position(|row| row.window_id == window_id)
}

fn row_index_by_pane(rows: &[SidebarRow], pane_id: &str) -> Option<usize> {
    rows.iter().position(|row| row.pane_id == pane_id)
}

fn render_sidebar_tui(frame: &mut Frame, app: &mut SidebarApp) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    let mut list_area = area;
    if let Some(error) = &app.last_error {
        let warning = fit_width(&format!("error: {error}"), area.width as usize);
        frame.render_widget(
            Paragraph::new(warning).style(Style::default().fg(WAITING_FG)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        list_area.y = list_area.y.saturating_add(1);
        list_area.height = list_area.height.saturating_sub(1);
    }

    if app.rows.is_empty() {
        render_no_agents(frame, list_area);
        return;
    }

    let items = app
        .rows
        .iter()
        .enumerate()
        .map(|(index, row)| {
            tile_item(
                row,
                index > 0,
                index + 1 == app.rows.len(),
                list_area.width as usize,
                is_selected(app, index),
            )
        })
        .collect::<Vec<_>>();
    let list = List::new(items);
    frame.render_stateful_widget(list, list_area, &mut app.list_state);
}

fn render_no_agents(frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let y = area.y + area.height / 2;
    frame.render_widget(
        Paragraph::new("No active agents")
            .style(Style::default().fg(DIM_FG))
            .alignment(Alignment::Center),
        Rect::new(area.x, y, area.width, 1),
    );
}

fn is_selected(app: &SidebarApp, index: usize) -> bool {
    app.list_state.selected() == Some(index)
}

fn tile_item(
    row: &SidebarRow,
    include_separator: bool,
    include_bottom_separator: bool,
    width: usize,
    selected: bool,
) -> ListItem<'static> {
    let mut lines = Vec::new();
    if include_separator {
        lines.push(separator_line(width));
    }

    lines.push(tile_line(row, LineKind::Primary, width, selected));
    lines.push(tile_line(row, LineKind::Secondary, width, selected));
    lines.push(tile_line(row, LineKind::Title, width, selected));
    if include_bottom_separator {
        lines.push(separator_line(width));
    }
    ListItem::new(lines)
}

#[derive(Debug, Clone, Copy)]
enum LineKind {
    Primary,
    Secondary,
    Title,
}

fn separator_line(width: usize) -> Line<'static> {
    Line::from(Span::styled(
        "─".repeat(width),
        Style::default().fg(BORDER_FG),
    ))
}

fn tile_line(row: &SidebarRow, kind: LineKind, width: usize, selected: bool) -> Line<'static> {
    let bg = selected.then_some(SELECTED_BG);
    if width < 6 {
        return narrow_tile_line(row, kind, width, bg);
    }

    let body_width = width - 6;
    let stripe_style = style_with_bg(Style::default().fg(status_color(row)), bg);
    let text_style = row_text_style(row, selected);
    let dim_style = style_with_bg(Style::default().fg(DIM_FG), bg);
    let status_style = style_with_bg(Style::default().fg(status_color(row)), bg);

    let mut spans = vec![Span::styled("▌ ", stripe_style)];
    match kind {
        LineKind::Primary => spans.push(Span::styled(fixed_width(&row.icon, 2), status_style)),
        LineKind::Secondary | LineKind::Title => spans.push(Span::styled("  ", dim_style)),
    }
    spans.push(Span::styled(" ", style_with_bg(Style::default(), bg)));

    let body_spans = match kind {
        LineKind::Primary => line_with_right(
            &row.primary,
            &row.elapsed,
            body_width,
            text_style.add_modifier(Modifier::BOLD),
            dim_style,
            bg,
        ),
        LineKind::Secondary => line_with_right(
            &row.secondary,
            row.status.as_str(),
            body_width,
            dim_style,
            status_style,
            bg,
        ),
        LineKind::Title => line_with_right(&row.title, "", body_width, dim_style, dim_style, bg),
    };
    spans.extend(body_spans);
    spans.push(Span::styled(" ", style_with_bg(Style::default(), bg)));
    pad_spans_to_width(&mut spans, width, bg);
    Line::from(spans)
}

fn narrow_tile_line(
    row: &SidebarRow,
    kind: LineKind,
    width: usize,
    bg: Option<Color>,
) -> Line<'static> {
    let style = style_with_bg(Style::default().fg(status_color(row)), bg);
    let text = match kind {
        LineKind::Primary => format!("{} {}", row.icon, row.primary),
        LineKind::Secondary => row.status.as_str().to_owned(),
        LineKind::Title => row.title.clone(),
    };
    Line::from(Span::styled(fixed_width(&text, width), style))
}

fn line_with_right(
    left: &str,
    right: &str,
    width: usize,
    left_style: Style,
    right_style: Style,
    bg: Option<Color>,
) -> Vec<Span<'static>> {
    if width == 0 {
        return Vec::new();
    }
    let right_width = display_width(right);
    if right.trim().is_empty() || right_width + 1 >= width {
        return vec![Span::styled(fixed_width(left, width), left_style)];
    }

    let left_width = width.saturating_sub(right_width + 1);
    let left_text = fit_width(left, left_width);
    let spacer_width = width.saturating_sub(display_width(&left_text) + right_width);
    vec![
        Span::styled(left_text, left_style),
        Span::styled(
            " ".repeat(spacer_width),
            style_with_bg(Style::default(), bg),
        ),
        Span::styled(right.to_owned(), right_style),
    ]
}

fn status_color(row: &SidebarRow) -> Color {
    if row.is_stale {
        return DIM_FG;
    }
    match row.status {
        AgentStatus::Working => WORKING_FG,
        AgentStatus::Waiting => WAITING_FG,
        AgentStatus::Done => DONE_FG,
    }
}

fn row_text_style(row: &SidebarRow, selected: bool) -> Style {
    let fg = if row.is_stale { DIM_FG } else { TEXT_FG };
    let mut style = Style::default().fg(fg);
    if selected {
        style = style.bg(SELECTED_BG);
    }
    if row.is_stale {
        style = style.add_modifier(Modifier::DIM);
    }
    style
}

fn style_with_bg(style: Style, bg: Option<Color>) -> Style {
    if let Some(color) = bg {
        style.bg(color)
    } else {
        style
    }
}

fn pad_spans_to_width(spans: &mut Vec<Span<'static>>, width: usize, bg: Option<Color>) {
    let current = spans
        .iter()
        .map(|span| display_width(span.content.as_ref()))
        .sum::<usize>();
    if current < width {
        spans.push(Span::styled(
            " ".repeat(width - current),
            style_with_bg(Style::default(), bg),
        ));
    }
}

fn render_agents(agents: &[AgentState], width: usize, now: u64) -> String {
    let width = width.max(12);
    let config = Config::default();
    let mut lines = vec![
        fixed_width("kmux agents", width),
        fixed_width("-----------", width),
    ];
    if agents.is_empty() {
        lines.push(fixed_width("No active agents", width));
        return finish_lines(lines);
    }

    for (index, row) in build_rows(agents, now, config.status_icons.sleeping())
        .iter()
        .enumerate()
    {
        if index > 0 {
            lines.push(String::new());
        }
        lines.push(fixed_width(&format!("{} {}", row.icon, row.primary), width));
        lines.push(fixed_width(
            &format!("  {} {}", row.status.as_str(), row.elapsed),
            width,
        ));
        lines.push(fixed_width(&format!("  {}", row.secondary), width));
        if !row.title.is_empty() {
            lines.push(fixed_width(&format!("  {}", row.title), width));
        }
    }

    finish_lines(lines)
}

fn finish_lines(lines: Vec<String>) -> String {
    let mut output = lines.join("\n");
    output.push('\n');
    output
}

fn fixed_width(value: &str, width: usize) -> String {
    let mut value = fit_width(value, width);
    let current = display_width(&value);
    if current < width {
        value.push_str(&" ".repeat(width - current));
    }
    value
}

fn fit_width(value: &str, width: usize) -> String {
    if display_width(value) <= width {
        return value.to_owned();
    }
    if width == 0 {
        return String::new();
    }
    if width == 1 {
        return "~".to_owned();
    }

    let target = width - 1;
    let mut output = String::new();
    let mut used = 0;
    for ch in value.chars() {
        let ch_width = UnicodeWidthChar::width(ch).unwrap_or(1);
        if used + ch_width > target {
            break;
        }
        output.push(ch);
        used += ch_width;
    }
    output.push('~');
    output
}

fn display_width(value: &str) -> usize {
    UnicodeWidthStr::width(value)
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
    use crate::state::PaneKey;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn render_agents_includes_status_elapsed_branch_and_title() {
        let agents = vec![agent_state(AgentStatus::Waiting, 120, "@1", "%1")];

        let output = render_agents(&agents, 28, 300);

        assert!(output.contains("kmux agents"));
        assert!(output.contains("? feature-sidebar"));
        assert!(output.contains("waiting 3m"));
        assert!(output.contains("project / feature/sidebar"));
        assert!(output.contains("Implement sidebar"));
    }

    #[test]
    fn render_agents_truncates_to_width() {
        let output = render_agents(&[], 12, 0);

        assert!(output.lines().all(|line| display_width(line) <= 12));
        assert!(output.contains("No active a~"));
    }

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
    fn row_model_prefers_worktree_and_marks_old_done_stale() {
        let agents = vec![agent_state(AgentStatus::Done, 0, "@1", "%1")];
        let rows = build_rows(&agents, STALE_AFTER_SECONDS + 1, TEST_SLEEPING_ICON);

        assert_eq!(rows[0].primary, "feature-sidebar");
        assert_eq!(rows[0].secondary, "project / feature/sidebar");
        assert_eq!(rows[0].title, "Implement sidebar");
        assert_eq!(rows[0].elapsed, "1h");
        assert_eq!(rows[0].icon, TEST_SLEEPING_ICON);
        assert!(rows[0].is_stale);
    }

    #[test]
    fn row_model_keeps_old_waiting_agent_active() {
        let agents = vec![agent_state(AgentStatus::Waiting, 0, "@1", "%1")];
        let rows = build_rows(&agents, STALE_AFTER_SECONDS + 1, TEST_SLEEPING_ICON);

        assert_eq!(rows[0].icon, "?");
        assert!(!rows[0].is_stale);
    }

    #[test]
    fn selection_follows_host_window_then_manual_navigation_takes_over() {
        let rows = vec![
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Working, 100, "@1", "%1"),
                100,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Waiting, 100, "@2", "%2"),
                100,
                TEST_SLEEPING_ICON,
            ),
        ];
        let mut app = SidebarApp::test(Some("@2"), rows);

        assert_eq!(app.list_state.selected(), Some(1));

        app.previous();

        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn manual_selection_survives_empty_refresh_and_pane_id_change() {
        let rows = vec![
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Working, 100, "@1", "%1"),
                100,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Waiting, 100, "@2", "%2"),
                100,
                TEST_SLEEPING_ICON,
            ),
        ];
        let mut app = SidebarApp::test(Some("@1"), rows);

        app.next();
        assert_eq!(app.list_state.selected(), Some(1));

        app.rows = Vec::new();
        app.sync_selection();
        app.rows = vec![
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Working, 200, "@1", "%10"),
                200,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Waiting, 200, "@2", "%20"),
                200,
                TEST_SLEEPING_ICON,
            ),
        ];
        app.sync_selection();

        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(app.list_state.selected(), Some(1));
        assert_eq!(app.selected_window_id.as_deref(), Some("@2"));
        assert_eq!(app.selected_pane_id.as_deref(), Some("%20"));
    }

    #[test]
    fn manual_selection_returns_to_host_when_sidebar_loses_focus() {
        let rows = vec![
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Working, 100, "@1", "%1"),
                100,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Waiting, 100, "@2", "%2"),
                100,
                TEST_SLEEPING_ICON,
            ),
        ];
        let mut app = SidebarApp::test(Some("@1"), rows);

        app.next();
        assert_eq!(app.selection_mode, SelectionMode::Manual);
        assert_eq!(app.list_state.selected(), Some(1));

        app.update_selection_mode_for_focus(false);
        app.sync_selection();

        assert_eq!(app.selection_mode, SelectionMode::FollowHost);
        assert_eq!(app.list_state.selected(), Some(0));
        assert_eq!(app.selected_window_id.as_deref(), Some("@1"));
        assert_eq!(app.selected_pane_id.as_deref(), Some("%1"));
    }

    #[test]
    fn quit_keys_request_disable_without_directly_exiting_test_app() {
        let rows = vec![SidebarRow::from_agent(
            &agent_state(AgentStatus::Waiting, 100, "@1", "%1"),
            100,
            TEST_SLEEPING_ICON,
        )];
        let mut app = SidebarApp::test(Some("@1"), rows);

        process_tui_event(
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )),
            &mut app,
        );

        assert!(app.should_quit);
        assert!(app.disable_requested);
    }

    #[test]
    fn jump_failure_is_reported_without_panicking_or_quitting() {
        let rows = vec![SidebarRow::from_agent(
            &agent_state(AgentStatus::Waiting, 100, "not-a-window", "%missing"),
            100,
            TEST_SLEEPING_ICON,
        )];
        let mut app = SidebarApp::test(Some("not-a-window"), rows);

        app.jump_to_selected();

        assert!(!app.should_quit);
        assert!(
            app.last_error
                .as_deref()
                .is_some_and(|error| error.contains("jump failed"))
        );
    }

    #[test]
    fn ratatui_renderer_draws_selected_tile_with_expected_text() -> Result<()> {
        let rows = vec![SidebarRow::from_agent(
            &agent_state(AgentStatus::Waiting, 120, "@1", "%1"),
            300,
            TEST_SLEEPING_ICON,
        )];
        let backend = TestBackend::new(42, 5);
        let mut terminal = Terminal::new(backend)?;
        let mut app = SidebarApp::test(Some("@1"), rows);

        terminal.draw(|frame| render_sidebar_tui(frame, &mut app))?;

        let buffer = terminal.backend().buffer();
        let text = buffer_text(buffer, 42, 5);
        assert!(text.contains("feature-sidebar"));
        assert!(text.contains("3m"));
        assert!(text.contains("waiting"));
        assert!(text.contains("Implement sidebar"));
        assert_eq!(buffer[(0, 0)].bg, SELECTED_BG);
        Ok(())
    }

    #[test]
    fn ratatui_renderer_draws_final_separator() -> Result<()> {
        let rows = vec![SidebarRow::from_agent(
            &agent_state(AgentStatus::Waiting, 120, "@1", "%1"),
            300,
            TEST_SLEEPING_ICON,
        )];
        let backend = TestBackend::new(42, 4);
        let mut terminal = Terminal::new(backend)?;
        let mut app = SidebarApp::test(Some("@1"), rows);

        terminal.draw(|frame| render_sidebar_tui(frame, &mut app))?;

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 3)].symbol(), "─");
        assert_eq!(buffer[(41, 3)].symbol(), "─");
        Ok(())
    }

    #[test]
    fn ratatui_renderer_truncates_narrow_tiles() -> Result<()> {
        let mut agent = agent_state(AgentStatus::Done, 120, "@1", "%1");
        agent.worktree_handle = Some("very-long-sidebar-worktree-name".to_owned());
        let rows = vec![SidebarRow::from_agent(&agent, 300, TEST_SLEEPING_ICON)];
        let backend = TestBackend::new(18, 4);
        let mut terminal = Terminal::new(backend)?;
        let mut app = SidebarApp::test(Some("@1"), rows);

        terminal.draw(|frame| render_sidebar_tui(frame, &mut app))?;

        let buffer = terminal.backend().buffer();
        let text = buffer_text(buffer, 18, 4);
        assert!(text.contains("very-lon~"));
        assert!(!text.contains("very-long-sidebar"));
        Ok(())
    }

    #[test]
    fn narrow_tile_lines_do_not_exceed_requested_width() {
        let row = SidebarRow::from_agent(
            &agent_state(AgentStatus::Done, 120, "@1", "%1"),
            300,
            TEST_SLEEPING_ICON,
        );

        for width in 0..6 {
            for kind in [LineKind::Primary, LineKind::Secondary, LineKind::Title] {
                let line = tile_line(&row, kind, width, true);
                assert!(line_width(&line) <= width);
            }
        }
    }

    fn agent_state(
        status: AgentStatus,
        updated_at: u64,
        window_id: &str,
        pane_id: &str,
    ) -> AgentState {
        AgentState {
            pane_key: PaneKey::new_tmux("test", pane_id),
            status,
            icon: "?".to_owned(),
            updated_at,
            pane_title: Some("Implement sidebar".to_owned()),
            pane_current_command: Some("nvim".to_owned()),
            worktree_handle: Some("feature-sidebar".to_owned()),
            worktree_path: Some("/repo__worktrees/feature-sidebar".to_owned()),
            branch: Some("feature/sidebar".to_owned()),
            session_name: "project".to_owned(),
            window_name: "kmux-feature-sidebar".to_owned(),
            window_id: window_id.to_owned(),
        }
    }

    fn buffer_text(buffer: &ratatui::buffer::Buffer, width: u16, height: u16) -> String {
        (0..height)
            .flat_map(|y| (0..width).map(move |x| buffer[(x, y)].symbol()))
            .collect::<String>()
    }

    fn line_width(line: &Line<'_>) -> usize {
        line.spans
            .iter()
            .map(|span| display_width(span.content.as_ref()))
            .sum()
    }
}
