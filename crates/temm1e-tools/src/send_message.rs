//! Send message tool — sends a text message to the user during tool execution.
//! This allows the agent to send intermediate messages (progress updates,
//! periodic outputs, etc.) without waiting for the final reply.

use std::sync::Arc;

use async_trait::async_trait;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::OutboundMessage;
use temm1e_core::{Channel, Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};

pub struct SendMessageTool {
    channel: Arc<dyn Channel>,
}

impl SendMessageTool {
    pub fn new(channel: Arc<dyn Channel>) -> Self {
        Self { channel }
    }
}

#[async_trait]
impl Tool for SendMessageTool {
    fn name(&self) -> &str {
        "send_message"
    }

    fn description(&self) -> &str {
        "Send a text message to the user immediately during tool execution. \
         Use this when you need to send intermediate results, progress updates, \
         or periodic messages before your final reply. The message is delivered \
         instantly — you don't have to wait until the end of your response."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "text": {
                    "type": "string",
                    "description": "The message text to send"
                },
                "chat_id": {
                    "type": "string",
                    "description": "The chat ID to send to. Omit to send to the current conversation."
                }
            },
            "required": ["text"]
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
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let text = input
            .arguments
            .get("text")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: text".into()))?;

        let chat_id = input
            .arguments
            .get("chat_id")
            .and_then(|v| v.as_str())
            .unwrap_or(&ctx.chat_id);

        let outbound = OutboundMessage {
            chat_id: chat_id.to_string(),
            text: text.to_string(),
            reply_to: None,
            parse_mode: None,
        };

        match self.channel.send_message(outbound).await {
            Ok(()) => Ok(ToolOutput {
                content: "Message sent".to_string(),
                is_error: false,
            }),
            Err(e) => Ok(ToolOutput {
                content: format!("Failed to send message: {}", e),
                is_error: true,
            }),
        }
    }
}
