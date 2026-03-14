//! Check messages tool — lets the agent peek at pending user messages that
//! arrived while it is busy processing a task. This gives the agent awareness
//! of its environment so it can decide whether to acknowledge, wrap up early,
//! or continue its current work.
//!
//! The tool reads from a shared pending-message queue and returns whatever is
//! waiting. Messages are cleared after reading so the agent won't see
//! duplicates on subsequent checks.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};

/// Shared pending-message queue. Maps chat_id → list of message texts.
pub type PendingMessages = Arc<Mutex<HashMap<String, Vec<String>>>>;

pub struct CheckMessagesTool {
    pending: PendingMessages,
}

impl CheckMessagesTool {
    pub fn new(pending: PendingMessages) -> Self {
        Self { pending }
    }
}

#[async_trait]
impl Tool for CheckMessagesTool {
    fn name(&self) -> &str {
        "check_messages"
    }

    fn description(&self) -> &str {
        "Check if the user sent any new messages while you are busy working. \
         Returns pending message texts or 'No pending messages'. \
         Use this during long-running tasks (every few rounds) to stay \
         responsive. If a message is waiting, acknowledge it with \
         send_message and decide whether to continue or wrap up."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {},
            "required": []
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
        _input: ToolInput,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let mut pending = self.pending.lock().unwrap_or_else(|e| e.into_inner());
        let messages = pending.remove(&ctx.chat_id).unwrap_or_default();

        let content = if messages.is_empty() {
            "No pending messages.".to_string()
        } else {
            let count = messages.len();
            let formatted: Vec<String> = messages
                .iter()
                .enumerate()
                .map(|(i, text)| format!("  {}. \"{}\"", i + 1, text))
                .collect();
            format!(
                "{} pending message(s) from user:\n{}",
                count,
                formatted.join("\n")
            )
        };

        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}
