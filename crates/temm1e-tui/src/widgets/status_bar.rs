//! Bottom status bar — model, tokens, cost, elapsed, health.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

/// Status bar data.
#[derive(Debug, Clone)]
pub struct StatusBarData {
    pub model: String,
    pub provider: String,
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    pub total_cost_usd: f64,
    pub is_agent_working: bool,
}

impl Default for StatusBarData {
    fn default() -> Self {
        Self {
            model: String::new(),
            provider: String::new(),
            total_input_tokens: 0,
            total_output_tokens: 0,
            total_cost_usd: 0.0,
            is_agent_working: false,
        }
    }
}

/// Renders the status bar at the bottom of the screen.
pub struct StatusBar<'a> {
    data: &'a StatusBarData,
    style: Style,
    accent_style: Style,
    info_style: Style,
}

impl<'a> StatusBar<'a> {
    pub fn new(
        data: &'a StatusBarData,
        style: Style,
        accent_style: Style,
        info_style: Style,
    ) -> Self {
        Self {
            data,
            style,
            accent_style,
            info_style,
        }
    }
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 {
            return;
        }

        // Fill background
        for x in area.left()..area.right() {
            buf[(x, area.top())].set_style(self.style);
        }

        let mut spans = Vec::new();

        // Model info
        if !self.data.model.is_empty() {
            spans.push(Span::styled(" ", self.style));
            spans.push(Span::styled(&self.data.model, self.accent_style));
        }

        // Provider
        if !self.data.provider.is_empty() {
            spans.push(Span::styled(" | ", self.style));
            spans.push(Span::styled(&self.data.provider, self.style));
        }

        // Token counts
        if self.data.total_input_tokens > 0 || self.data.total_output_tokens > 0 {
            spans.push(Span::styled(" | ", self.style));
            spans.push(Span::styled(
                format!(
                    "{}in/{}out",
                    format_tokens(self.data.total_input_tokens),
                    format_tokens(self.data.total_output_tokens),
                ),
                self.info_style,
            ));
        }

        // Cost
        if self.data.total_cost_usd > 0.0 {
            spans.push(Span::styled(" | ", self.style));
            spans.push(Span::styled(
                format!("${:.4}", self.data.total_cost_usd),
                self.info_style,
            ));
        }

        let line = Line::from(spans);
        buf.set_line(area.left(), area.top(), &line, area.width);
    }
}

fn format_tokens(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
