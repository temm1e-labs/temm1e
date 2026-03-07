use async_trait::async_trait;
use futures::stream::BoxStream;
use reqwest::Client;
use serde::Deserialize;
use skyclaw_core::types::error::SkyclawError;
use skyclaw_core::types::message::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart, MessageContent, Role,
    StreamChunk, ToolDefinition, Usage,
};
use skyclaw_core::Provider;
use tracing::{debug, error};

/// Anthropic Messages API provider.
pub struct AnthropicProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl AnthropicProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: "https://api.anthropic.com".to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url;
        self
    }

    /// Build the JSON body for the Anthropic Messages API.
    fn build_request_body(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> Result<serde_json::Value, SkyclawError> {
        let messages = request
            .messages
            .iter()
            .filter(|m| !matches!(m.role, Role::System))
            .map(|m| convert_message_to_anthropic(m))
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
                .map(|t| convert_tool_to_anthropic(t))
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
}

#[derive(Debug, Deserialize)]
struct AnthropicUsage {
    input_tokens: u32,
    output_tokens: u32,
}

// SSE event types
#[derive(Debug, Deserialize)]
struct AnthropicSseMessageStart {
    message: AnthropicSseMessageMeta,
}

#[derive(Debug, Deserialize)]
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
#[serde(tag = "type")]
enum AnthropicDelta {
    #[serde(rename = "text_delta")]
    TextDelta { text: String },
    #[serde(rename = "input_json_delta")]
    InputJsonDelta { partial_json: String },
}

#[derive(Debug, Deserialize)]
struct AnthropicSseMessageDelta {
    delta: AnthropicMessageDeltaBody,
    usage: Option<AnthropicUsage>,
}

#[derive(Debug, Deserialize)]
struct AnthropicMessageDeltaBody {
    stop_reason: Option<String>,
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn convert_message_to_anthropic(msg: &ChatMessage) -> Result<serde_json::Value, SkyclawError> {
    let role = match msg.role {
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "user", // tool results are sent as user messages in Anthropic API
        Role::System => {
            // System messages are handled separately; skip here.
            return Err(SkyclawError::Provider(
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
                    ContentPart::ToolUse { id, name, input } => serde_json::json!({
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
        AnthropicContentBlock::Text { text } => ContentPart::Text {
            text: text.clone(),
        },
        AnthropicContentBlock::ToolUse { id, name, input } => ContentPart::ToolUse {
            id: id.clone(),
            name: name.clone(),
            input: input.clone(),
        },
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
    ) -> Result<CompletionResponse, SkyclawError> {
        let body = self.build_request_body(&request, false)?;

        debug!(provider = "anthropic", model = %request.model, "Sending completion request");

        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| SkyclawError::Provider(format!("Anthropic request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".into());
            error!(provider = "anthropic", %status, "API error: {}", error_body);
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(SkyclawError::RateLimited(error_body));
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                return Err(SkyclawError::Auth(error_body));
            }
            return Err(SkyclawError::Provider(format!(
                "Anthropic API error ({status}): {error_body}"
            )));
        }

        let api_response: AnthropicResponse = response
            .json()
            .await
            .map_err(|e| SkyclawError::Provider(format!("Failed to parse Anthropic response: {e}")))?;

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
            },
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<BoxStream<'_, Result<StreamChunk, SkyclawError>>, SkyclawError> {
        let body = self.build_request_body(&request, true)?;

        debug!(provider = "anthropic", model = %request.model, "Sending streaming request");

        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| SkyclawError::Provider(format!("Anthropic stream request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".into());
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(SkyclawError::RateLimited(error_body));
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                return Err(SkyclawError::Auth(error_body));
            }
            return Err(SkyclawError::Provider(format!(
                "Anthropic API error ({status}): {error_body}"
            )));
        }

        // Track state across SSE events for tool_use accumulation
        let byte_stream = response.bytes_stream();

        let event_stream = futures::stream::unfold(
            (
                byte_stream,
                String::new(),                                     // buffer for incomplete lines
                Vec::<(String, String, serde_json::Value)>::new(), // active tool_use blocks: (id, name, partial_json)
            ),
            |(mut byte_stream, mut buffer, mut tool_blocks)| async move {
                use futures::StreamExt;

                loop {
                    // Try to extract a complete SSE event from the buffer
                    if let Some(event) =
                        extract_sse_event(&mut buffer, &mut tool_blocks)
                    {
                        return Some((event, (byte_stream, buffer, tool_blocks)));
                    }

                    // Need more data
                    match byte_stream.next().await {
                        Some(Ok(bytes)) => {
                            let text = String::from_utf8_lossy(&bytes);
                            buffer.push_str(&text);
                        }
                        Some(Err(e)) => {
                            return Some((
                                Err(SkyclawError::Provider(format!("Stream read error: {e}"))),
                                (byte_stream, buffer, tool_blocks),
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

    async fn health_check(&self) -> Result<bool, SkyclawError> {
        let resp = self
            .client
            .head(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await
            .map_err(|e| SkyclawError::Provider(format!("Health check failed: {e}")))?;

        // Anthropic may return 405 for HEAD which still means the server is reachable
        Ok(resp.status().is_success() || resp.status() == reqwest::StatusCode::METHOD_NOT_ALLOWED)
    }

    async fn list_models(&self) -> Result<Vec<String>, SkyclawError> {
        Ok(vec![
            "claude-sonnet-4-20250514".to_string(),
            "claude-opus-4-20250514".to_string(),
            "claude-3-5-sonnet-20241022".to_string(),
            "claude-3-5-haiku-20241022".to_string(),
            "claude-3-opus-20240229".to_string(),
            "claude-3-haiku-20240307".to_string(),
        ])
    }
}

// ---------------------------------------------------------------------------
// SSE parsing helpers
// ---------------------------------------------------------------------------

/// Try to extract and parse the next complete SSE event from the buffer.
/// Returns `Some(Result<StreamChunk>)` if an event was parsed, `None` if more data is needed.
fn extract_sse_event(
    buffer: &mut String,
    tool_blocks: &mut Vec<(String, String, serde_json::Value)>,
) -> Option<Result<StreamChunk, SkyclawError>> {
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
            } else if line.starts_with("data:") {
                // "data:" with no space
                data_parts.push(line[5..].to_string());
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
                    match parsed.content_block {
                        AnthropicContentBlock::ToolUse { id, name, .. } => {
                            // Start accumulating a tool_use block
                            tool_blocks.push((id, name, serde_json::Value::Null));
                        }
                        AnthropicContentBlock::Text { .. } => {
                            // Text block start, no content yet
                        }
                    }
                }
                continue;
            }
            "content_block_delta" => {
                if let Ok(parsed) = serde_json::from_str::<AnthropicSseContentBlockDelta>(&data) {
                    match parsed.delta {
                        AnthropicDelta::TextDelta { text } => {
                            return Some(Ok(StreamChunk {
                                delta: Some(text),
                                tool_use: None,
                                stop_reason: None,
                            }));
                        }
                        AnthropicDelta::InputJsonDelta { partial_json } => {
                            // Accumulate partial JSON for the current tool_use block
                            if let Some(tb) = tool_blocks.last_mut() {
                                match &mut tb.2 {
                                    serde_json::Value::Null => {
                                        tb.2 = serde_json::Value::String(partial_json);
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
                // If there is a completed tool_use block, emit it
                if let Some((id, name, raw_input)) = tool_blocks.pop() {
                    let input = match raw_input {
                        serde_json::Value::String(s) => {
                            serde_json::from_str(&s).unwrap_or(serde_json::Value::Object(
                                serde_json::Map::new(),
                            ))
                        }
                        serde_json::Value::Null => {
                            serde_json::Value::Object(serde_json::Map::new())
                        }
                        other => other,
                    };
                    return Some(Ok(StreamChunk {
                        delta: None,
                        tool_use: Some(ContentPart::ToolUse { id, name, input }),
                        stop_reason: None,
                    }));
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
                return Some(Err(SkyclawError::Provider(format!(
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
