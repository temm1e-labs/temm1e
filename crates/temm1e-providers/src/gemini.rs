//! Native Gemini API provider.
//!
//! Uses Google's native `generateContent` REST endpoint instead of the
//! OpenAI-compatible shim. This properly handles `systemInstruction` as
//! a first-class field, which Gemini respects for structured output
//! (unlike the OpenAI-compat endpoint which often ignores system prompts).
//!
//! Endpoint: `https://generativelanguage.googleapis.com/v1beta/models/{model}:generateContent`
//! Auth: API key via `?key=` query parameter.
//! Roles: `user` and `model` only (no `system` or `assistant`).

use async_trait::async_trait;
use futures::stream::BoxStream;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use tracing::debug;

use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart, MessageContent, Role,
    StreamChunk, Usage,
};
use temm1e_core::Provider;

// ---------------------------------------------------------------------------
// Gemini-native request/response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiRequest {
    contents: Vec<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    system_instruction: Option<GeminiContent>,
    #[serde(skip_serializing_if = "Option::is_none")]
    generation_config: Option<GeminiGenerationConfig>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Vec<GeminiTool>>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiContent {
    #[serde(skip_serializing_if = "Option::is_none")]
    role: Option<String>,
    parts: Vec<GeminiPart>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiPart {
    #[serde(skip_serializing_if = "Option::is_none")]
    text: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_call: Option<GeminiFunctionCall>,
    #[serde(skip_serializing_if = "Option::is_none")]
    function_response: Option<GeminiFunctionResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    inline_data: Option<GeminiInlineData>,
    /// Gemini 3 thought signature — sibling of functionCall, must be echoed back.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    thought_signature: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiFunctionCall {
    name: String,
    args: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
struct GeminiFunctionResponse {
    name: String,
    response: serde_json::Value,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiInlineData {
    mime_type: String,
    data: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiGenerationConfig {
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_output_tokens: Option<u32>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
struct GeminiTool {
    function_declarations: Vec<GeminiFunctionDeclaration>,
}

#[derive(Debug, Serialize)]
struct GeminiFunctionDeclaration {
    name: String,
    description: String,
    parameters: serde_json::Value,
}

// Response types

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiResponse {
    candidates: Option<Vec<GeminiCandidate>>,
    usage_metadata: Option<GeminiUsageMetadata>,
    #[serde(default)]
    prompt_feedback: Option<serde_json::Value>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiCandidate {
    content: Option<GeminiContent>,
    finish_reason: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
struct GeminiUsageMetadata {
    #[serde(default)]
    prompt_token_count: u32,
    #[serde(default)]
    candidates_token_count: u32,
    #[serde(default)]
    total_token_count: u32,
}

// ---------------------------------------------------------------------------
// Provider implementation
// ---------------------------------------------------------------------------

/// Recursively strip fields that Gemini's native API doesn't support
/// in function declaration schemas (e.g., `additionalProperties`).
fn strip_unsupported_schema_fields(schema: &serde_json::Value) -> serde_json::Value {
    match schema {
        serde_json::Value::Object(map) => {
            let mut cleaned = serde_json::Map::new();
            for (key, value) in map {
                // Gemini doesn't support these JSON Schema fields
                if key == "additionalProperties" || key == "$schema" {
                    continue;
                }
                cleaned.insert(key.clone(), strip_unsupported_schema_fields(value));
            }
            serde_json::Value::Object(cleaned)
        }
        serde_json::Value::Array(arr) => {
            serde_json::Value::Array(arr.iter().map(strip_unsupported_schema_fields).collect())
        }
        other => other.clone(),
    }
}

pub struct GeminiProvider {
    api_key: String,
    client: Client,
    base_url: String,
}

impl GeminiProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            api_key,
            client: Client::new(),
            base_url: "https://generativelanguage.googleapis.com/v1beta".to_string(),
        }
    }

    /// Convert TEMM1E messages to Gemini format.
    /// Gemini only supports "user" and "model" roles.
    /// System messages are extracted and placed in `systemInstruction`.
    fn convert_request(&self, request: &CompletionRequest) -> GeminiRequest {
        let mut system_text = request.system.clone().unwrap_or_default();
        let mut contents = Vec::new();

        for msg in &request.messages {
            match msg.role {
                Role::System => {
                    // Accumulate system messages into systemInstruction
                    let text = match &msg.content {
                        MessageContent::Text(t) => t.clone(),
                        MessageContent::Parts(parts) => parts
                            .iter()
                            .filter_map(|p| match p {
                                ContentPart::Text { text } => Some(text.clone()),
                                _ => None,
                            })
                            .collect::<Vec<_>>()
                            .join("\n"),
                    };
                    if !system_text.is_empty() {
                        system_text.push('\n');
                    }
                    system_text.push_str(&text);
                }
                Role::User => {
                    contents.push(self.convert_message(msg, "user"));
                }
                Role::Assistant => {
                    contents.push(self.convert_message(msg, "model"));
                }
                Role::Tool => {
                    // Tool results → function_response parts
                    if let MessageContent::Parts(parts) = &msg.content {
                        let gemini_parts: Vec<GeminiPart> = parts
                            .iter()
                            .filter_map(|p| match p {
                                ContentPart::ToolResult {
                                    tool_use_id,
                                    content,
                                    ..
                                } => Some(GeminiPart {
                                    text: None,
                                    function_call: None,
                                    function_response: Some(GeminiFunctionResponse {
                                        name: tool_use_id.clone(),
                                        response: serde_json::json!({ "result": content }),
                                    }),
                                    inline_data: None,
                                    thought_signature: None,
                                }),
                                _ => None,
                            })
                            .collect();
                        if !gemini_parts.is_empty() {
                            contents.push(GeminiContent {
                                role: Some("user".to_string()),
                                parts: gemini_parts,
                            });
                        }
                    }
                }
            }
        }

        // Ensure conversation doesn't start with "model" (Gemini requires "user" first)
        if contents
            .first()
            .and_then(|c| c.role.as_deref())
            .unwrap_or("")
            == "model"
        {
            contents.insert(
                0,
                GeminiContent {
                    role: Some("user".to_string()),
                    parts: vec![GeminiPart {
                        text: Some(".".to_string()),
                        function_call: None,
                        function_response: None,
                        inline_data: None,
                        thought_signature: None,
                    }],
                },
            );
        }

        let system_instruction = if system_text.is_empty() {
            None
        } else {
            Some(GeminiContent {
                role: None,
                parts: vec![GeminiPart {
                    text: Some(system_text),
                    function_call: None,
                    function_response: None,
                    inline_data: None,
                    thought_signature: None,
                }],
            })
        };

        let generation_config = Some(GeminiGenerationConfig {
            temperature: request.temperature,
            max_output_tokens: request.max_tokens,
        });

        let tools = if request.tools.is_empty() {
            None
        } else {
            Some(vec![GeminiTool {
                function_declarations: request
                    .tools
                    .iter()
                    .map(|t| GeminiFunctionDeclaration {
                        name: t.name.clone(),
                        description: t.description.clone(),
                        parameters: strip_unsupported_schema_fields(&t.parameters),
                    })
                    .collect(),
            }])
        };

        GeminiRequest {
            contents,
            system_instruction,
            generation_config,
            tools,
        }
    }

    fn convert_message(&self, msg: &ChatMessage, role: &str) -> GeminiContent {
        let parts = match &msg.content {
            MessageContent::Text(text) => {
                vec![GeminiPart {
                    text: Some(text.clone()),
                    function_call: None,
                    function_response: None,
                    inline_data: None,
                    thought_signature: None,
                }]
            }
            MessageContent::Parts(parts) => parts
                .iter()
                .filter_map(|p| match p {
                    ContentPart::Text { text } => Some(GeminiPart {
                        text: Some(text.clone()),
                        function_call: None,
                        function_response: None,
                        inline_data: None,
                        thought_signature: None,
                    }),
                    ContentPart::ToolUse {
                        name,
                        input,
                        thought_signature,
                        ..
                    } => Some(GeminiPart {
                        text: None,
                        function_call: Some(GeminiFunctionCall {
                            name: name.clone(),
                            args: input.clone(),
                        }),
                        function_response: None,
                        inline_data: None,
                        // Echo thought_signature as sibling of functionCall
                        thought_signature: thought_signature.clone(),
                    }),
                    ContentPart::Image { media_type, data } => Some(GeminiPart {
                        text: None,
                        function_call: None,
                        function_response: None,
                        inline_data: Some(GeminiInlineData {
                            mime_type: media_type.clone(),
                            data: data.clone(),
                        }),
                        thought_signature: None,
                    }),
                    ContentPart::ToolResult { .. } => None, // handled in Tool role
                })
                .collect(),
        };

        GeminiContent {
            role: Some(role.to_string()),
            parts,
        }
    }

    /// Convert Gemini response to TEMM1E format.
    fn convert_response(&self, response: GeminiResponse) -> CompletionResponse {
        let mut content_parts = Vec::new();
        let mut stop_reason = None;

        if let Some(candidates) = &response.candidates {
            if let Some(candidate) = candidates.first() {
                stop_reason = candidate.finish_reason.clone();
                if let Some(ref content) = candidate.content {
                    for part in &content.parts {
                        if let Some(ref text) = part.text {
                            content_parts.push(ContentPart::Text { text: text.clone() });
                        }
                        if let Some(ref fc) = part.function_call {
                            // Strip default_api: prefix that Gemini 3 adds to tool names
                            let name = fc
                                .name
                                .strip_prefix("default_api:")
                                .unwrap_or(&fc.name)
                                .to_string();
                            content_parts.push(ContentPart::ToolUse {
                                id: format!("gemini-{}", uuid::Uuid::new_v4()),
                                name,
                                input: fc.args.clone(),
                                // thoughtSignature is a sibling of functionCall in the part
                                thought_signature: part.thought_signature.clone(),
                            });
                        }
                    }
                }
            }
        }

        let usage = response
            .usage_metadata
            .map(|u| Usage {
                input_tokens: u.prompt_token_count,
                output_tokens: u.candidates_token_count,
                cost_usd: 0.0,
            })
            .unwrap_or_default();

        CompletionResponse {
            id: uuid::Uuid::new_v4().to_string(),
            content: content_parts,
            stop_reason,
            usage,
        }
    }
}

#[async_trait]
impl Provider for GeminiProvider {
    fn name(&self) -> &str {
        "gemini"
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, Temm1eError> {
        let model = &request.model;
        let url = format!(
            "{}/models/{}:generateContent?key={}",
            self.base_url, model, self.api_key
        );

        let gemini_req = self.convert_request(&request);

        debug!(model = model, "Gemini native: sending request");

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .json(&gemini_req)
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Gemini HTTP error: {e}")))?;

        let status = response.status();
        let body = response
            .text()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Gemini response read error: {e}")))?;

        if !status.is_success() {
            return Err(Temm1eError::Provider(format!(
                "Gemini API error ({}): {}",
                status,
                &body[..body.len().min(500)]
            )));
        }

        let gemini_resp: GeminiResponse = serde_json::from_str(&body).map_err(|e| {
            Temm1eError::Provider(format!(
                "Gemini response parse error: {e}\nBody: {}",
                &body[..body.len().min(500)]
            ))
        })?;

        Ok(self.convert_response(gemini_resp))
    }

    async fn stream(
        &self,
        request: CompletionRequest,
    ) -> Result<BoxStream<'_, Result<StreamChunk, Temm1eError>>, Temm1eError> {
        // For now, simulate streaming by making a non-streaming call
        // and yielding the result as a single chunk.
        // Full SSE streaming can be added later.
        let response = self.complete(request).await?;

        let text = response
            .content
            .iter()
            .filter_map(|p| match p {
                ContentPart::Text { text } => Some(text.clone()),
                _ => None,
            })
            .collect::<String>();

        let chunks = vec![Ok(StreamChunk {
            delta: Some(text),
            tool_use: None,
            stop_reason: response.stop_reason,
        })];

        Ok(Box::pin(futures::stream::iter(chunks)))
    }

    async fn health_check(&self) -> Result<bool, Temm1eError> {
        let url = format!("{}/models?key={}", self.base_url, self.api_key);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Gemini health check error: {e}")))?;

        Ok(response.status().is_success())
    }

    async fn list_models(&self) -> Result<Vec<String>, Temm1eError> {
        let url = format!("{}/models?key={}", self.base_url, self.api_key);

        let response = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Gemini list models error: {e}")))?;

        let body: serde_json::Value = response
            .json()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Gemini parse error: {e}")))?;

        let models = body["models"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["name"].as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        Ok(models)
    }
}
