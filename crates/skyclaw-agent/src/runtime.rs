//! AgentRuntime — main agent loop that processes messages through the
//! provider, executing tool calls in a loop until a final text reply.

use std::sync::Arc;

use skyclaw_core::{Memory, Provider, Tool};
use skyclaw_core::types::error::SkyclawError;
use skyclaw_core::types::message::{
    ChatMessage, ContentPart, InboundMessage, MessageContent,
    OutboundMessage, ParseMode, Role,
};
use skyclaw_core::types::session::SessionContext;
use tracing::{debug, info, warn};

use crate::context::build_context;
use crate::executor::execute_tool;

/// Maximum number of tool-use rounds before forcing a text reply.
const MAX_TOOL_ROUNDS: usize = 10;

/// The core agent runtime. Holds references to the AI provider, memory backend,
/// and registered tools.
pub struct AgentRuntime {
    provider: Arc<dyn Provider>,
    memory: Arc<dyn Memory>,
    tools: Vec<Arc<dyn Tool>>,
    model: String,
    system_prompt: Option<String>,
}

impl AgentRuntime {
    /// Create a new AgentRuntime.
    pub fn new(
        provider: Arc<dyn Provider>,
        memory: Arc<dyn Memory>,
        tools: Vec<Arc<dyn Tool>>,
        model: String,
        system_prompt: Option<String>,
    ) -> Self {
        Self {
            provider,
            memory,
            tools,
            model,
            system_prompt,
        }
    }

    /// Process an inbound message through the full agent loop:
    /// 1. Build context (history + memory + tools)
    /// 2. Call the provider
    /// 3. If the provider returns tool_use, execute tools and loop
    /// 4. Return the final text reply as an OutboundMessage
    pub async fn process_message(
        &self,
        msg: &InboundMessage,
        session: &mut SessionContext,
    ) -> Result<OutboundMessage, SkyclawError> {
        info!(
            channel = %msg.channel,
            chat_id = %msg.chat_id,
            user_id = %msg.user_id,
            "Processing inbound message"
        );

        // Append the user message to session history
        let user_text = msg.text.clone().unwrap_or_default();
        session.history.push(ChatMessage {
            role: Role::User,
            content: MessageContent::Text(user_text),
        });

        // Tool-use loop
        let mut rounds = 0;
        loop {
            rounds += 1;
            if rounds > MAX_TOOL_ROUNDS {
                warn!("Exceeded maximum tool rounds ({}), forcing text reply", MAX_TOOL_ROUNDS);
                break;
            }

            // Build the completion request from full context
            let request = build_context(
                session,
                self.memory.as_ref(),
                &self.tools,
                &self.model,
                self.system_prompt.as_deref(),
            )
            .await;

            debug!(round = rounds, messages = request.messages.len(), "Sending completion request");

            // Call the provider
            let response = self.provider.complete(request).await?;

            // Separate text content from tool-use content
            let mut text_parts: Vec<String> = Vec::new();
            let mut tool_uses: Vec<(String, String, serde_json::Value)> = Vec::new();

            for part in &response.content {
                match part {
                    ContentPart::Text { text } => {
                        text_parts.push(text.clone());
                    }
                    ContentPart::ToolUse { id, name, input } => {
                        tool_uses.push((id.clone(), name.clone(), input.clone()));
                    }
                    ContentPart::ToolResult { .. } => {
                        // Should not appear in provider response, ignore
                    }
                }
            }

            // If no tool calls, we have our final reply
            if tool_uses.is_empty() {
                let reply_text = text_parts.join("\n");

                // Record assistant reply in history
                session.history.push(ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Text(reply_text.clone()),
                });

                return Ok(OutboundMessage {
                    chat_id: msg.chat_id.clone(),
                    text: reply_text,
                    reply_to: Some(msg.id.clone()),
                    parse_mode: Some(ParseMode::Markdown),
                });
            }

            // Record the assistant message (with tool_use parts) in history
            session.history.push(ChatMessage {
                role: Role::Assistant,
                content: MessageContent::Parts(response.content.clone()),
            });

            // Execute each tool call and collect results
            let mut tool_result_parts: Vec<ContentPart> = Vec::new();

            for (tool_use_id, tool_name, arguments) in &tool_uses {
                info!(tool = %tool_name, id = %tool_use_id, "Executing tool call");

                let result = execute_tool(tool_name, arguments.clone(), &self.tools, session).await;

                let (content, is_error) = match result {
                    Ok(output) => (output.content, output.is_error),
                    Err(e) => (format!("Tool execution error: {}", e), true),
                };

                tool_result_parts.push(ContentPart::ToolResult {
                    tool_use_id: tool_use_id.clone(),
                    content,
                    is_error,
                });
            }

            // Append tool results as a Tool message in history
            session.history.push(ChatMessage {
                role: Role::Tool,
                content: MessageContent::Parts(tool_result_parts),
            });

            // Continue the loop — provider will see the tool results and may
            // issue more tool calls or produce a final text reply.
        }

        // Fallback: if we exited the loop due to max rounds
        Ok(OutboundMessage {
            chat_id: msg.chat_id.clone(),
            text: "I reached the maximum number of tool execution steps. Here is what I have so far. Please let me know if you need me to continue.".to_string(),
            reply_to: Some(msg.id.clone()),
            parse_mode: Some(ParseMode::Plain),
        })
    }

    /// Get a reference to the provider.
    pub fn provider(&self) -> &dyn Provider {
        self.provider.as_ref()
    }

    /// Get a reference to the memory backend.
    pub fn memory(&self) -> &dyn Memory {
        self.memory.as_ref()
    }

    /// Get the registered tools.
    pub fn tools(&self) -> &[Arc<dyn Tool>] {
        &self.tools
    }
}
