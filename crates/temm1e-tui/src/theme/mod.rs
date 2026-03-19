//! Color/theme system with graceful terminal capability degradation.

mod palette;
pub use palette::TemPalette;

use ratatui::style::{Color, Modifier, Style};

/// Terminal color capability level.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorLevel {
    /// NO_COLOR or TERM=dumb — no styling at all.
    None,
    /// 256-color terminal.
    Ansi256,
    /// 24-bit truecolor (COLORTERM=truecolor).
    TrueColor,
}

/// Semantic theme for the entire TUI.
#[derive(Debug, Clone)]
pub struct Theme {
    pub level: ColorLevel,
    // Semantic roles
    pub prompt: Style,
    pub accent: Style,
    pub command: Style,
    pub info: Style,
    pub secondary: Style,
    pub error: Style,
    pub text: Style,
    pub heading: Style,
    pub code_bg: Style,
    pub tool_running: Style,
    pub tool_success: Style,
    pub tool_failed: Style,
    pub phase_done: Style,
    pub phase_active: Style,
    pub phase_pending: Style,
    pub input_cursor: Style,
    pub status_bar: Style,
    pub border: Style,
}

impl Theme {
    /// Detect terminal capabilities and build the theme.
    pub fn detect() -> Self {
        let level = detect_color_level();
        Self::for_level(level)
    }

    /// Build a theme for a specific color level.
    pub fn for_level(level: ColorLevel) -> Self {
        match level {
            ColorLevel::None => Self::no_color(),
            ColorLevel::Ansi256 => Self::ansi256(),
            ColorLevel::TrueColor => Self::truecolor(),
        }
    }

    fn truecolor() -> Self {
        use TemPalette as P;
        Self {
            level: ColorLevel::TrueColor,
            prompt: Style::default()
                .fg(P::HOT_PINK)
                .add_modifier(Modifier::BOLD),
            accent: Style::default().fg(P::HOT_PINK),
            command: Style::default().fg(P::AMBER),
            info: Style::default().fg(P::ICE_BLUE),
            secondary: Style::default().fg(P::LAVENDER),
            error: Style::default()
                .fg(P::DEEP_RED)
                .add_modifier(Modifier::BOLD),
            text: Style::default().fg(P::WHITE),
            heading: Style::default()
                .fg(P::HOT_PINK)
                .add_modifier(Modifier::BOLD),
            code_bg: Style::default().bg(P::DARK_GRAY),
            tool_running: Style::default().fg(P::AMBER),
            tool_success: Style::default().fg(P::ICE_BLUE),
            tool_failed: Style::default().fg(P::DEEP_RED),
            phase_done: Style::default().fg(P::ICE_BLUE),
            phase_active: Style::default()
                .fg(P::HOT_PINK)
                .add_modifier(Modifier::BOLD),
            phase_pending: Style::default().fg(P::LAVENDER).add_modifier(Modifier::DIM),
            input_cursor: Style::default().add_modifier(Modifier::REVERSED),
            status_bar: Style::default().bg(Color::Rgb(30, 30, 50)).fg(P::LAVENDER),
            border: Style::default().fg(P::LAVENDER),
        }
    }

    fn ansi256() -> Self {
        Self {
            level: ColorLevel::Ansi256,
            prompt: Style::default()
                .fg(Color::Indexed(205))
                .add_modifier(Modifier::BOLD),
            accent: Style::default().fg(Color::Indexed(205)),
            command: Style::default().fg(Color::Indexed(214)),
            info: Style::default().fg(Color::Indexed(75)),
            secondary: Style::default().fg(Color::Indexed(183)),
            error: Style::default()
                .fg(Color::Indexed(160))
                .add_modifier(Modifier::BOLD),
            text: Style::default().fg(Color::White),
            heading: Style::default()
                .fg(Color::Indexed(205))
                .add_modifier(Modifier::BOLD),
            code_bg: Style::default().bg(Color::Indexed(234)),
            tool_running: Style::default().fg(Color::Indexed(214)),
            tool_success: Style::default().fg(Color::Indexed(75)),
            tool_failed: Style::default().fg(Color::Indexed(160)),
            phase_done: Style::default().fg(Color::Indexed(75)),
            phase_active: Style::default()
                .fg(Color::Indexed(205))
                .add_modifier(Modifier::BOLD),
            phase_pending: Style::default()
                .fg(Color::Indexed(183))
                .add_modifier(Modifier::DIM),
            input_cursor: Style::default().add_modifier(Modifier::REVERSED),
            status_bar: Style::default()
                .bg(Color::Indexed(234))
                .fg(Color::Indexed(183)),
            border: Style::default().fg(Color::Indexed(183)),
        }
    }

    fn no_color() -> Self {
        let plain = Style::default();
        Self {
            level: ColorLevel::None,
            prompt: plain.add_modifier(Modifier::BOLD),
            accent: plain,
            command: plain,
            info: plain,
            secondary: plain.add_modifier(Modifier::DIM),
            error: plain.add_modifier(Modifier::BOLD),
            text: plain,
            heading: plain.add_modifier(Modifier::BOLD),
            code_bg: plain,
            tool_running: plain,
            tool_success: plain,
            tool_failed: plain.add_modifier(Modifier::BOLD),
            phase_done: plain,
            phase_active: plain.add_modifier(Modifier::BOLD),
            phase_pending: plain.add_modifier(Modifier::DIM),
            input_cursor: plain.add_modifier(Modifier::REVERSED),
            status_bar: plain.add_modifier(Modifier::REVERSED),
            border: plain,
        }
    }
}

/// Detect terminal color capability from environment variables.
fn detect_color_level() -> ColorLevel {
    // NO_COLOR spec: https://no-color.org/
    if std::env::var("NO_COLOR").is_ok() {
        return ColorLevel::None;
    }
    if std::env::var("TERM").map(|t| t == "dumb").unwrap_or(false) {
        return ColorLevel::None;
    }
    if std::env::var("COLORTERM")
        .map(|ct| ct == "truecolor" || ct == "24bit")
        .unwrap_or(false)
    {
        return ColorLevel::TrueColor;
    }
    // macOS Terminal.app and most modern terminals support 256 colors
    ColorLevel::Ansi256
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn theme_levels_construct() {
        let _tc = Theme::for_level(ColorLevel::TrueColor);
        let _a256 = Theme::for_level(ColorLevel::Ansi256);
        let _none = Theme::for_level(ColorLevel::None);
    }
}
