//! Web fetch tool — retrieves content from URLs via HTTP GET.

use async_trait::async_trait;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};

/// Default request timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 10;

/// Maximum response body size (32 KB — keeps tool output within token budget).
const MAX_RESPONSE_SIZE: usize = 32 * 1024;

pub struct WebFetchTool {
    client: reqwest::Client,
}

impl Default for WebFetchTool {
    fn default() -> Self {
        Self::new()
    }
}

impl WebFetchTool {
    pub fn new() -> Self {
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .user_agent("TEMM1E/0.1")
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Self { client }
    }
}

#[async_trait]
impl Tool for WebFetchTool {
    fn name(&self) -> &str {
        "web_fetch"
    }

    fn description(&self) -> &str {
        "Fetch the content of a web page or API endpoint via HTTP GET. \
         Returns the response body as text. Use this to look up documentation, \
         check APIs, fetch data, or research information on the web."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "url": {
                    "type": "string",
                    "description": "The URL to fetch (must start with http:// or https://)"
                },
                "headers": {
                    "type": "object",
                    "description": "Optional HTTP headers as key-value pairs",
                    "additionalProperties": { "type": "string" }
                }
            },
            "required": ["url"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: Vec::new(),
            network_access: vec!["*".to_string()],
            shell_access: false,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let url = input
            .arguments
            .get("url")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: url".into()))?;

        // Validate URL scheme
        if !url.starts_with("http://") && !url.starts_with("https://") {
            return Ok(ToolOutput {
                content: "URL must start with http:// or https://".to_string(),
                is_error: true,
            });
        }

        let mut request = self.client.get(url);

        // Add optional headers
        if let Some(headers) = input.arguments.get("headers").and_then(|v| v.as_object()) {
            for (key, value) in headers {
                if let Some(val_str) = value.as_str() {
                    request = request.header(key.as_str(), val_str);
                }
            }
        }

        tracing::info!(url = %url, "Fetching URL");

        match request.send().await {
            Ok(response) => {
                let status = response.status();
                let status_code = status.as_u16();

                match response.text().await {
                    Ok(mut body) => {
                        if body.len() > MAX_RESPONSE_SIZE {
                            body.truncate(MAX_RESPONSE_SIZE);
                            body.push_str("\n... [response truncated]");
                        }

                        let content = format!(
                            "HTTP {} {}\n\n{}",
                            status_code,
                            status.canonical_reason().unwrap_or(""),
                            body,
                        );

                        Ok(ToolOutput {
                            content,
                            is_error: status.is_client_error() || status.is_server_error(),
                        })
                    }
                    Err(e) => Ok(ToolOutput {
                        content: format!("Failed to read response body: {}", e),
                        is_error: true,
                    }),
                }
            }
            Err(e) => Ok(ToolOutput {
                content: format!("Request failed: {}", e),
                is_error: true,
            }),
        }
    }
}
