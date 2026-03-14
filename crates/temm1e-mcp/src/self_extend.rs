//! SelfExtendTool — agent tool for discovering MCP servers by capability.
//!
//! When the agent needs a capability it doesn't have, it calls this tool
//! with a query describing what it needs. The tool searches a built-in
//! registry of known MCP servers and returns matching candidates with
//! their install commands.

use async_trait::async_trait;
use temm1e_core::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};

/// A known MCP server in the built-in registry.
struct McpServerEntry {
    name: &'static str,
    description: &'static str,
    command: &'static str,
    args: &'static [&'static str],
    keywords: &'static [&'static str],
    env_vars: &'static [&'static str],
}

/// Built-in registry of popular, verified MCP servers.
const REGISTRY: &[McpServerEntry] = &[
    McpServerEntry {
        name: "playwright",
        description: "Browser automation — navigate, click, type, screenshot, extract text from web pages",
        command: "npx",
        args: &["@playwright/mcp@latest"],
        keywords: &["browser", "web", "automation", "navigate", "click", "screenshot", "scrape", "playwright", "webpage", "website", "browse", "html"],
        env_vars: &[],
    },
    McpServerEntry {
        name: "puppeteer",
        description: "Browser automation via Puppeteer — headless Chrome control, screenshots, PDF generation",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-puppeteer"],
        keywords: &["browser", "puppeteer", "chrome", "headless", "pdf", "screenshot", "web"],
        env_vars: &[],
    },
    McpServerEntry {
        name: "filesystem",
        description: "Sandboxed file system access — read, write, search, move files within allowed directories",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-filesystem", "/tmp"],
        keywords: &["file", "filesystem", "directory", "read", "write", "folder", "disk", "storage", "files"],
        env_vars: &[],
    },
    McpServerEntry {
        name: "postgres",
        description: "PostgreSQL database — run SQL queries, inspect schema, manage tables",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-postgres"],
        keywords: &["postgres", "postgresql", "database", "sql", "query", "table", "schema", "db", "relational"],
        env_vars: &["DATABASE_URL"],
    },
    McpServerEntry {
        name: "sqlite",
        description: "SQLite database — run SQL queries on local .db files",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-sqlite"],
        keywords: &["sqlite", "database", "sql", "query", "db", "local"],
        env_vars: &[],
    },
    McpServerEntry {
        name: "github",
        description: "GitHub API — repos, issues, PRs, code search, file contents, commits",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-github"],
        keywords: &["github", "git", "repo", "repository", "issue", "pull request", "pr", "code", "commit", "branch"],
        env_vars: &["GITHUB_TOKEN"],
    },
    McpServerEntry {
        name: "brave-search",
        description: "Web search via Brave Search API — search the internet for current information",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-brave-search"],
        keywords: &["search", "web search", "internet", "brave", "find", "lookup", "current", "news", "research"],
        env_vars: &["BRAVE_API_KEY"],
    },
    McpServerEntry {
        name: "memory",
        description: "Knowledge graph memory — store and retrieve structured knowledge as entities and relations",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-memory"],
        keywords: &["memory", "knowledge", "graph", "remember", "store", "entity", "relation", "knowledge base"],
        env_vars: &[],
    },
    McpServerEntry {
        name: "fetch",
        description: "HTTP fetch — make HTTP requests, download web pages, call REST APIs",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-fetch"],
        keywords: &["fetch", "http", "api", "rest", "download", "request", "url", "get", "post", "web"],
        env_vars: &[],
    },
    McpServerEntry {
        name: "slack",
        description: "Slack integration — send messages, read channels, manage conversations",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-slack"],
        keywords: &["slack", "chat", "message", "channel", "team", "communication"],
        env_vars: &["SLACK_TOKEN"],
    },
    McpServerEntry {
        name: "redis",
        description: "Redis — key-value store operations, caching, pub/sub",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-redis"],
        keywords: &["redis", "cache", "key-value", "kv", "store", "pubsub"],
        env_vars: &["REDIS_URL"],
    },
    McpServerEntry {
        name: "sequential-thinking",
        description: "Structured reasoning — break down complex problems step by step with revision support",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-sequential-thinking"],
        keywords: &["thinking", "reasoning", "analysis", "step by step", "problem solving", "logic", "chain of thought"],
        env_vars: &[],
    },
    McpServerEntry {
        name: "google-maps",
        description: "Google Maps — geocoding, directions, places search, distance calculations",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-google-maps"],
        keywords: &["map", "maps", "google maps", "location", "directions", "geocode", "places", "distance", "navigation"],
        env_vars: &["GOOGLE_MAPS_API_KEY"],
    },
    McpServerEntry {
        name: "everart",
        description: "AI image generation — create images from text prompts",
        command: "npx",
        args: &["-y", "@modelcontextprotocol/server-everart"],
        keywords: &["image", "generate", "art", "picture", "illustration", "draw", "create image", "ai art"],
        env_vars: &["EVERART_API_KEY"],
    },
];

/// Agent tool for discovering MCP servers by capability.
pub struct SelfExtendTool;

impl SelfExtendTool {
    pub fn new() -> Self {
        Self
    }

    /// Search the registry for servers matching the query.
    fn search(&self, query: &str) -> Vec<&'static McpServerEntry> {
        let query_lower = query.to_lowercase();
        let query_words: Vec<&str> = query_lower.split_whitespace().collect();

        let mut scored: Vec<(&McpServerEntry, usize)> = REGISTRY
            .iter()
            .filter_map(|entry| {
                let mut score = 0usize;

                // Check keywords
                for keyword in entry.keywords {
                    let kw_lower = keyword.to_lowercase();
                    for word in &query_words {
                        if kw_lower.contains(word) || word.contains(&kw_lower) {
                            score += 2;
                        }
                    }
                    // Exact keyword match in query
                    if query_lower.contains(&kw_lower) {
                        score += 3;
                    }
                }

                // Check name
                if query_lower.contains(entry.name) {
                    score += 5;
                }

                // Check description
                for word in &query_words {
                    if word.len() >= 3 && entry.description.to_lowercase().contains(word) {
                        score += 1;
                    }
                }

                if score > 0 {
                    Some((entry, score))
                } else {
                    None
                }
            })
            .collect();

        scored.sort_by(|a, b| b.1.cmp(&a.1));
        scored.into_iter().map(|(entry, _)| entry).collect()
    }
}

impl Default for SelfExtendTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for SelfExtendTool {
    fn name(&self) -> &str {
        "self_extend_tool"
    }

    fn description(&self) -> &str {
        "Search for MCP servers that provide capabilities you need. \
         Describe what you need (e.g., 'browser automation', 'database queries', \
         'web search') and get matching MCP servers with install commands. \
         Use self_add_mcp to install the one you want."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "query": {
                    "type": "string",
                    "description": "What capability you need (e.g., 'browse websites', 'search the web', 'query postgres database', 'generate images')"
                }
            },
            "required": ["query"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: vec![],
            network_access: vec![],
            shell_access: false,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, temm1e_core::types::error::Temm1eError> {
        let query = input
            .arguments
            .get("query")
            .and_then(|v| v.as_str())
            .unwrap_or("");

        if query.is_empty() {
            return Ok(ToolOutput {
                content: "Please provide a query describing what capability you need.".to_string(),
                is_error: true,
            });
        }

        let matches = self.search(query);

        if matches.is_empty() {
            return Ok(ToolOutput {
                content: format!(
                    "No MCP servers found matching '{}'. \
                     Try a broader query, or search npm with shell: \
                     npm search mcp-server-<topic>",
                    query
                ),
                is_error: false,
            });
        }

        let mut result = format!(
            "Found {} MCP server(s) matching '{}':\n\n",
            matches.len(),
            query
        );

        for (i, entry) in matches.iter().enumerate().take(5) {
            result.push_str(&format!("{}. **{}**\n", i + 1, entry.name));
            result.push_str(&format!("   {}\n", entry.description));
            result.push_str(&format!(
                "   Install: self_add_mcp(name=\"{}\", command=\"{}\", args={:?})\n",
                entry.name, entry.command, entry.args
            ));
            if !entry.env_vars.is_empty() {
                result.push_str(&format!("   Requires env: {}\n", entry.env_vars.join(", ")));
            }
            result.push('\n');
        }

        if matches.len() > 5 {
            result.push_str(&format!(
                "... and {} more. Refine your query to narrow results.\n",
                matches.len() - 5
            ));
        }

        Ok(ToolOutput {
            content: result,
            is_error: false,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn search_browser() {
        let tool = SelfExtendTool::new();
        let results = tool.search("browse the web");
        assert!(!results.is_empty());
        assert!(results[0].name == "playwright" || results[0].name == "puppeteer");
    }

    #[test]
    fn search_database() {
        let tool = SelfExtendTool::new();
        let results = tool.search("postgres database");
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "postgres");
    }

    #[test]
    fn search_no_match() {
        let tool = SelfExtendTool::new();
        let results = tool.search("quantum teleportation");
        assert!(results.is_empty());
    }

    #[test]
    fn search_by_name() {
        let tool = SelfExtendTool::new();
        let results = tool.search("playwright");
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "playwright");
    }

    #[test]
    fn search_slack() {
        let tool = SelfExtendTool::new();
        let results = tool.search("send slack messages");
        assert!(!results.is_empty());
        assert_eq!(results[0].name, "slack");
    }

    #[tokio::test]
    async fn execute_empty_query() {
        let tool = SelfExtendTool::new();
        let ctx = ToolContext {
            workspace_path: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            chat_id: "test".to_string(),
        };
        let input = ToolInput {
            name: "self_extend_tool".to_string(),
            arguments: serde_json::json!({"query": ""}),
        };
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(output.is_error);
    }

    #[tokio::test]
    async fn execute_valid_query() {
        let tool = SelfExtendTool::new();
        let ctx = ToolContext {
            workspace_path: std::path::PathBuf::from("/tmp"),
            session_id: "test".to_string(),
            chat_id: "test".to_string(),
        };
        let input = ToolInput {
            name: "self_extend_tool".to_string(),
            arguments: serde_json::json!({"query": "web search"}),
        };
        let output = tool.execute(input, &ctx).await.unwrap();
        assert!(!output.is_error);
        assert!(output.content.contains("self_add_mcp"));
    }

    #[test]
    fn parameters_schema_valid() {
        let tool = SelfExtendTool::new();
        let schema = tool.parameters_schema();
        assert_eq!(schema["type"], "object");
        assert!(schema["properties"]["query"].is_object());
    }
}
