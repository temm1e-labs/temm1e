//! Tem's Design Palette — the 7-color system used throughout the TUI.

use ratatui::style::Color;

/// Tem's canonical color palette.
pub struct TemPalette;

impl TemPalette {
    pub const HOT_PINK: Color = Color::Rgb(255, 105, 180);
    pub const AMBER: Color = Color::Rgb(255, 180, 40);
    pub const ICE_BLUE: Color = Color::Rgb(80, 200, 255);
    pub const LAVENDER: Color = Color::Rgb(190, 168, 220);
    pub const DEEP_RED: Color = Color::Rgb(220, 40, 80);
    pub const WHITE: Color = Color::White;
    pub const BLACK: Color = Color::Black;
    pub const DARK_GRAY: Color = Color::Rgb(26, 26, 46);
}
