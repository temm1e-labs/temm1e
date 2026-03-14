//! Usage audit tool — query usage history and configure usage display.
//!
//! Provides three actions:
//! - `summary`  — get aggregated usage stats for the current chat
//! - `recent`   — get recent per-turn usage records
//! - `config`   — toggle usage display on/off for the current chat

use std::sync::Arc;

use async_trait::async_trait;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput, UsageStore};

pub struct UsageAuditTool {
    store: Arc<dyn UsageStore>,
}

impl UsageAuditTool {
    pub fn new(store: Arc<dyn UsageStore>) -> Self {
        Self { store }
    }

    async fn handle_summary(&self, ctx: &ToolContext) -> Result<ToolOutput, Temm1eError> {
        let summary = self.store.usage_summary(&ctx.chat_id).await?;

        if summary.turn_count == 0 {
            return Ok(ToolOutput {
                content: "No usage records found for this chat.".to_string(),
                is_error: false,
            });
        }

        let combined = summary.combined_tokens();
        let output = format!(
            "Usage Summary for this chat:\n\
             Turns: {}\n\
             Total API Calls: {}\n\
             Total Input Tokens: {}\n\
             Total Output Tokens: {}\n\
             Total Combined Tokens: {}\n\
             Total Tools Used: {}\n\
             Total Cost: ${:.4}",
            summary.turn_count,
            summary.total_api_calls,
            summary.total_input_tokens,
            summary.total_output_tokens,
            combined,
            summary.total_tools_used,
            summary.total_cost_usd,
        );

        Ok(ToolOutput {
            content: output,
            is_error: false,
        })
    }

    async fn handle_recent(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let limit = input.get("limit").and_then(|v| v.as_u64()).unwrap_or(10) as u32;

        let records = self.store.query_usage(&ctx.chat_id, Some(limit)).await?;

        if records.is_empty() {
            return Ok(ToolOutput {
                content: "No usage records found for this chat.".to_string(),
                is_error: false,
            });
        }

        let mut output = format!("Recent usage ({} records):\n", records.len());
        for (i, r) in records.iter().enumerate() {
            let combined = r.input_tokens + r.output_tokens;
            output.push_str(&format!(
                "\n{}. {} ({})\n\
                    Model: {} | API Calls: {} | Input: {} | Output: {} | Combined: {} | Tools: {} | Cost: ${:.4}\n",
                i + 1,
                r.timestamp.format("%Y-%m-%d %H:%M:%S UTC"),
                r.provider,
                r.model,
                r.api_calls,
                r.input_tokens,
                r.output_tokens,
                combined,
                r.tools_used,
                r.total_cost_usd,
            ));
        }

        Ok(ToolOutput {
            content: output,
            is_error: false,
        })
    }

    async fn handle_config(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let enabled = input.get("show_usage").and_then(|v| v.as_bool());

        match enabled {
            Some(true) => {
                self.store.set_usage_display(&ctx.chat_id, true).await?;
                Ok(ToolOutput {
                    content: "Usage display enabled. You'll see usage metrics after each response."
                        .to_string(),
                    is_error: false,
                })
            }
            Some(false) => {
                self.store.set_usage_display(&ctx.chat_id, false).await?;
                Ok(ToolOutput {
                    content: "Usage display disabled. Use /usage to check stats anytime."
                        .to_string(),
                    is_error: false,
                })
            }
            None => {
                let current = self.store.is_usage_display_enabled(&ctx.chat_id).await?;
                Ok(ToolOutput {
                    content: format!(
                        "Usage display is currently {}. Set show_usage to true/false to change.",
                        if current { "enabled" } else { "disabled" }
                    ),
                    is_error: false,
                })
            }
        }
    }
}

#[async_trait]
impl Tool for UsageAuditTool {
    fn name(&self) -> &str {
        "usage_audit"
    }

    fn description(&self) -> &str {
        "Query usage statistics and configure usage display. Use this when the user asks about \
         token usage, API costs, or wants to see/hide the usage summary after each response. \
         Actions: 'summary' (total stats), 'recent' (per-turn history), 'config' (toggle display)."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["summary", "recent", "config"],
                    "description": "The usage operation to perform"
                },
                "limit": {
                    "type": "integer",
                    "description": "Number of recent records to return (for 'recent' action). Default: 10."
                },
                "show_usage": {
                    "type": "boolean",
                    "description": "For 'config' action: true to show usage after each response, false to hide it."
                }
            },
            "required": ["action"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: Vec::new(),
            network_access: Vec::new(),
            shell_access: false,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let action = input
            .arguments
            .get("action")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: action".into()))?;

        tracing::info!(action = %action, "Executing usage_audit tool");

        match action {
            "summary" => self.handle_summary(ctx).await,
            "recent" => self.handle_recent(&input.arguments, ctx).await,
            "config" => self.handle_config(&input.arguments, ctx).await,
            _ => Ok(ToolOutput {
                content: format!(
                    "Unknown action '{}'. Valid actions: summary, recent, config",
                    action
                ),
                is_error: true,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use std::sync::Mutex;
    use temm1e_core::{UsageRecord, UsageSummary};

    /// Mock UsageStore for testing.
    struct MockUsageStore {
        records: Mutex<Vec<UsageRecord>>,
        display_enabled: Mutex<bool>,
    }

    impl MockUsageStore {
        fn new() -> Self {
            Self {
                records: Mutex::new(Vec::new()),
                display_enabled: Mutex::new(true),
            }
        }
    }

    #[async_trait]
    impl UsageStore for MockUsageStore {
        async fn record_usage(&self, record: UsageRecord) -> Result<(), Temm1eError> {
            self.records.lock().unwrap().push(record);
            Ok(())
        }

        async fn query_usage(
            &self,
            chat_id: &str,
            limit: Option<u32>,
        ) -> Result<Vec<UsageRecord>, Temm1eError> {
            let records = self.records.lock().unwrap();
            let filtered: Vec<UsageRecord> = records
                .iter()
                .filter(|r| r.chat_id == chat_id)
                .cloned()
                .collect();
            let limit = limit.unwrap_or(10) as usize;
            Ok(filtered.into_iter().take(limit).collect())
        }

        async fn usage_summary(&self, chat_id: &str) -> Result<UsageSummary, Temm1eError> {
            let records = self.records.lock().unwrap();
            let filtered: Vec<&UsageRecord> =
                records.iter().filter(|r| r.chat_id == chat_id).collect();
            let mut summary = UsageSummary::default();
            for r in &filtered {
                summary.total_api_calls += r.api_calls as u64;
                summary.total_input_tokens += r.input_tokens as u64;
                summary.total_output_tokens += r.output_tokens as u64;
                summary.total_tools_used += r.tools_used as u64;
                summary.total_cost_usd += r.total_cost_usd;
                summary.turn_count += 1;
            }
            Ok(summary)
        }

        async fn set_usage_display(
            &self,
            _chat_id: &str,
            enabled: bool,
        ) -> Result<(), Temm1eError> {
            *self.display_enabled.lock().unwrap() = enabled;
            Ok(())
        }

        async fn is_usage_display_enabled(&self, _chat_id: &str) -> Result<bool, Temm1eError> {
            Ok(*self.display_enabled.lock().unwrap())
        }
    }

    fn test_ctx() -> ToolContext {
        ToolContext {
            workspace_path: PathBuf::from("/tmp/test"),
            session_id: "test-session".to_string(),
            chat_id: "chat-123".to_string(),
        }
    }

    fn make_input(args: serde_json::Value) -> ToolInput {
        ToolInput {
            name: "usage_audit".to_string(),
            arguments: args,
        }
    }

    #[tokio::test]
    async fn summary_empty() {
        let store = Arc::new(MockUsageStore::new());
        let tool = UsageAuditTool::new(store);
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({"action": "summary"}));
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("No usage records"));
    }

    #[tokio::test]
    async fn summary_with_records() {
        let store = Arc::new(MockUsageStore::new());
        store
            .record_usage(UsageRecord {
                id: "r1".to_string(),
                chat_id: "chat-123".to_string(),
                session_id: "s1".to_string(),
                timestamp: chrono::Utc::now(),
                api_calls: 2,
                input_tokens: 5000,
                output_tokens: 1000,
                tools_used: 1,
                total_cost_usd: 0.03,
                provider: "anthropic".to_string(),
                model: "claude-sonnet-4-6".to_string(),
            })
            .await
            .unwrap();

        let tool = UsageAuditTool::new(store);
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({"action": "summary"}));
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("Turns: 1"));
        assert!(output.content.contains("Total API Calls: 2"));
        assert!(output.content.contains("$0.0300"));
    }

    #[tokio::test]
    async fn recent_records() {
        let store = Arc::new(MockUsageStore::new());
        store
            .record_usage(UsageRecord {
                id: "r1".to_string(),
                chat_id: "chat-123".to_string(),
                session_id: "s1".to_string(),
                timestamp: chrono::Utc::now(),
                api_calls: 1,
                input_tokens: 2000,
                output_tokens: 500,
                tools_used: 0,
                total_cost_usd: 0.01,
                provider: "openai".to_string(),
                model: "gpt-4o".to_string(),
            })
            .await
            .unwrap();

        let tool = UsageAuditTool::new(store);
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({"action": "recent", "limit": 5}));
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("gpt-4o"));
        assert!(output.content.contains("openai"));
    }

    #[tokio::test]
    async fn config_toggle() {
        let store = Arc::new(MockUsageStore::new());
        let tool = UsageAuditTool::new(store);
        let ctx = test_ctx();

        // Disable
        let input = make_input(serde_json::json!({"action": "config", "show_usage": false}));
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("disabled"));

        // Enable
        let input = make_input(serde_json::json!({"action": "config", "show_usage": true}));
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("enabled"));
    }

    #[tokio::test]
    async fn config_query_current() {
        let store = Arc::new(MockUsageStore::new());
        let tool = UsageAuditTool::new(store);
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({"action": "config"}));
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("currently"));
    }

    #[tokio::test]
    async fn invalid_action() {
        let store = Arc::new(MockUsageStore::new());
        let tool = UsageAuditTool::new(store);
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({"action": "invalid"}));
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(output.is_error);
    }

    #[tokio::test]
    async fn missing_action() {
        let store = Arc::new(MockUsageStore::new());
        let tool = UsageAuditTool::new(store);
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({}));
        let result = tool.execute(input, &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn tool_metadata() {
        let store = Arc::new(MockUsageStore::new());
        let tool = UsageAuditTool::new(store);

        assert_eq!(tool.name(), "usage_audit");
        assert!(tool.description().contains("usage"));
        assert!(!tool.declarations().shell_access);

        let schema = tool.parameters_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("action").is_some());
        assert!(props.get("limit").is_some());
        assert!(props.get("show_usage").is_some());
    }
}
