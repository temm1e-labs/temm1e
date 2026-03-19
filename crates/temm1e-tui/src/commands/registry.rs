//! Command registry — stores commands, dispatches, provides completions.

use std::collections::HashMap;

/// Context passed to command handlers.
pub struct CommandContext {
    pub current_model: String,
    pub current_provider: String,
}

/// Result of executing a slash command.
#[derive(Debug)]
pub enum CommandResult {
    /// Display a message to the user.
    DisplayMessage(String),
    /// Show an overlay (help, model picker, config).
    ShowOverlay(OverlayKind),
    /// Clear the chat display.
    ClearChat,
    /// Quit the TUI.
    Quit,
    /// No output (command handled internally).
    Silent,
    /// Error message.
    Error(String),
}

/// Overlay types that commands can trigger.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OverlayKind {
    Help,
    ModelPicker,
    Config,
    Keys,
    Usage,
    Status,
}

/// Handler function type for slash commands.
pub type CommandHandler = Box<dyn Fn(&str, &CommandContext) -> CommandResult + Send + Sync>;

/// A registered slash command.
pub struct CommandDef {
    pub name: &'static str,
    pub description: &'static str,
    pub handler: CommandHandler,
}

/// Registry of all available slash commands.
pub struct CommandRegistry {
    commands: HashMap<&'static str, CommandDef>,
}

impl CommandRegistry {
    pub fn new() -> Self {
        let mut registry = Self {
            commands: HashMap::new(),
        };
        super::builtin::register_builtins(&mut registry);
        registry
    }

    pub fn register(&mut self, cmd: CommandDef) {
        self.commands.insert(cmd.name, cmd);
    }

    /// Try to dispatch a slash command. Returns None if input is not a command.
    pub fn dispatch(&self, input: &str, ctx: &CommandContext) -> Option<CommandResult> {
        let trimmed = input.trim();
        let after_slash = trimmed.strip_prefix('/')?;

        let (name, args) = match after_slash.split_once(' ') {
            Some((n, a)) => (n, a.trim()),
            None => (after_slash, ""),
        };

        // Don't treat file paths as commands — commands are short alphanumeric words
        if name.contains('/') || name.contains('.') || name.len() > 20 {
            return None;
        }

        if let Some(cmd) = self.commands.get(name) {
            Some((cmd.handler)(args, ctx))
        } else {
            Some(CommandResult::Error(format!(
                "Unknown command: /{}. Type /help for available commands.",
                name
            )))
        }
    }

    /// Get completion candidates for a partial command name.
    pub fn completions(&self, partial: &str) -> Vec<(&'static str, &'static str)> {
        let query = partial.strip_prefix('/').unwrap_or(partial);
        let lower = query.to_lowercase();

        let mut matches: Vec<_> = self
            .commands
            .values()
            .filter(|cmd| cmd.name.starts_with(&lower))
            .map(|cmd| (cmd.name, cmd.description))
            .collect();
        matches.sort_by_key(|(name, _)| *name);
        matches
    }

    /// Get all commands sorted by name.
    pub fn all_commands(&self) -> Vec<(&'static str, &'static str)> {
        let mut cmds: Vec<_> = self
            .commands
            .values()
            .map(|cmd| (cmd.name, cmd.description))
            .collect();
        cmds.sort_by_key(|(name, _)| *name);
        cmds
    }
}

impl Default for CommandRegistry {
    fn default() -> Self {
        Self::new()
    }
}
