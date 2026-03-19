//! Mode switch tool — toggles Temm1e's personality mode at runtime.
//!
//! The agent can use this tool to switch between PLAY mode (:3), WORK mode (>:3), and PRO mode (professional).

use std::sync::Arc;

use async_trait::async_trait;
use temm1e_core::types::config::Temm1eMode;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};
use tokio::sync::RwLock;

/// Shared runtime mode state. Wrap this in `Arc<RwLock<Temm1eMode>>` and pass
/// the same handle to the tool AND the system-prompt builder so both see
/// real-time updates.
pub type SharedMode = Arc<RwLock<Temm1eMode>>;

pub struct ModeSwitchTool {
    mode: SharedMode,
}

impl ModeSwitchTool {
    pub fn new(mode: SharedMode) -> Self {
        Self { mode }
    }
}

#[async_trait]
impl Tool for ModeSwitchTool {
    fn name(&self) -> &str {
        "mode_switch"
    }

    fn description(&self) -> &str {
        "Switch Tem's personality mode between PLAY (warm, chaotic, :3), \
         WORK (sharp, analytical, >:3), or PRO (professional, no emoticons). \
         Use this when the user asks to change the vibe or when a task requires a different energy."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "mode": {
                    "type": "string",
                    "enum": ["play", "work", "pro"],
                    "description": "The personality mode to switch to: 'play' for warm/chaotic energy, 'work' for sharp/analytical precision, 'pro' for professional/business tone"
                }
            },
            "required": ["mode"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: Vec::new(),
            network_access: Vec::new(),
            shell_access: false,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let mode_str = input
            .arguments
            .get("mode")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: mode".into()))?;

        let new_mode = match mode_str.to_lowercase().as_str() {
            "play" => Temm1eMode::Play,
            "work" => Temm1eMode::Work,
            "pro" => Temm1eMode::Pro,
            other => {
                return Ok(ToolOutput {
                    content: format!("Unknown mode '{}'. Valid modes: play, work, pro", other),
                    is_error: true,
                });
            }
        };

        let old_mode = {
            let mut guard = self.mode.write().await;
            let old = *guard;
            *guard = new_mode;
            old
        };

        tracing::info!(from = %old_mode, to = %new_mode, "Temm1e personality mode switched");

        let message = match new_mode {
            Temm1eMode::Play => "Mode switched to PLAY! Let's have some fun! :3".to_string(),
            Temm1eMode::Work => "Mode switched to WORK. Ready to execute. >:3".to_string(),
            Temm1eMode::Pro => "Mode switched to PRO. Professional mode engaged.".to_string(),
            // None is never reachable here — the tool only accepts play/work/pro
            // and is not registered when personality is None (locked).
            Temm1eMode::None => "Mode unchanged.".to_string(),
        };

        Ok(ToolOutput {
            content: message,
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn test_ctx() -> ToolContext {
        ToolContext {
            workspace_path: PathBuf::from("/tmp/test"),
            session_id: "test-session".to_string(),
            chat_id: "chat-123".to_string(),
        }
    }

    fn make_input(args: serde_json::Value) -> ToolInput {
        ToolInput {
            name: "mode_switch".to_string(),
            arguments: args,
        }
    }

    #[tokio::test]
    async fn switch_to_play() {
        let mode = Arc::new(RwLock::new(Temm1eMode::Work));
        let tool = ModeSwitchTool::new(mode.clone());
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({"mode": "play"}));
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("PLAY"));
        assert!(output.content.contains(":3"));
        assert_eq!(*mode.read().await, Temm1eMode::Play);
    }

    #[tokio::test]
    async fn switch_to_work() {
        let mode = Arc::new(RwLock::new(Temm1eMode::Play));
        let tool = ModeSwitchTool::new(mode.clone());
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({"mode": "work"}));
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("WORK"));
        assert!(output.content.contains(">:3"));
        assert_eq!(*mode.read().await, Temm1eMode::Work);
    }

    #[tokio::test]
    async fn switch_to_pro() {
        let mode = Arc::new(RwLock::new(Temm1eMode::Play));
        let tool = ModeSwitchTool::new(mode.clone());
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({"mode": "pro"}));
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("PRO"));
        assert!(!output.content.contains(":3"));
        assert_eq!(*mode.read().await, Temm1eMode::Pro);
    }

    #[tokio::test]
    async fn invalid_mode() {
        let mode = Arc::new(RwLock::new(Temm1eMode::Play));
        let tool = ModeSwitchTool::new(mode.clone());
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({"mode": "chaos"}));
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("Unknown mode"));
        // Mode should not change
        assert_eq!(*mode.read().await, Temm1eMode::Play);
    }

    #[tokio::test]
    async fn missing_mode_param() {
        let mode = Arc::new(RwLock::new(Temm1eMode::Play));
        let tool = ModeSwitchTool::new(mode.clone());
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({}));
        let result = tool.execute(input, &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn tool_metadata() {
        let mode = Arc::new(RwLock::new(Temm1eMode::Play));
        let tool = ModeSwitchTool::new(mode);

        assert_eq!(tool.name(), "mode_switch");
        assert!(tool.description().contains("personality"));
        assert!(!tool.declarations().shell_access);

        let schema = tool.parameters_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("mode").is_some());
    }
}
