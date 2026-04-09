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

    // Transient copy feedback wins over the contextual hint — users
    // get an immediate confirmation that drag-to-select + release
    // actually copied to the clipboard.
    if let Some(ref feedback) = state.copy_feedback {
        let line = Line::from(Span::styled(
            format!(" ✓ {feedback}"),
            state.theme.phase_done.add_modifier(Modifier::DIM),
        ));
        buf.set_line(area.left(), area.top(), &line, area.width);
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

    // Active drag selection — show in-flight state
    if state.mouse_selection.is_some() {
        return "SELECTED · ^C copy · Esc clear · click elsewhere to clear";
    }

    // Raw select mode (mouse capture disabled via Alt+S) — terminal
    // handles selection natively, we show nothing special
    if !state.mouse_capture_enabled {
        return "TERMINAL SELECT · Alt+S restore TUI mouse · ^O activity";
    }

    // Idle default (mouse capture ON, exclusive TUI, drag-to-select)
    "Enter submit · ^C cancel · ^Y yank · drag to copy · ^O activity · ? help"
}
