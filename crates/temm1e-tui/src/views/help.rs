//! Help overlay — shows available commands and keyboard shortcuts.
//!
//! Commands come from the registry dynamically. Keybinds are grouped
//! by category (Editing, Navigation, Copy & Cancel, Overlays, Session)
//! to be scannable at a glance.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use crate::commands::CommandRegistry;
use crate::theme::Theme;

/// Render the help overlay centered on screen.
pub fn render_help(registry: &CommandRegistry, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let popup_width = 68.min(area.width.saturating_sub(4));
    let popup_height = 32.min(area.height.saturating_sub(4));
    let popup = centered_rect(popup_width, popup_height, area);

    Clear.render(popup, buf);

    let block = Block::default()
        .title(" Help ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(theme.accent);
    let inner = block.inner(popup);
    block.render(popup, buf);

    let mut lines: Vec<Line<'static>> = Vec::new();
    lines.push(Line::from(""));

    // ── Commands (from registry) ──────────────────────
    lines.push(Line::from(Span::styled("  Commands", theme.heading)));
    lines.push(Line::from(""));
    for (name, desc) in registry.all_commands() {
        lines.push(Line::from(vec![
            Span::styled(format!("    /{:<12}", name), theme.command),
            Span::styled(desc.to_string(), theme.text),
        ]));
    }

    lines.push(Line::from(""));

    // ── Editing ───────────────────────────────────────
    lines.push(Line::from(Span::styled("  Editing", theme.heading)));
    lines.push(Line::from(""));
    for (key, desc) in &[
        ("Enter", "Submit message"),
        ("Shift+Enter", "Insert newline"),
        ("Tab", "Complete slash command"),
        ("Ctrl+A", "Move to line start"),
        ("Ctrl+E", "Move to line end"),
        ("Ctrl+K", "Delete to line end"),
        ("Ctrl+U", "Delete to line start"),
    ] {
        lines.push(shortcut(theme, key, desc));
    }

    lines.push(Line::from(""));

    // ── Navigation ────────────────────────────────────
    lines.push(Line::from(Span::styled("  Navigation", theme.heading)));
    lines.push(Line::from(""));
    for (key, desc) in &[
        ("PageUp/Down", "Scroll messages"),
        ("Shift+Up/Down", "Scroll 3 lines"),
        ("Ctrl+L", "Redraw terminal"),
    ] {
        lines.push(shortcut(theme, key, desc));
    }

    lines.push(Line::from(""));

    // ── Copy & Cancel ─────────────────────────────────
    lines.push(Line::from(Span::styled("  Copy & Cancel", theme.heading)));
    lines.push(Line::from(""));
    for (key, desc) in &[
        ("Esc", "Cancel Tem mid-task · close overlay"),
        ("Ctrl+C", "Cancel Tem · press twice to quit"),
        ("Ctrl+Y", "Yank a code block to clipboard"),
        ("Drag (mouse)", "Select text natively (default mode)"),
        ("Alt+S", "Toggle scroll-mode (mouse wheel / blocks drag)"),
    ] {
        lines.push(shortcut(theme, key, desc));
    }

    lines.push(Line::from(""));

    // ── Overlays ──────────────────────────────────────
    lines.push(Line::from(Span::styled("  Overlays", theme.heading)));
    lines.push(Line::from(""));
    for (key, desc) in &[
        ("Ctrl+O", "Toggle activity panel"),
        ("Esc", "Close any open overlay"),
    ] {
        lines.push(shortcut(theme, key, desc));
    }

    lines.push(Line::from(""));

    // ── Session ───────────────────────────────────────
    lines.push(Line::from(Span::styled("  Session", theme.heading)));
    lines.push(Line::from(""));
    lines.push(shortcut(theme, "Ctrl+D", "Quit (empty input only)"));
    lines.push(shortcut(theme, "/quit", "Exit via slash command"));

    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        "  Press Esc to close",
        theme.secondary,
    )));

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    para.render(inner, buf);
}

fn shortcut(theme: &Theme, key: &str, desc: &str) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("    {:<16}", key), theme.info),
        Span::styled(desc.to_string(), theme.text),
    ])
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.left() + (area.width.saturating_sub(width)) / 2;
    let y = area.top() + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
