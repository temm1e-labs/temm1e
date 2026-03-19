//! Incremental streaming markdown renderer.
//!
//! Appends text chunks and re-renders only the last block for efficiency.

use ratatui::style::Style;

use crate::widgets::markdown::{render_markdown, RenderedLine};

/// Renders streaming markdown incrementally.
pub struct StreamingRenderer {
    buffer: String,
    rendered_lines: Vec<RenderedLine>,
    base_style: Style,
    heading_style: Style,
    code_style: Style,
    link_style: Style,
    quote_style: Style,
}

impl StreamingRenderer {
    pub fn new(
        base_style: Style,
        heading_style: Style,
        code_style: Style,
        link_style: Style,
        quote_style: Style,
    ) -> Self {
        Self {
            buffer: String::new(),
            rendered_lines: Vec::new(),
            base_style,
            heading_style,
            code_style,
            link_style,
            quote_style,
        }
    }

    /// Append a text chunk from the stream.
    pub fn push(&mut self, delta: &str) {
        self.buffer.push_str(delta);
        self.rerender();
    }

    /// Get the current rendered lines.
    pub fn lines(&self) -> &[RenderedLine] {
        &self.rendered_lines
    }

    /// Get the raw buffer text.
    pub fn text(&self) -> &str {
        &self.buffer
    }

    /// Reset for a new message.
    pub fn reset(&mut self) {
        self.buffer.clear();
        self.rendered_lines.clear();
    }

    /// Whether the buffer is empty.
    pub fn is_empty(&self) -> bool {
        self.buffer.is_empty()
    }

    fn rerender(&mut self) {
        self.rendered_lines = render_markdown(
            &self.buffer,
            self.base_style,
            self.heading_style,
            self.code_style,
            self.link_style,
            self.quote_style,
        );
    }
}
