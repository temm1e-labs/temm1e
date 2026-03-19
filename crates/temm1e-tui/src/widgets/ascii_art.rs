//! Tem mascot ASCII art for the welcome/onboarding screen.

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::TemPalette;

/// Get the Tem mascot ASCII art as styled lines.
pub fn tem_mascot(accent_style: Style, secondary_style: Style) -> Vec<Line<'static>> {
    let pink = Style::default().fg(TemPalette::HOT_PINK);
    let amber = Style::default().fg(TemPalette::AMBER);
    let blue = Style::default().fg(TemPalette::ICE_BLUE);
    let lav = Style::default().fg(TemPalette::LAVENDER);
    let w = Style::default().add_modifier(Modifier::BOLD);

    vec![
        Line::from(vec![
            Span::styled("    ", w),
            Span::styled("+", amber),
            Span::styled("                  ", w),
            Span::styled("*", amber),
        ]),
        Line::from(vec![
            Span::styled("        ", w),
            Span::styled("/\\_/\\", w),
        ]),
        Line::from(vec![
            Span::styled("   ", w),
            Span::styled("*", lav),
            Span::styled("   ", w),
            Span::styled("( ", w),
            Span::styled("o", amber),
            Span::styled(".", w),
            Span::styled("o", blue),
            Span::styled(" )", w),
            Span::styled("   +", amber),
        ]),
        Line::from(vec![
            Span::styled("        ", w),
            Span::styled(" > ", w),
            Span::styled("^", pink),
            Span::styled(" <", w),
        ]),
        Line::from(vec![
            Span::styled("       ", w),
            Span::styled("/|", w),
            Span::styled("~~~", pink),
            Span::styled("|\\", w),
        ]),
        Line::from(vec![
            Span::styled("       ", w),
            Span::styled("( ", w),
            Span::styled("\u{2665}", pink.add_modifier(Modifier::BOLD)),
            Span::styled("   )", w),
        ]),
        Line::from(vec![
            Span::styled("    ", w),
            Span::styled("*", lav),
            Span::styled("   ~~   ~~", w),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("     T E M M", accent_style.add_modifier(Modifier::BOLD)),
            Span::styled("1", amber.add_modifier(Modifier::BOLD)),
            Span::styled("E", accent_style.add_modifier(Modifier::BOLD)),
        ]),
        Line::from(Span::styled("   your local AI agent", secondary_style)),
    ]
}
