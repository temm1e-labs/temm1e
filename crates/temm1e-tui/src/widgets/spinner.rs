//! Animated spinner/progress indicators.

use ratatui::style::Style;
use ratatui::text::Span;

const SPINNER_FRAMES: &[&str] = &["\u{25dc}", "\u{25dd}", "\u{25de}", "\u{25df}"];
const DOTS_FRAMES: &[&str] = &[
    "\u{2801}", "\u{2802}", "\u{2804}", "\u{2840}", "\u{2880}", "\u{2820}", "\u{2810}", "\u{2808}",
];

/// Spinner animation state.
#[derive(Debug, Clone)]
pub struct Spinner {
    frame: usize,
    variant: SpinnerVariant,
}

#[derive(Debug, Clone, Copy)]
pub enum SpinnerVariant {
    Circle,
    Dots,
}

impl Spinner {
    pub fn new(variant: SpinnerVariant) -> Self {
        Self { frame: 0, variant }
    }

    /// Advance to the next frame. Call on each tick.
    pub fn tick(&mut self) {
        let len = match self.variant {
            SpinnerVariant::Circle => SPINNER_FRAMES.len(),
            SpinnerVariant::Dots => DOTS_FRAMES.len(),
        };
        self.frame = (self.frame + 1) % len;
    }

    /// Get the current frame as a styled span.
    pub fn span(&self, style: Style) -> Span<'static> {
        let ch = match self.variant {
            SpinnerVariant::Circle => SPINNER_FRAMES[self.frame],
            SpinnerVariant::Dots => DOTS_FRAMES[self.frame],
        };
        Span::styled(ch.to_string(), style)
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new(SpinnerVariant::Circle)
    }
}
