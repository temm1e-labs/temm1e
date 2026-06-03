//! Engram tool — natural-language permanent memory.
//!
//! The agent calls this in-loop (when the user says "remember…") and at
//! end-of-loop (to persist a durable learning). Permanence is curated by the
//! Engram scoring engine + the post-run curator; this tool is the write path.
//!
//! Actions:
//! - `remember` — store a durable fact (upsert; a `subject_key` supersedes an
//!   earlier fact with the same key). User-stated facts pin (`pinned=user`,
//!   sacrosanct); agent self-notes use `pinned=agent` (auto-demotable).
//! - `recall`   — search permanent facts visible in this scope.
//! - `forget`   — delete the best-matching fact for a query.
//!
//! Scopes (v1): `global` (default) and `chat`. `user` scope requires a user id
//! in `ToolContext` (tracked follow-up); the data model already supports it.

use std::hash::{Hash, Hasher};
use std::sync::Arc;

use async_trait::async_trait;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{
    EngramFact, FactType, Memory, MemoryScope, PinnedBy, Tool, ToolContext, ToolDeclarations,
    ToolInput, ToolOutput,
};

pub struct EngramTool {
    memory: Arc<dyn Memory>,
}

impl EngramTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }

    fn now() -> u64 {
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0)
    }

    fn parse_scope(scope: &str, ctx: &ToolContext) -> MemoryScope {
        match scope {
            "chat" => MemoryScope::Chat(ctx.chat_id.clone()),
            // "user" needs a user id not yet in ToolContext — v1 falls back to global.
            _ => MemoryScope::Global,
        }
    }

    fn parse_type(t: &str) -> FactType {
        match t {
            "identity" => FactType::Identity,
            "preference" => FactType::Preference,
            "project" => FactType::Project,
            "constraint" => FactType::Constraint,
            _ => FactType::Reference,
        }
    }

    /// Stable id: derived from scope + subject_key (so a newer fact with the
    /// same subject supersedes via upsert), or from scope + content otherwise.
    /// `DefaultHasher` uses fixed keys, so ids are stable across runs/sessions.
    fn make_id(scope: &MemoryScope, subject_key: Option<&str>, content: &str) -> String {
        let basis = match subject_key {
            Some(s) => format!("{}|subj|{s}", scope.as_key()),
            None => format!("{}|body|{content}", scope.as_key()),
        };
        let mut h = std::collections::hash_map::DefaultHasher::new();
        basis.hash(&mut h);
        format!("eg{:016x}", h.finish())
    }

    async fn handle_remember(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let content = input
            .get("content")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: content".into()))?;
        let scope_s = input
            .get("scope")
            .and_then(|v| v.as_str())
            .unwrap_or("global");
        let type_s = input
            .get("type")
            .and_then(|v| v.as_str())
            .unwrap_or("reference");
        let subject_key = input.get("subject_key").and_then(|v| v.as_str());
        let pinned = match input.get("pinned").and_then(|v| v.as_str()) {
            Some("agent") => PinnedBy::Agent,
            _ => PinnedBy::User,
        };
        let importance = input
            .get("importance")
            .and_then(|v| v.as_f64())
            .map(|x| x as f32)
            .unwrap_or(if pinned == PinnedBy::User { 5.0 } else { 4.0 })
            .clamp(0.0, 5.0);
        let tags: Vec<String> = input
            .get("tags")
            .and_then(|v| v.as_array())
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(String::from))
                    .collect()
            })
            .unwrap_or_default();

        let scope = Self::parse_scope(scope_s, ctx);
        let id = Self::make_id(&scope, subject_key, content);
        let now = Self::now();
        let summary: String = content.lines().next().unwrap_or(content).chars().take(160).collect();
        let essence: String = summary
            .split_whitespace()
            .take(6)
            .collect::<Vec<_>>()
            .join(" ");

        let fact = EngramFact {
            id: id.clone(),
            content: content.to_string(),
            summary,
            essence,
            fact_type: Self::parse_type(type_s),
            scope,
            pinned_by: pinned,
            subject_key: subject_key.map(String::from),
            importance,
            created_at: now,
            last_accessed: now,
            tags,
            links: Vec::new(),
        };
        self.memory.engram_store(fact).await?;
        tracing::info!(id = %id, scope = %scope_s, "Engram fact remembered");

        Ok(ToolOutput {
            content: format!(
                "Remembered permanently (scope: {scope_s}): \"{content}\". \
                 Briefly let the user know you'll remember this.",
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
        // v1: tool sees global + chat scopes (user_id not available here).
        let facts = self
            .memory
            .engram_recall(query, "", &ctx.chat_id, 20)
            .await?;
        if facts.is_empty() {
            return Ok(ToolOutput {
                content: "No permanent facts found matching that.".to_string(),
                is_error: false,
            });
        }
        let mut out = format!("Found {} permanent fact(s):\n", facts.len());
        for f in &facts {
            out.push_str(&format!("\n  - {}", f.content));
        }
        Ok(ToolOutput {
            content: out,
            is_error: false,
        })
    }

    async fn handle_forget(
        &self,
        input: &serde_json::Value,
        ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let query = input
            .get("query")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: query".into()))?;
        let matches = self.memory.engram_recall(query, "", &ctx.chat_id, 1).await?;
        match matches.into_iter().next() {
            Some(f) => {
                self.memory.engram_forget(&f.id).await?;
                tracing::info!(id = %f.id, "Engram fact forgotten");
                Ok(ToolOutput {
                    content: format!(
                        "Forgotten: \"{}\". Let the user know you removed it.",
                        f.content
                    ),
                    is_error: false,
                })
            }
            None => Ok(ToolOutput {
                content: format!("No permanent fact found matching \"{query}\". Nothing removed."),
                is_error: false,
            }),
        }
    }
}

#[async_trait]
impl Tool for EngramTool {
    fn name(&self) -> &str {
        "engram"
    }

    fn description(&self) -> &str {
        "Your permanent long-term memory for durable facts about the user and project \
         (identity, preferences, standing constraints, project details). Use 'remember' \
         when the user says to remember something OR when you learn a durable fact worth \
         keeping across sessions; reuse the same 'subject_key' to update/correct a fact. \
         'recall' to look one up, 'forget' to remove. Permanent facts are always shown to \
         you automatically — only remember things that stay true over time, not one-off details."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["remember", "recall", "forget"],
                    "description": "The permanent-memory operation."
                },
                "content": {
                    "type": "string",
                    "description": "The durable fact to remember (required for 'remember')."
                },
                "query": {
                    "type": "string",
                    "description": "Search text (required for 'recall' and 'forget')."
                },
                "type": {
                    "type": "string",
                    "enum": ["identity", "preference", "project", "constraint", "reference"],
                    "description": "Category of the fact (default 'reference')."
                },
                "subject_key": {
                    "type": "string",
                    "description": "Stable key (e.g. 'pref:gpu-provider'); reuse it to update/supersede an earlier fact."
                },
                "scope": {
                    "type": "string",
                    "enum": ["global", "chat"],
                    "description": "'global' (default) persists everywhere; 'chat' is limited to this conversation."
                },
                "pinned": {
                    "type": "string",
                    "enum": ["user", "agent"],
                    "description": "'user' (default) = the user asked, never auto-forgotten; 'agent' = your own note, may fade if unused."
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
        tracing::info!(action = %action, "Executing engram tool");
        match action {
            "remember" => self.handle_remember(&input.arguments, ctx).await,
            "recall" => self.handle_recall(&input.arguments, ctx).await,
            "forget" => self.handle_forget(&input.arguments, ctx).await,
            _ => Ok(ToolOutput {
                content: format!(
                    "Unknown action '{action}'. Valid actions: remember, recall, forget"
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

    fn ctx() -> ToolContext {
        ToolContext {
            workspace_path: PathBuf::from("/tmp/test"),
            session_id: "tg-123".to_string(),
            chat_id: "chat-1".to_string(),
            read_tracker: None,
        }
    }

    fn input(args: serde_json::Value) -> ToolInput {
        ToolInput {
            name: "engram".to_string(),
            arguments: args,
        }
    }

    #[tokio::test]
    async fn remember_requires_content() {
        let tool = EngramTool::new(Arc::new(MockMemory::new()));
        let r = tool
            .execute(input(serde_json::json!({"action": "remember"})), &ctx())
            .await;
        assert!(r.is_err());
    }

    #[tokio::test]
    async fn remember_succeeds_and_reports() {
        let tool = EngramTool::new(Arc::new(MockMemory::new()));
        let out = tool
            .execute(
                input(serde_json::json!({
                    "action": "remember",
                    "content": "User's birthday is 1994-03-02",
                    "type": "identity"
                })),
                &ctx(),
            )
            .await
            .unwrap();
        assert!(!out.is_error);
        assert!(out.content.contains("Remembered permanently"));
    }

    #[tokio::test]
    async fn unknown_action_is_error() {
        let tool = EngramTool::new(Arc::new(MockMemory::new()));
        let out = tool
            .execute(input(serde_json::json!({"action": "wat"})), &ctx())
            .await
            .unwrap();
        assert!(out.is_error);
    }

    #[test]
    fn make_id_is_stable_and_subject_keyed() {
        let s = MemoryScope::Global;
        // same subject -> same id (supersession via upsert)
        assert_eq!(
            EngramTool::make_id(&s, Some("pref:gpu"), "Lambda"),
            EngramTool::make_id(&s, Some("pref:gpu"), "RunPod")
        );
        // different content, no subject -> different id
        assert_ne!(
            EngramTool::make_id(&s, None, "a"),
            EngramTool::make_id(&s, None, "b")
        );
    }
}
