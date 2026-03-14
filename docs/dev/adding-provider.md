# Developer Guide: Adding a New AI Provider

This tutorial walks through adding a new AI provider to TEMM1E. By the end, you will have a fully integrated provider that handles completion requests, streaming responses, and health checks.

## Overview

Adding a provider requires:

1. Implementing the `Provider` trait
2. Mapping TEMM1E types to the provider's API format
3. Handling streaming responses
4. Wiring the provider into the agent runtime
5. Adding configuration support

## Step 1: Understand the Trait

The `Provider` trait is defined in `crates/temm1e-core/src/traits/provider.rs`:

```rust
#[async_trait]
pub trait Provider: Send + Sync {
    /// Provider name (e.g., "anthropic", "openai-compatible")
    fn name(&self) -> &str;

    /// Send a completion request and get a full response
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, Temm1eError>;

    /// Send a completion request and get a streaming response
    async fn stream(&self, request: CompletionRequest) -> Result<BoxStream<'_, Result<StreamChunk, Temm1eError>>, Temm1eError>;

    /// Check if the provider is healthy and reachable
    async fn health_check(&self) -> Result<bool, Temm1eError>;

    /// List available models for this provider
    async fn list_models(&self) -> Result<Vec<String>, Temm1eError>;
}
```

The key types:

- `CompletionRequest` -- model, messages (with roles), tools, max_tokens, temperature, system prompt
- `CompletionResponse` -- id, content parts, stop reason, usage
- `StreamChunk` -- text delta, tool use, stop reason
- `ContentPart` -- `Text`, `ToolUse`, `ToolResult`

## Step 2: Create the Provider Module

Create a new file in `crates/temm1e-providers/src/`. For this example, we will add a hypothetical "Cohere" provider.

**File**: `crates/temm1e-providers/src/cohere.rs`

```rust
use async_trait::async_trait;
use futures::stream::BoxStream;
use futures::StreamExt;
use reqwest::Client;
use temm1e_core::traits::Provider;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::{
    ChatMessage, CompletionRequest, CompletionResponse, ContentPart,
    Role, StreamChunk, Usage,
};

pub struct CohereProvider {
    client: Client,
    api_key: String,
    base_url: String,
}

impl CohereProvider {
    pub fn new(api_key: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            base_url: "https://api.cohere.ai/v1".to_string(),
        }
    }

    /// Convert TEMM1E messages to the Cohere API format
    fn convert_messages(&self, messages: &[ChatMessage]) -> serde_json::Value {
        // Map Role::User, Role::Assistant, etc. to Cohere's format
        // Map ContentPart::ToolUse/ToolResult to Cohere's tool calling format
        serde_json::json!([])  // placeholder
    }

    /// Convert TEMM1E tool definitions to Cohere's format
    fn convert_tools(&self, request: &CompletionRequest) -> serde_json::Value {
        // Map ToolDefinition { name, description, parameters } to Cohere's schema
        serde_json::json!([])  // placeholder
    }
}

#[async_trait]
impl Provider for CohereProvider {
    fn name(&self) -> &str {
        "cohere"
    }

    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, Temm1eError> {
        let body = serde_json::json!({
            "model": request.model,
            "messages": self.convert_messages(&request.messages),
            "tools": self.convert_tools(&request),
            "max_tokens": request.max_tokens.unwrap_or(4096),
            "temperature": request.temperature.unwrap_or(0.7),
        });

        let response = self.client
            .post(format!("{}/chat", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Cohere API error: {}", e)))?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            return Err(Temm1eError::Provider(
                format!("Cohere API returned {}: {}", status, body)
            ));
        }

        let api_response: serde_json::Value = response.json().await
            .map_err(|e| Temm1eError::Provider(format!("Failed to parse response: {}", e)))?;

        // Convert the Cohere response to TEMM1E's CompletionResponse
        // Extract content, tool calls, stop reason, and usage
        Ok(CompletionResponse {
            id: api_response["id"].as_str().unwrap_or("").to_string(),
            content: vec![ContentPart::Text {
                text: api_response["text"].as_str().unwrap_or("").to_string(),
            }],
            stop_reason: Some("end_turn".to_string()),
            usage: Usage {
                input_tokens: api_response["meta"]["tokens"]["input_tokens"]
                    .as_u64().unwrap_or(0) as u32,
                output_tokens: api_response["meta"]["tokens"]["output_tokens"]
                    .as_u64().unwrap_or(0) as u32,
            },
        })
    }

    async fn stream(&self, request: CompletionRequest) -> Result<BoxStream<'_, Result<StreamChunk, Temm1eError>>, Temm1eError> {
        let body = serde_json::json!({
            "model": request.model,
            "messages": self.convert_messages(&request.messages),
            "stream": true,
            "max_tokens": request.max_tokens.unwrap_or(4096),
        });

        let response = self.client
            .post(format!("{}/chat", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Cohere stream error: {}", e)))?;

        // Parse SSE (Server-Sent Events) stream
        let byte_stream = response.bytes_stream();

        let chunk_stream = byte_stream.map(move |chunk_result| {
            match chunk_result {
                Ok(bytes) => {
                    // Parse the SSE event from bytes
                    // Extract text deltas and tool calls
                    let text = String::from_utf8_lossy(&bytes);
                    Ok(StreamChunk {
                        delta: Some(text.to_string()),
                        tool_use: None,
                        stop_reason: None,
                    })
                }
                Err(e) => Err(Temm1eError::Provider(format!("Stream error: {}", e))),
            }
        });

        Ok(Box::pin(chunk_stream))
    }

    async fn health_check(&self) -> Result<bool, Temm1eError> {
        // Hit a lightweight endpoint to verify connectivity
        let response = self.client
            .get(format!("{}/models", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Health check failed: {}", e)))?;

        Ok(response.status().is_success())
    }

    async fn list_models(&self) -> Result<Vec<String>, Temm1eError> {
        let response = self.client
            .get(format!("{}/models", self.base_url))
            .header("Authorization", format!("Bearer {}", self.api_key))
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("List models failed: {}", e)))?;

        let body: serde_json::Value = response.json().await
            .map_err(|e| Temm1eError::Provider(format!("Parse error: {}", e)))?;

        let models = body["models"]
            .as_array()
            .map(|arr| {
                arr.iter()
                    .filter_map(|m| m["name"].as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        Ok(models)
    }
}
```

## Step 3: Register the Module

Edit `crates/temm1e-providers/src/lib.rs`:

```rust
pub mod anthropic;
pub mod openai_compat;
pub mod google;
pub mod mistral;
pub mod groq;
pub mod cohere;  // <-- Add this
```

## Step 4: Add Dependencies

If needed, add provider-specific dependencies to `crates/temm1e-providers/Cargo.toml`:

```toml
[dependencies]
temm1e-core.workspace = true
async-trait.workspace = true
reqwest.workspace = true
futures.workspace = true
serde.workspace = true
serde_json.workspace = true
tracing.workspace = true
```

Most providers use the standard HTTP API via `reqwest`, so no additional dependencies are usually needed.

## Step 5: Wire into the Agent Runtime

In the code that creates the provider from configuration (typically in the binary crate or gateway initialization), add a match arm:

```rust
fn create_provider(config: &ProviderConfig) -> Result<Box<dyn Provider>, Temm1eError> {
    match config.name.as_deref() {
        Some("anthropic") => Ok(Box::new(AnthropicProvider::new(
            config.api_key.clone().unwrap_or_default(),
        ))),
        Some("openai-compatible") => Ok(Box::new(OpenAiCompatProvider::new(
            config.api_key.clone().unwrap_or_default(),
            config.base_url.clone(),
        ))),
        Some("cohere") => Ok(Box::new(CohereProvider::new(   // <-- Add this
            config.api_key.clone().unwrap_or_default(),
        ))),
        Some(other) => Err(Temm1eError::Config(
            format!("Unknown provider: {}", other)
        )),
        None => Err(Temm1eError::Config(
            "No provider configured".to_string()
        )),
    }
}
```

## Step 6: Configuration

Users configure the new provider in their `config.toml`:

```toml
[provider]
name = "cohere"
api_key = "${COHERE_API_KEY}"
model = "command-r-plus"
```

## Step 7: Handle Tool Calling

Most modern AI providers support tool/function calling. The key mapping is:

| TEMM1E Type | Maps To |
|-------------|---------|
| `ToolDefinition { name, description, parameters }` | Provider's tool schema format |
| `ContentPart::ToolUse { id, name, input }` | Provider's tool call response |
| `ContentPart::ToolResult { tool_use_id, content, is_error }` | Provider's tool result message |

When the model's response contains tool calls:

1. The agent runtime extracts `ContentPart::ToolUse` from the response
2. Executes the tool via `Tool::execute()`
3. Creates a `ContentPart::ToolResult` with the output
4. Sends the result back to the provider in a follow-up request

Your provider must correctly map between TEMM1E's format and the provider's native format in both directions.

## Step 8: Implement Retry Logic

Production providers should include retry logic for transient failures:

```rust
async fn complete_with_retry(
    &self,
    request: CompletionRequest,
    max_retries: u32,
) -> Result<CompletionResponse, Temm1eError> {
    let mut last_error = None;
    for attempt in 0..=max_retries {
        if attempt > 0 {
            let delay = std::time::Duration::from_millis(100 * 2u64.pow(attempt - 1));
            tokio::time::sleep(delay).await;
        }
        match self.complete(request.clone()).await {
            Ok(response) => return Ok(response),
            Err(e) => {
                tracing::warn!(attempt, error = %e, "Provider request failed, retrying");
                last_error = Some(e);
            }
        }
    }
    Err(last_error.unwrap())
}
```

## Step 9: Write Tests

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_provider_name() {
        let provider = CohereProvider::new("test-key".into());
        assert_eq!(provider.name(), "cohere");
    }

    #[tokio::test]
    async fn test_health_check_invalid_key() {
        let provider = CohereProvider::new("invalid-key".into());
        // Health check should return false or an error with an invalid key
        let result = provider.health_check().await;
        // Depending on implementation: assert error or assert Ok(false)
        assert!(result.is_err() || result == Ok(false));
    }

    #[test]
    fn test_message_conversion() {
        let provider = CohereProvider::new("test".into());
        let messages = vec![
            ChatMessage {
                role: Role::User,
                content: MessageContent::Text("Hello".into()),
            },
        ];
        let converted = provider.convert_messages(&messages);
        // Verify the conversion matches Cohere's expected format
        assert!(converted.is_array());
    }
}
```

## Checklist

- [ ] `Provider` trait implemented: `name()`, `complete()`, `stream()`, `health_check()`, `list_models()`
- [ ] Request mapping: `CompletionRequest` -> provider's API format
- [ ] Response mapping: provider's response -> `CompletionResponse` / `StreamChunk`
- [ ] Tool calling: `ToolDefinition` -> provider format; provider tool calls -> `ContentPart::ToolUse`
- [ ] Streaming: SSE parsing with proper chunk-to-`StreamChunk` conversion
- [ ] Error handling: HTTP errors mapped to `Temm1eError::Provider(...)`
- [ ] Retry logic with exponential backoff for transient failures
- [ ] Health check hits a lightweight endpoint
- [ ] Provider wired into the creation function with config matching
- [ ] Unit tests for name, message conversion, error handling
- [ ] Integration tests gated behind an environment variable for API key
