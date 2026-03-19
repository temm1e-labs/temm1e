//! Multi-line input area widget for ratatui.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use crate::input::InputState;

/// Renders the input area with prompt and cursor.
pub struct InputArea<'a> {
    state: &'a InputState,
    prompt: &'a str,
    prompt_style: Style,
    text_style: Style,
    cursor_style: Style,
    focused: bool,
}

impl<'a> InputArea<'a> {
    pub fn new(state: &'a InputState) -> Self {
        Self {
            state,
            prompt: "tem> ",
            prompt_style: Style::default(),
            text_style: Style::default(),
            cursor_style: Style::default().add_modifier(Modifier::REVERSED),
            focused: true,
        }
    }

    pub fn prompt(mut self, prompt: &'a str) -> Self {
        self.prompt = prompt;
        self
    }

    pub fn prompt_style(mut self, style: Style) -> Self {
        self.prompt_style = style;
        self
    }

    pub fn text_style(mut self, style: Style) -> Self {
        self.text_style = style;
        self
    }

    pub fn cursor_style(mut self, style: Style) -> Self {
        self.cursor_style = style;
        self
    }

    pub fn focused(mut self, focused: bool) -> Self {
        self.focused = focused;
        self
    }
}

impl<'a> Widget for InputArea<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let prompt_width = self.prompt.len() as u16;

        for (line_idx, line_text) in self.state.lines.iter().enumerate() {
            let y = area.top() + line_idx as u16;
            if y >= area.bottom() {
                break;
            }

            // Draw prompt on first line
            if line_idx == 0 {
                buf.set_line(
                    area.left(),
                    y,
                    &Line::from(Span::styled(self.prompt, self.prompt_style)),
                    area.width,
                );
            }

            // Draw text (prompt width applies to all lines for alignment)
            let text_x = area.left() + prompt_width;
            let available_width = area.width.saturating_sub(prompt_width);

            buf.set_line(
                text_x,
                y,
                &Line::from(Span::styled(line_text.as_str(), self.text_style)),
                available_width,
            );

            // Draw cursor
            if self.focused && line_idx == self.state.cursor.0 {
                let cursor_x = text_x + self.state.cursor.1 as u16;
                if cursor_x < area.right() {
                    let cursor_char = line_text
                        .get(self.state.cursor.1..)
                        .and_then(|s| s.chars().next())
                        .unwrap_or(' ');
                    buf[(cursor_x, y)]
                        .set_char(cursor_char)
                        .set_style(self.cursor_style);
                }
            }
        }
    }
}
