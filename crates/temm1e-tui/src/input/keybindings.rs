//! Key binding definitions and mapping.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Actions that can be triggered by key events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Insert a character at cursor position.
    InsertChar(char),
    /// Submit the current input.
    Submit,
    /// Insert a newline (Shift+Enter or trailing backslash).
    NewLine,
    /// Delete character before cursor.
    Backspace,
    /// Delete character at cursor.
    Delete,
    /// Move cursor left.
    Left,
    /// Move cursor right.
    Right,
    /// Move cursor up (history or multi-line).
    Up,
    /// Move cursor down (history or multi-line).
    Down,
    /// Move cursor to start of line.
    Home,
    /// Move cursor to end of line.
    End,
    /// Kill to end of line (Ctrl+K).
    KillToEnd,
    /// Kill to start of line (Ctrl+U).
    KillToStart,
    /// Cycle tab completion.
    TabComplete,
    /// Interrupt agent or clear input (Ctrl+C).
    Interrupt,
    /// Quit if input empty (Ctrl+D).
    Quit,
    /// Force redraw (Ctrl+L).
    Redraw,
    /// Toggle observability panel (Ctrl+O).
    ToggleActivityPanel,
    /// Scroll messages up.
    ScrollUp,
    /// Scroll messages down.
    ScrollDown,
    /// Page up in message history.
    PageUp,
    /// Page down in message history.
    PageDown,
    /// Close overlay / go back (Esc).
    Escape,
    /// No action for this key.
    None,
}

/// Map a crossterm key event to a TUI action.
pub fn map_key(key: KeyEvent) -> Action {
    match (key.modifiers, key.code) {
        // Submit
        (KeyModifiers::NONE, KeyCode::Enter) => Action::Submit,
        // Newline
        (KeyModifiers::SHIFT, KeyCode::Enter) => Action::NewLine,
        // Navigation
        (KeyModifiers::NONE, KeyCode::Left) => Action::Left,
        (KeyModifiers::NONE, KeyCode::Right) => Action::Right,
        (KeyModifiers::NONE, KeyCode::Up) => Action::Up,
        (KeyModifiers::NONE, KeyCode::Down) => Action::Down,
        (KeyModifiers::NONE, KeyCode::Home) => Action::Home,
        (KeyModifiers::NONE, KeyCode::End) => Action::End,
        // Editing
        (KeyModifiers::NONE, KeyCode::Backspace) => Action::Backspace,
        (KeyModifiers::NONE, KeyCode::Delete) => Action::Delete,
        (KeyModifiers::NONE, KeyCode::Tab) => Action::TabComplete,
        // Ctrl combos
        (KeyModifiers::CONTROL, KeyCode::Char('a')) => Action::Home,
        (KeyModifiers::CONTROL, KeyCode::Char('e')) => Action::End,
        (KeyModifiers::CONTROL, KeyCode::Char('k')) => Action::KillToEnd,
        (KeyModifiers::CONTROL, KeyCode::Char('u')) => Action::KillToStart,
        (KeyModifiers::CONTROL, KeyCode::Char('c')) => Action::Interrupt,
        (KeyModifiers::CONTROL, KeyCode::Char('d')) => Action::Quit,
        (KeyModifiers::CONTROL, KeyCode::Char('l')) => Action::Redraw,
        (KeyModifiers::CONTROL, KeyCode::Char('o')) => Action::ToggleActivityPanel,
        // Scrolling
        (KeyModifiers::NONE, KeyCode::PageUp) => Action::PageUp,
        (KeyModifiers::NONE, KeyCode::PageDown) => Action::PageDown,
        (KeyModifiers::SHIFT, KeyCode::Up) => Action::ScrollUp,
        (KeyModifiers::SHIFT, KeyCode::Down) => Action::ScrollDown,
        // Escape
        (KeyModifiers::NONE, KeyCode::Esc) => Action::Escape,
        // Character input
        (KeyModifiers::NONE | KeyModifiers::SHIFT, KeyCode::Char(c)) => Action::InsertChar(c),
        _ => Action::None,
    }
}
