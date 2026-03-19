//! Input handling — keyboard dispatch, multi-line editing, key bindings.

pub mod handler;
pub mod keybindings;
pub mod multiline;

pub use handler::{handle_input_event, InputResult};
pub use keybindings::Action;
pub use multiline::InputState;
