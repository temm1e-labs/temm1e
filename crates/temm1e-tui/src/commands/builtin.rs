//! Built-in slash command implementations.

use super::registry::{CommandDef, CommandRegistry, CommandResult, OverlayKind};

/// Register all built-in commands.
pub fn register_builtins(registry: &mut CommandRegistry) {
    registry.register(CommandDef {
        name: "help",
        description: "Show help and available commands",
        handler: Box::new(|_, _| CommandResult::ShowOverlay(OverlayKind::Help)),
    });

    registry.register(CommandDef {
        name: "clear",
        description: "Clear chat display",
        handler: Box::new(|_, _| CommandResult::ClearChat),
    });

    registry.register(CommandDef {
        name: "model",
        description: "Show or switch the current model",
        handler: Box::new(|args, ctx| {
            if args.is_empty() {
                CommandResult::ShowOverlay(OverlayKind::ModelPicker)
            } else {
                CommandResult::DisplayMessage(format!(
                    "Model switch to '{}' requested (current: {})",
                    args, ctx.current_model,
                ))
            }
        }),
    });

    registry.register(CommandDef {
        name: "config",
        description: "Show configuration",
        handler: Box::new(|_, _| CommandResult::ShowOverlay(OverlayKind::Config)),
    });

    registry.register(CommandDef {
        name: "keys",
        description: "List configured API providers",
        handler: Box::new(|_, _| CommandResult::ShowOverlay(OverlayKind::Keys)),
    });

    registry.register(CommandDef {
        name: "usage",
        description: "Show token and cost summary",
        handler: Box::new(|_, _| CommandResult::ShowOverlay(OverlayKind::Usage)),
    });

    registry.register(CommandDef {
        name: "status",
        description: "Show system status",
        handler: Box::new(|_, _| CommandResult::ShowOverlay(OverlayKind::Status)),
    });

    registry.register(CommandDef {
        name: "quit",
        description: "Exit the TUI",
        handler: Box::new(|_, _| CommandResult::Quit),
    });

    registry.register(CommandDef {
        name: "compact",
        description: "Compact conversation history",
        handler: Box::new(|_, _| {
            CommandResult::DisplayMessage("Conversation compacted.".to_string())
        }),
    });
}
