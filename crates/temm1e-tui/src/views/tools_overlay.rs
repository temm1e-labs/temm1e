//! `/tools` — tool call history overlay.
//!
//! Shows every tool call made during this session, grouped by turn,
//! with args preview, duration, and success/fail status. Read-only
//! developer audit view.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use crate::app::AppState;

pub fn render_tools_overlay(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup_width = 86.min(area.width.saturating_sub(4));
    let popup_height = 26.min(area.height.saturating_sub(4));
    let popup = centered_rect(popup_width, popup_height, area);

    Clear.render(popup, buf);

    let title = format!(
        " Tool Call History — {} calls ",
        state.tool_call_history.len()
    );
    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(state.theme.accent);
    let inner = block.inner(popup);
    block.render(popup, buf);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(""));

    if state.tool_call_history.is_empty() {
        lines.push(Line::from(Span::styled(
            "  No tool calls yet.",
            state.theme.info,
        )));
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "  Tool calls will appear here once Tem runs any tools.",
            state.theme.secondary,
        )));
    } else {
        let mut current_turn: u32 = 0;
        for record in &state.tool_call_history {
            if record.turn_number != current_turn {
                current_turn = record.turn_number;
                if !lines.is_empty() {
                    lines.push(Line::from(""));
                }
                lines.push(Line::from(Span::styled(
                    format!("  turn {}", current_turn),
                    state.theme.heading,
                )));
            }

            let (symbol, symbol_style) = match record.ok {
                Some(true) => ("✓", state.theme.phase_done),
                Some(false) => ("✗", state.theme.error),
                None => ("▸", state.theme.tool_running),
            };

            let duration = match record.duration_ms {
                Some(ms) if ms >= 1000 => format!("{:.1}s", ms as f64 / 1000.0),
                Some(ms) => format!("{}ms", ms),
                None => "(running)".to_string(),
            };

            let args_trimmed: String = record.args_preview.chars().take(46).collect();

            lines.push(Line::from(vec![
                Span::styled("  ", state.theme.secondary),
                Span::styled(format!("{} ", symbol), symbol_style),
                Span::styled(format!("{:<12}", record.tool_name), state.theme.text),
                Span::styled(format!("{:<48}", args_trimmed), state.theme.secondary),
                Span::styled(format!(" {:>8}", duration), state.theme.info),
            ]));
        }
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press Esc to close",
        state.theme.secondary,
    )));

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    para.render(inner, buf);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.left() + (area.width.saturating_sub(width)) / 2;
    let y = area.top() + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
