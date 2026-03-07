//! Context builder — assembles a CompletionRequest from session history,
//! memory search results, system prompt, and tool definitions.

use std::sync::Arc;

use skyclaw_core::Memory;
use skyclaw_core::SearchOpts;
use skyclaw_core::Tool;
use skyclaw_core::types::message::{
    ChatMessage, CompletionRequest, MessageContent, Role, ToolDefinition,
};
use skyclaw_core::types::session::SessionContext;

/// Build a CompletionRequest from all available context.
pub async fn build_context(
    session: &SessionContext,
    memory: &dyn Memory,
    tools: &[Arc<dyn Tool>],
    model: &str,
    system_prompt: Option<&str>,
) -> CompletionRequest {
    let mut messages: Vec<ChatMessage> = Vec::new();

    // 1. Retrieve relevant memory entries for context augmentation
    let query = session
        .history
        .last()
        .and_then(|m| match &m.content {
            MessageContent::Text(t) => Some(t.clone()),
            MessageContent::Parts(parts) => parts.iter().find_map(|p| match p {
                skyclaw_core::types::message::ContentPart::Text { text } => Some(text.clone()),
                _ => None,
            }),
        })
        .unwrap_or_default();

    if !query.is_empty() {
        let opts = SearchOpts {
            limit: 5,
            session_filter: Some(session.session_id.clone()),
            ..Default::default()
        };

        if let Ok(entries) = memory.search(&query, opts).await {
            if !entries.is_empty() {
                let memory_text: String = entries
                    .iter()
                    .map(|e| format!("[{}] {}", e.timestamp.format("%Y-%m-%d %H:%M"), e.content))
                    .collect::<Vec<_>>()
                    .join("\n");

                messages.push(ChatMessage {
                    role: Role::System,
                    content: MessageContent::Text(format!(
                        "Relevant context from memory:\n{}",
                        memory_text
                    )),
                });
            }
        }
    }

    // 2. Append session conversation history
    messages.extend(session.history.clone());

    // 3. Build tool definitions
    let tool_defs: Vec<ToolDefinition> = tools
        .iter()
        .map(|t| ToolDefinition {
            name: t.name().to_string(),
            description: t.description().to_string(),
            parameters: t.parameters_schema(),
        })
        .collect();

    // 4. Assemble the system prompt
    let system = system_prompt.map(|s| s.to_string()).or_else(|| {
        Some(
            "You are SkyClaw, a cloud-native AI agent. You have access to tools for \
             shell execution, file operations, browsing, and more. Use them when needed \
             to assist the user. Always be precise and security-conscious."
                .to_string(),
        )
    });

    CompletionRequest {
        model: model.to_string(),
        messages,
        tools: tool_defs,
        max_tokens: Some(4096),
        temperature: Some(0.7),
        system,
    }
}
