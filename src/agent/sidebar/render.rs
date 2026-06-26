use ratatui::{
    Frame,
    layout::{Alignment, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, Paragraph},
};
use unicode_width::{UnicodeWidthChar, UnicodeWidthStr};

use crate::agent::sidebar::app::SidebarApp;
use crate::agent::sidebar::model::{SidebarRow, SidebarRowState};

const ACTIVE_BG: Color = Color::Rgb(34, 41, 54);
const CURSOR_BG: Color = Color::Rgb(40, 48, 62);
const TEXT_FG: Color = Color::Rgb(205, 214, 244);
const DIM_FG: Color = Color::Rgb(108, 112, 134);
const BORDER_FG: Color = Color::Rgb(58, 74, 94);
const WORKING_FG: Color = Color::Rgb(120, 225, 213);
const WAITING_FG: Color = Color::Rgb(203, 166, 247);
const DONE_FG: Color = Color::Rgb(166, 218, 149);

pub(super) fn render_sidebar_tui(frame: &mut Frame, app: &mut SidebarApp) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    let mut list_area = area;
    if let Some(error) = app.last_error() {
        let warning = fit_width(&format!("error: {error}"), area.width as usize);
        frame.render_widget(
            Paragraph::new(warning).style(Style::default().fg(WAITING_FG)),
            Rect::new(area.x, area.y, area.width, 1),
        );
        list_area.y = list_area.y.saturating_add(1);
        list_area.height = list_area.height.saturating_sub(1);
    }

    if app.rows().is_empty() {
        render_no_agents(frame, list_area);
        return;
    }

    let active_index = app.active_index();
    let cursor_index = app.cursor_index();
    let row_count = app.rows().len();
    let items = app
        .rows()
        .iter()
        .enumerate()
        .map(|(index, row)| {
            tile_item(
                row,
                index > 0,
                index + 1 == row_count,
                list_area.width as usize,
                row_highlight(index, active_index, cursor_index),
            )
        })
        .collect::<Vec<_>>();
    let list = List::new(items);
    frame.render_stateful_widget(list, list_area, app.list_state_mut());
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

fn tile_item(
    row: &SidebarRow,
    include_separator: bool,
    include_bottom_separator: bool,
    width: usize,
    highlight: RowHighlight,
) -> ListItem<'static> {
    let mut lines = Vec::new();
    if include_separator {
        lines.push(separator_line(width));
    }

    lines.push(tile_line(row, LineKind::Primary, width, highlight));
    lines.push(tile_line(row, LineKind::Secondary, width, highlight));
    lines.push(tile_line(row, LineKind::Title, width, highlight));
    if include_bottom_separator {
        lines.push(separator_line(width));
    }
    ListItem::new(lines)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum RowHighlight {
    None,
    Active,
    Cursor,
}

impl RowHighlight {
    fn bg(self) -> Option<Color> {
        match self {
            Self::None => None,
            Self::Active => Some(ACTIVE_BG),
            Self::Cursor => Some(CURSOR_BG),
        }
    }
}

fn row_highlight(
    index: usize,
    active_index: Option<usize>,
    cursor_index: Option<usize>,
) -> RowHighlight {
    if cursor_index == Some(index) {
        RowHighlight::Cursor
    } else if active_index == Some(index) {
        RowHighlight::Active
    } else {
        RowHighlight::None
    }
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

fn tile_line(
    row: &SidebarRow,
    kind: LineKind,
    width: usize,
    highlight: RowHighlight,
) -> Line<'static> {
    let bg = highlight.bg();
    if width < 6 {
        return narrow_tile_line(row, kind, width, bg);
    }

    let body_width = width - 6;
    let stripe_style = style_with_bg(Style::default().fg(status_color(row)), bg);
    let text_style = row_text_style(row, bg);
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
            elapsed_style(row, bg),
            bg,
        ),
        LineKind::Secondary => line_with_right(
            &row.secondary,
            &row.secondary_right,
            body_width,
            dim_style,
            dim_style,
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
        LineKind::Secondary => row.secondary.clone(),
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
    match row.state {
        SidebarRowState::Working => WORKING_FG,
        SidebarRowState::Waiting => WAITING_FG,
        SidebarRowState::Done => DONE_FG,
        SidebarRowState::Idle => DIM_FG,
    }
}

fn elapsed_style(row: &SidebarRow, bg: Option<Color>) -> Style {
    let mut style = Style::default().fg(status_color(row));
    if row.is_idle() {
        style = style.add_modifier(Modifier::DIM);
    }
    style_with_bg(style, bg)
}

fn row_text_style(row: &SidebarRow, bg: Option<Color>) -> Style {
    let fg = if row.is_idle() { DIM_FG } else { TEXT_FG };
    let mut style = Style::default().fg(fg);
    if let Some(bg) = bg {
        style = style.bg(bg);
    }
    if row.is_idle() {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::sidebar::model::{TEST_SLEEPING_ICON, agent_state};
    use crate::config::DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS;
    use crate::state::AgentStatus;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    #[test]
    fn ratatui_renderer_draws_active_tile_with_expected_text() -> anyhow::Result<()> {
        let mut agent = agent_state(AgentStatus::Waiting, 120, "@1", "%1");
        agent.title = Some("Implement richer sidebar".to_owned());
        agent.context = Some("163.2K (41%)".to_owned());
        let rows = vec![SidebarRow::from_agent(&agent, 300, TEST_SLEEPING_ICON)];
        let backend = TestBackend::new(42, 5);
        let mut terminal = Terminal::new(backend)?;
        let mut app = SidebarApp::test(Some("@1"), rows);

        terminal.draw(|frame| render_sidebar_tui(frame, &mut app))?;

        let buffer = terminal.backend().buffer();
        let text = buffer_text(buffer, 42, 5);
        assert!(text.contains("feature-sidebar"));
        assert!(text.contains("3m"));
        assert!(text.contains("163.2K (41%)"));
        assert!(text.contains("Implement richer sidebar"));
        assert_eq!(buffer[(0, 0)].bg, ACTIVE_BG);
        Ok(())
    }

    #[test]
    fn ratatui_renderer_draws_cursor_over_active_tile() -> anyhow::Result<()> {
        let rows = vec![
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Waiting, 120, "@1", "%1"),
                300,
                TEST_SLEEPING_ICON,
            ),
            SidebarRow::from_agent(
                &agent_state(AgentStatus::Working, 120, "@2", "%2"),
                300,
                TEST_SLEEPING_ICON,
            ),
        ];
        let backend = TestBackend::new(42, 8);
        let mut terminal = Terminal::new(backend)?;
        let mut app = SidebarApp::test(Some("@1"), rows);

        app.next();
        terminal.draw(|frame| render_sidebar_tui(frame, &mut app))?;

        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].bg, ACTIVE_BG);
        assert_eq!(buffer[(0, 4)].bg, CURSOR_BG);
        Ok(())
    }

    #[test]
    fn ratatui_renderer_draws_final_separator() -> anyhow::Result<()> {
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
    fn ratatui_renderer_truncates_narrow_tiles() -> anyhow::Result<()> {
        let mut agent = agent_state(AgentStatus::Done, 120, "@1", "%1");
        agent.target.worktree_handle = Some("very-long-sidebar-worktree-name".to_owned());
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
                let line = tile_line(&row, kind, width, RowHighlight::Cursor);
                assert!(line_width(&line) <= width);
            }
        }
    }

    #[test]
    fn ratatui_renderer_colors_elapsed_by_row_state() -> anyhow::Result<()> {
        assert_eq!(
            rendered_elapsed_fg(AgentStatus::Working, 120, 300, "3m")?,
            WORKING_FG
        );
        assert_eq!(
            rendered_elapsed_fg(AgentStatus::Waiting, 120, 300, "3m")?,
            WAITING_FG
        );
        assert_eq!(
            rendered_elapsed_fg(AgentStatus::Done, 120, 300, "3m")?,
            DONE_FG
        );
        assert_eq!(
            rendered_elapsed_fg(
                AgentStatus::Done,
                0,
                DEFAULT_SIDEBAR_IDLE_AFTER_SECONDS + 1,
                "30m"
            )?,
            DIM_FG
        );
        Ok(())
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

    fn rendered_elapsed_fg(
        status: AgentStatus,
        status_changed_at: u64,
        now: u64,
        elapsed: &str,
    ) -> anyhow::Result<Color> {
        let rows = vec![SidebarRow::from_agent(
            &agent_state(status, status_changed_at, "@1", "%1"),
            now,
            TEST_SLEEPING_ICON,
        )];
        let backend = TestBackend::new(42, 4);
        let mut terminal = Terminal::new(backend)?;
        let mut app = SidebarApp::test(Some("@1"), rows);

        terminal.draw(|frame| render_sidebar_tui(frame, &mut app))?;

        let buffer = terminal.backend().buffer();
        elapsed_fg(buffer, 42, 0, elapsed).ok_or_else(|| {
            anyhow::anyhow!("elapsed label {elapsed:?} was not rendered on primary row")
        })
    }

    fn elapsed_fg(
        buffer: &ratatui::buffer::Buffer,
        width: u16,
        y: u16,
        elapsed: &str,
    ) -> Option<Color> {
        let chars = elapsed.chars().map(|ch| ch.to_string()).collect::<Vec<_>>();
        let len = u16::try_from(chars.len()).ok()?;
        if len == 0 || len > width {
            return None;
        }

        for x in 0..=width - len {
            if chars
                .iter()
                .enumerate()
                .all(|(offset, ch)| buffer[(x + offset as u16, y)].symbol() == ch)
            {
                return Some(buffer[(x, y)].fg);
            }
        }
        None
    }
}
