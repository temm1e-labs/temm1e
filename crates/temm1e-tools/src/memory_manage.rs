//! Memory management tool — CRUD for persistent agent knowledge.
//!
//! Provides five actions:
//! - `remember` — save a new knowledge entry (key + content + optional tags)
//! - `recall`   — search/retrieve knowledge entries (by key, tags, or free-text)
//! - `forget`   — delete a knowledge entry by key
//! - `update`   — update an existing knowledge entry's content
//! - `list`     — list all knowledge entries (optionally filtered by tags)
//!
//! Entries are stored via the Memory trait with `MemoryEntryType::Knowledge` and
//! key prefix `knowledge:`. Scoping is controlled by key prefixes:
//! - `global:` — shared across all users and chats
//! - `user:{id}:` — scoped to a specific user
//! - `chat:{id}:` — scoped to a specific chat

use std::sync::Arc;

use async_trait::async_trait;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{
    Memory, MemoryEntry, MemoryEntryType, SearchOpts, Tool, ToolContext, ToolDeclarations,
    ToolInput, ToolOutput,
};

pub struct MemoryManageTool {
    memory: Arc<dyn Memory>,
}

impl MemoryManageTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }

    /// Extract tags from a memory entry's metadata as a comma-separated string.
    fn extract_tags(metadata: &serde_json::Value) -> String {
        metadata
            .get("tags")
            .and_then(|v: &serde_json::Value| v.as_array())
            .map(|arr: &Vec<serde_json::Value>| {
                arr.iter()
                    .filter_map(|v: &serde_json::Value| v.as_str())
                    .collect::<Vec<_>>()
                    .join(", ")
            })
            .unwrap_or_default()
    }

    /// Extract tags from metadata as a Vec<String>.
    fn extract_tags_vec(metadata: &serde_json::Value) -> Vec<String> {
        metadata
            .get("tags")
            .and_then(|v: &serde_json::Value| v.as_array())
            .map(|arr: &Vec<serde_json::Value>| {
                arr.iter()
                    .filter_map(|v: &serde_json::Value| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Build the full storage key: `knowledge:{scope}:{user_key}`
    fn build_key(scope: &str, user_key: &str, ctx: &ToolContext) -> String {
        let scope_prefix = match scope {
            "user" => format!("user:{}:", ctx.session_id),
            "chat" => format!("chat:{}:", ctx.chat_id),
            _ => "global:".to_string(),
        };
        format!("knowledge:{}{}", scope_prefix, user_key)
    }

    async fn handle_remember(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let key = input
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: key".into()))?;
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: content".into()))?;
        let scope = input
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global");
        let tags: Vec<String> = input
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|arr| {
                arr.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();

        let full_key = Self::build_key(scope, key, ctx);

        // Check if key already exists
        if let Ok(Some(_)) = self.memory.get(&full_key).await {
            return Ok(ToolOutput {
                content: format!(
                    "A knowledge entry with key '{}' already exists (scope: {}). \
                     Use 'update' action to modify it, or 'forget' to delete it first.",
                    key, scope
                ),
                is_error: true,
            });
        }

        let metadata = serde_json::json!({
            "user_key": key,
            "scope": scope,
            "tags": tags,
        });

        let entry = MemoryEntry {
            id: full_key.clone(),
            content: content.to_string(),
            metadata,
            timestamp: chrono::Utc::now(),
            session_id: Some(ctx.session_id.clone()),
            entry_type: MemoryEntryType::Knowledge,
        };

        self.memory.store(entry).await?;

        tracing::info!(key = %key, scope = %scope, "Knowledge entry saved");

        Ok(ToolOutput {
            content: format!(
                "Remembered: saved knowledge entry '{}' (scope: {}). \
                 IMPORTANT: You MUST now inform the user that you saved this memory. \
                 Tell them what you remembered and under what key.",
                key, scope
            ),
            is_error: false,
        })
    }

    async fn handle_recall(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let query = input.get("query").and_then(|v| v.as_str()).unwrap_or("");
        let key = input.get("key").and_then(|v| v.as_str());
        let scope = input
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global");
        let tags: Vec<String> = Self::extract_tags_vec(input);

        tracing::debug!(
            query = %query,
            key = ?key,
            scope = %scope,
            "Recall request"
        );

        // Direct key lookup — try the given scope, then fall back to other scopes
        if let Some(k) = key {
            let full_key = Self::build_key(scope, k, ctx);
            if let Ok(Some(entry)) = self.memory.get(&full_key).await {
                let entry_tags = Self::extract_tags(&entry.metadata);
                return Ok(ToolOutput {
                    content: format!(
                        "Found knowledge entry:\n  Key: {}\n  Scope: {}\n  Tags: [{}]\n  Content: {}",
                        k, scope, entry_tags, entry.content
                    ),
                    is_error: false,
                });
            }
            // Try other scopes as fallback
            let other_scopes = ["global", "user", "chat"];
            for alt_scope in other_scopes {
                if alt_scope == scope {
                    continue;
                }
                let alt_key = Self::build_key(alt_scope, k, ctx);
                if let Ok(Some(entry)) = self.memory.get(&alt_key).await {
                    let entry_tags = Self::extract_tags(&entry.metadata);
                    return Ok(ToolOutput {
                        content: format!(
                            "Found knowledge entry:\n  Key: {}\n  Scope: {}\n  Tags: [{}]\n  Content: {}",
                            k, alt_scope, entry_tags, entry.content
                        ),
                        is_error: false,
                    });
                }
            }
            // Key not found in any scope — fall through to search instead of failing
        }

        // Search by query or tags — rely on entry_type_filter for scoping
        let search_query = if !query.is_empty() {
            query.to_string()
        } else if !tags.is_empty() {
            tags.join(" ")
        } else {
            String::new()
        };

        let opts = SearchOpts {
            limit: 10,
            entry_type_filter: Some(MemoryEntryType::Knowledge),
            ..Default::default()
        };

        let entries = self.memory.search(&search_query, opts).await?;

        if entries.is_empty() {
            return Ok(ToolOutput {
                content: "No knowledge entries found matching your query.".to_string(),
                is_error: false,
            });
        }

        // Build scope prefix for filtering
        let scope_prefix = match scope {
            "user" => format!("knowledge:user:{}:", ctx.session_id),
            "chat" => format!("knowledge:chat:{}:", ctx.chat_id),
            _ => "knowledge:global:".to_string(),
        };

        // Filter by scope and tags
        let results: Vec<&MemoryEntry> = entries
            .iter()
            .filter(|e| e.id.starts_with(&scope_prefix))
            .filter(|e| {
                if tags.is_empty() {
                    return true;
                }
                let entry_tags = Self::extract_tags_vec(&e.metadata);
                tags.iter().any(|t| entry_tags.contains(t))
            })
            .collect();

        if results.is_empty() {
            return Ok(ToolOutput {
                content: format!(
                    "No knowledge entries found matching your query (scope: {}).",
                    scope
                ),
                is_error: false,
            });
        }

        let mut output = format!("Found {} knowledge entries:\n", results.len());
        for entry in results {
            let user_key = entry
                .metadata
                .get("user_key")
                .and_then(|v: &serde_json::Value| v.as_str())
                .unwrap_or(&entry.id);
            let entry_tags = Self::extract_tags(&entry.metadata);
            output.push_str(&format!(
                "\n  Key: {}\n  Tags: [{}]\n  Content: {}\n",
                user_key, entry_tags, entry.content
            ));
        }

        Ok(ToolOutput {
            content: output,
            is_error: false,
        })
    }

    async fn handle_forget(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let key = input
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: key".into()))?;
        let scope = input
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global");

        let full_key = Self::build_key(scope, key, ctx);

        // Check if entry exists before deleting
        if let Ok(None) = self.memory.get(&full_key).await {
            return Ok(ToolOutput {
                content: format!(
                    "No knowledge entry found with key '{}' (scope: {}). Nothing to delete.",
                    key, scope
                ),
                is_error: true,
            });
        }

        self.memory.delete(&full_key).await?;

        tracing::info!(key = %key, scope = %scope, "Knowledge entry deleted");

        Ok(ToolOutput {
            content: format!(
                "Forgotten: deleted knowledge entry '{}' (scope: {}). \
                 IMPORTANT: You MUST now inform the user that you deleted this memory. \
                 Tell them what you forgot.",
                key, scope
            ),
            is_error: false,
        })
    }

    async fn handle_update(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let key = input
            .get("key")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: key".into()))?;
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: content".into()))?;
        let scope = input
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global");

        let full_key = Self::build_key(scope, key, ctx);

        // Get existing entry to preserve tags
        let existing = match self.memory.get(&full_key).await? {
            Some(e) => e,
            None => {
                return Ok(ToolOutput {
                    content: format!(
                        "No knowledge entry found with key '{}' (scope: {}). \
                         Use 'remember' action to create it first.",
                        key, scope
                    ),
                    is_error: true,
                });
            }
        };

        // Allow updating tags if provided, otherwise keep existing
        let tags: serde_json::Value = input
            .get("tags")
            .and_then(|v: &serde_json::Value| v.as_array())
            .map(|arr: &Vec<serde_json::Value>| {
                serde_json::Value::Array(
                    arr.iter()
                        .filter_map(|v: &serde_json::Value| {
                            v.as_str().map(|s| serde_json::Value::String(s.to_string()))
                        })
                        .collect(),
                )
            })
            .unwrap_or_else(|| {
                existing
                    .metadata
                    .get("tags")
                    .cloned()
                    .unwrap_or(serde_json::Value::Array(vec![]))
            });

        let metadata = serde_json::json!({
            "user_key": key,
            "scope": scope,
            "tags": tags,
        });

        // Delete old entry, store new one
        self.memory.delete(&full_key).await?;

        let entry = MemoryEntry {
            id: full_key,
            content: content.to_string(),
            metadata,
            timestamp: chrono::Utc::now(),
            session_id: Some(ctx.session_id.clone()),
            entry_type: MemoryEntryType::Knowledge,
        };

        self.memory.store(entry).await?;

        tracing::info!(key = %key, scope = %scope, "Knowledge entry updated");

        Ok(ToolOutput {
            content: format!(
                "Updated: knowledge entry '{}' (scope: {}) now contains new content. \
                 IMPORTANT: You MUST now inform the user that you updated this memory. \
                 Tell them what was changed.",
                key, scope
            ),
            is_error: false,
        })
    }

    async fn handle_list(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let scope = input
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global");
        let tags: Vec<String> = Self::extract_tags_vec(input);

        let scope_prefix = match scope {
            "user" => format!("knowledge:user:{}:", ctx.session_id),
            "chat" => format!("knowledge:chat:{}:", ctx.chat_id),
            _ => "knowledge:global:".to_string(),
        };

        // Search broadly for all knowledge entries
        let opts = SearchOpts {
            limit: 50,
            entry_type_filter: Some(MemoryEntryType::Knowledge),
            ..Default::default()
        };

        let entries = self.memory.search("", opts).await?;

        // Filter by scope prefix and tags
        let results: Vec<&MemoryEntry> = entries
            .iter()
            .filter(|e| e.id.starts_with(&scope_prefix))
            .filter(|e| {
                if tags.is_empty() {
                    return true;
                }
                let entry_tags = Self::extract_tags_vec(&e.metadata);
                tags.iter().any(|t| entry_tags.contains(t))
            })
            .collect();

        if results.is_empty() {
            let tag_note = if !tags.is_empty() {
                format!(" with tags [{}]", tags.join(", "))
            } else {
                String::new()
            };
            return Ok(ToolOutput {
                content: format!("No knowledge entries found (scope: {}{}).", scope, tag_note),
                is_error: false,
            });
        }

        let mut output = format!(
            "Knowledge entries (scope: {}, {} found):\n",
            scope,
            results.len()
        );
        for entry in results {
            let user_key = entry
                .metadata
                .get("user_key")
                .and_then(|v: &serde_json::Value| v.as_str())
                .unwrap_or(&entry.id);
            let entry_tags = Self::extract_tags(&entry.metadata);
            let preview = if entry.content.len() > 100 {
                format!("{}...", &entry.content[..100])
            } else {
                entry.content.clone()
            };
            output.push_str(&format!("\n  - {} [{}]: {}", user_key, entry_tags, preview));
        }

        Ok(ToolOutput {
            content: output,
            is_error: false,
        })
    }
}

#[async_trait]
impl Tool for MemoryManageTool {
    fn name(&self) -> &str {
        "memory_manage"
    }

    fn description(&self) -> &str {
        "Manage your persistent knowledge store. Use this to remember important facts, \
         user preferences, project details, or anything you want to recall in future \
         conversations. Actions: 'remember' (save new), 'recall' (search/retrieve), \
         'forget' (delete), 'update' (modify existing), 'list' (show all). \
         After every remember/update/forget action, you MUST inform the user what you did."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["remember", "recall", "forget", "update", "list"],
                    "description": "The memory operation to perform"
                },
                "key": {
                    "type": "string",
                    "description": "Unique identifier for the knowledge entry. Required for remember, forget, update. Optional for recall (direct lookup)."
                },
                "content": {
                    "type": "string",
                    "description": "The knowledge content to store. Required for remember and update."
                },
                "query": {
                    "type": "string",
                    "description": "Free-text search query for recall action."
                },
                "tags": {
                    "type": "array",
                    "items": { "type": "string" },
                    "description": "Tags for categorization. Used for filtering in recall/list, and for organizing in remember/update."
                },
                "scope": {
                    "type": "string",
                    "enum": ["global", "user", "chat"],
                    "description": "Scope of the knowledge entry. 'global' (default) = shared across all conversations, 'user' = per-user, 'chat' = per-chat."
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

        tracing::info!(action = %action, "Executing memory_manage tool");

        match action {
            "remember" => self.handle_remember(&input.arguments, ctx).await,
            "recall" => self.handle_recall(&input.arguments, ctx).await,
            "forget" => self.handle_forget(&input.arguments, ctx).await,
            "update" => self.handle_update(&input.arguments, ctx).await,
            "list" => self.handle_list(&input.arguments, ctx).await,
            _ => Ok(ToolOutput {
                content: format!(
                    "Unknown action '{}'. Valid actions: remember, recall, forget, update, list",
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
    use temm1e_test_utils::MockMemory;

    fn test_ctx() -> ToolContext {
        ToolContext {
            workspace_path: PathBuf::from("/tmp/test"),
            session_id: "test-session-123".to_string(),
            chat_id: "chat-456".to_string(),
        }
    }

    fn make_input(args: serde_json::Value) -> ToolInput {
        ToolInput {
            name: "memory_manage".to_string(),
            arguments: args,
        }
    }

    #[tokio::test]
    async fn remember_stores_entry() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({
            "action": "remember",
            "key": "user_name",
            "content": "The user's name is Alice",
            "tags": ["personal", "identity"]
        }));

        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("Remembered"));
        assert!(output.content.contains("user_name"));
    }

    #[tokio::test]
    async fn remember_rejects_duplicate_key() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        // Store first entry
        let input = make_input(serde_json::json!({
            "action": "remember",
            "key": "fav_color",
            "content": "Blue"
        }));
        tool.execute(input, &ctx).await.unwrap();

        // Try to store duplicate
        let input2 = make_input(serde_json::json!({
            "action": "remember",
            "key": "fav_color",
            "content": "Red"
        }));
        let output = tool.execute(input2, &ctx).await.unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("already exists"));
    }

    #[tokio::test]
    async fn recall_by_key() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        // Store entry
        let input = make_input(serde_json::json!({
            "action": "remember",
            "key": "project",
            "content": "Working on TEMM1E v1.4",
            "tags": ["dev"]
        }));
        tool.execute(input, &ctx).await.unwrap();

        // Recall by key
        let recall = make_input(serde_json::json!({
            "action": "recall",
            "key": "project"
        }));
        let output = tool.execute(recall, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("TEMM1E v1.4"));
    }

    #[tokio::test]
    async fn recall_missing_key_returns_not_found() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        let recall = make_input(serde_json::json!({
            "action": "recall",
            "key": "nonexistent"
        }));
        let output = tool.execute(recall, &ctx).await.unwrap();
        assert!(!output.is_error);
        // Key not found in any scope, falls through to search which finds nothing
        assert!(output.content.contains("No knowledge entries found"));
    }

    #[tokio::test]
    async fn forget_deletes_entry() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        // Store entry
        let input = make_input(serde_json::json!({
            "action": "remember",
            "key": "temp_note",
            "content": "Delete me later"
        }));
        tool.execute(input, &ctx).await.unwrap();

        // Forget it
        let forget = make_input(serde_json::json!({
            "action": "forget",
            "key": "temp_note"
        }));
        let output = tool.execute(forget, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("Forgotten"));

        // Verify gone
        let recall = make_input(serde_json::json!({
            "action": "recall",
            "key": "temp_note"
        }));
        let output = tool.execute(recall, &ctx).await.unwrap();
        // Key not found in any scope after deletion, falls through to search
        assert!(output.content.contains("No knowledge entries found"));
    }

    #[tokio::test]
    async fn forget_nonexistent_returns_error() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        let forget = make_input(serde_json::json!({
            "action": "forget",
            "key": "ghost"
        }));
        let output = tool.execute(forget, &ctx).await.unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("Nothing to delete"));
    }

    #[tokio::test]
    async fn update_modifies_content() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        // Store entry
        let input = make_input(serde_json::json!({
            "action": "remember",
            "key": "status",
            "content": "In progress",
            "tags": ["project"]
        }));
        tool.execute(input, &ctx).await.unwrap();

        // Update it
        let update = make_input(serde_json::json!({
            "action": "update",
            "key": "status",
            "content": "Completed!"
        }));
        let output = tool.execute(update, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("Updated"));

        // Verify updated content
        let recall = make_input(serde_json::json!({
            "action": "recall",
            "key": "status"
        }));
        let output = tool.execute(recall, &ctx).await.unwrap();
        assert!(output.content.contains("Completed!"));
    }

    #[tokio::test]
    async fn update_nonexistent_returns_error() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        let update = make_input(serde_json::json!({
            "action": "update",
            "key": "nonexistent",
            "content": "New content"
        }));
        let output = tool.execute(update, &ctx).await.unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("Use 'remember'"));
    }

    #[tokio::test]
    async fn scope_isolation() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        // Store in global scope
        let input = make_input(serde_json::json!({
            "action": "remember",
            "key": "greeting",
            "content": "Hello globally",
            "scope": "global"
        }));
        tool.execute(input, &ctx).await.unwrap();

        // Store same key in chat scope
        let input2 = make_input(serde_json::json!({
            "action": "remember",
            "key": "greeting",
            "content": "Hello chat-specific",
            "scope": "chat"
        }));
        tool.execute(input2, &ctx).await.unwrap();

        // Recall from global
        let recall_global = make_input(serde_json::json!({
            "action": "recall",
            "key": "greeting",
            "scope": "global"
        }));
        let output = tool.execute(recall_global, &ctx).await.unwrap();
        assert!(output.content.contains("Hello globally"));

        // Recall from chat
        let recall_chat = make_input(serde_json::json!({
            "action": "recall",
            "key": "greeting",
            "scope": "chat"
        }));
        let output = tool.execute(recall_chat, &ctx).await.unwrap();
        assert!(output.content.contains("Hello chat-specific"));
    }

    #[tokio::test]
    async fn invalid_action_returns_error() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({
            "action": "invalid_action"
        }));
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(output.is_error);
        assert!(output.content.contains("Unknown action"));
    }

    #[tokio::test]
    async fn missing_action_returns_error() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({}));
        let result = tool.execute(input, &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn remember_missing_key_returns_error() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({
            "action": "remember",
            "content": "No key provided"
        }));
        let result = tool.execute(input, &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn remember_missing_content_returns_error() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory.clone());
        let ctx = test_ctx();

        let input = make_input(serde_json::json!({
            "action": "remember",
            "key": "test_key"
        }));
        let result = tool.execute(input, &ctx).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn tool_metadata() {
        let memory = Arc::new(MockMemory::new());
        let tool = MemoryManageTool::new(memory);

        assert_eq!(tool.name(), "memory_manage");
        assert!(tool.description().contains("persistent knowledge"));
        assert!(!tool.declarations().shell_access);

        let schema = tool.parameters_schema();
        let props = schema.get("properties").unwrap();
        assert!(props.get("action").is_some());
        assert!(props.get("key").is_some());
        assert!(props.get("content").is_some());
        assert!(props.get("query").is_some());
        assert!(props.get("tags").is_some());
        assert!(props.get("scope").is_some());
    }
}
