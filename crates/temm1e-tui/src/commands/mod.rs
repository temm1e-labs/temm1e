//! Slash command system — registry, parsing, tab completion.

pub mod builtin;
pub mod completer;
pub mod registry;

pub use registry::{CommandContext, CommandRegistry, CommandResult};
