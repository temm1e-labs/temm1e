use serde::{Deserialize, Serialize};

/// Normalized inbound message from any channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    pub id: String,
    pub channel: String,
    pub chat_id: String,
    pub user_id: String,
    pub username: Option<String>,
    pub text: Option<String>,
    pub attachments: Vec<AttachmentRef>,
    pub reply_to: Option<String>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
}

/// Reference to a file attachment (platform-specific ID for lazy download)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AttachmentRef {
    pub file_id: String,
    pub file_name: Option<String>,
    pub mime_type: Option<String>,
    pub size: Option<usize>,
}

/// Outbound message to send via a channel
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub chat_id: String,
    pub text: String,
    pub reply_to: Option<String>,
    pub parse_mode: Option<ParseMode>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ParseMode {
    Markdown,
    Html,
    Plain,
}

/// Request to an AI model provider
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionRequest {
    pub model: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDefinition>,
    pub max_tokens: Option<u32>,
    pub temperature: Option<f32>,
    pub system: Option<String>,
}

/// A single message in the conversation
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub role: Role,
    pub content: MessageContent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ContentPart {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse { id: String, name: String, input: serde_json::Value },
    #[serde(rename = "tool_result")]
    ToolResult { tool_use_id: String, content: String, is_error: bool },
}

/// Tool definition for the AI model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ToolDefinition {
    pub name: String,
    pub description: String,
    pub parameters: serde_json::Value,
}

/// Response from an AI model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CompletionResponse {
    pub id: String,
    pub content: Vec<ContentPart>,
    pub stop_reason: Option<String>,
    pub usage: Usage,
}

/// Streaming chunk from an AI model
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamChunk {
    pub delta: Option<String>,
    pub tool_use: Option<ContentPart>,
    pub stop_reason: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}
