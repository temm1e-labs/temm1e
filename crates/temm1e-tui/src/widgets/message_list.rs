//! Scrollable message list widget.

use std::collections::VecDeque;

use chrono::{DateTime, Utc};
use ratatui::style::Style;
use ratatui::text::{Line, Span};

use super::markdown::RenderedLine;

/// Message role for display styling.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Agent,
    System,
}

/// Usage statistics for a single turn.
#[derive(Debug, Clone, Default)]
pub struct TurnUsage {
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: f64,
    pub elapsed_ms: u64,
}

/// A display-ready message.
#[derive(Debug, Clone)]
pub struct DisplayMessage {
    pub role: MessageRole,
    pub content: Vec<RenderedLine>,
    pub timestamp: DateTime<Utc>,
    pub usage: Option<TurnUsage>,
}

/// Message list state.
pub struct MessageList {
    pub messages: VecDeque<DisplayMessage>,
    pub scroll_offset: usize,
    pub max_messages: usize,
}

impl Default for MessageList {
    fn default() -> Self {
        Self {
            messages: VecDeque::new(),
            scroll_offset: 0,
            max_messages: 10_000,
        }
    }
}

impl MessageList {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn push(&mut self, msg: DisplayMessage) {
        let was_at_bottom = self.scroll_offset == 0;
        self.messages.push_back(msg);
        if self.messages.len() > self.max_messages {
            self.messages.pop_front();
            if self.scroll_offset > 0 {
                self.scroll_offset = self.scroll_offset.saturating_sub(1);
            }
        }
        // Only auto-scroll if already at bottom — don't snap away from history
        if was_at_bottom {
            self.scroll_to_bottom();
        }
    }

    pub fn scroll_up(&mut self, amount: usize) {
        // Allow scrolling up generously — chat view clamps to actual content
        self.scroll_offset = self.scroll_offset.saturating_add(amount).min(100_000);
    }

    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll_offset = self.scroll_offset.saturating_sub(amount);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.scroll_offset = 0;
    }

    pub fn clear(&mut self) {
        self.messages.clear();
        self.scroll_offset = 0;
    }

    /// Render messages into ratatui Lines for display.
    pub fn render_lines(
        &self,
        user_style: Style,
        agent_style: Style,
        system_style: Style,
        secondary_style: Style,
    ) -> Vec<Line<'static>> {
        let mut lines = Vec::new();

        for msg in &self.messages {
            let (prefix, prefix_style) = match msg.role {
                MessageRole::User => ("> ", user_style),
                MessageRole::Agent => ("", agent_style),
                MessageRole::System => ("[system] ", system_style),
            };

            // Content lines with role prefix
            for rendered in &msg.content {
                let mut line_spans = Vec::new();
                if !prefix.is_empty() {
                    line_spans.push(Span::styled(prefix.to_string(), prefix_style));
                }
                line_spans.extend(rendered.spans.clone());
                lines.push(Line::from(line_spans));
            }

            // Usage info
            if let Some(usage) = &msg.usage {
                lines.push(Line::from(Span::styled(
                    format!(
                        "  [{} in / {} out | ${:.4} | {:.1}s]",
                        usage.input_tokens,
                        usage.output_tokens,
                        usage.cost_usd,
                        usage.elapsed_ms as f64 / 1000.0,
                    ),
                    secondary_style,
                )));
            }

            // Blank line between messages
            lines.push(Line::from(""));
        }

        lines
    }
}
