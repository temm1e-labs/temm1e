//! Configuration overlay panels (model picker, keys, usage, status).

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use crate::commands::registry::OverlayKind;
use crate::theme::Theme;

/// Render a config overlay.
pub fn render_config_overlay(kind: &OverlayKind, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let popup_width = 50.min(area.width.saturating_sub(4));
    let popup_height = 15.min(area.height.saturating_sub(4));
    let popup = centered_rect(popup_width, popup_height, area);

    Clear.render(popup, buf);

    let title = match kind {
        OverlayKind::ModelPicker => " Model ",
        OverlayKind::Config => " Configuration ",
        OverlayKind::Keys => " API Keys ",
        OverlayKind::Usage => " Usage ",
        OverlayKind::Status => " Status ",
        OverlayKind::Help => " Help ",
    };

    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(theme.accent);
    let inner = block.inner(popup);
    block.render(popup, buf);

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {} panel", title.trim()),
            theme.text,
        )),
        Line::from(""),
        Line::from(Span::styled("  Press Esc to close", theme.secondary)),
    ];

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    para.render(inner, buf);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.left() + (area.width.saturating_sub(width)) / 2;
    let y = area.top() + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
