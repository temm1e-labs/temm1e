//! λ-Memory recall tool — lets Tem retrieve faded memories by hash.
//!
//! When Tem sees a `[faded]` or `[cool]` memory in its context with a hash
//! prefix, it can use this tool to retrieve the full memory content.
//! Recalling a memory "reheats" it — updating `last_accessed` so it
//! naturally appears as hot in subsequent turns.

use async_trait::async_trait;
use std::sync::Arc;
use temm1e_core::error::Temm1eError;
use temm1e_core::{Memory, Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};

pub struct LambdaRecallTool {
    memory: Arc<dyn Memory>,
}

impl LambdaRecallTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for LambdaRecallTool {
    fn name(&self) -> &str {
        "lambda_recall"
    }

    fn description(&self) -> &str {
        "Recall a faded λ-memory by its hash prefix. Use this when you see a \
         [faded] or [cool] memory in your λ-Memory section that you need full \
         details for. Provide the hash shown (e.g., 'a7f3b2c') to retrieve \
         the complete memory. Recalling a memory reheats it — it will appear \
         as hot in subsequent turns."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "hash": {
                    "type": "string",
                    "description": "The memory hash prefix (e.g., 'a7f3b2c'). Can include or omit the leading #."
                }
            },
            "required": ["hash"]
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
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let hash = input
            .arguments
            .get("hash")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: hash".into()))?;

        // Strip leading # if present
        let hash_clean = hash.trim_start_matches('#');

        match self.memory.lambda_recall(hash_clean).await? {
            Some(entry) => {
                // Touch the memory — reheat it
                if let Err(e) = self.memory.lambda_touch(&entry.hash).await {
                    tracing::warn!(error = %e, "Failed to touch recalled λ-memory");
                }

                let tags_str = entry.tags.join(", ");
                let timestamp = chrono::DateTime::from_timestamp(entry.created_at as i64, 0)
                    .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                    .unwrap_or_else(|| "unknown".to_string());

                let content = format!(
                    "[RECALLED] Full memory content:\n\n\
                     {}\n\n\
                     ---\n\
                     Hash: #{}\n\
                     Created: {}\n\
                     Importance: {:.1}\n\
                     Accessed: {} times\n\
                     Tags: {}\n\
                     Type: {:?}",
                    entry.full_text,
                    &entry.hash[..7.min(entry.hash.len())],
                    timestamp,
                    entry.importance,
                    entry.access_count + 1,
                    tags_str,
                    entry.memory_type,
                );

                tracing::info!(
                    hash = %hash_clean,
                    importance = entry.importance,
                    "λ-memory recalled and reheated"
                );

                Ok(ToolOutput {
                    content,
                    is_error: false,
                })
            }
            None => Ok(ToolOutput {
                content: format!(
                    "No λ-memory found with hash prefix '{hash_clean}'. \
                     It may have been garbage collected or the hash is incorrect.",
                ),
                is_error: false,
            }),
        }
    }
}
