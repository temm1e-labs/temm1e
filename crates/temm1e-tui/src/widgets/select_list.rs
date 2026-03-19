//! Arrow-key selectable list widget for onboarding and config.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

/// An item in a selectable list.
#[derive(Debug, Clone)]
pub struct SelectItem<T: Clone> {
    pub value: T,
    pub label: String,
    pub description: String,
}

/// State for a selectable list.
#[derive(Debug, Clone)]
pub struct SelectState<T: Clone> {
    pub items: Vec<SelectItem<T>>,
    pub selected: usize,
    pub viewport_offset: usize,
}

impl<T: Clone> SelectState<T> {
    pub fn new(items: Vec<SelectItem<T>>) -> Self {
        Self {
            items,
            selected: 0,
            viewport_offset: 0,
        }
    }

    pub fn move_up(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            if self.selected < self.viewport_offset {
                self.viewport_offset = self.selected;
            }
        }
    }

    pub fn move_down(&mut self) {
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
        }
    }

    pub fn selected_value(&self) -> Option<&T> {
        self.items.get(self.selected).map(|i| &i.value)
    }

    pub fn selected_item(&self) -> Option<&SelectItem<T>> {
        self.items.get(self.selected)
    }
}

/// Renders a selectable list.
pub struct SelectListWidget<'a, T: Clone> {
    state: &'a SelectState<T>,
    normal_style: Style,
    selected_style: Style,
    description_style: Style,
    visible_count: usize,
}

impl<'a, T: Clone> SelectListWidget<'a, T> {
    pub fn new(state: &'a SelectState<T>) -> Self {
        Self {
            state,
            normal_style: Style::default(),
            selected_style: Style::default().add_modifier(Modifier::REVERSED),
            description_style: Style::default().add_modifier(Modifier::DIM),
            visible_count: 10,
        }
    }

    pub fn normal_style(mut self, style: Style) -> Self {
        self.normal_style = style;
        self
    }

    pub fn selected_style(mut self, style: Style) -> Self {
        self.selected_style = style;
        self
    }

    pub fn description_style(mut self, style: Style) -> Self {
        self.description_style = style;
        self
    }

    pub fn visible_count(mut self, count: usize) -> Self {
        self.visible_count = count;
        self
    }
}

impl<'a, T: Clone> Widget for SelectListWidget<'a, T> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        let visible = self.visible_count.min(area.height as usize);

        for (i, item) in self
            .state
            .items
            .iter()
            .skip(self.state.viewport_offset)
            .take(visible)
            .enumerate()
        {
            let y = area.top() + i as u16;
            if y >= area.bottom() {
                break;
            }

            let actual_idx = self.state.viewport_offset + i;
            let is_selected = actual_idx == self.state.selected;

            let prefix = if is_selected { "> " } else { "  " };
            let style = if is_selected {
                self.selected_style
            } else {
                self.normal_style
            };

            let label_width = item.label.len() + 2; // prefix
            let desc_start = 20.min(area.width as usize); // Align descriptions

            let line = Line::from(vec![
                Span::styled(prefix.to_string(), style),
                Span::styled(item.label.clone(), style),
                Span::styled(
                    " ".repeat(desc_start.saturating_sub(label_width)),
                    self.normal_style,
                ),
                Span::styled(item.description.clone(), self.description_style),
            ]);

            buf.set_line(area.left(), y, &line, area.width);
        }
    }
}
