//! Context-sensitive keybind hint bar.
//!
//! Shows a single dim line above the status bar with the most
//! relevant keybinds for the current UI state. Reduces mode
//! discoverability anxiety.

use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};

use crate::app::{AppState, Overlay};

pub fn render_hint_bar(state: &AppState, area: Rect, buf: &mut Buffer) {
    if area.height == 0 || area.width == 0 {
        return;
    }

    let hint = hint_for_state(state);
    let line = Line::from(Span::styled(
        format!(" {}", hint),
        state.theme.secondary.add_modifier(Modifier::DIM),
    ));
    buf.set_line(area.left(), area.top(), &line, area.width);
}

fn hint_for_state(state: &AppState) -> &'static str {
    // Overlay hints take precedence — modal interactions dominate
    match state.overlay {
        Overlay::Help => return "Esc close",
        Overlay::Config(_) => return "Esc close",
        Overlay::CopyPicker => return "1-9 copy · Esc cancel",
        Overlay::None => {}
    }

    // Scroll mode — user scrolled away from the bottom
    if state.message_list.scroll_offset > 0 {
        return "SCROLL · G bottom · Esc exit scroll";
    }

    // Agent working — offer cancel
    if state.is_agent_working {
        return "Esc cancel · ^O activity · ^C cancel (×2 quit)";
    }

    // Scroll-wheel mode (mouse capture ON) — unusual; surface the toggle
    if state.mouse_capture_enabled {
        return "SCROLL MODE · Alt+S select · Enter submit · ^C cancel";
    }

    // Idle default (select mode — mouse capture OFF)
    "Enter submit · ^C cancel · ^Y yank · drag to select · ^O activity · ? help"
}
