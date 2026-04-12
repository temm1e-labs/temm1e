//! Exa web search tool — AI-powered web search via the Exa API.

use async_trait::async_trait;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};

/// Default request timeout in seconds.
const DEFAULT_TIMEOUT_SECS: u64 = 15;

/// Default number of search results.
const DEFAULT_NUM_RESULTS: u64 = 10;

/// Maximum response body size (64 KB — search results can be rich).
const MAX_RESPONSE_SIZE: usize = 64 * 1024;

pub struct WebSearchTool {
    client: reqwest::Client,
    api_key: String,
}

impl WebSearchTool {
    /// Create a new Exa web search tool. Returns `None` if `EXA_API_KEY` is not set.
    pub fn new() -> Option<Self> {
        let api_key = std::env::var("EXA_API_KEY").ok()?;
        if api_key.is_empty() {
            return None;
        }

        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(DEFAULT_TIMEOUT_SECS))
            .build()
            .unwrap_or_else(|_| reqwest::Client::new());

        Some(Self { client, api_key })
    }
}

#[async_trait]
impl Tool for WebSearchTool {
    fn name(&self) -> &str {
        "web_search"
    }

    fn description(&self) -> &str {
        "Search the web using Exa AI-powered search. Returns relevant web pages \
         with titles, URLs, and content. Supports neural and fast search types, \
         content retrieval (text, highlights, summary), category filtering \
         (company, research paper, news, personal site, financial report, people), \
         domain filtering, text filtering, and date ranges."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "The search query"
                },
                "num_results": {
                    "type": "integer",
                    "description": "Number of results (1-100, default 10)",
                    "default": 10
                },
                "type": {
                    "type": "string",
                    "enum": ["auto", "neural", "fast"],
                    "description": "Search type: 'auto' (default), 'neural' (embeddings-based), or 'fast' (low latency)",
                    "default": "auto"
                },
                "category": {
                    "type": "string",
                    "enum": ["company", "research paper", "news", "personal site", "financial report", "people"],
                    "description": "Optional category filter"
                },
                "include_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Only include results from these domains"
                },
                "exclude_domains": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Exclude results from these domains"
                },
                "include_text": {
                    "type": "string",
                    "description": "Only include pages containing this text (max 5 words)"
                },
                "exclude_text": {
                    "type": "string",
                    "description": "Exclude pages containing this text (max 5 words)"
                },
                "start_published_date": {
                    "type": "string",
                    "description": "Only include pages published after this ISO 8601 date"
                },
                "end_published_date": {
                    "type": "string",
                    "description": "Only include pages published before this ISO 8601 date"
                },
                "contents": {
                    "type": "string",
                    "enum": ["text", "highlights", "summary"],
                    "description": "Content retrieval mode: 'text' (full page text), 'highlights' (key passages), or 'summary' (AI summary). Default: highlights",
                    "default": "highlights"
                }
            },
            "required": ["query"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: Vec::new(),
            network_access: vec!["api.exa.ai".to_string()],
            shell_access: false,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let args = &input.arguments;

        let query = args
            .get("query")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: query".into()))?;

        let num_results = args
            .get("num_results")
            .and_then(|v| v.as_u64())
            .unwrap_or(DEFAULT_NUM_RESULTS)
            .min(100)
            .max(1);

        let search_type = args
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("auto");

        let contents_mode = args
            .get("contents")
            .and_then(|v| v.as_str())
            .unwrap_or("highlights");

        // Build the contents object based on the chosen mode
        let contents = match contents_mode {
            "text" => serde_json::json!({ "text": true }),
            "summary" => serde_json::json!({ "summary": { "query": query } }),
            _ => serde_json::json!({ "highlights": true }),
        };

        // Build request body
        let mut body = serde_json::json!({
            "query": query,
            "numResults": num_results,
            "type": search_type,
            "contents": contents,
        });

        let body_obj = body.as_object_mut().unwrap();

        // Optional filters
        if let Some(category) = args.get("category").and_then(|v| v.as_str()) {
            body_obj.insert("category".into(), serde_json::json!(category));
        }

        if let Some(domains) = args.get("include_domains").and_then(|v| v.as_array()) {
            body_obj.insert("includeDomains".into(), serde_json::json!(domains));
        }

        if let Some(domains) = args.get("exclude_domains").and_then(|v| v.as_array()) {
            body_obj.insert("excludeDomains".into(), serde_json::json!(domains));
        }

        if let Some(text) = args.get("include_text").and_then(|v| v.as_str()) {
            body_obj.insert("includeText".into(), serde_json::json!([text]));
        }

        if let Some(text) = args.get("exclude_text").and_then(|v| v.as_str()) {
            body_obj.insert("excludeText".into(), serde_json::json!([text]));
        }

        if let Some(date) = args.get("start_published_date").and_then(|v| v.as_str()) {
            body_obj.insert("startPublishedDate".into(), serde_json::json!(date));
        }

        if let Some(date) = args.get("end_published_date").and_then(|v| v.as_str()) {
            body_obj.insert("endPublishedDate".into(), serde_json::json!(date));
        }

        tracing::info!(query = %query, search_type = %search_type, "Exa web search");

        let response = self
            .client
            .post("https://api.exa.ai/search")
            .header("x-api-key", &self.api_key)
            .header("x-exa-integration", "temm1e")
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Temm1eError::Tool(format!("Exa search request failed: {}", e)))?;

        let status = response.status();

        if !status.is_success() {
            let error_body = response.text().await.unwrap_or_default();
            return Ok(ToolOutput {
                content: format!("Exa search failed (HTTP {}): {}", status.as_u16(), error_body),
                is_error: true,
            });
        }

        let mut response_text = response
            .text()
            .await
            .map_err(|e| Temm1eError::Tool(format!("Failed to read Exa response: {}", e)))?;

        if response_text.len() > MAX_RESPONSE_SIZE {
            let mut end = MAX_RESPONSE_SIZE;
            while end > 0 && !response_text.is_char_boundary(end) {
                end -= 1;
            }
            response_text.truncate(end);
        }

        // Parse and format results for readability
        let content = match serde_json::from_str::<serde_json::Value>(&response_text) {
            Ok(json) => format_results(&json, contents_mode),
            Err(_) => response_text,
        };

        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}

/// Format Exa search results into a readable string for the agent.
fn format_results(json: &serde_json::Value, contents_mode: &str) -> String {
    let results = match json.get("results").and_then(|v| v.as_array()) {
        Some(r) => r,
        None => return "No results found.".to_string(),
    };

    if results.is_empty() {
        return "No results found.".to_string();
    }

    let mut output = String::new();
    for (i, result) in results.iter().enumerate() {
        let title = result.get("title").and_then(|v| v.as_str()).unwrap_or("(no title)");
        let url = result.get("url").and_then(|v| v.as_str()).unwrap_or("");
        let published = result
            .get("publishedDate")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        output.push_str(&format!("{}. {}\n", i + 1, title));
        output.push_str(&format!("   URL: {}\n", url));
        if !published.is_empty() {
            output.push_str(&format!("   Published: {}\n", published));
        }

        match contents_mode {
            "highlights" => {
                if let Some(highlights) = result.get("highlights").and_then(|v| v.as_array()) {
                    for hl in highlights {
                        if let Some(text) = hl.as_str() {
                            output.push_str(&format!("   > {}\n", text));
                        }
                    }
                }
            }
            "text" => {
                if let Some(text) = result.get("text").and_then(|v| v.as_str()) {
                    // Truncate long text per result
                    let preview = if text.len() > 500 {
                        let mut end = 500;
                        while end > 0 && !text.is_char_boundary(end) {
                            end -= 1;
                        }
                        format!("{}...", &text[..end])
                    } else {
                        text.to_string()
                    };
                    output.push_str(&format!("   {}\n", preview));
                }
            }
            "summary" => {
                if let Some(summary) = result.get("summary").and_then(|v| v.as_str()) {
                    output.push_str(&format!("   {}\n", summary));
                }
            }
            _ => {}
        }

        output.push('\n');
    }

    output
}
