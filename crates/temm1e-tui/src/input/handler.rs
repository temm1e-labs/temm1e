//! Keyboard event dispatch — maps crossterm events to input state mutations.

use crossterm::event::{Event as CrosstermEvent, KeyEventKind};

use super::keybindings::{map_key, Action};
use super::multiline::InputState;

/// Result of handling a key event in the input area.
#[derive(Debug)]
pub enum InputResult {
    /// Input was consumed by the input widget (e.g. character typed).
    Consumed,
    /// User submitted input text.
    Submit(String),
    /// User wants to interrupt the agent (Ctrl+C while agent working).
    Interrupt,
    /// User wants to quit (Ctrl+D on empty input).
    Quit,
    /// Force terminal redraw (Ctrl+L).
    Redraw,
    /// Toggle observability panel (Ctrl+O).
    ToggleActivityPanel,
    /// Scroll message history.
    ScrollUp,
    ScrollDown,
    PageUp,
    PageDown,
    /// Close overlay or go back (Esc).
    Escape,
    /// Tab completion requested.
    TabComplete,
    /// Yank a code block (Ctrl+Y).
    YankCodeBlock,
    /// Toggle mouse capture / select mode (Alt+S).
    ToggleMouseCapture,
    /// Key was not handled by input.
    NotHandled,
}

/// Handle a crossterm event for the input area.
pub fn handle_input_event(
    event: &CrosstermEvent,
    input: &mut InputState,
    is_agent_working: bool,
) -> InputResult {
    let key = match event {
        CrosstermEvent::Key(key) if key.kind == KeyEventKind::Press => key,
        _ => return InputResult::NotHandled,
    };

    let action = map_key(*key);

    match action {
        Action::InsertChar(c) => {
            input.insert_char(c);
            InputResult::Consumed
        }
        Action::Submit => {
            if input.is_empty() {
                return InputResult::Consumed;
            }
            let text = input.submit();
            InputResult::Submit(text)
        }
        Action::NewLine => {
            input.insert_newline();
            InputResult::Consumed
        }
        Action::Backspace => {
            input.backspace();
            InputResult::Consumed
        }
        Action::Delete => {
            input.delete();
            InputResult::Consumed
        }
        Action::Left => {
            input.move_left();
            InputResult::Consumed
        }
        Action::Right => {
            input.move_right();
            InputResult::Consumed
        }
        Action::Up => {
            input.move_up();
            InputResult::Consumed
        }
        Action::Down => {
            input.move_down();
            InputResult::Consumed
        }
        Action::Home => {
            input.home();
            InputResult::Consumed
        }
        Action::End => {
            input.end();
            InputResult::Consumed
        }
        Action::KillToEnd => {
            input.kill_to_end();
            InputResult::Consumed
        }
        Action::KillToStart => {
            input.kill_to_start();
            InputResult::Consumed
        }
        Action::Interrupt => {
            if is_agent_working {
                InputResult::Interrupt
            } else {
                input.clear();
                InputResult::Consumed
            }
        }
        Action::Quit => {
            if input.is_empty() {
                InputResult::Quit
            } else {
                InputResult::NotHandled
            }
        }
        Action::TabComplete => InputResult::TabComplete,
        Action::Redraw => InputResult::Redraw,
        Action::ToggleActivityPanel => InputResult::ToggleActivityPanel,
        Action::ScrollUp => InputResult::ScrollUp,
        Action::ScrollDown => InputResult::ScrollDown,
        Action::PageUp => InputResult::PageUp,
        Action::PageDown => InputResult::PageDown,
        Action::Escape => InputResult::Escape,
        Action::YankCodeBlock => InputResult::YankCodeBlock,
        Action::ToggleMouseCapture => InputResult::ToggleMouseCapture,
        Action::None => InputResult::NotHandled,
    }
}
