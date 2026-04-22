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
    last_rotation: std::sync::Mutex<std::time::Instant>,
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
            last_rotation: std::sync::Mutex::new(
                std::time::Instant::now() - std::time::Duration::from_secs(10),
            ),
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
    /// Skips rotation if the last rotation was less than 2 seconds ago
    /// (all keys likely exhausted — cycling faster won't help).
    fn rotate_key(&self) {
        if self.keys.is_empty() {
            return;
        }
        let mut last = self.last_rotation.lock().unwrap_or_else(|e| e.into_inner());
        if last.elapsed() < std::time::Duration::from_secs(2) {
            return;
        }
        *last = std::time::Instant::now();
        drop(last);

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

        // Consolidate ALL system content into a single leading system message.
        // The OpenAI Chat Completions spec expects one system message at the
        // top; MiniMax and Gemini's OpenAI-compat endpoint strictly enforce
        // this (MiniMax returns error 2013 "invalid message role: system"
        // when extra system entries appear mid-conversation). OpenAI/Grok/
        // OpenRouter accept multi-system leniently today but the spec is
        // single-leading-system — consolidating is strictly more correct.
        //
        // Sources of system content:
        //   1. `request.system_flattened()` — the base prompt + volatile tail
        //   2. Any `Role::System` messages inside `request.messages` — these
        //      are injected by the agent context (λ-memory, blueprints,
        //      knowledge, learnings, chat digest, dropped-history summary,
        //      DONE criteria). See crates/temm1e-agent/src/context.rs.
        //
        // GH-59: memory-driven injections accumulated mid-list system
        // entries that MiniMax rejected once memory had built up. Anthropic
        // (anthropic.rs:94) and Gemini native (gemini.rs:190) already
        // consolidate this way; this brings OpenAI-compat to parity.
        let mut system_parts: Vec<String> = Vec::new();
        if let Some(system) = request.system_flattened() {
            if !system.is_empty() {
                system_parts.push(system);
            }
        }
        for msg in &request.messages {
            if !matches!(msg.role, Role::System) {
                continue;
            }
            let text = match &msg.content {
                MessageContent::Text(t) => t.clone(),
                MessageContent::Parts(parts) => parts
                    .iter()
                    .filter_map(|p| match p {
                        ContentPart::Text { text } => Some(text.as_str()),
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("\n"),
            };
            if !text.is_empty() {
                system_parts.push(text);
            }
        }
        if !system_parts.is_empty() {
            messages.push(serde_json::json!({
                "role": "system",
                "content": system_parts.join("\n\n"),
            }));
        }

        // Pre-scan: build tool_use_id → tool_name map so tool result messages
        // can include the `name` field (required by Gemini, accepted by OpenAI).
        let tool_name_map = build_tool_name_map(&request.messages);

        for msg in &request.messages {
            // System messages were consolidated above — skip them here so we
            // never emit `role: system` mid-conversation.
            if matches!(msg.role, Role::System) {
                continue;
            }
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
        // providers). See https://ai.google.dev/gemini-api/docs/thought-signatures.
        //
        // GH-59 (follow-up): gate to Gemini target models only. The previous
        // comment claimed "non-Gemini providers ignore the extra_content
        // field" — MiniMax proves that wrong. MiniMax (and likely other
        // strict OpenAI-compat backends) rejects the Google-specific
        // `extra_content.google.thought_signature` field with error 2013
        // "invalid params". Only inject when the target is actually Gemini,
        // matching both native naming (`gemini-*`) and OpenRouter's prefixed
        // variant (`google/gemini-*`).
        if is_gemini_target(&request.model) {
            inject_thought_signature_bypass(&mut messages);
        }

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
    // Reasoning-model fields. Providers split answer text across multiple field
    // names; we accept them all and fall back when `content` is empty. See
    // extract_reasoning() for priority order and the provider-to-field map.
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
    #[serde(default)]
    reasoning_details: Option<Vec<serde_json::Value>>,
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
    #[serde(default)]
    reasoning_content: Option<String>,
    #[serde(default)]
    reasoning: Option<String>,
    #[serde(default)]
    thinking: Option<String>,
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
/// Sanitize non-streaming response body from OpenAI-compatible providers.
/// Some providers (OpenRouter, proxied Claude) append SSE markers like
/// `data: [DONE]` to non-streaming responses, causing JSON parse failures.
fn sanitize_nonstream_body(body: &str) -> String {
    let mut s = body.trim();
    // Remove BOM
    s = s.trim_start_matches('\u{feff}');
    // Strip trailing SSE marker
    if let Some(idx) = s.rfind("data: [DONE]") {
        // Only strip if it's AFTER the JSON body (after a closing brace)
        if let Some(brace) = s[..idx].rfind('}') {
            s = s[..=brace].trim();
        }
    }
    // Handle accidental `data: {json}` wrapping
    if s.starts_with("data:") && !s.starts_with("data: [") {
        let rest = s.trim_start_matches("data:").trim();
        if rest.starts_with('{') {
            s = rest;
        }
    }
    s.to_string()
}

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

/// Return true if the model name targets a Gemini endpoint. Matches native
/// names (`gemini-3-flash-preview`, `gemini-2.0-pro`) and OpenRouter's
/// `google/gemini-*` prefix. Case-insensitive.
fn is_gemini_target(model: &str) -> bool {
    let m = model.to_ascii_lowercase();
    m.starts_with("gemini") || m.starts_with("google/gemini")
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
                        ContentPart::ToolUse {
                            id, name, input, ..
                        } => {
                            // Skip tool calls with empty name — OpenAI rejects
                            // empty strings with `invalid_request_error` (GH-21).
                            if name.is_empty() {
                                continue;
                            }
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
                        // Skip if empty — OpenAI rejects empty strings (GH-21).
                        if let Some(name) = tool_name_map.get(tool_use_id) {
                            if !name.is_empty() {
                                msg_obj["name"] = serde_json::json!(name);
                            }
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
// Reasoning field extraction
// ---------------------------------------------------------------------------
//
// "OpenAI-compatible" isn't a real spec. Reasoning-model providers each invented
// their own field for the answer text. This extractor handles all known
// variants with one priority list — adding a new one is a one-line change.
//
//   reasoning_content  DeepSeek R1/V3, Zhipu GLM 4.5+, MiniMax M-series, Kimi K2+,
//                      Qwen/vLLM/SGLang, xAI Grok reasoning, AWS Bedrock
//   reasoning          OpenRouter canonical (also accepts reasoning_content alias)
//   thinking           proxy shims, litellm's normalization target
//   reasoning_details  OpenRouter structured (array of typed text blocks)

/// Extract fallback reasoning text from a non-streaming message.
///
/// Returns the first non-empty field in priority order, or flattens
/// `reasoning_details[].text` if only the structured form is populated.
fn extract_reasoning(msg: &OpenAIMessage) -> Option<String> {
    for s in [&msg.reasoning_content, &msg.reasoning, &msg.thinking]
        .into_iter()
        .flatten()
    {
        if !s.is_empty() {
            return Some(s.clone());
        }
    }
    if let Some(details) = &msg.reasoning_details {
        let joined: String = details
            .iter()
            .filter_map(|d| d.get("text").and_then(|t| t.as_str()))
            .collect::<Vec<_>>()
            .join("");
        if !joined.is_empty() {
            return Some(joined);
        }
    }
    None
}

/// Extract reasoning text from a streaming delta. Same priority as
/// `extract_reasoning` but operates on the delta's string fields only —
/// `reasoning_details` is streamed per-index by OpenRouter and is redundant
/// with the flat `reasoning` field for our fallback purpose.
fn extract_delta_reasoning(delta: &OpenAIStreamDelta) -> Option<&str> {
    delta
        .reasoning_content
        .as_deref()
        .or(delta.reasoning.as_deref())
        .or(delta.thinking.as_deref())
        .filter(|s| !s.is_empty())
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

        // Body-read retry: handles transient body-read failures (connection drop after 200 OK).
        const MAX_BODY_RETRIES: u32 = 2;

        // Outer loop: rate-limit (429) retry with exponential backoff.
        // Inner loop: body-read retry (unchanged semantics). Non-429/401 errors
        // return immediately from inside. 429 signals the outer loop to back off.
        let body_text = 'rate_limit: {
            for rl_attempt in 0..=crate::rate_limit::MAX_RATELIMIT_RETRIES {
                let mut last_body_err: Option<Temm1eError> = None;
                let mut rate_limit_signal: Option<(String, std::time::Duration)> = None;

                let inner_text: Option<String> = 'body_retry: {
                    for attempt in 0..=MAX_BODY_RETRIES {
                        if attempt > 0 {
                            tracing::warn!(attempt, "Retrying after response body read failure");
                            tokio::time::sleep(std::time::Duration::from_millis(
                                500 * (1 << attempt),
                            ))
                            .await;
                        }

                        let mut req = self
                            .client
                            .post(format!("{}/chat/completions", self.base_url))
                            .header("Authorization", format!("Bearer {}", self.current_key()))
                            .header("Content-Type", "application/json");
                        for (k, v) in &self.extra_headers {
                            req = req.header(k.as_str(), v.as_str());
                        }
                        let response = match req.json(&body).send().await {
                            Ok(r) => r,
                            Err(e) => {
                                last_body_err = Some(Temm1eError::Provider(format!(
                                    "OpenAI-compat request failed: {e}"
                                )));
                                continue;
                            }
                        };

                        let status = response.status();

                        // 429 handling: signal outer loop to back off + retry.
                        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                            let wait = crate::rate_limit::parse_retry_after(&response)
                                .unwrap_or_else(|| crate::rate_limit::default_backoff(rl_attempt));
                            let error_body = response
                                .text()
                                .await
                                .unwrap_or_else(|_| "unknown error".into());
                            self.rotate_key();
                            rate_limit_signal = Some((error_body, wait));
                            break 'body_retry None;
                        }

                        // Non-success, non-429 errors return immediately.
                        if !status.is_success() {
                            let error_body = response
                                .text()
                                .await
                                .unwrap_or_else(|_| "unknown error".into());
                            error!(provider = "openai-compat", %status, "API error: {}", error_body);
                            if status == reqwest::StatusCode::UNAUTHORIZED {
                                self.rotate_key();
                                return Err(Temm1eError::Auth(error_body));
                            }
                            return Err(Temm1eError::Provider(format!(
                                "OpenAI-compat API error ({status}): {error_body}"
                            )));
                        }

                        let content_len = response.content_length();

                        match response.text().await {
                            Ok(text) => break 'body_retry Some(text),
                            Err(e) => {
                                tracing::warn!(
                                    provider = "openai-compat",
                                    %status,
                                    content_length = ?content_len,
                                    attempt,
                                    "Response body read failed: {e}"
                                );
                                last_body_err = Some(Temm1eError::Provider(format!(
                                    "Failed to read response body (status={status}, len={content_len:?}): {e}"
                                )));
                                continue;
                            }
                        }
                    }
                    None
                };

                if let Some(text) = inner_text {
                    break 'rate_limit text;
                }

                if let Some((error_body, wait)) = rate_limit_signal {
                    if rl_attempt == crate::rate_limit::MAX_RATELIMIT_RETRIES {
                        error!(
                            provider = "openai-compat",
                            attempts = rl_attempt + 1,
                            "Rate limit: retries exhausted"
                        );
                        return Err(Temm1eError::RateLimited(error_body));
                    }
                    tracing::warn!(
                        provider = "openai-compat",
                        attempt = rl_attempt + 1,
                        wait_ms = wait.as_millis() as u64,
                        "Rate limited, backing off before retry"
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }

                // Body-read retries exhausted without 429.
                return Err(last_body_err.unwrap_or_else(|| {
                    Temm1eError::Provider("Request failed after retries".into())
                }));
            }
            unreachable!("rate-limit retry loop must exit via break or return")
        };

        // Sanitize: some OpenAI-compatible providers (OpenRouter, proxied Claude)
        // append SSE trailers like "data: [DONE]" to non-streaming responses.
        let body_text = sanitize_nonstream_body(&body_text);

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

        let reasoning_fallback = extract_reasoning(&choice.message);
        let has_tool_calls = choice
            .message
            .tool_calls
            .as_ref()
            .is_some_and(|tc| !tc.is_empty());

        if let Some(text) = choice.message.content {
            if !text.is_empty() {
                content.push(ContentPart::Text { text });
            }
        }

        if content.is_empty() && !has_tool_calls {
            if let Some(reasoning) = reasoning_fallback {
                info!(
                    provider = "openai-compat",
                    model = %request.model,
                    "Using reasoning field as fallback — content was empty"
                );
                content.push(ContentPart::Text { text: reasoning });
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
                    thought_signature: None,
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

        // Safety net: loud log when the response is truly empty despite
        // completion tokens being billed. Any new quirky field surfaces here.
        if content.is_empty() && usage.output_tokens > 0 {
            tracing::warn!(
                provider = "openai-compat",
                model = %request.model,
                completion_tokens = usage.output_tokens,
                raw_body_preview = %body_text.chars().take(2000).collect::<String>(),
                "Empty response despite completion_tokens > 0 — possible unknown reasoning field"
            );
        }

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

        // Rate-limit retry at REQUEST-INITIATION ONLY. Once bytes_stream begins
        // yielding, a mid-stream 429 is not recoverable (SSE parser state is lost).
        let response = 'retry: {
            for attempt in 0..=crate::rate_limit::MAX_RATELIMIT_RETRIES {
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
                if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
                    let wait = crate::rate_limit::parse_retry_after(&response)
                        .unwrap_or_else(|| crate::rate_limit::default_backoff(attempt));
                    let error_body = response
                        .text()
                        .await
                        .unwrap_or_else(|_| "unknown error".into());
                    self.rotate_key();
                    if attempt == crate::rate_limit::MAX_RATELIMIT_RETRIES {
                        error!(
                            provider = "openai-compat",
                            attempts = attempt + 1,
                            "Rate limit (stream): retries exhausted"
                        );
                        return Err(Temm1eError::RateLimited(error_body));
                    }
                    tracing::warn!(
                        provider = "openai-compat",
                        attempt = attempt + 1,
                        wait_ms = wait.as_millis() as u64,
                        "Rate limited (stream), backing off before retry"
                    );
                    tokio::time::sleep(wait).await;
                    continue;
                }

                if !status.is_success() {
                    let error_body = response
                        .text()
                        .await
                        .unwrap_or_else(|_| "unknown error".into());
                    if status == reqwest::StatusCode::UNAUTHORIZED {
                        self.rotate_key();
                        return Err(Temm1eError::Auth(error_body));
                    }
                    return Err(Temm1eError::Provider(format!(
                        "OpenAI-compat API error ({status}): {error_body}"
                    )));
                }

                break 'retry response;
            }
            unreachable!("rate-limit retry loop must exit via return or break")
        };

        let byte_stream = response.bytes_stream();

        // State: (byte_stream, buffer, tool_calls, reasoning_buffer, content_emitted)
        //
        // reasoning_buffer accumulates `delta.reasoning_content` / `delta.reasoning`
        // / `delta.thinking` chunks. content_emitted tracks whether any non-empty
        // content delta was emitted. On finish/stream-end, if content_emitted is
        // still false and reasoning_buffer has text, we flush the buffer as one
        // final text chunk — otherwise the user sees silent failure.
        let event_stream = futures::stream::unfold(
            (
                byte_stream,
                String::new(),
                Vec::<(String, String, String)>::new(),
                String::new(),
                false,
            ),
            |(
                mut byte_stream,
                mut buffer,
                mut tool_calls,
                mut reasoning_buffer,
                mut content_emitted,
            )| async move {
                loop {
                    // Try to extract a complete SSE event from the buffer
                    if let Some(result) = extract_openai_sse_event(
                        &mut buffer,
                        &mut tool_calls,
                        &mut reasoning_buffer,
                        &mut content_emitted,
                    ) {
                        return Some((
                            result,
                            (
                                byte_stream,
                                buffer,
                                tool_calls,
                                reasoning_buffer,
                                content_emitted,
                            ),
                        ));
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
                                (
                                    byte_stream,
                                    buffer,
                                    tool_calls,
                                    reasoning_buffer,
                                    content_emitted,
                                ),
                            ));
                        }
                        None => {
                            // Stream ended. If content was never emitted but we
                            // accumulated reasoning, flush it first so the user
                            // sees a response instead of silence.
                            if !content_emitted && !reasoning_buffer.is_empty() {
                                let text = std::mem::take(&mut reasoning_buffer);
                                content_emitted = true;
                                tracing::info!(
                                    provider = "openai-compat",
                                    "Stream ended without finish_reason — flushing reasoning buffer"
                                );
                                return Some((
                                    Ok(StreamChunk {
                                        delta: Some(text),
                                        tool_use: None,
                                        stop_reason: None,
                                    }),
                                    (
                                        byte_stream,
                                        buffer,
                                        tool_calls,
                                        reasoning_buffer,
                                        content_emitted,
                                    ),
                                ));
                            }
                            // Stream ended; emit any remaining tool calls
                            if let Some(result) = flush_tool_calls(&mut tool_calls) {
                                return Some((
                                    result,
                                    (
                                        byte_stream,
                                        buffer,
                                        tool_calls,
                                        reasoning_buffer,
                                        content_emitted,
                                    ),
                                ));
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
    reasoning_buffer: &mut String,
    content_emitted: &mut bool,
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
            // Flush reasoning buffer if content was never emitted
            if !*content_emitted && !reasoning_buffer.is_empty() {
                let text = std::mem::take(reasoning_buffer);
                *content_emitted = true;
                tracing::info!(
                    provider = "openai-compat",
                    "Streaming [DONE]: flushing reasoning buffer — content was never emitted"
                );
                return Some(Ok(StreamChunk {
                    delta: Some(text),
                    tool_use: None,
                    stop_reason: None,
                }));
            }
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

            // Text delta — emit immediately. Track non-empty emissions so we
            // know whether to flush reasoning_buffer on finish.
            if let Some(ref text) = choice.delta.content {
                if !text.is_empty() {
                    *content_emitted = true;
                }
                return Some(Ok(StreamChunk {
                    delta: Some(text.clone()),
                    tool_use: None,
                    stop_reason: None,
                }));
            }

            // Reasoning delta — accumulate silently. Flushed only on finish
            // if content was never streamed (fallback for reasoning-only
            // responses like max_tokens-cut-off-during-thinking).
            if let Some(reasoning) = extract_delta_reasoning(&choice.delta) {
                reasoning_buffer.push_str(reasoning);
                continue;
            }

            // Finish reason
            if let Some(ref reason) = choice.finish_reason {
                // If finish reason is "tool_calls", flush accumulated tool calls
                if reason == "tool_calls" || reason == "stop" {
                    if let Some(result) = flush_tool_calls(tool_calls) {
                        return Some(result);
                    }
                }

                // Reasoning-only response: flush buffer WITH stop_reason so
                // the stream terminates cleanly in one chunk. Without the
                // stop_reason here the downstream receives the text but
                // never sees an end-of-stream signal (buffer is drained).
                if !*content_emitted && !reasoning_buffer.is_empty() {
                    let text = std::mem::take(reasoning_buffer);
                    *content_emitted = true;
                    tracing::info!(
                        provider = "openai-compat",
                        finish_reason = %reason,
                        "Streaming: flushing reasoning buffer — content was never emitted"
                    );
                    return Some(Ok(StreamChunk {
                        delta: Some(text),
                        tool_use: None,
                        stop_reason: Some(reason.clone()),
                    }));
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
        tool_use: Some(ContentPart::ToolUse {
            id,
            name,
            input,
            thought_signature: None,
        }),
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
            system_volatile: None,
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

    /// GH-59 regression: mid-list `Role::System` messages (from λ-memory,
    /// blueprints, knowledge, digests, etc.) must be consolidated into a
    /// single leading system message. MiniMax rejects multi-system with
    /// error 2013; OpenAI/Grok tolerate it but the spec is single-leading.
    #[test]
    fn build_request_body_consolidates_multiple_system_messages() {
        let provider = OpenAICompatProvider::new("key".to_string());
        let request = CompletionRequest {
            model: "MiniMax-M2.5".to_string(),
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: MessageContent::Text("[Dropped-history summary]".to_string()),
                },
                ChatMessage {
                    role: Role::System,
                    content: MessageContent::Text("Blueprint: python-debug".to_string()),
                },
                ChatMessage {
                    role: Role::System,
                    content: MessageContent::Text("λ-memory: user prefers TS".to_string()),
                },
                ChatMessage {
                    role: Role::User,
                    content: MessageContent::Text("hello".to_string()),
                },
                ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Text("hi".to_string()),
                },
                ChatMessage {
                    role: Role::User,
                    content: MessageContent::Text("follow up".to_string()),
                },
            ],
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            system: Some("You are Tem.".to_string()),
            system_volatile: Some("Current mode: coding.".to_string()),
        };

        let body = provider.build_request_body(&request, false).unwrap();
        let msgs = body["messages"].as_array().unwrap();

        // Exactly one `system` role, at index 0
        let system_count = msgs.iter().filter(|m| m["role"] == "system").count();
        assert_eq!(
            system_count, 1,
            "expected exactly one system message, got {system_count}: {msgs:?}"
        );
        assert_eq!(msgs[0]["role"], "system");

        // Base, volatile, and all three mid-list System messages must be
        // present in the merged content (write-order preserved).
        let content = msgs[0]["content"].as_str().unwrap();
        assert!(content.contains("You are Tem."), "base missing: {content}");
        assert!(
            content.contains("Current mode: coding."),
            "volatile missing: {content}"
        );
        assert!(
            content.contains("[Dropped-history summary]"),
            "summary missing: {content}"
        );
        assert!(
            content.contains("Blueprint: python-debug"),
            "blueprint missing: {content}"
        );
        assert!(
            content.contains("λ-memory: user prefers TS"),
            "lambda missing: {content}"
        );

        // Mid-list user/assistant ordering must be preserved, no `system`
        // roles leak into positions 1..N.
        assert_eq!(msgs[1]["role"], "user");
        assert_eq!(msgs[1]["content"], "hello");
        assert_eq!(msgs[2]["role"], "assistant");
        assert_eq!(msgs[3]["role"], "user");
        assert_eq!(msgs[3]["content"], "follow up");
    }

    /// GH-59 follow-up: `extra_content.google.thought_signature` is a
    /// Google-specific field. It must NOT leak to non-Gemini OpenAI-compat
    /// backends — MiniMax (and likely others) reject it with error 2013.
    /// Gate by target model.
    #[test]
    fn thought_signature_gated_to_gemini_target() {
        let provider = OpenAICompatProvider::new("key".to_string());
        // Build a request with a prior assistant tool_call in history —
        // this is the shape that used to trigger extra_content injection.
        let mk_request = |model: &str| CompletionRequest {
            model: model.to_string(),
            messages: vec![
                ChatMessage {
                    role: Role::User,
                    content: MessageContent::Text("use a tool".to_string()),
                },
                ChatMessage {
                    role: Role::Assistant,
                    content: MessageContent::Parts(vec![ContentPart::ToolUse {
                        id: "call_1".to_string(),
                        name: "read_file".to_string(),
                        input: serde_json::json!({"path": "/tmp/x"}),
                        thought_signature: None,
                    }]),
                },
                ChatMessage {
                    role: Role::Tool,
                    content: MessageContent::Parts(vec![ContentPart::ToolResult {
                        tool_use_id: "call_1".to_string(),
                        content: "contents".to_string(),
                        is_error: false,
                    }]),
                },
                ChatMessage {
                    role: Role::User,
                    content: MessageContent::Text("now what?".to_string()),
                },
            ],
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            system: None,
            system_volatile: None,
        };

        // MiniMax: no extra_content anywhere in the body.
        let body = provider
            .build_request_body(&mk_request("MiniMax-M2.5"), false)
            .unwrap();
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            !serialized.contains("extra_content"),
            "extra_content leaked to MiniMax body: {serialized}"
        );
        assert!(
            !serialized.contains("thought_signature"),
            "thought_signature leaked to MiniMax body: {serialized}"
        );

        // GPT / OpenAI: also no extra_content.
        let body = provider
            .build_request_body(&mk_request("gpt-4o"), false)
            .unwrap();
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            !serialized.contains("extra_content"),
            "extra_content leaked to OpenAI body: {serialized}"
        );

        // Grok: also no extra_content.
        let body = provider
            .build_request_body(&mk_request("grok-4"), false)
            .unwrap();
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            !serialized.contains("extra_content"),
            "extra_content leaked to Grok body: {serialized}"
        );

        // Gemini native-ish name: thought_signature bypass MUST still fire.
        let body = provider
            .build_request_body(&mk_request("gemini-3-flash-preview"), false)
            .unwrap();
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            serialized.contains("skip_thought_signature_validator"),
            "thought_signature bypass missing for Gemini: {serialized}"
        );

        // Gemini via OpenRouter prefix: bypass still fires.
        let body = provider
            .build_request_body(&mk_request("google/gemini-3-flash-preview"), false)
            .unwrap();
        let serialized = serde_json::to_string(&body).unwrap();
        assert!(
            serialized.contains("skip_thought_signature_validator"),
            "thought_signature bypass missing for OpenRouter-Gemini: {serialized}"
        );
    }

    #[test]
    fn is_gemini_target_matches_native_and_openrouter() {
        assert!(is_gemini_target("gemini-3-flash-preview"));
        assert!(is_gemini_target("gemini-2.0-pro"));
        assert!(is_gemini_target("GEMINI-3"));
        assert!(is_gemini_target("google/gemini-3-flash-preview"));
        assert!(is_gemini_target("Google/Gemini-3"));
        assert!(!is_gemini_target("gpt-4o"));
        assert!(!is_gemini_target("grok-4"));
        assert!(!is_gemini_target("MiniMax-M2.5"));
        assert!(!is_gemini_target("deepseek-chat"));
        assert!(!is_gemini_target("claude-sonnet-4-6"));
    }

    /// System-only consolidation still works when `request.system` is None
    /// and the only system content comes from mid-list `Role::System`.
    #[test]
    fn build_request_body_merges_midlist_system_when_base_absent() {
        let provider = OpenAICompatProvider::new("key".to_string());
        let request = CompletionRequest {
            model: "MiniMax-M2.5".to_string(),
            messages: vec![
                ChatMessage {
                    role: Role::System,
                    content: MessageContent::Text("only-system".to_string()),
                },
                ChatMessage {
                    role: Role::User,
                    content: MessageContent::Text("hi".to_string()),
                },
            ],
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            system: None,
            system_volatile: None,
        };

        let body = provider.build_request_body(&request, false).unwrap();
        let msgs = body["messages"].as_array().unwrap();
        let system_count = msgs.iter().filter(|m| m["role"] == "system").count();
        assert_eq!(system_count, 1);
        assert_eq!(msgs[0]["role"], "system");
        assert_eq!(msgs[0]["content"], "only-system");
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
            system_volatile: None,
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
                    thought_signature: None,
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
    fn empty_tool_name_skipped_in_tool_calls_gh21() {
        // GH-21: empty `name` causes OpenAI 400 Bad Request
        let msg = ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Parts(vec![
                ContentPart::ToolUse {
                    id: "call_good".to_string(),
                    name: "shell".to_string(),
                    input: serde_json::json!({"command": "ls"}),
                    thought_signature: None,
                },
                ContentPart::ToolUse {
                    id: "call_bad".to_string(),
                    name: "".to_string(),
                    input: serde_json::json!({}),
                    thought_signature: None,
                },
            ]),
        };
        let json = convert_message_to_openai(&msg, &HashMap::new()).unwrap();
        let tool_calls = json["tool_calls"].as_array().unwrap();
        // Only the non-empty name tool call should be included
        assert_eq!(tool_calls.len(), 1);
        assert_eq!(tool_calls[0]["function"]["name"], "shell");
    }

    #[test]
    fn empty_tool_name_omitted_from_tool_result_gh21() {
        // GH-21: empty `name` on tool result messages causes OpenAI 400
        let msg = ChatMessage {
            role: Role::Tool,
            content: MessageContent::Parts(vec![ContentPart::ToolResult {
                tool_use_id: "call_1".to_string(),
                content: "output".to_string(),
                is_error: false,
            }]),
        };
        let mut name_map = HashMap::new();
        name_map.insert("call_1".to_string(), "".to_string());
        let json = convert_message_to_openai(&msg, &name_map).unwrap();
        // Empty name should NOT be included
        assert!(json.get("name").is_none());
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
        let mut reasoning_buffer = String::new();
        let mut content_emitted = false;

        let result = extract_openai_sse_event(
            &mut buffer,
            &mut tool_calls,
            &mut reasoning_buffer,
            &mut content_emitted,
        );
        assert!(result.is_some());
        let chunk = result.unwrap().unwrap();
        assert_eq!(chunk.delta.as_deref(), Some("Hello"));
    }

    #[test]
    fn sanitize_clean_body() {
        let body = r#"{"id":"123","choices":[{"message":{"content":"hi"}}]}"#;
        assert_eq!(sanitize_nonstream_body(body), body);
    }

    #[test]
    fn sanitize_trailing_sse_done() {
        let body = r#"{"id":"123","choices":[{"message":{"content":"hi"}}]}data: [DONE]"#;
        let cleaned = sanitize_nonstream_body(body);
        assert!(cleaned.ends_with('}'));
        assert!(!cleaned.contains("[DONE]"));
    }

    #[test]
    fn sanitize_trailing_sse_done_with_newlines() {
        let body = "{\"id\":\"123\"}\n\ndata: [DONE]\n\n";
        let cleaned = sanitize_nonstream_body(body);
        assert!(cleaned.ends_with('}'));
    }

    #[test]
    fn sanitize_bom_prefix() {
        let body = "\u{feff}{\"id\":\"123\"}";
        let cleaned = sanitize_nonstream_body(body);
        assert!(cleaned.starts_with('{'));
    }

    #[test]
    fn sanitize_data_wrapped() {
        let body = "data: {\"id\":\"123\"}";
        let cleaned = sanitize_nonstream_body(body);
        assert_eq!(cleaned, "{\"id\":\"123\"}");
    }

    #[test]
    fn sse_done_signal() {
        let mut buffer = "data: [DONE]\n\n".to_string();
        let mut tool_calls = Vec::new();
        let mut reasoning_buffer = String::new();
        let mut content_emitted = false;

        let result = extract_openai_sse_event(
            &mut buffer,
            &mut tool_calls,
            &mut reasoning_buffer,
            &mut content_emitted,
        );
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
        let mut reasoning_buffer = String::new();
        let mut content_emitted = false;

        let result = extract_openai_sse_event(
            &mut buffer,
            &mut tool_calls,
            &mut reasoning_buffer,
            &mut content_emitted,
        );
        assert!(result.is_some());
        let chunk = result.unwrap().unwrap();
        assert_eq!(chunk.delta.as_deref(), Some("Hi"));
    }

    #[test]
    fn sse_multiple_comment_lines_ignored() {
        // Multiple keepalive comments before actual data
        let mut buffer = ": OPENROUTER PROCESSING\n\n: OPENROUTER PROCESSING\n\ndata: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"OK\"},\"finish_reason\":null}]}\n\n".to_string();
        let mut tool_calls = Vec::new();
        let mut reasoning_buffer = String::new();
        let mut content_emitted = false;

        let result = extract_openai_sse_event(
            &mut buffer,
            &mut tool_calls,
            &mut reasoning_buffer,
            &mut content_emitted,
        );
        assert!(result.is_some());
        let chunk = result.unwrap().unwrap();
        assert_eq!(chunk.delta.as_deref(), Some("OK"));
    }

    #[test]
    fn sse_midstream_error_finish_reason() {
        // OpenRouter sends finish_reason: "error" for mid-stream provider errors
        let mut buffer = "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"upstream timeout\"},\"finish_reason\":\"error\"}]}\n\n".to_string();
        let mut tool_calls = Vec::new();
        let mut reasoning_buffer = String::new();
        let mut content_emitted = false;

        let result = extract_openai_sse_event(
            &mut buffer,
            &mut tool_calls,
            &mut reasoning_buffer,
            &mut content_emitted,
        );
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
        let mut reasoning_buffer = String::new();
        let mut content_emitted = false;

        let result = extract_openai_sse_event(
            &mut buffer,
            &mut tool_calls,
            &mut reasoning_buffer,
            &mut content_emitted,
        );
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

    // -----------------------------------------------------------------------
    // GH-49: reasoning-field fallback (DeepSeek, GLM, Kimi, MiniMax, Qwen,
    // Grok reasoning, OpenRouter). Covers the 10-scenario matrix from the
    // zero-risk report.
    // -----------------------------------------------------------------------

    fn build_msg_from_json(json: &str) -> OpenAIMessage {
        serde_json::from_str(json).expect("test fixture must deserialize")
    }

    #[test]
    fn reasoning_content_fallback_when_content_null() {
        // DeepSeek R1 truncated during reasoning: content=null, reasoning_content populated.
        let msg = build_msg_from_json(
            r#"{"role":"assistant","content":null,"reasoning_content":"The answer is 42."}"#,
        );
        assert_eq!(
            extract_reasoning(&msg).as_deref(),
            Some("The answer is 42.")
        );
    }

    #[test]
    fn reasoning_fallback_when_content_empty_string() {
        // OpenRouter canonical field: content="", reasoning populated.
        let msg = build_msg_from_json(
            r#"{"role":"assistant","content":"","reasoning":"Step by step solution."}"#,
        );
        assert_eq!(
            extract_reasoning(&msg).as_deref(),
            Some("Step by step solution.")
        );
    }

    #[test]
    fn thinking_fallback() {
        // litellm normalization target — some proxy shims use `thinking`.
        let msg = build_msg_from_json(r#"{"role":"assistant","thinking":"Thinking aloud..."}"#);
        assert_eq!(
            extract_reasoning(&msg).as_deref(),
            Some("Thinking aloud...")
        );
    }

    #[test]
    fn reasoning_details_fallback() {
        // OpenRouter structured reasoning blocks — flatten .text fields.
        let msg = build_msg_from_json(
            r#"{
                "role":"assistant",
                "content":null,
                "reasoning_details":[
                    {"type":"reasoning.text","text":"First I need to ","format":"anthropic-claude-v1","index":0},
                    {"type":"reasoning.text","text":"analyze the problem.","format":"anthropic-claude-v1","index":1}
                ]
            }"#,
        );
        assert_eq!(
            extract_reasoning(&msg).as_deref(),
            Some("First I need to analyze the problem.")
        );
    }

    #[test]
    fn content_wins_when_both_populated() {
        // DeepSeek normal response: both content and reasoning_content populated.
        // content always wins — reasoning is scratch and should be dropped.
        let json = r#"{
            "id": "chatcmpl-x",
            "choices":[{
                "message":{
                    "role":"assistant",
                    "content":"The final answer.",
                    "reasoning_content":"Internal thinking..."
                },
                "finish_reason":"stop"
            }]
        }"#;
        let response: OpenAIResponse = serde_json::from_str(json).unwrap();
        let msg = &response.choices[0].message;
        // Content is populated — fallback should NOT be used downstream.
        assert_eq!(msg.content.as_deref(), Some("The final answer."));
        // But extract_reasoning itself does return the reasoning text if asked.
        // The gating logic in complete() ensures content wins.
        assert_eq!(
            extract_reasoning(msg).as_deref(),
            Some("Internal thinking...")
        );
    }

    #[test]
    fn extract_reasoning_returns_none_for_all_empty() {
        // No reasoning fields populated — extractor returns None.
        let msg = build_msg_from_json(r#"{"role":"assistant","content":null}"#);
        assert!(extract_reasoning(&msg).is_none());
    }

    #[test]
    fn extract_reasoning_skips_empty_strings() {
        // Empty strings must not trip the fallback.
        let msg = build_msg_from_json(
            r#"{"role":"assistant","reasoning_content":"","reasoning":"","thinking":"actual"}"#,
        );
        assert_eq!(extract_reasoning(&msg).as_deref(), Some("actual"));
    }

    #[test]
    fn sse_reasoning_only_stream_flushes_on_finish() {
        // DeepSeek-style reasoning phase that never transitions to content
        // (e.g., max_tokens cutoff during thinking). Buffer must flush on
        // finish_reason so the user sees the reasoning as the response.
        let mut buffer = String::from(
            "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"role\":\"assistant\",\"reasoning_content\":\"Let me think about this...\"},\"finish_reason\":null}]}\n\n\
             data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"reasoning_content\":\" the answer is 42.\"},\"finish_reason\":null}]}\n\n\
             data: {\"id\":\"1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"length\"}]}\n\n",
        );
        let mut tool_calls = Vec::new();
        let mut reasoning_buffer = String::new();
        let mut content_emitted = false;

        // Drain: first two reasoning chunks accumulate silently (None + None),
        // third finish chunk flushes buffer as a text chunk, then stop chunk.
        let mut emitted = Vec::new();
        loop {
            match extract_openai_sse_event(
                &mut buffer,
                &mut tool_calls,
                &mut reasoning_buffer,
                &mut content_emitted,
            ) {
                Some(Ok(chunk)) => emitted.push(chunk),
                Some(Err(e)) => panic!("unexpected error: {e:?}"),
                None => break,
            }
        }

        // Expect: one text chunk with the joined reasoning, then one stop chunk.
        let text_chunks: Vec<_> = emitted.iter().filter_map(|c| c.delta.as_deref()).collect();
        assert_eq!(text_chunks.len(), 1, "expected one flushed text chunk");
        assert_eq!(
            text_chunks[0],
            "Let me think about this... the answer is 42."
        );

        let stop_chunks: Vec<_> = emitted
            .iter()
            .filter_map(|c| c.stop_reason.as_deref())
            .collect();
        assert_eq!(stop_chunks, vec!["length"]);
    }

    #[test]
    fn sse_reasoning_then_content_drops_reasoning() {
        // Normal DeepSeek flow: reasoning phase → content phase → stop.
        // Content is emitted; reasoning buffer is discarded (it's scratch).
        let mut buffer = String::from(
            "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"reasoning_content\":\"thinking\"},\"finish_reason\":null}]}\n\n\
             data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"Answer.\"},\"finish_reason\":null}]}\n\n\
             data: {\"id\":\"1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        );
        let mut tool_calls = Vec::new();
        let mut reasoning_buffer = String::new();
        let mut content_emitted = false;

        let mut emitted = Vec::new();
        loop {
            match extract_openai_sse_event(
                &mut buffer,
                &mut tool_calls,
                &mut reasoning_buffer,
                &mut content_emitted,
            ) {
                Some(Ok(chunk)) => emitted.push(chunk),
                Some(Err(e)) => panic!("unexpected error: {e:?}"),
                None => break,
            }
        }

        let text_chunks: Vec<_> = emitted
            .iter()
            .filter_map(|c| c.delta.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(
            text_chunks,
            vec!["Answer."],
            "only content should be emitted, reasoning is scratch"
        );
        let stop_chunks: Vec<_> = emitted
            .iter()
            .filter_map(|c| c.stop_reason.as_deref())
            .collect();
        assert_eq!(stop_chunks, vec!["stop"]);
    }

    #[test]
    fn sse_content_then_reasoning_drops_reasoning() {
        // GLM ordering quirk: content BEFORE reasoning. Still emit only content.
        let mut buffer = String::from(
            "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"content\":\"First.\"},\"finish_reason\":null}]}\n\n\
             data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"reasoning_content\":\"post-hoc thinking\"},\"finish_reason\":null}]}\n\n\
             data: {\"id\":\"1\",\"choices\":[{\"delta\":{},\"finish_reason\":\"stop\"}]}\n\n",
        );
        let mut tool_calls = Vec::new();
        let mut reasoning_buffer = String::new();
        let mut content_emitted = false;

        let mut emitted = Vec::new();
        loop {
            match extract_openai_sse_event(
                &mut buffer,
                &mut tool_calls,
                &mut reasoning_buffer,
                &mut content_emitted,
            ) {
                Some(Ok(chunk)) => emitted.push(chunk),
                Some(Err(e)) => panic!("unexpected error: {e:?}"),
                None => break,
            }
        }

        let text_chunks: Vec<_> = emitted.iter().filter_map(|c| c.delta.as_deref()).collect();
        assert_eq!(text_chunks, vec!["First."]);
        assert!(content_emitted, "content_emitted should be true");
    }

    #[test]
    fn sse_reasoning_only_done_marker_flushes() {
        // Reasoning-only stream terminated by [DONE] (no explicit finish_reason).
        // The [DONE] handler must also flush the reasoning buffer.
        let mut buffer = String::from(
            "data: {\"id\":\"1\",\"choices\":[{\"delta\":{\"reasoning\":\"partial thought\"},\"finish_reason\":null}]}\n\n\
             data: [DONE]\n\n",
        );
        let mut tool_calls = Vec::new();
        let mut reasoning_buffer = String::new();
        let mut content_emitted = false;

        let mut emitted = Vec::new();
        loop {
            match extract_openai_sse_event(
                &mut buffer,
                &mut tool_calls,
                &mut reasoning_buffer,
                &mut content_emitted,
            ) {
                Some(Ok(chunk)) => emitted.push(chunk),
                Some(Err(e)) => panic!("unexpected error: {e:?}"),
                None => break,
            }
        }

        let text_chunks: Vec<_> = emitted.iter().filter_map(|c| c.delta.as_deref()).collect();
        assert_eq!(text_chunks, vec!["partial thought"]);
    }

    #[test]
    fn stream_delta_deserializes_reasoning_fields() {
        // Verify all three flat reasoning fields deserialize from a delta.
        let d1: OpenAIStreamDelta = serde_json::from_str(r#"{"reasoning_content":"a"}"#).unwrap();
        assert_eq!(extract_delta_reasoning(&d1), Some("a"));

        let d2: OpenAIStreamDelta = serde_json::from_str(r#"{"reasoning":"b"}"#).unwrap();
        assert_eq!(extract_delta_reasoning(&d2), Some("b"));

        let d3: OpenAIStreamDelta = serde_json::from_str(r#"{"thinking":"c"}"#).unwrap();
        assert_eq!(extract_delta_reasoning(&d3), Some("c"));

        // Priority: reasoning_content wins over reasoning wins over thinking.
        let d4: OpenAIStreamDelta = serde_json::from_str(
            r#"{"reasoning_content":"win","reasoning":"lose","thinking":"lose"}"#,
        )
        .unwrap();
        assert_eq!(extract_delta_reasoning(&d4), Some("win"));

        // Empty strings don't trigger extraction.
        let d5: OpenAIStreamDelta =
            serde_json::from_str(r#"{"reasoning_content":"","role":"assistant"}"#).unwrap();
        assert_eq!(extract_delta_reasoning(&d5), None);
    }
}
