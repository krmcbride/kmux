use std::io;
use std::time::Duration;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::agent::sidebar::app::SidebarApp;
use crate::agent::sidebar::render::render_sidebar_tui;

const REFRESH_INTERVAL: Duration = Duration::from_millis(750);

pub(super) fn run_terminal_app(app: &mut SidebarApp) -> Result<bool> {
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    app.refresh_rows();
    loop {
        terminal.draw(|frame| render_sidebar_tui(frame, app))?;
        if app.should_quit() {
            return Ok(app.disable_requested());
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sidebar::model::{SidebarRow, TEST_SLEEPING_ICON, agent_state};
    use crate::state::AgentStatus;

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

        assert!(app.should_quit());
        assert!(app.disable_requested());
    }
}
