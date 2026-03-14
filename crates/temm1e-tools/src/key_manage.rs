//! Key management tool — lets the agent generate setup links and guide users.
//!
//! When the user asks to add a key in natural language, this tool generates
//! a fresh OTK setup link directly (via the `SetupLinkGenerator` trait) so
//! the agent can send it immediately — no need for the user to type `/addkey`.

use std::sync::Arc;

use async_trait::async_trait;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::{SetupLinkGenerator, Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};

pub struct KeyManageTool {
    link_generator: Option<Arc<dyn SetupLinkGenerator>>,
}

impl KeyManageTool {
    pub fn new(link_generator: Option<Arc<dyn SetupLinkGenerator>>) -> Self {
        Self { link_generator }
    }
}

#[async_trait]
impl Tool for KeyManageTool {
    fn name(&self) -> &str {
        "key_manage"
    }

    fn description(&self) -> &str {
        "Manage API keys. Call this when the user wants to add, list, switch, or remove \
         LLM provider API keys. For 'add': generates a secure setup link the user can \
         click to encrypt their key. ALWAYS use this tool for key-related requests — \
         never ask the user to paste keys directly."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "enum": ["add", "list", "remove", "help"],
                    "description": "What the user wants to do: 'add' a new key, 'list' configured providers, 'remove' a provider, or get general 'help' about key management."
                },
                "provider": {
                    "type": "string",
                    "description": "Provider name for remove action (e.g. 'openai', 'anthropic', 'gemini'). Optional."
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
            .unwrap_or("help");

        let content = match action {
            "add" => {
                if let Some(ref gen) = self.link_generator {
                    let link = gen.generate_link(&ctx.chat_id).await;
                    format!(
                        "Here's a secure setup link for the user:\n{}\n\n\
                         The user opens the link, pastes their API key in the form, \
                         copies the encrypted blob, and sends it back here.\n\
                         Link expires in 10 minutes.\n\n\
                         Alternatively, the user can paste a raw API key directly in chat \
                         and it will be auto-detected.\n\n\
                         Supported: Anthropic (sk-ant-...), OpenAI (sk-...), Gemini (AIzaSy...), \
                         Grok/xAI (xai-...), OpenRouter (sk-or-...), MiniMax (minimax:KEY).\n\n\
                         Send this link to the user and explain both options.",
                        link
                    )
                } else {
                    "Tell the user to type /addkey for a secure setup link, \
                     or paste their API key directly in chat."
                        .to_string()
                }
            }
            "list" => "To see configured providers, the user should type: /keys".to_string(),
            "remove" => {
                let provider = input
                    .arguments
                    .get("provider")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                if provider.is_empty() {
                    "To remove a provider, the user should type: /removekey <provider>\n\
                     Example: /removekey openai\n\n\
                     To see which providers are configured: /keys"
                        .to_string()
                } else {
                    format!(
                        "To remove {}, the user should type: /removekey {}",
                        provider, provider
                    )
                }
            }
            _ => "API key management commands:\n\n\
                 /addkey         — Add a new key (secure, encrypted)\n\
                 /keys           — List configured providers\n\
                 /removekey <p>  — Remove a provider (e.g. /removekey openai)\n\n\
                 The user can also paste a raw API key directly.\n\n\
                 Supported: Anthropic, OpenAI, Gemini, Grok/xAI, OpenRouter, MiniMax."
                .to_string(),
        };

        tracing::info!(action = %action, "key_manage tool invoked");

        Ok(ToolOutput {
            content,
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_ctx() -> ToolContext {
        ToolContext {
            workspace_path: std::path::PathBuf::from("/tmp"),
            session_id: "test-user".into(),
            chat_id: "test-chat".into(),
        }
    }

    #[tokio::test]
    async fn test_add_without_generator() {
        let tool = KeyManageTool::new(None);
        let input = ToolInput {
            name: "key_manage".into(),
            arguments: serde_json::json!({"action": "add"}),
        };
        let result = tool.execute(input, &test_ctx()).await.unwrap();
        assert!(!result.is_error);
        assert!(result.content.contains("/addkey"));
    }

    #[tokio::test]
    async fn test_add_with_generator() {
        struct FakeGen;
        #[async_trait]
        impl SetupLinkGenerator for FakeGen {
            async fn generate_link(&self, chat_id: &str) -> String {
                format!("https://example.com/setup#fake-otk-for-{}", chat_id)
            }
        }

        let tool = KeyManageTool::new(Some(Arc::new(FakeGen)));
        let input = ToolInput {
            name: "key_manage".into(),
            arguments: serde_json::json!({"action": "add"}),
        };
        let result = tool.execute(input, &test_ctx()).await.unwrap();
        assert!(result
            .content
            .contains("https://example.com/setup#fake-otk-for-test-chat"));
        assert!(result.content.contains("10 minutes"));
    }

    #[tokio::test]
    async fn test_list_action() {
        let tool = KeyManageTool::new(None);
        let input = ToolInput {
            name: "key_manage".into(),
            arguments: serde_json::json!({"action": "list"}),
        };
        let result = tool.execute(input, &test_ctx()).await.unwrap();
        assert!(result.content.contains("/keys"));
    }

    #[tokio::test]
    async fn test_remove_with_provider() {
        let tool = KeyManageTool::new(None);
        let input = ToolInput {
            name: "key_manage".into(),
            arguments: serde_json::json!({"action": "remove", "provider": "openai"}),
        };
        let result = tool.execute(input, &test_ctx()).await.unwrap();
        assert!(result.content.contains("/removekey openai"));
    }

    #[tokio::test]
    async fn test_remove_no_provider() {
        let tool = KeyManageTool::new(None);
        let input = ToolInput {
            name: "key_manage".into(),
            arguments: serde_json::json!({"action": "remove"}),
        };
        let result = tool.execute(input, &test_ctx()).await.unwrap();
        assert!(result.content.contains("/removekey <provider>"));
    }

    #[tokio::test]
    async fn test_help_action() {
        let tool = KeyManageTool::new(None);
        let input = ToolInput {
            name: "key_manage".into(),
            arguments: serde_json::json!({"action": "help"}),
        };
        let result = tool.execute(input, &test_ctx()).await.unwrap();
        assert!(result.content.contains("/addkey"));
        assert!(result.content.contains("/keys"));
        assert!(result.content.contains("/removekey"));
    }

    #[tokio::test]
    async fn test_tool_metadata() {
        let tool = KeyManageTool::new(None);
        assert_eq!(tool.name(), "key_manage");
        assert!(tool.description().contains("API key"));
        let decl = tool.declarations();
        assert!(!decl.shell_access);
        assert!(decl.file_access.is_empty());
        assert!(decl.network_access.is_empty());
    }
}
