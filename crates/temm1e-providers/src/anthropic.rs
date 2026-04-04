use async_trait::async_trait;
use futures::stream::BoxStream;
use reqwest::Client;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart, MessageContent, Role,
    StreamChunk, ToolDefinition, Usage,
};
use temm1e_core::Provider;
use tracing::{debug, error, info};

/// Anthropic Messages API provider with key rotation.
pub struct AnthropicProvider {
    client: Client,
    keys: Vec<String>,
    key_index: AtomicUsize,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| Client::new()),
            keys: vec![api_key],
            key_index: AtomicUsize::new(0),
            base_url: "https://api.anthropic.com".to_string(),
        }
    }

    pub fn with_keys(mut self, keys: Vec<String>) -> Self {
        if !keys.is_empty() {
            self.keys = keys;
        }
        self
    }

    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }

    /// Get the current API key via round-robin rotation.
    fn current_key(&self) -> &str {
        if self.keys.is_empty() {
            return "";
        }
        let idx = self.key_index.load(Ordering::Relaxed) % self.keys.len();
        &self.keys[idx]
    }

    /// Advance to the next key (called on rate limit).
    fn rotate_key(&self) {
        if self.keys.is_empty() {
            return;
        }
        let old = self.key_index.fetch_add(1, Ordering::Relaxed);
        let new_idx = (old + 1) % self.keys.len();
        if self.keys.len() > 1 {
            info!(
                new_index = new_idx,
                total_keys = self.keys.len(),
                "Rotated API key"
            );
        }
    }

    /// Build the JSON body for the Anthropic Messages API.
    fn build_request_body(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> Result<serde_json::Value, Temm1eError> {
        let messages = request
            .messages
            .iter()
            .filter(|m| !matches!(m.role, Role::System))
            .map(convert_message_to_anthropic)
            .collect::<Result<Vec<_>, _>>()?;

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
            "max_tokens": request.max_tokens.unwrap_or(4096),
        });

        if let Some(ref system) = request.system {
            body["system"] = serde_json::json!(system);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(convert_tool_to_anthropic)
                .collect();
            body["tools"] = serde_json::json!(tools);
        }

        if stream {
            body["stream"] = serde_json::json!(true);
        }

        Ok(body)
    }
}

// ---------------------------------------------------------------------------
// Anthropic API serde types (response)
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct AnthropicResponse {
    id: String,
    #[serde(default)]
    r#type: Option<String>,
    content: Vec<AnthropicContentBlock>,
    stop_reason: Option<String>,
    usage: AnthropicUsage,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: serde_json::Value,
    },
    #[serde(rename = "thinking")]
    Thinking {
        thinking: String,
        signature: Option<String>,
    },
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
    #[serde(default)]
    cache_creation_input_tokens: Option<u32>,
    #[serde(default)]
    cache_read_input_tokens: Option<u32>,
}

// SSE event types
#[derive(Debug, Deserialize)]
struct AnthropicSseMessageStart {
    message: AnthropicSseMessageMeta,
}

#[derive(Debug, Default, Deserialize)]
struct AnthropicSseMessageMeta {
    id: String,
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseContentBlockStart {
    index: usize,
    content_block: AnthropicContentBlock,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseContentBlockDelta {
    index: usize,
    delta: AnthropicDelta,
}

#[derive(Debug, Deserialize)]
struct AnthropicSseContentBlockStop {
    index: usize,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "type")]
enum AnthropicDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
    #[serde(rename = "thinking_delta")]
    ThinkingDelta { thinking: String, signature: Option<String> },
}

#[derive(Debug, Default, Deserialize)]
struct AnthropicSseMessageDelta {
    #[serde(default)]
    delta: AnthropicMessageDeltaBody,
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Default, Deserialize)]
struct AnthropicMessageDeltaBody {
    #[serde(default)]
    stop_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn convert_message_to_anthropic(msg: &ChatMessage) -> Result<serde_json::Value, Temm1eError> {
    let role = match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "user", // tool results are sent as user messages in Anthropic API
        Role::System => {
            // System messages are handled separately; skip here.
            return Err(Temm1eError::Provider(
                "System role should not appear in messages list".into(),
            ));
        }
    };

    let content = match &msg.content {
        MessageContent::Text(text) => {
            if matches!(msg.role, Role::Tool) {
                // Shouldn't normally hit here, but handle gracefully
                serde_json::json!(text)
            } else {
                serde_json::json!(text)
            }
        }
        MessageContent::Parts(parts) => {
            let blocks: Vec<serde_json::Value> = parts
                .iter()
                .map(|p| match p {
                    ContentPart::Text { text } => serde_json::json!({
                        "type": "text",
                        "text": text,
                    }),
                    ContentPart::ToolUse {
                        id, name, input, ..
                    } => serde_json::json!({
                        "type": "tool_use",
                        "id": id,
                        "name": name,
                        "input": input,
                    }),
                    ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        is_error,
                    } => serde_json::json!({
                        "type": "tool_result",
                        "tool_use_id": tool_use_id,
                        "content": content,
                        "is_error": is_error,
                    }),
                    ContentPart::Image { media_type, data } => serde_json::json!({
                        "type": "image",
                        "source": {
                            "type": "base64",
                            "media_type": media_type,
                            "data": data,
                        },
                    }),
                })
                .collect();
            serde_json::json!(blocks)
        }
    };

    Ok(serde_json::json!({
        "role": role,
        "content": content,
    }))
}

fn convert_tool_to_anthropic(tool: &ToolDefinition) -> serde_json::Value {
    serde_json::json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.parameters,
    })
}

fn convert_anthropic_content(block: &AnthropicContentBlock) -> ContentPart {
    match block {
        AnthropicContentBlock::Text { text } => ContentPart::Text { text: text.clone() },
        AnthropicContentBlock::ToolUse { id, name, input } => ContentPart::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
            thought_signature: None,
        },
        // Skip thinking blocks to save tokens in context
        AnthropicContentBlock::Thinking { .. } => ContentPart::Text { text: String::new() },
    }
}

// ---------------------------------------------------------------------------
// Provider trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Provider for AnthropicProvider {
    fn name(&self) -> &str {
        "anthropic"
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, Temm1eError> {
        let body = self.build_request_body(&request, false)?;

        debug!(provider = "anthropic", model = %request.model, "Sending completion request");

        let api_key = self.current_key().to_string();
        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Anthropic request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".into());
            error!(provider = "anthropic", %status, "API error: {}", error_body);
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                self.rotate_key();
                return Err(Temm1eError::RateLimited(error_body));
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.rotate_key();
                return Err(Temm1eError::Auth(error_body));
            }
            return Err(Temm1eError::Provider(format!(
                "Anthropic API error ({status}): {error_body}"
            )));
        }

        let api_response: AnthropicResponse = response.json().await.map_err(|e| {
            Temm1eError::Provider(format!("Failed to parse Anthropic response: {e}"))
        })?;

        let content = api_response
            .content
            .iter()
            .map(convert_anthropic_content)
            .collect();

        Ok(CompletionResponse {
            id: api_response.id,
            content,
            stop_reason: api_response.stop_reason,
            usage: Usage {
                input_tokens: api_response.usage.input_tokens,
                output_tokens: api_response.usage.output_tokens,
                cost_usd: 0.0,
            },
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<BoxStream<'_, Result<StreamChunk, Temm1eError>>, Temm1eError> {
        let body = self.build_request_body(&request, true)?;

        debug!(provider = "anthropic", model = %request.model, "Sending streaming request");

        let api_key = self.current_key().to_string();
        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Anthropic stream request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".into());
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                self.rotate_key();
                return Err(Temm1eError::RateLimited(error_body));
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.rotate_key();
                return Err(Temm1eError::Auth(error_body));
            }
            return Err(Temm1eError::Provider(format!(
                "Anthropic API error ({status}): {error_body}"
            )));
        }

        // Track state across SSE events for tool_use accumulation
        let byte_stream = response.bytes_stream();

        let event_stream = futures::stream::unfold(
            (
                byte_stream,
                String::new(), // buffer for incomplete lines
                HashMap::<usize, (String, String, serde_json::Value)>::new(), // index → (id, name, partial_json)
                None::<usize>, // current_block_index — tracks last seen block index
            ),
            |(mut byte_stream, mut buffer, mut pending_blocks, mut current_index)| async move {
                use futures::StreamExt;

                loop {
                    // Try to extract a complete SSE event from the buffer
                    if let Some(event) = extract_sse_event(&mut buffer, &mut pending_blocks, &mut current_index) {
                        return Some((event, (byte_stream, buffer, pending_blocks, current_index)));
                    }

                    // Need more data
                    match byte_stream.next().await {
                        Some(Ok(bytes)) => {
                            let text = String::from_utf8_lossy(&bytes);
                            buffer.push_str(&text);
                        }
                        Some(Err(e)) => {
                            return Some((
                                Err(Temm1eError::Provider(format!("Stream read error: {e}"))),
                                (byte_stream, buffer, pending_blocks, current_index),
                            ));
                        }
                        None => {
                            // Stream ended
                            return None;
                        }
                    }
                }
            },
        );

        Ok(Box::pin(event_stream))
    }

    async fn health_check(&self) -> Result<bool, Temm1eError> {
        let resp = self
            .client
            .head(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", self.current_key())
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Health check failed: {e}")))?;

        // Anthropic may return 405 for HEAD which still means the server is reachable
        Ok(resp.status().is_success() || resp.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED)
    }

    async fn list_models(&self) -> Result<Vec<String>, Temm1eError> {
        Ok(vec![
            "claude-opus-4-6".to_string(),
            "claude-sonnet-4-6".to_string(),
            "claude-haiku-4-5-20251001".to_string(),
            "claude-3-5-sonnet-20241022".to_string(),
            "claude-3-5-haiku-20241022".to_string(),
        ])
    }
}

// ---------------------------------------------------------------------------
// SSE parsing helpers
// ---------------------------------------------------------------------------

// Make the SSE parsing function visible to tests
/// Try to extract and parse the next complete SSE event from the buffer.
/// Returns `Some(Result<StreamChunk>)` if an event was parsed, `None` if more data is needed.
fn extract_sse_event(
    buffer: &mut String,
    pending_blocks: &mut HashMap<usize, (String, String, serde_json::Value)>,
    current_index: &mut Option<usize>,
) -> Option<Result<StreamChunk, Temm1eError>> {
    // SSE events are terminated by a blank line (\n\n)
    loop {
        let double_newline = buffer.find("\n\n")?;
        let event_text: String = buffer.drain(..=double_newline + 1).collect();

        let mut event_type = String::new();
        let mut data_parts = Vec::new();

        for line in event_text.lines() {
            if let Some(rest) = line.strip_prefix("event: ") {
                event_type = rest.trim().to_string();
            } else if let Some(rest) = line.strip_prefix("data: ") {
                data_parts.push(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                // "data:" with no space
                data_parts.push(rest.to_string());
            }
        }

        let data = data_parts.join("\n");
        if data.is_empty() && event_type.is_empty() {
            // Empty event (keep-alive), skip
            continue;
        }

        match event_type.as_str() {
            "message_start" => {
                // Contains the message id; we don't emit a chunk for this
                continue;
            }
            "content_block_start" => {
                if let Ok(parsed) = serde_json::from_str::<AnthropicSseContentBlockStart>(&data) {
                    let index = parsed.index;
                    *current_index = Some(index);
                    match parsed.content_block {
                        AnthropicContentBlock::ToolUse { id, name, .. } => {
                            // Index-keyed accumulation — safe against thinking blocks interleaving
                            pending_blocks.insert(index, (id, name, serde_json::Value::Null));
                        }
                        AnthropicContentBlock::Text { .. } => {
                            // Text block start, no content yet
                        }
                        AnthropicContentBlock::Thinking { .. } => {
                            // Thinking blocks: track index but don't accumulate
                        }
                    }
                }
                continue;
            }
            "content_block_delta" => {
                if let Ok(parsed) = serde_json::from_str::<AnthropicSseContentBlockDelta>(&data) {
                    // Always sync current_index from the event — delta index wins
                    *current_index = Some(parsed.index);

                    match parsed.delta {
                        AnthropicDelta::TextDelta { text } => {
                            return Some(Ok(StreamChunk {
                                delta: Some(text),
                                tool_use: None,
                                stop_reason: None,
                            }));
                        }
                        AnthropicDelta::ThinkingDelta { .. } => {
                            // Skip thinking delta — don't emit, don't accumulate
                            continue;
                        }
                        AnthropicDelta::InputJsonDelta { partial_json } => {
                            // Accumulate partial JSON for the block at parsed.index (not current_index)
                            if let Some((_, _, val)) = pending_blocks.get_mut(&parsed.index) {
                                match val {
                                    serde_json::Value::Null => {
                                        *val = serde_json::Value::String(partial_json);
                                    }
                                    serde_json::Value::String(ref mut s) => {
                                        s.push_str(&partial_json);
                                    }
                                    _ => {}
                                }
                            }
                            continue;
                        }
                    }
                } else {
                    continue;
                }
            }
            "content_block_stop" => {
                // Emit the tool_use block at the index from the stop event
                if let Ok(stop) = serde_json::from_str::<AnthropicSseContentBlockStop>(&data) {
                    let idx = stop.index;
                    if let Some((id, name, raw_input)) = pending_blocks.remove(&idx) {
                        let input = match raw_input {
                            serde_json::Value::String(s) => serde_json::from_str(&s)
                                .unwrap_or(serde_json::Value::Object(serde_json::Map::new())),
                            serde_json::Value::Null => {
                                serde_json::Value::Object(serde_json::Map::new())
                            }
                            other => other,
                        };
                        return Some(Ok(StreamChunk {
                            delta: None,
                            tool_use: Some(ContentPart::ToolUse {
                                id,
                                name,
                                input,
                                thought_signature: None,
                            }),
                            stop_reason: None,
                        }));
                    }
                }
                continue;
            }
            "message_delta" => {
                if let Ok(parsed) = serde_json::from_str::<AnthropicSseMessageDelta>(&data) {
                    if parsed.delta.stop_reason.is_some() {
                        return Some(Ok(StreamChunk {
                            delta: None,
                            tool_use: None,
                            stop_reason: parsed.delta.stop_reason,
                        }));
                    }
                }
                continue;
            }
            "message_stop" => {
                // Final event
                return None;
            }
            "ping" => {
                continue;
            }
            "error" => {
                return Some(Err(Temm1eError::Provider(format!(
                    "Anthropic stream error: {data}"
                ))));
            }
            _ => {
                // Unknown event type, skip
                debug!(event_type = %event_type, "Unknown Anthropic SSE event type");
                continue;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_body_basic() {
        let provider = AnthropicProvider::new("test-key".to_string());
        let request = CompletionRequest {
            model: "claude-sonnet-4-6".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text("Hello".to_string()),
            }],
            tools: Vec::new(),
            max_tokens: Some(1024),
            temperature: Some(0.5),
            system: Some("Be helpful".to_string()),
        };

        let body = provider.build_request_body(&request, false).unwrap();
        assert_eq!(body["model"], "claude-sonnet-4-6");
        assert_eq!(body["max_tokens"], 1024);
        assert_eq!(body["temperature"], 0.5);
        assert_eq!(body["system"], "Be helpful");
        assert!(body.get("stream").is_none());
    }

    #[test]
    fn build_request_body_with_stream() {
        let provider = AnthropicProvider::new("key".to_string());
        let request = CompletionRequest {
            model: "m".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text("Hi".to_string()),
            }],
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            system: None,
        };

        let body = provider.build_request_body(&request, true).unwrap();
        assert_eq!(body["stream"], true);
    }

    #[test]
    fn build_request_body_with_tools() {
        let provider = AnthropicProvider::new("key".to_string());
        let request = CompletionRequest {
            model: "m".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text("Hi".to_string()),
            }],
            tools: vec![ToolDefinition {
                name: "shell".to_string(),
                description: "Run shell commands".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            max_tokens: None,
            temperature: None,
            system: None,
        };

        let body = provider.build_request_body(&request, false).unwrap();
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "shell");
        assert_eq!(tools[0]["input_schema"]["type"], "object");
    }

    #[test]
    fn convert_message_filters_system() {
        let msg = ChatMessage {
            role: Role::System,
            content: MessageContent::Text("system prompt".to_string()),
        };
        let result = convert_message_to_anthropic(&msg);
        assert!(result.is_err());
    }

    #[test]
    fn convert_message_user_text() {
        let msg = ChatMessage {
            role: Role::User,
            content: MessageContent::Text("Hello".to_string()),
        };
        let json = convert_message_to_anthropic(&msg).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "Hello");
    }

    #[test]
    fn convert_message_tool_role_becomes_user() {
        let msg = ChatMessage {
            role: Role::Tool,
            content: MessageContent::Text("tool result".to_string()),
        };
        let json = convert_message_to_anthropic(&msg).unwrap();
        assert_eq!(json["role"], "user");
    }

    #[test]
    fn convert_tool_definition() {
        let tool = ToolDefinition {
            name: "browser".to_string(),
            description: "Browse the web".to_string(),
            parameters: serde_json::json!({"type": "object", "properties": {}}),
        };
        let json = convert_tool_to_anthropic(&tool);
        assert_eq!(json["name"], "browser");
        assert_eq!(json["description"], "Browse the web");
        assert!(json["input_schema"].is_object());
    }

    #[test]
    fn sse_text_delta_event() {
        let mut buffer = "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hello\"}}\n\n".to_string();
        let mut tool_blocks = HashMap::new();
        let mut current_index = None;

        let result = extract_sse_event(&mut buffer, &mut tool_blocks, &mut current_index);
        assert!(result.is_some());
        let chunk = result.unwrap().unwrap();
        assert_eq!(chunk.delta.as_deref(), Some("Hello"));
    }

    #[test]
    fn sse_message_delta_stop() {
        let mut buffer = "event: message_delta\ndata: {\"delta\":{\"stop_reason\":\"end_turn\"},\"usage\":{\"input_tokens\":10,\"output_tokens\":20}}\n\n".to_string();
        let mut tool_blocks = HashMap::new();
        let mut current_index = None;

        let result = extract_sse_event(&mut buffer, &mut tool_blocks, &mut current_index);
        assert!(result.is_some());
        let chunk = result.unwrap().unwrap();
        assert_eq!(chunk.stop_reason.as_deref(), Some("end_turn"));
    }

    #[test]
    fn sse_ping_event_skipped() {
        let mut buffer = "event: ping\ndata: {}\n\nevent: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"text_delta\",\"text\":\"Hi\"}}\n\n".to_string();
        let mut tool_blocks = HashMap::new();
        let mut current_index = None;

        let result = extract_sse_event(&mut buffer, &mut tool_blocks, &mut current_index);
        assert!(result.is_some());
        let chunk = result.unwrap().unwrap();
        assert_eq!(chunk.delta.as_deref(), Some("Hi"));
    }

    #[test]
    fn sse_error_event() {
        let mut buffer = "event: error\ndata: {\"type\":\"overloaded_error\"}\n\n".to_string();
        let mut tool_blocks = HashMap::new();
        let mut current_index = None;

        let result = extract_sse_event(&mut buffer, &mut tool_blocks, &mut current_index);
        assert!(result.is_some());
        assert!(result.unwrap().is_err());
    }

    #[test]
    fn convert_message_with_image() {
        let msg = ChatMessage {
            role: Role::User,
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "What is in this image?".to_string(),
                },
                ContentPart::Image {
                    media_type: "image/jpeg".to_string(),
                    data: "abc123base64".to_string(),
                },
            ]),
        };
        let json = convert_message_to_anthropic(&msg).unwrap();
        assert_eq!(json["role"], "user");
        let content = json["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[1]["type"], "image");
        assert_eq!(content[1]["source"]["type"], "base64");
        assert_eq!(content[1]["source"]["media_type"], "image/jpeg");
        assert_eq!(content[1]["source"]["data"], "abc123base64");
    }

    #[test]
    fn anthropic_provider_name() {
        let provider = AnthropicProvider::new("key".to_string());
        assert_eq!(provider.name(), "anthropic");
    }

    #[test]
    fn anthropic_with_base_url() {
        let provider = AnthropicProvider::new("key".to_string())
            .with_base_url("https://custom.api.com".to_string());
        assert_eq!(provider.base_url, "https://custom.api.com");
    }

    // Regression test: thinking blocks interleaved with tool_use blocks must not corrupt tool input.
    // The proxy may send thinking_delta events between tool_use deltas. Index-keyed tracking
    // ensures each delta accumulates into the correct block regardless of interleaving.
    #[test]
    fn sse_thinking_blocks_interleaved_with_tool_use() {
        // Simulate: tool_use[0] start → thinking[1] start → tool_use[0] delta → thinking[1] delta → tool_use[0] stop
        let mut pending = HashMap::new();
        let mut idx = None;

        // tool_use block at index 0 starts
        let mut buf = "event: content_block_start\ndata: {\"index\":0,\"type\":\"content_block_start\",\"content_block\":{\"type\":\"tool_use\",\"id\":\"tool1\",\"name\":\"search\",\"input\":{}}}\n\n".to_string();
        extract_sse_event(&mut buf, &mut pending, &mut idx);
        assert_eq!(pending.len(), 1);
        assert!(pending.contains_key(&0));

        // thinking block at index 1 starts — should track but not accumulate
        let mut buf2 = "event: content_block_start\ndata: {\"index\":1,\"type\":\"content_block_start\",\"content_block\":{\"type\":\"thinking\",\"thinking\":\"searching...\",\"signature\":null}}\n\n".to_string();
        extract_sse_event(&mut buf2, &mut pending, &mut idx);
        assert_eq!(pending.len(), 1); // thinking not added to pending
        assert_eq!(idx, Some(1));

        // thinking delta — should be skipped (no accumulation)
        let mut buf3 = "event: content_block_delta\ndata: {\"index\":1,\"delta\":{\"type\":\"thinking_delta\",\"thinking\":\"step 1\",\"signature\":null}}\n\n".to_string();
        let result3 = extract_sse_event(&mut buf3, &mut pending, &mut idx);
        assert!(result3.is_none()); // skipped, not emitted

        // tool_use delta for index 0 — must accumulate into tool1 (NOT tool2)
        let mut buf4 = "event: content_block_delta\ndata: {\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"query\\\":\\\"ai agents\\\"}\"}}\n\n".to_string();
        let result4 = extract_sse_event(&mut buf4, &mut pending, &mut idx);
        assert!(result4.is_none()); // partial, not emitted yet

        // tool_use stop for index 0 — must emit correct tool1 with full input
        let mut buf5 = "event: content_block_stop\ndata: {\"index\":0}\n\n".to_string();
        let result5 = extract_sse_event(&mut buf5, &mut pending, &mut idx);
        assert!(result5.is_some());
        let chunk = result5.unwrap().unwrap();
        let tool = chunk.tool_use.unwrap();
        match tool {
            ContentPart::ToolUse { id, name, input, .. } => {
                assert_eq!(id, "tool1");
                assert_eq!(name, "search");
                let input_str = serde_json::to_string(&input).unwrap();
                assert!(input_str.contains("ai agents"));
            }
            _ => panic!("expected ToolUse variant"),
        }
    }
}
