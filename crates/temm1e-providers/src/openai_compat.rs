use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
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

/// OpenAI-compatible provider with key rotation.
///
/// Works with OpenAI, Ollama, vLLM, LM Studio, Groq, Mistral, xAI Grok,
/// OpenRouter, MiniMax, and any other service that implements the OpenAI
/// Chat Completions API.
pub struct OpenAICompatProvider {
    client: Client,
    keys: Vec<String>,
    key_index: AtomicUsize,
    base_url: String,
    extra_headers: HashMap<String, String>,
}

impl OpenAICompatProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .unwrap_or_else(|_| Client::new()),
            keys: vec![api_key],
            key_index: AtomicUsize::new(0),
            base_url: "https://api.openai.com/v1".to_string(),
            extra_headers: HashMap::new(),
        }
    }

    pub fn with_keys(mut self, keys: Vec<String>) -> Self {
        if !keys.is_empty() {
            self.keys = keys;
        }
        self
    }

    pub fn with_base_url(mut self, base_url: String) -> Self {
        self.base_url = base_url.trim_end_matches('/').to_string();
        self
    }

    pub fn with_extra_headers(mut self, headers: HashMap<String, String>) -> Self {
        self.extra_headers = headers;
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

    /// Build the JSON body for the OpenAI Chat Completions API.
    fn build_request_body(
        &self,
        request: &CompletionRequest,
        stream: bool,
    ) -> Result<serde_json::Value, Temm1eError> {
        let mut messages: Vec<serde_json::Value> = Vec::new();

        // System message goes first
        if let Some(ref system) = request.system {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system,
            }));
        }

        // Pre-scan: build tool_use_id → tool_name map so tool result messages
        // can include the `name` field (required by Gemini, accepted by OpenAI).
        let tool_name_map = build_tool_name_map(&request.messages);

        for msg in &request.messages {
            let converted = convert_message_to_openai(msg, &tool_name_map)?;
            // Tool messages may be returned as a JSON array when there are
            // multiple ToolResult parts (DF-15 fix).
            if let serde_json::Value::Array(arr) = converted {
                messages.extend(arr);
            } else {
                messages.push(converted);
            }
        }

        // Sanitize tool message ordering: Gemini requires tool_result messages
        // to immediately follow their corresponding tool_call assistant messages.
        // Strip any orphaned tool messages that violate this constraint.
        sanitize_tool_ordering(&mut messages);

        // Gemini 3 models require a `thought_signature` on the first tool_call
        // in each assistant message. Inject the documented bypass value for
        // tool_calls that don't carry a real signature (old history, other
        // providers). Non-Gemini providers ignore the extra_content field.
        // See: https://ai.google.dev/gemini-api/docs/thought-signatures
        inject_thought_signature_bypass(&mut messages);

        let mut body = serde_json::json!({
            "model": request.model,
            "messages": messages,
        });

        if let Some(max_tokens) = request.max_tokens {
            // OpenAI deprecated max_tokens in favor of max_completion_tokens
            // for newer models (GPT-4o, o1, etc.). Other OpenAI-compatible
            // providers (Gemini, Grok, OpenRouter) still use max_tokens.
            if self.base_url.contains("api.openai.com") {
                body["max_completion_tokens"] = serde_json::json!(max_tokens);
            } else {
                body["max_tokens"] = serde_json::json!(max_tokens);
            }
        }

        if let Some(temp) = request.temperature {
            body["temperature"] = serde_json::json!(temp);
        }

        if !request.tools.is_empty() {
            let tools: Vec<serde_json::Value> =
                request.tools.iter().map(convert_tool_to_openai).collect();
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
    #[serde(default)]
    id: Option<String>,
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

/// Build a map of tool_use_id → tool_name from all messages in the conversation.
/// Used to populate the `name` field on tool result messages (required by Gemini).
fn build_tool_name_map(messages: &[ChatMessage]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for msg in messages {
        if let MessageContent::Parts(parts) = &msg.content {
            for part in parts {
                if let ContentPart::ToolUse { id, name, .. } = part {
                    map.insert(id.clone(), name.clone());
                }
            }
        }
    }
    map
}

/// Sanitize tool message ordering for providers like Gemini that require strict
/// tool_call → tool_result adjacency.
///
/// Removes tool result messages (`"role": "tool"`) that don't immediately follow
/// an assistant message with matching `tool_calls`, and strips `tool_calls` from
/// assistant messages whose results were removed.
fn sanitize_tool_ordering(messages: &mut Vec<serde_json::Value>) {
    // Collect all tool_call IDs from assistant messages that ARE followed by
    // their tool result messages (i.e., properly paired).
    let mut valid_tool_call_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    // First pass: identify which tool_call_ids have matching tool results
    // anywhere in the message list (we just need them to exist).
    let mut all_tool_call_ids: HashMap<String, usize> = HashMap::new(); // id → assistant msg index
    let mut all_tool_result_ids: std::collections::HashSet<String> =
        std::collections::HashSet::new();

    for (i, msg) in messages.iter().enumerate() {
        if msg.get("role").and_then(|r| r.as_str()) == Some("assistant") {
            if let Some(tool_calls) = msg.get("tool_calls").and_then(|tc| tc.as_array()) {
                for tc in tool_calls {
                    if let Some(id) = tc.get("id").and_then(|id| id.as_str()) {
                        all_tool_call_ids.insert(id.to_string(), i);
                    }
                }
            }
        }
        if msg.get("role").and_then(|r| r.as_str()) == Some("tool") {
            if let Some(id) = msg.get("tool_call_id").and_then(|id| id.as_str()) {
                all_tool_result_ids.insert(id.to_string());
            }
        }
    }

    // Valid = has both a call and a result
    for id in &all_tool_result_ids {
        if all_tool_call_ids.contains_key(id) {
            valid_tool_call_ids.insert(id.clone());
        }
    }

    // Second pass: remove tool result messages without matching tool_calls,
    // and remove tool_calls entries from assistant messages without matching results.
    messages.retain(|msg| {
        let role = msg.get("role").and_then(|r| r.as_str()).unwrap_or("");
        if role == "tool" {
            // Keep only if this tool result has a matching tool_call
            if let Some(id) = msg.get("tool_call_id").and_then(|id| id.as_str()) {
                return valid_tool_call_ids.contains(id);
            }
            return false; // No tool_call_id → remove
        }
        true
    });

    // Clean up assistant messages: remove tool_calls entries that have no matching results
    for msg in messages.iter_mut() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        if let Some(tool_calls) = msg
            .get("tool_calls")
            .cloned()
            .and_then(|tc| tc.as_array().cloned())
        {
            let filtered: Vec<serde_json::Value> = tool_calls
                .into_iter()
                .filter(|tc| {
                    tc.get("id")
                        .and_then(|id| id.as_str())
                        .map(|id| valid_tool_call_ids.contains(id))
                        .unwrap_or(false)
                })
                .collect();
            if filtered.is_empty() {
                // Remove the tool_calls key entirely
                if let Some(obj) = msg.as_object_mut() {
                    obj.remove("tool_calls");
                }
            } else {
                msg["tool_calls"] = serde_json::json!(filtered);
            }
        }
    }
}

/// Inject Gemini 3 thought_signature bypass on assistant messages with tool_calls.
///
/// Gemini 3 models require a `thought_signature` in `extra_content.google` on
/// the first `tool_calls` entry of each assistant message. When the real
/// signature isn't available (old history, other providers), we use the
/// officially documented bypass value.
///
/// Non-Gemini providers ignore the `extra_content` field, so this is safe to
/// run unconditionally.
fn inject_thought_signature_bypass(messages: &mut [serde_json::Value]) {
    const BYPASS: &str = "skip_thought_signature_validator";

    for msg in messages.iter_mut() {
        if msg.get("role").and_then(|r| r.as_str()) != Some("assistant") {
            continue;
        }
        if let Some(tool_calls) = msg.get_mut("tool_calls").and_then(|tc| tc.as_array_mut()) {
            // Only the first tool_call in each step needs the signature
            if let Some(first_tc) = tool_calls.first_mut() {
                // Don't overwrite a real signature if one already exists
                let has_signature = first_tc
                    .get("extra_content")
                    .and_then(|ec| ec.get("google"))
                    .and_then(|g| g.get("thought_signature"))
                    .and_then(|ts| ts.as_str())
                    .is_some();

                if !has_signature {
                    first_tc["extra_content"] = serde_json::json!({
                        "google": {
                            "thought_signature": BYPASS
                        }
                    });
                }
            }
        }
    }
}

fn convert_message_to_openai(
    msg: &ChatMessage,
    tool_name_map: &HashMap<String, String>,
) -> Result<serde_json::Value, Temm1eError> {
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
                        ContentPart::Image { .. } => {
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
                // OpenAI's API expects one message per tool_call_id, so we
                // collect ALL ToolResult parts and return them as a JSON array
                // so the caller can flatten them into multiple messages.
                let mut tool_messages: Vec<serde_json::Value> = Vec::new();
                for part in parts {
                    if let ContentPart::ToolResult {
                        tool_use_id,
                        content,
                        ..
                    } = part
                    {
                        let mut msg_obj = serde_json::json!({
                            "role": "tool",
                            "tool_call_id": tool_use_id,
                            "content": content,
                        });
                        // Include `name` field — required by Gemini, accepted by OpenAI.
                        if let Some(name) = tool_name_map.get(tool_use_id) {
                            msg_obj["name"] = serde_json::json!(name);
                        }
                        tool_messages.push(msg_obj);
                    }
                }
                if tool_messages.len() == 1 {
                    return tool_messages.into_iter().next().ok_or_else(|| {
                        Temm1eError::Provider("Expected tool message but got none".into())
                    });
                }
                if !tool_messages.is_empty() {
                    // Return first tool result message; remaining ones are
                    // appended to the messages array in build_request_body
                    // via the __extra_tool_messages convention.
                    // For now, concatenate all tool results into one message
                    // keyed to the first tool_call_id, since the OpenAI API
                    // requires exactly one response per tool_call_id.
                    // Actually: return them properly. We must return multiple messages.
                    // Since this function returns a single Value, we encode
                    // multiple messages as a JSON array and handle it in the caller.
                    return Ok(serde_json::Value::Array(tool_messages));
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
                // User or system message with parts
                // Check if any Image parts are present — if so, use multipart content array
                let has_images = parts.iter().any(|p| matches!(p, ContentPart::Image { .. }));

                if has_images {
                    let content_parts: Vec<serde_json::Value> = parts
                        .iter()
                        .filter_map(|p| match p {
                            ContentPart::Text { text } => Some(serde_json::json!({
                                "type": "text",
                                "text": text,
                            })),
                            ContentPart::Image { media_type, data } => Some(serde_json::json!({
                                "type": "image_url",
                                "image_url": {
                                    "url": format!("data:{};base64,{}", media_type, data),
                                    "detail": "auto",
                                },
                            })),
                            _ => None,
                        })
                        .collect();
                    Ok(serde_json::json!({
                        "role": role,
                        "content": content_parts,
                    }))
                } else {
                    // No images — concatenate text parts
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
    ) -> Result<CompletionResponse, Temm1eError> {
        let body = self.build_request_body(&request, false)?;

        debug!(provider = "openai-compat", model = %request.model, base_url = %self.base_url, "Sending completion request");

        let mut req = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.current_key()))
            .header("Content-Type", "application/json");
        for (k, v) in &self.extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let response = req
            .json(&body)
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("OpenAI-compat request failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            let error_body = response
                .text()
                .await
                .unwrap_or_else(|_| "unknown error".into());
            error!(provider = "openai-compat", %status, "API error: {}", error_body);
            if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                self.rotate_key();
                return Err(Temm1eError::RateLimited(error_body));
            }
            if status == reqwest::StatusCode::UNAUTHORIZED {
                self.rotate_key();
                return Err(Temm1eError::Auth(error_body));
            }
            return Err(Temm1eError::Provider(format!(
                "OpenAI-compat API error ({status}): {error_body}"
            )));
        }

        // Read the body first so we can log it on parse failure
        let body_text = response.text().await.map_err(|e| {
            Temm1eError::Provider(format!("Failed to read OpenAI-compat response body: {e}"))
        })?;

        let api_response: OpenAIResponse = serde_json::from_str(&body_text).map_err(|e| {
            error!(
                provider = "openai-compat",
                body = %body_text,
                "Response parse failure — raw body logged above"
            );
            Temm1eError::Provider(format!(
                "Failed to parse OpenAI-compat response: {e}\nRaw body: {}",
                if body_text.len() > 500 {
                    let end = body_text
                        .char_indices()
                        .map(|(i, _)| i)
                        .take_while(|&i| i <= 500)
                        .last()
                        .unwrap_or(0);
                    format!("{}...", &body_text[..end])
                } else {
                    body_text.clone()
                }
            ))
        })?;

        let choice = api_response
            .choices
            .into_iter()
            .next()
            .ok_or_else(|| Temm1eError::Provider("No choices in response".into()))?;

        let mut content = Vec::new();

        if let Some(text) = choice.message.content {
            if !text.is_empty() {
                content.push(ContentPart::Text { text });
            }
        }

        if let Some(tool_calls) = choice.message.tool_calls {
            for tc in tool_calls {
                let input: serde_json::Value = serde_json::from_str(&tc.function.arguments)
                    .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));
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
                cost_usd: 0.0,
            })
            .unwrap_or_default();

        Ok(CompletionResponse {
            id: api_response.id.unwrap_or_default(),
            content,
            stop_reason: choice.finish_reason,
            usage,
        })
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<BoxStream<'_, Result<StreamChunk, Temm1eError>>, Temm1eError> {
        let body = self.build_request_body(&request, true)?;

        debug!(provider = "openai-compat", model = %request.model, "Sending streaming request");

        let mut req = self
            .client
            .post(format!("{}/chat/completions", self.base_url))
            .header("Authorization", format!("Bearer {}", self.current_key()))
            .header("Content-Type", "application/json");
        for (k, v) in &self.extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let response = req.json(&body).send().await.map_err(|e| {
            Temm1eError::Provider(format!("OpenAI-compat stream request failed: {e}"))
        })?;

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
                    if let Some(result) = extract_openai_sse_event(&mut buffer, &mut tool_calls) {
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
                                Err(Temm1eError::Provider(format!("Stream read error: {e}"))),
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

    async fn health_check(&self) -> Result<bool, Temm1eError> {
        let mut req = self
            .client
            .get(format!("{}/models", self.base_url))
            .header("Authorization", format!("Bearer {}", self.current_key()));
        for (k, v) in &self.extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Health check failed: {e}")))?;

        Ok(resp.status().is_success())
    }

    async fn list_models(&self) -> Result<Vec<String>, Temm1eError> {
        let mut req = self
            .client
            .get(format!("{}/models", self.base_url))
            .header("Authorization", format!("Bearer {}", self.current_key()));
        for (k, v) in &self.extra_headers {
            req = req.header(k.as_str(), v.as_str());
        }
        let resp = req
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Failed to list models: {e}")))?;

        let status = resp.status();
        if !status.is_success() {
            let error_body = resp.text().await.unwrap_or_default();
            return Err(Temm1eError::Provider(format!(
                "Failed to list models ({status}): {error_body}"
            )));
        }

        let models_response: OpenAIModelsResponse = resp
            .json()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Failed to parse models response: {e}")))?;

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
) -> Option<Result<StreamChunk, Temm1eError>> {
    loop {
        // Look for a complete event (terminated by double newline)
        let double_newline = buffer.find("\n\n")?;
        let event_text: String = buffer.drain(..=double_newline + 1).collect();

        let mut data_parts = Vec::new();

        for line in event_text.lines() {
            if let Some(rest) = line.strip_prefix("data: ") {
                data_parts.push(rest.to_string());
            } else if let Some(rest) = line.strip_prefix("data:") {
                data_parts.push(rest.to_string());
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
            // Check for mid-stream error first (OpenRouter sends finish_reason: "error")
            if choice.finish_reason.as_deref() == Some("error") {
                let error_text = choice
                    .delta
                    .content
                    .as_deref()
                    .unwrap_or("Unknown mid-stream error from provider");
                return Some(Err(Temm1eError::Provider(format!(
                    "Mid-stream provider error: {error_text}"
                ))));
            }

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
#[allow(dead_code)]
fn flush_tool_calls(
    tool_calls: &mut Vec<(String, String, String)>,
) -> Option<Result<StreamChunk, Temm1eError>> {
    if tool_calls.is_empty() {
        return None;
    }

    let (id, name, arguments) = tool_calls.remove(0);
    let input: serde_json::Value = serde_json::from_str(&arguments)
        .unwrap_or_else(|_| serde_json::Value::Object(serde_json::Map::new()));

    Some(Ok(StreamChunk {
        delta: None,
        tool_use: Some(ContentPart::ToolUse { id, name, input }),
        stop_reason: None,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_request_body_basic() {
        let provider = OpenAICompatProvider::new("test-key".to_string());
        let request = CompletionRequest {
            model: "gpt-4o".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text("Hello".to_string()),
            }],
            tools: Vec::new(),
            max_tokens: Some(2048),
            temperature: Some(0.9),
            system: Some("Be concise".to_string()),
        };

        let body = provider.build_request_body(&request, false).unwrap();
        assert_eq!(body["model"], "gpt-4o");
        // OpenAI uses max_completion_tokens, not max_tokens
        assert_eq!(body["max_completion_tokens"], 2048);
        assert!(body.get("max_tokens").is_none());
        // f32 precision: compare approximately
        let temp = body["temperature"].as_f64().unwrap();
        assert!(
            (temp - 0.9).abs() < 0.01,
            "temperature should be ~0.9, got {temp}"
        );
        // System message should be first in messages array
        let msgs = body["messages"].as_array().unwrap();
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "Be concise");
        assert_eq!(msgs[1]["role"], "user");
    }

    #[test]
    fn build_request_body_with_tools() {
        let provider = OpenAICompatProvider::new("key".to_string());
        let request = CompletionRequest {
            model: "m".to_string(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text("hi".to_string()),
            }],
            tools: vec![ToolDefinition {
                name: "read_file".to_string(),
                description: "Read a file".to_string(),
                parameters: serde_json::json!({"type": "object"}),
            }],
            max_tokens: None,
            temperature: None,
            system: None,
        };

        let body = provider.build_request_body(&request, false).unwrap();
        let tools = body["tools"].as_array().unwrap();
        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["type"], "function");
        assert_eq!(tools[0]["function"]["name"], "read_file");
    }

    #[test]
    fn convert_user_message() {
        let msg = ChatMessage {
            role: Role::User,
            content: MessageContent::Text("Hello".to_string()),
        };
        let json = convert_message_to_openai(&msg, &HashMap::new()).unwrap();
        assert_eq!(json["role"], "user");
        assert_eq!(json["content"], "Hello");
    }

    #[test]
    fn convert_assistant_with_tool_calls() {
        let msg = ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "Let me check".to_string(),
                },
                ContentPart::ToolUse {
                    id: "call_1".to_string(),
                    name: "shell".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                },
            ]),
        };
        let json = convert_message_to_openai(&msg, &HashMap::new()).unwrap();
        assert_eq!(json["role"], "assistant");
        assert_eq!(json["content"], "Let me check");
        let tool_calls = json["tool_calls"].as_array().unwrap();
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["function"]["name"], "shell");
    }

    #[test]
    fn convert_tool_result_message() {
        let msg = ChatMessage {
            role: Role::Tool,
            content: MessageContent::Parts(vec![ContentPart::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "file.txt".to_string(),
                is_error: false,
            }]),
        };
        // Without name map — no name field
        let json = convert_message_to_openai(&msg, &HashMap::new()).unwrap();
        assert_eq!(json["role"], "tool");
        assert_eq!(json["tool_call_id"], "call_1");
        assert_eq!(json["content"], "file.txt");
        assert!(json.get("name").is_none());

        // With name map — name field included (required by Gemini)
        let mut name_map = HashMap::new();
        name_map.insert("call_1".to_string(), "shell".to_string());
        let json = convert_message_to_openai(&msg, &name_map).unwrap();
        assert_eq!(json["name"], "shell");
    }

    #[test]
    fn convert_user_message_with_image() {
        let msg = ChatMessage {
            role: Role::User,
            content: MessageContent::Parts(vec![
                ContentPart::Text {
                    text: "What is in this image?".to_string(),
                },
                ContentPart::Image {
                    media_type: "image/png".to_string(),
                    data: "abc123base64".to_string(),
                },
            ]),
        };
        let json = convert_message_to_openai(&msg, &HashMap::new()).unwrap();
        assert_eq!(json["role"], "user");
        let content = json["content"].as_array().unwrap();
        assert_eq!(content.len(), 2);
        assert_eq!(content[0]["type"], "text");
        assert_eq!(content[0]["text"], "What is in this image?");
        assert_eq!(content[1]["type"], "image_url");
        let url = content[1]["image_url"]["url"].as_str().unwrap();
        assert!(url.starts_with("data:image/png;base64,"));
        assert!(url.contains("abc123base64"));
        assert_eq!(content[1]["image_url"]["detail"], "auto");
    }

    #[test]
    fn sse_text_delta() {
        let mut buffer = "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"Hello\"},\"finish_reason\":null}]}\n\n".to_string();
        let mut tool_calls = Vec::new();

        let result = extract_openai_sse_event(&mut buffer, &mut tool_calls);
        assert!(result.is_some());
        let chunk = result.unwrap().unwrap();
        assert_eq!(chunk.delta.as_deref(), Some("Hello"));
    }

    #[test]
    fn sse_done_signal() {
        let mut buffer = "data: [DONE]\n\n".to_string();
        let mut tool_calls = Vec::new();

        let result = extract_openai_sse_event(&mut buffer, &mut tool_calls);
        assert!(result.is_none());
    }

    #[test]
    fn flush_tool_calls_emits_first() {
        let mut calls = vec![
            (
                "id1".to_string(),
                "shell".to_string(),
                r#"{"cmd":"ls"}"#.to_string(),
            ),
            (
                "id2".to_string(),
                "file".to_string(),
                r#"{"path":"."}"#.to_string(),
            ),
        ];

        let result = flush_tool_calls(&mut calls);
        assert!(result.is_some());
        let chunk = result.unwrap().unwrap();
        match chunk.tool_use {
            Some(ContentPart::ToolUse { id, name, .. }) => {
                assert_eq!(id, "id1");
                assert_eq!(name, "shell");
            }
            _ => panic!("expected ToolUse"),
        }
        assert_eq!(calls.len(), 1);
    }

    #[test]
    fn flush_tool_calls_empty() {
        let mut calls = Vec::new();
        assert!(flush_tool_calls(&mut calls).is_none());
    }

    #[test]
    fn provider_name() {
        let provider = OpenAICompatProvider::new("key".to_string());
        assert_eq!(provider.name(), "openai-compatible");
    }

    #[test]
    fn with_base_url_strips_trailing_slash() {
        let provider = OpenAICompatProvider::new("key".to_string())
            .with_base_url("https://api.example.com/v1/".to_string());
        assert_eq!(provider.base_url, "https://api.example.com/v1");
    }

    #[test]
    fn with_extra_headers() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("HTTP-Referer".to_string(), "https://myapp.com".to_string());
        headers.insert("X-Title".to_string(), "TEMM1E".to_string());
        let provider = OpenAICompatProvider::new("key".to_string()).with_extra_headers(headers);
        assert_eq!(provider.extra_headers.len(), 2);
        assert_eq!(provider.extra_headers["HTTP-Referer"], "https://myapp.com");
    }

    #[test]
    fn sse_comment_lines_ignored() {
        // OpenRouter sends `: OPENROUTER PROCESSING` as SSE keepalive comments.
        // These must be ignored by the parser.
        let mut buffer = ": OPENROUTER PROCESSING\n\ndata: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"Hi\"},\"finish_reason\":null}]}\n\n".to_string();
        let mut tool_calls = Vec::new();

        let result = extract_openai_sse_event(&mut buffer, &mut tool_calls);
        assert!(result.is_some());
        let chunk = result.unwrap().unwrap();
        assert_eq!(chunk.delta.as_deref(), Some("Hi"));
    }

    #[test]
    fn sse_multiple_comment_lines_ignored() {
        // Multiple keepalive comments before actual data
        let mut buffer = ": OPENROUTER PROCESSING\n\n: OPENROUTER PROCESSING\n\ndata: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}\n\n".to_string();
        let mut tool_calls = Vec::new();

        let result = extract_openai_sse_event(&mut buffer, &mut tool_calls);
        assert!(result.is_some());
        let chunk = result.unwrap().unwrap();
        assert_eq!(chunk.delta.as_deref(), Some("OK"));
    }

    #[test]
    fn sse_midstream_error_finish_reason() {
        // OpenRouter sends finish_reason: "error" for mid-stream provider errors
        let mut buffer = "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"upstream timeout\"},\"finish_reason\":\"error\"}]}\n\n".to_string();
        let mut tool_calls = Vec::new();

        let result = extract_openai_sse_event(&mut buffer, &mut tool_calls);
        assert!(result.is_some());
        let err = result.unwrap().unwrap_err();
        match err {
            Temm1eError::Provider(msg) => {
                assert!(msg.contains("Mid-stream provider error"));
                assert!(msg.contains("upstream timeout"));
            }
            other => panic!("expected Provider error, got: {other:?}"),
        }
    }

    #[test]
    fn sse_midstream_error_without_content() {
        let mut buffer =
            "data: {\"id\":\"1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"error\"}]}\n\n"
                .to_string();
        let mut tool_calls = Vec::new();

        let result = extract_openai_sse_event(&mut buffer, &mut tool_calls);
        assert!(result.is_some());
        let err = result.unwrap().unwrap_err();
        match err {
            Temm1eError::Provider(msg) => {
                assert!(msg.contains("Mid-stream provider error"));
            }
            other => panic!("expected Provider error, got: {other:?}"),
        }
    }

    #[test]
    fn response_with_missing_id_field() {
        // Some providers (e.g., Ollama) may omit the `id` field
        let json = r#"{
            "choices": [{
                "message": {"role": "assistant", "content": "Hello!"},
                "finish_reason": "stop"
            }],
            "usage": {"prompt_tokens": 5, "completion_tokens": 3}
        }"#;
        let response: OpenAIResponse = serde_json::from_str(json).unwrap();
        assert!(response.id.is_none());
        assert_eq!(response.choices.len(), 1);
        assert_eq!(
            response.choices[0].message.content.as_deref(),
            Some("Hello!")
        );
    }

    #[test]
    fn response_with_null_id_field() {
        let json = r#"{
            "id": null,
            "choices": [{
                "message": {"role": "assistant", "content": "Hi"},
                "finish_reason": "stop"
            }]
        }"#;
        let response: OpenAIResponse = serde_json::from_str(json).unwrap();
        assert!(response.id.is_none());
        assert!(response.usage.is_none());
    }

    #[test]
    fn response_with_present_id_field() {
        let json = r#"{
            "id": "chatcmpl-123",
            "choices": [{
                "message": {"role": "assistant", "content": "Hello"},
                "finish_reason": "stop"
            }]
        }"#;
        let response: OpenAIResponse = serde_json::from_str(json).unwrap();
        assert_eq!(response.id.as_deref(), Some("chatcmpl-123"));
    }
}
