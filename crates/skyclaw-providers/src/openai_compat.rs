use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use skyclaw_core::types::error::SkyclawError;
use skyclaw_core::types::message::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart, MessageContent, Role,
    StreamChunk, ToolDefinition, Usage,
};
use skyclaw_core::Provider;
use tracing::{debug, error};

/// OpenAI-compatible provider.
///
/// Works with OpenAI, Ollama, vLLM, LM Studio, Groq, Mistral, and any other
/// service that implements the OpenAI Chat Completions API.
pub struct OpenAICompatProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl OpenAICompatProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: "https://api.openai.com/v1".to_string(),
        }
    }

    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url.trim_end_matches('/').to_string();
        self
    }

    /// Build the JSON body for the OpenAI Chat Completions API.
    fn build_request_body(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> Result<serde_json::Value, SkyclawError> {
        let mut messages: Vec<serde_json::Value> = Vec::new();

        // System message goes first
        if let Some(ref system) = request.system {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }

        for msg in &request.messages {
            messages.push(convert_message_to_openai(msg)?);
        }

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
        });

        if let Some(max_tokens) = request.max_tokens {
            body["max_tokens"] = serde_json::json!(max_tokens);
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> = request
                .tools
                .iter()
                .map(convert_tool_to_openai)
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
// OpenAI API serde types
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct OpenAIResponse {
    id: String,
    choices: Vec<OpenAIChoice>,
    usage: Option<OpenAIUsage>,
}

#[derive(Debug, Deserialize)]
struct OpenAIChoice {
    message: OpenAIMessage,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIMessage {
    role: String,
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIToolCall {
    id: String,
    #[serde(rename = "type")]
    call_type: String,
    function: OpenAIFunctionCall,
}

#[derive(Debug, Deserialize)]
struct OpenAIFunctionCall {
    name: String,
    arguments: String,
}

#[derive(Debug, Deserialize)]
struct OpenAIUsage {
    prompt_tokens: u32,
    completion_tokens: u32,
}

// Streaming types
#[derive(Debug, Deserialize)]
struct OpenAIStreamChunk {
    id: Option<String>,
    choices: Vec<OpenAIStreamChoice>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamChoice {
    delta: OpenAIStreamDelta,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamDelta {
    role: Option<String>,
    content: Option<String>,
    tool_calls: Option<Vec<OpenAIStreamToolCall>>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamToolCall {
    index: Option<usize>,
    id: Option<String>,
    #[serde(rename = "type")]
    call_type: Option<String>,
    function: Option<OpenAIStreamFunctionCall>,
}

#[derive(Debug, Deserialize)]
struct OpenAIStreamFunctionCall {
    name: Option<String>,
    arguments: Option<String>,
}

// Models list response
#[derive(Debug, Deserialize)]
struct OpenAIModelsResponse {
    data: Vec<OpenAIModel>,
}

#[derive(Debug, Deserialize)]
struct OpenAIModel {
    id: String,
}

// ---------------------------------------------------------------------------
// Conversion helpers
// ---------------------------------------------------------------------------

fn convert_message_to_openai(msg: &ChatMessage) -> Result<serde_json::Value, SkyclawError> {
    let role = match msg.role {
        Role::System => "system",
        Role::User => "user",
        Role::Assistant => "assistant",
        Role::Tool => "tool",
    };

    match &msg.content {
        MessageContent::Text(text) => {
            let obj = serde_json::json!({
                "role": role,
                "content": text,
            });
            Ok(obj)
        }
        MessageContent::Parts(parts) => {
            // For assistant messages that contain tool_use parts, we need to
            // convert to the OpenAI tool_calls format.
            if matches!(msg.role, Role::Assistant) {
                let mut text_content: Option<String> = None;
                let mut tool_calls: Vec<serde_json::Value> = Vec::new();

                for part in parts {
                    match part {
                        ContentPart::Text { text } => {
                            text_content = Some(text.clone());
                        }
                        ContentPart::ToolUse { id, name, input } => {
                            tool_calls.push(serde_json::json!({
                                "id": id,
                                "type": "function",
                                "function": {
                                    "name": name,
                                    "arguments": serde_json::to_string(input)
                                        .unwrap_or_else(|_| "{}".to_string()),
                                },
                            }));
                        }
                        ContentPart::ToolResult { .. } => {
                            // Should not appear in assistant messages
                        }
                    }
                }

                let mut obj = serde_json::json!({ "role": "assistant" });
                if let Some(text) = text_content {
                    obj["content"] = serde_json::json!(text);
                } else {
                    obj["content"] = serde_json::Value::Null;
                }
                if !tool_calls.is_empty() {
                    obj["tool_calls"] = serde_json::json!(tool_calls);
                }
                Ok(obj)
            } else if matches!(msg.role, Role::Tool) {
                // Tool results: each ToolResult part becomes a separate "tool" message.
                // For simplicity here, we handle the first ToolResult we find.
                for part in parts {
                    if let ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } = part
                    {
                        return Ok(serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": content,
                        }));
                    }
                }
                // Fallback: concatenate text parts
                let text: String = parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(serde_json::json!({
                    "role": "tool",
                    "content": text,
                }))
            } else {
                // User or system message with parts -- concatenate text
                let text: String = parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n");
                Ok(serde_json::json!({
                    "role": role,
                    "content": text,
                }))
            }
        }
    }
}

fn convert_tool_to_openai(tool: &ToolDefinition) -> serde_json::Value {
    serde_json::json!({
        "type": "function",
        "function": {
            "name": tool.name,
            "description": tool.description,
            "parameters": tool.parameters,
        },
    })
}

// ---------------------------------------------------------------------------
// Provider trait implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl Provider for OpenAICompatProvider {
    fn name(&self) -> &str {
        "openai-compatible"
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, SkyclawError> {
        let body = self.build_request_body(&request, false)?;

        debug!(provider = "openai-compat", model = %request.model, base_url = %self.base_url, "Sending completion request");

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| SkyclawError::Provider(format!("OpenAI-compat request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".into());
            error!(provider = "openai-compat", %status, "API error: {}", error_body);
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                return Err(SkyclawError::RateLimited(error_body));
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                return Err(SkyclawError::Auth(error_body));
            }
            return Err(SkyclawError::Provider(format!(
                "OpenAI-compat API error ({status}): {error_body}"
            )));
        }

        let api_response: OpenAIResponse = response
            .json()
            .await
            .map_err(|e| {
                SkyclawError::Provider(format!("Failed to parse OpenAI-compat response: {e}"))
            })?;

        let choice = api_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| SkyclawError::Provider("No choices in response".into()))?;

        let mut content = Vec::new();

        if let Some(text) = choice.message.content {
            if !text.is_empty() {
                content.push(ContentPart::Text { text });
            }
        }

        if let Some(tool_calls) = choice.message.tool_calls {
            for tc in tool_calls {
                let input: serde_json::Value =
                    serde_json::from_str(&tc.function.arguments).unwrap_or_else(|_| {
                        serde_json::Value::Object(serde_json::Map::new())
                    });
                content.push(ContentPart::ToolUse {
                    id: tc.id,
                    name: tc.function.name,
                    input,
                });
            }
        }

        let usage = api_response
            .usage
            .map(|u| Usage {
                input_tokens: u.prompt_tokens,
                output_tokens: u.completion_tokens,
            })
            .unwrap_or_default();

        Ok(CompletionResponse {
            id: api_response.id,
            content,
            stop_reason: choice.finish_reason,
            usage,
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<BoxStream<'_, Result<StreamChunk, SkyclawError>>, SkyclawError> {
        let body = self.build_request_body(&request, true)?;

        debug!(provider = "openai-compat", model = %request.model, "Sending streaming request");

        let response = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| SkyclawError::Provider(format!("OpenAI-compat stream request failed: {e}")))?;

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
                "OpenAI-compat API error ({status}): {error_body}"
            )));
        }

        let byte_stream = response.bytes_stream();

        // State: (byte_stream, buffer, active_tool_calls: Vec<(id, name, arguments_json)>)
        let event_stream = futures::stream::unfold(
            (
                byte_stream,
                String::new(),
                Vec::<(String, String, String)>::new(), // accumulated tool calls
            ),
            |(mut byte_stream, mut buffer, mut tool_calls)| async move {
                loop {
                    // Try to extract a complete SSE event from the buffer
                    if let Some(result) =
                        extract_openai_sse_event(&mut buffer, &mut tool_calls)
                    {
                        return Some((result, (byte_stream, buffer, tool_calls)));
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
                                (byte_stream, buffer, tool_calls),
                            ));
                        }
                        None => {
                            // Stream ended; emit any remaining tool calls
                            if let Some(result) = flush_tool_calls(&mut tool_calls) {
                                return Some((result, (byte_stream, buffer, tool_calls)));
                            }
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
            .get(format!("{}/models", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| SkyclawError::Provider(format!("Health check failed: {e}")))?;

        Ok(resp.status().is_success())
    }

    async fn list_models(&self) -> Result<Vec<String>, SkyclawError> {
        let resp = self
            .client
            .get(format!("{}/models", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| SkyclawError::Provider(format!("Failed to list models: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let error_body = resp.text().await.unwrap_or_default();
            return Err(SkyclawError::Provider(format!(
                "Failed to list models ({status}): {error_body}"
            )));
        }

        let models_response: OpenAIModelsResponse = resp
            .json()
            .await
            .map_err(|e| SkyclawError::Provider(format!("Failed to parse models response: {e}")))?;

        Ok(models_response.data.into_iter().map(|m| m.id).collect())
    }
}

// ---------------------------------------------------------------------------
// SSE parsing helpers
// ---------------------------------------------------------------------------

/// Try to extract the next complete SSE event from the buffer.
fn extract_openai_sse_event(
    buffer: &mut String,
    tool_calls: &mut Vec<(String, String, String)>,
) -> Option<Result<StreamChunk, SkyclawError>> {
    loop {
        // Look for a complete event (terminated by double newline)
        let double_newline = buffer.find("\n\n")?;
        let event_text: String = buffer.drain(..=double_newline + 1).collect();

        let mut data_parts = Vec::new();

        for line in event_text.lines() {
            if let Some(rest) = line.strip_prefix("data: ") {
                data_parts.push(rest.to_string());
            } else if line.starts_with("data:") {
                data_parts.push(line[5..].to_string());
            }
        }

        if data_parts.is_empty() {
            continue;
        }

        let data = data_parts.join("\n");

        // [DONE] signals stream end
        if data.trim() == "[DONE]" {
            // Flush any remaining tool calls
            if let Some(result) = flush_tool_calls(tool_calls) {
                // Put a marker so we don't re-process [DONE]
                return Some(result);
            }
            return None;
        }

        let chunk: OpenAIStreamChunk = match serde_json::from_str(&data) {
            Ok(c) => c,
            Err(_) => continue,
        };

        for choice in &chunk.choices {
            // Handle tool call deltas (accumulate)
            if let Some(ref tc_deltas) = choice.delta.tool_calls {
                for tc in tc_deltas {
                    let idx = tc.index.unwrap_or(0);
                    // Ensure we have enough slots
                    while tool_calls.len() <= idx {
                        tool_calls.push((String::new(), String::new(), String::new()));
                    }

                    if let Some(ref id) = tc.id {
                        tool_calls[idx].0 = id.clone();
                    }
                    if let Some(ref func) = tc.function {
                        if let Some(ref name) = func.name {
                            tool_calls[idx].1 = name.clone();
                        }
                        if let Some(ref args) = func.arguments {
                            tool_calls[idx].2.push_str(args);
                        }
                    }
                }
            }

            // Text delta
            if let Some(ref text) = choice.delta.content {
                return Some(Ok(StreamChunk {
                    delta: Some(text.clone()),
                    tool_use: None,
                    stop_reason: None,
                }));
            }

            // Finish reason
            if let Some(ref reason) = choice.finish_reason {
                // If finish reason is "tool_calls", flush accumulated tool calls
                if reason == "tool_calls" || reason == "stop" {
                    if let Some(result) = flush_tool_calls(tool_calls) {
                        return Some(result);
                    }
                }

                return Some(Ok(StreamChunk {
                    delta: None,
                    tool_use: None,
                    stop_reason: Some(reason.clone()),
                }));
            }
        }

        // If we get here, the chunk had no actionable content (e.g., just a role delta)
        continue;
    }
}

/// Emit the first accumulated tool call, if any.
fn flush_tool_calls(
    tool_calls: &mut Vec<(String, String, String)>,
) -> Option<Result<StreamChunk, SkyclawError>> {
    if tool_calls.is_empty() {
        return None;
    }

    let (id, name, arguments) = tool_calls.remove(0);
    let input: serde_json::Value =
        serde_json::from_str(&arguments).unwrap_or_else(|_| {
            serde_json::Value::Object(serde_json::Map::new())
        });

    Some(Ok(StreamChunk {
        delta: None,
        tool_use: Some(ContentPart::ToolUse { id, name, input }),
        stop_reason: None,
    }))
}
