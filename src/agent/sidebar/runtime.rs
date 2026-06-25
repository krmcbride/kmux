use std::io;
use std::time::{Duration, Instant};

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyCode, KeyEventKind, KeyModifiers},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};

use crate::agent::sidebar::app::SidebarApp;
use crate::agent::sidebar::render::render_sidebar_tui;

const MODEL_REFRESH_INTERVAL: Duration = Duration::from_millis(750);
const SPINNER_INTERVAL: Duration = Duration::from_millis(80);

pub(super) fn run_terminal_app(app: &mut SidebarApp) -> Result<bool> {
    enable_raw_mode()?;
    let _guard = TerminalGuard;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    app.refresh_rows();
    let mut schedule = RefreshSchedule::new(Instant::now());

    loop {
        if app.should_quit() {
            return Ok(app.disable_requested());
        }

        let now = Instant::now();
        if schedule.model_due(now) {
            let was_animating = app.should_animate_spinner();
            app.refresh_rows();
            let now = Instant::now();
            schedule.reset_model(now);
            if !was_animating || !app.should_animate_spinner() {
                schedule.reset_spinner(now);
            }
        }

        let now = Instant::now();
        if app.should_animate_spinner() && schedule.spinner_due(now) {
            app.tick_spinner();
            schedule.reset_spinner(Instant::now());
        }

        if app.window_visible() {
            terminal.draw(|frame| render_sidebar_tui(frame, app))?;
            if app.should_quit() {
                return Ok(app.disable_requested());
            }
        }

        let now = Instant::now();
        let timeout = schedule.next_timeout(now, app.should_animate_spinner());

        if event::poll(timeout)?
            && process_tui_event(event::read()?, app) == EventOutcome::ModelRefreshed
        {
            let now = Instant::now();
            schedule.reset_model(now);
            schedule.reset_spinner(now);
        }
    }
}

struct RefreshSchedule {
    next_model_refresh: Instant,
    next_spinner_tick: Instant,
}

impl RefreshSchedule {
    fn new(now: Instant) -> Self {
        Self {
            next_model_refresh: now + MODEL_REFRESH_INTERVAL,
            next_spinner_tick: now + SPINNER_INTERVAL,
        }
    }

    fn model_due(&self, now: Instant) -> bool {
        now >= self.next_model_refresh
    }

    fn spinner_due(&self, now: Instant) -> bool {
        now >= self.next_spinner_tick
    }

    fn reset_model(&mut self, now: Instant) {
        self.next_model_refresh = now + MODEL_REFRESH_INTERVAL;
    }

    fn reset_spinner(&mut self, now: Instant) {
        self.next_spinner_tick = now + SPINNER_INTERVAL;
    }

    fn next_timeout(&self, now: Instant, animate_spinner: bool) -> Duration {
        let next_tick = if animate_spinner {
            self.next_model_refresh.min(self.next_spinner_tick)
        } else {
            self.next_model_refresh
        };
        next_tick.saturating_duration_since(now)
    }
}

struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum EventOutcome {
    None,
    ModelRefreshed,
}

fn process_tui_event(event: Event, app: &mut SidebarApp) -> EventOutcome {
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
        Event::Resize(_, _) => {
            app.refresh_rows();
            return EventOutcome::ModelRefreshed;
        }
        _ => {}
    }
    EventOutcome::None
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

        let outcome = process_tui_event(
            Event::Key(crossterm::event::KeyEvent::new(
                KeyCode::Esc,
                KeyModifiers::NONE,
            )),
            &mut app,
        );

        assert_eq!(outcome, EventOutcome::None);
        assert!(app.should_quit());
        assert!(app.disable_requested());
    }

    #[test]
    fn schedule_uses_spinner_deadline_only_when_animating() {
        let now = Instant::now();
        let schedule = RefreshSchedule::new(now);

        assert_eq!(schedule.next_timeout(now, true), SPINNER_INTERVAL);
        assert_eq!(schedule.next_timeout(now, false), MODEL_REFRESH_INTERVAL);
    }

    #[test]
    fn schedule_resets_overdue_model_deadlines_without_catchup() {
        let now = Instant::now();
        let mut schedule = RefreshSchedule::new(now);
        let overdue = now + MODEL_REFRESH_INTERVAL + Duration::from_secs(2);

        assert!(schedule.model_due(overdue));

        schedule.reset_model(overdue);

        assert_eq!(
            schedule.next_timeout(overdue, false),
            MODEL_REFRESH_INTERVAL
        );
    }

    #[test]
    fn resize_event_reports_model_refresh_for_deadline_reset() {
        let rows = Vec::new();
        let mut app = SidebarApp::test(None, rows);

        let outcome = process_tui_event(Event::Resize(42, 10), &mut app);

        assert_eq!(outcome, EventOutcome::ModelRefreshed);
    }
}
