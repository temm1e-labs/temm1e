//! Help overlay — shows available commands and keyboard shortcuts.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use crate::commands::CommandRegistry;
use crate::theme::Theme;

/// Render the help overlay centered on screen.
pub fn render_help(registry: &CommandRegistry, theme: &Theme, area: Rect, buf: &mut Buffer) {
    // Center the overlay
    let popup_width = 60.min(area.width.saturating_sub(4));
    let popup_height = 25.min(area.height.saturating_sub(4));
    let popup = centered_rect(popup_width, popup_height, area);

    // Clear the area behind the popup
    Clear.render(popup, buf);

    let block = Block::default()
        .title(" Help ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(theme.accent);
    let inner = block.inner(popup);
    block.render(popup, buf);

    let mut lines = Vec::new();
    lines.push(Line::from(Span::styled("Commands", theme.heading)));
    lines.push(Line::from(""));

    for (name, desc) in registry.all_commands() {
        lines.push(Line::from(vec![
            Span::styled(format!("  /{:<12}", name), theme.command),
            Span::styled(desc.to_string(), theme.text),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "Keyboard Shortcuts",
        theme.heading,
    )));
    lines.push(Line::from(""));

    let shortcuts = [
        ("Enter", "Submit input"),
        ("Shift+Enter", "Insert newline"),
        ("Tab", "Cycle completions"),
        ("Ctrl+C", "Interrupt / clear"),
        ("Ctrl+D", "Quit (empty input)"),
        ("Ctrl+L", "Redraw terminal"),
        ("Ctrl+O", "Toggle activity panel"),
        ("PageUp/Down", "Scroll messages"),
        ("Esc", "Close overlay"),
    ];

    for (key, desc) in &shortcuts {
        lines.push(Line::from(vec![
            Span::styled(format!("  {:<16}", key), theme.info),
            Span::styled(desc.to_string(), theme.text),
        ]));
    }

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press Esc to close",
        theme.secondary,
    )));

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    para.render(inner, buf);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.left() + (area.width.saturating_sub(width)) / 2;
    let y = area.top() + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
