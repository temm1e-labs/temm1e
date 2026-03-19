use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine as _;
use clap::{Parser, Subcommand};
use futures::FutureExt;
use temm1e_core::config::credentials::{
    credentials_path, detect_api_key, is_placeholder_key, load_active_provider_keys,
    load_credentials_file, load_saved_credentials, save_credentials,
};
use temm1e_core::types::model_registry::{
    available_models_for_provider, default_model, is_vision_model,
};
use temm1e_core::Channel;
use tokio::sync::Mutex;

// ── Secret-censoring channel wrapper ──────────────────────
// Wraps any Channel to censor known API keys from outbound messages.
// This is the hardcoded last-line-of-defense filter — the system prompt
// tells the agent not to leak secrets, but this catches anything that slips.
struct SecretCensorChannel {
    inner: Arc<dyn Channel>,
}

#[async_trait]
impl Channel for SecretCensorChannel {
    fn name(&self) -> &str {
        self.inner.name()
    }
    async fn start(&mut self) -> std::result::Result<(), temm1e_core::types::error::Temm1eError> {
        Ok(())
    }
    async fn stop(&mut self) -> std::result::Result<(), temm1e_core::types::error::Temm1eError> {
        Ok(())
    }
    async fn send_message(
        &self,
        mut msg: temm1e_core::types::message::OutboundMessage,
    ) -> std::result::Result<(), temm1e_core::types::error::Temm1eError> {
        msg.text = censor_secrets(&msg.text);
        self.inner.send_message(msg).await
    }
    fn file_transfer(&self) -> Option<&dyn temm1e_core::FileTransfer> {
        self.inner.file_transfer()
    }
    fn is_allowed(&self, user_id: &str) -> bool {
        self.inner.is_allowed(user_id)
    }
    async fn delete_message(
        &self,
        chat_id: &str,
        message_id: &str,
    ) -> std::result::Result<(), temm1e_core::types::error::Temm1eError> {
        self.inner.delete_message(chat_id, message_id).await
    }
}

#[derive(Parser)]
#[command(name = "temm1e")]
#[command(about = "Cloud-native Rust AI agent runtime — Telegram-native")]
#[command(version = concat!(env!("CARGO_PKG_VERSION"), " — commit: ", env!("GIT_HASH"), " — date: ", env!("BUILD_DATE")))]
struct Cli {
    /// Path to config file
    #[arg(short, long)]
    config: Option<String>,

    /// Runtime mode: cloud, local, or auto
    #[arg(long, default_value = "auto")]
    mode: String,

    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start the TEMM1E gateway daemon
    Start {
        /// Run as a background daemon (requires prior setup via `temm1e start` first)
        #[arg(short, long)]
        daemon: bool,
        /// Log file path when running as daemon (default: ~/.temm1e/temm1e.log)
        #[arg(long)]
        log: Option<String>,
        /// Temm1e personality mode: play (warm, chaotic :3), work (sharp, precise >:3), pro (professional, no emoticons), or none (no personality, minimal identity)
        #[arg(long, default_value = "play")]
        personality: String,
    },
    /// Stop a running daemon
    Stop,
    /// Interactive CLI chat with the agent
    Chat,
    /// Show gateway status, connected channels, provider health
    Status,
    /// Manage skills
    Skill {
        #[command(subcommand)]
        command: SkillCommands,
    },
    /// Manage configuration
    Config {
        #[command(subcommand)]
        command: ConfigCommands,
    },
    /// Show version information
    Version,
    /// Check for updates and install if available
    Update,
    /// Factory reset — wipe all local state and start fresh
    Reset {
        /// Skip confirmation prompt (for scripted use)
        #[arg(long)]
        confirm: bool,
    },
    /// Manage OpenAI Codex OAuth authentication
    #[cfg(feature = "codex-oauth")]
    Auth {
        #[command(subcommand)]
        command: AuthCommands,
    },
    /// Interactive TUI with rich rendering, observability, and slash commands
    #[cfg(feature = "tui")]
    Tui,
}

#[cfg(feature = "codex-oauth")]
#[derive(Subcommand)]
enum AuthCommands {
    /// Authenticate with your ChatGPT Plus/Pro subscription via OAuth
    Login {
        /// Use headless mode (paste URL instead of browser redirect)
        #[arg(long)]
        headless: bool,
        /// Export oauth.json to a custom path (for Docker/remote deployments)
        #[arg(long)]
        output: Option<String>,
    },
    /// Show current OAuth authentication status
    Status,
    /// Remove OAuth tokens and log out
    Logout,
}

#[derive(Subcommand)]
enum SkillCommands {
    /// List installed skills
    List,
    /// Show skill details
    Info { name: String },
    /// Install a skill from a path
    Install { path: String },
}

#[derive(Subcommand)]
enum ConfigCommands {
    /// Validate the current configuration
    Validate,
    /// Show resolved configuration
    Show,
}

// ── Onboarding helpers ─────────────────────────────────────

/// Validate a provider key by making a minimal API call.
/// Returns Ok(provider_arc) if the key works, Err(message) if not.
async fn validate_provider_key(
    config: &temm1e_core::types::config::ProviderConfig,
) -> Result<Arc<dyn temm1e_core::Provider>, String> {
    let provider = temm1e_providers::create_provider(config)
        .map_err(|e| format!("Failed to create provider: {}", e))?;
    let provider_arc: Arc<dyn temm1e_core::Provider> = Arc::from(provider);

    let test_req = temm1e_core::types::message::CompletionRequest {
        model: config.model.clone().unwrap_or_default(),
        messages: vec![temm1e_core::types::message::ChatMessage {
            role: temm1e_core::types::message::Role::User,
            content: temm1e_core::types::message::MessageContent::Text("Hi".to_string()),
        }],
        tools: Vec::new(),
        max_tokens: Some(1),
        temperature: Some(0.0),
        system: None,
    };

    match provider_arc.complete(test_req).await {
        Ok(_) => Ok(provider_arc),
        Err(e) => {
            let err_str = format!("{}", e);
            let err_lower = err_str.to_lowercase();
            // Auth errors or invalid model errors — reject the reload
            if err_lower.contains("401")
                || err_lower.contains("403")
                || err_lower.contains("unauthorized")
                || err_lower.contains("invalid api key")
                || err_lower.contains("invalid x-api-key")
                || err_lower.contains("authentication")
                || err_lower.contains("permission")
                || err_lower.contains("404")
                || err_lower.contains("not_found")
                || err_lower.contains("model:")
            {
                Err(err_str)
            } else {
                // Non-auth errors (400 max_tokens, 429 rate limit, etc.) mean
                // the key IS valid — the API accepted the auth, just rejected
                // the request params. This is fine for validation.
                tracing::debug!(error = %err_str, "Key validation got non-auth error — key is valid");
                Ok(provider_arc)
            }
        }
    }
}

// detect_api_key, parse_proxy_config, normalize_provider_name, default_model,
// CredentialsFile, CredentialsProvider, credentials_path, load_credentials_file,
// save_credentials, load_saved_credentials, load_active_provider_keys
// → imported from temm1e_core::config::credentials and temm1e_core::types::model_registry

// Placeholder to satisfy the compiler for the deleted block below.
// The actual functions are now imported at the top of this file.
/// Check if a user is the admin by reading `~/.temm1e/allowlist.toml`.
fn is_admin_user(user_id: &str) -> bool {
    let path = dirs::home_dir().map(|h| h.join(".temm1e").join("allowlist.toml"));
    let path = match path {
        Some(p) => p,
        None => return false,
    };
    let content = match std::fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return false,
    };
    // Parse just the admin field — keep it minimal to avoid coupling with channel types
    #[derive(serde::Deserialize)]
    struct AllowlistCheck {
        admin: String,
    }
    match toml::from_str::<AllowlistCheck>(&content) {
        Ok(al) => al.admin == user_id,
        Err(_) => false,
    }
}

// ── Daemon helpers ───────────────────────────────────────────────────────

/// Get the path to the PID file: `~/.temm1e/temm1e.pid`
fn pid_file_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".temm1e").join("temm1e.pid"))
}

/// Write the current process PID to the PID file.
fn write_pid_file() {
    if let Some(path) = pid_file_path() {
        let _ = std::fs::write(&path, std::process::id().to_string());
    }
}

/// Remove the PID file.
fn remove_pid_file() {
    if let Some(path) = pid_file_path() {
        let _ = std::fs::remove_file(path);
    }
}

/// Read the PID from the PID file.
fn read_pid_file() -> Option<u32> {
    let path = pid_file_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    content.trim().parse().ok()
}

/// Check if a process with the given PID is still running.
fn is_process_alive(pid: u32) -> bool {
    std::path::Path::new(&format!("/proc/{}", pid)).exists()
        || std::process::Command::new("kill")
            .args(["-0", &pid.to_string()])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false)
}

/// Build the onboarding welcome message with a pre-generated setup link.
fn onboarding_message_with_link(setup_link: &str) -> String {
    format!(
        "Welcome to TEMM1E!\n\n\
         To get started, open this secure setup link:\n\
         {}\n\n\
         Paste your API key in the form, copy the encrypted blob, \
         and send it back here.\n\n\
         Or just paste your API key directly below — \
         I'll auto-detect the provider and get you online.\n\n\
         You can add more keys later with /addkey, \
         list them with /keys, or remove with /removekey.",
        setup_link
    )
}

const ONBOARDING_REFERENCE: &str = "\
Supported formats:\n\n\
1\u{fe0f}\u{20e3} Auto-detect (just paste the key):\n\
sk-ant-...     \u{2192} Anthropic\n\
sk-...         \u{2192} OpenAI\n\
AIzaSy...      \u{2192} Gemini\n\
xai-...        \u{2192} Grok\n\
sk-or-...      \u{2192} OpenRouter\n\n\
2\u{fe0f}\u{20e3} Explicit (for keys without unique prefix):\n\
zai:YOUR_KEY\n\
minimax:YOUR_KEY\n\
openrouter:YOUR_KEY\n\
ollama:YOUR_KEY\n\n\
3\u{fe0f}\u{20e3} Proxy / custom endpoint:\n\
proxy <provider> <base_url> <api_key>\n\n\
Example:\n\
proxy openai https://my-proxy.com/v1 sk-xxx\n\
proxy anthropic https://gateway.ai/v1/anthropic sk-ant-xxx\n\
proxy ollama https://ollama.com/v1 your-ollama-key";

const SYSTEM_PROMPT_BASE: &str = "\
You are TEMM1E, a cloud-native AI agent running on a remote server. \
Your personal nickname is Tem. Your official name is TEMM1E. \
Always refer to yourself as Tem.\n\n\
You have full access to these tools:\n\
- shell: run any command\n\
- file_read / file_write / file_list: filesystem operations\n\
- web_fetch: HTTP GET requests\n\
- browser: control a real Chrome browser with these actions:\n\
  * navigate: browser(action=\"navigate\", url=\"https://example.com\")\n\
  * click: browser(action=\"click\", selector=\"button.submit\")\n\
  * type: browser(action=\"type\", selector=\"input#search\", text=\"query\")\n\
  * screenshot: browser(action=\"screenshot\", filename=\"page.png\")\n\
  * get_text: browser(action=\"get_text\") - get page content\n\
  * evaluate: browser(action=\"evaluate\", script=\"document.title\")\n\
  * close: browser(action=\"close\") - close browser when done\n\
- send_message: send real-time messages to the user during tasks\n\
- send_file: send files to the user\n\
- memory_manage: your persistent knowledge store (remember/recall/forget/update/list)\n\n\
KEY RULES:\n\
- Shell output (stdout/stderr) is NOT visible to the user. Only YOUR \
  final text reply and send_message calls reach the user.\n\
- To send real-time updates during multi-step work, call send_message \
  each round. You can do this repeatedly — each call delivers instantly.\n\
- For timed/periodic tasks (e.g. send something every N seconds), use \
  shell(\"sleep N\") then send_message in a loop across tool rounds.\n\
- When asked to visit a website, open a page, or interact with a web app, \
  USE THE BROWSER TOOL with proper action parameter. Example: browser(action=\"navigate\", url=\"https://youtube.com\")\n\
- After finishing browser work, call browser(action=\"close\") to shut it down.\n\
- Reply in the same language the user writes in.\n\
- Be concise. No emoji unless the user uses them.\n\
- NEVER give up on a task by explaining limitations. You have a multi-round \
  tool loop — keep calling tools until the task is done or you hit a real \
  error. Do not stop early to explain what you 'cannot' do.\n\n\
PERSISTENT MEMORY:\n\
You have a persistent knowledge store via the memory_manage tool. Use it to:\n\
- Remember important facts the user tells you (name, preferences, project details)\n\
- Save useful context that should persist across conversations\n\
- Recall previously saved knowledge when relevant to the conversation\n\
When to use memory_manage:\n\
- When the user explicitly asks you to remember something\n\
- When you learn an important fact about the user or their project\n\
- When the user corrects you — update the relevant memory\n\
- When you need context from a previous conversation\n\
CRITICAL: After EVERY remember/update/forget action, you MUST tell the user \
what you did. For example: 'I've remembered that your name is Alice' or \
'I've updated the project status to completed' or 'I've forgotten the old API endpoint'. \
Never silently save or delete memories.";

/// Build the full system prompt with dynamic provider/model context.
/// This ensures the bot always knows what's actually configured.
fn build_system_prompt() -> String {
    let mut prompt = SYSTEM_PROMPT_BASE.to_string();

    // ── Provider/model context ────────────────────────────────
    prompt.push_str("\n\nSUPPORTED PROVIDERS & DEFAULT MODELS:\n");
    prompt.push_str("- anthropic: claude-sonnet-4-6, claude-opus-4-6, claude-haiku-4-6\n");
    prompt.push_str("- openai: gpt-5.2, gpt-4.1, gpt-4.1-mini, o4-mini\n");
    prompt.push_str("- gemini: gemini-3-flash-preview, gemini-3.1-pro-preview, gemini-2.5-flash, gemini-2.5-pro\n");
    prompt.push_str("- grok (xai): grok-4-1-fast-non-reasoning, grok-3\n");
    prompt.push_str(
        "- openrouter: any model via anthropic/claude-sonnet-4-6, openai/gpt-5.2, etc.\n",
    );
    prompt.push_str("- zai (zhipu): glm-4.7-flash, glm-4.7, glm-5, glm-5-code, glm-4.6v\n");
    prompt.push_str("- minimax: MiniMax-M2.5\n");
    prompt.push_str("- openai-codex: gpt-5.4 (recommended), gpt-5.3-codex, gpt-5.2-codex (OAuth subscription)\n");

    // ── Vision capability ──────────────────────────────────────
    prompt.push_str(
        "\nVISION (IMAGE) SUPPORT:\n\
         Models that can see images: all claude-*, all gpt-4o/gpt-4.1/gpt-5.*, all gemini-*, \
         grok-3/grok-4, glm-*v* (V-suffix only, e.g. glm-4.6v-flash).\n\
         Text-only (NO vision): gpt-3.5-turbo, glm-4.7-flash, glm-4.7, glm-5, glm-5-code, \
         glm-4.5-flash, all MiniMax models.\n\
         If the user sends an image on a text-only model, images are auto-stripped and \
         the user is notified. Suggest switching to a vision model.\n",
    );

    // ── Current configuration ─────────────────────────────────
    if let Some(creds) = load_credentials_file() {
        prompt.push_str("\nCURRENT CONFIGURATION:\n");
        prompt.push_str(&format!("Active provider: {}\n", creds.active));
        for p in &creds.providers {
            let key_count = p.keys.iter().filter(|k| !is_placeholder_key(k)).count();
            let base_note = if let Some(ref url) = p.base_url {
                format!(" (via {})", url)
            } else {
                String::new()
            };
            prompt.push_str(&format!(
                "- {}: model={}, {} key(s){}\n",
                p.name, p.model, key_count, base_note
            ));
        }
    }

    // ── Self-configuration rules ──────────────────────────────
    prompt.push_str(
        "\n\
SELF-CONFIGURATION:\n\
Your config lives at ~/.temm1e/credentials.toml.\n\
To change the active provider or model, edit ONLY the 'active' field or 'model' \
field in credentials.toml. NEVER modify or add API keys directly — keys are \
managed by the onboarding system. If the user wants to add a key, tell them to \
paste it in chat.\n\
Changes take effect immediately — TEMM1E validates the key and auto-reloads \
after each response. If a key is invalid, the switch is rejected and the \
current provider stays active.\n\
Users can add keys anytime by pasting them in chat. TEMM1E auto-detects the \
provider and validates before saving.\n\n\
SECRET HANDLING (MANDATORY — NEVER VIOLATE):\n\
There are 3 environments: USER (human) → CLAW (you, the agent) → PC (the server you run on).\n\
- Users give you secrets (API keys, passwords, tokens, account IDs) for YOU to use.\n\
- You ARE allowed to use secrets on the PC: log into services, call APIs, configure tools, \
  do personal tasks for the user. This is your job.\n\
- You must NEVER send secrets BACK to the user in your replies. Secrets flow one way: \
  user → claw. Never claw → user.\n\
- You must NEVER post secrets on the internet (no pasting keys in public repos, \
  web forms, or chat services other than the user's own channel).\n\
Specific rules:\n\
- NEVER echo back an API key the user pasted, not even partially.\n\
- NEVER read credentials.toml and show its contents to the user.\n\
- NEVER include API keys in shell commands visible to the user.\n\
- If the user asks to see their key, say it's stored securely and cannot be displayed.\n\
- When confirming a key was added, say 'Key saved for [provider]' — never show the key.\n\
- This applies to ALL secrets: API keys, tokens, passwords, encrypted blobs, account IDs.\n\
A secondary output filter censors any key that leaks, but you must not rely on it. \
The primary defense is YOU never including secrets in your output.",
    );

    // ── MCP self-extension ────────────────────────────────────
    #[cfg(feature = "mcp")]
    prompt.push_str(
        "\n\n\
MCP (MODEL CONTEXT PROTOCOL) — SELF-EXTENSION:\n\
You can extend your own capabilities at runtime by connecting MCP servers. \
MCP servers are external processes that provide additional tools via the \
Model Context Protocol. You have THREE tools for this:\n\n\
1. self_extend_tool — DISCOVER: search for MCP servers by capability. \
   Call with a query like 'browse websites' or 'query database' and get \
   matching servers with install commands.\n\
2. self_add_mcp — INSTALL: install a discovered MCP server. Its tools \
   become available to you immediately, no restart needed.\n\
3. mcp_manage — MANAGE: list/remove/restart installed MCP servers.\n\n\
SELF-EXTENSION WORKFLOW:\n\
When you need a capability you don't have:\n\
1. Tell the user: 'I don't have X built-in, but I can install an MCP server for it.'\n\
2. Call self_extend_tool with what you need → get candidates.\n\
3. Pick the best match → call self_add_mcp to install it.\n\
4. Use the new tools to complete the user's task.\n\n\
WHEN TO SELF-EXTEND:\n\
- User asks for something beyond your built-in tools (e.g., 'search the web', \
  'query my database', 'generate an image').\n\
- User explicitly asks to connect an MCP server.\n\
- A task would clearly benefit from a specialized tool.\n\n\
SAFETY RULES:\n\
- ALWAYS tell the user what you're installing and why BEFORE calling self_add_mcp.\n\
- If an MCP server needs env vars (API keys), ask the user to set them first.\n\
- If install fails, tell the user and suggest alternatives or manual setup.\n\
- Keep server names short: 'playwright', 'postgres', 'github'.\n\
- Use mcp_manage(action='list') to check what's already connected.",
    );

    // ── Custom tool authoring ────────────────────────────────────
    prompt.push_str(
        "\n\n\
CUSTOM TOOL AUTHORING — SELF-CREATE:\n\
You can create your own tools at runtime using self_create_tool. Created tools \
persist across sessions in ~/.temm1e/custom-tools/.\n\n\
HOW IT WORKS:\n\
1. Call self_create_tool with action='create', providing: name, description, \
   language (bash/python/node), script content, and a JSON Schema for parameters.\n\
2. The script receives input as JSON via stdin and should write output to stdout.\n\
3. The tool becomes available immediately — no restart needed.\n\n\
WHEN TO CREATE A TOOL:\n\
- User asks for a repeatable task (e.g., 'check my server status', 'format this data').\n\
- You find yourself running the same shell commands repeatedly.\n\
- A task would benefit from a dedicated, named, reusable tool.\n\n\
ACTIONS:\n\
- create: Write a new script tool (name + description + language + script + parameters).\n\
- list: Show all custom tools.\n\
- delete: Remove a custom tool by name.\n\n\
RULES:\n\
- Keep scripts simple and focused — one tool, one job.\n\
- Always test the tool after creating it by calling it once.\n\
- Tool names must be alphanumeric with underscores/hyphens (e.g., 'check_status').\n\
- Scripts have a 30-second timeout. For long tasks, use async patterns.",
    );

    prompt
}

/// Hardcoded output filter: replaces any known API key in the text with [REDACTED].
/// This is the last line of defense — the system prompt tells the agent not to leak
/// secrets, but this filter catches any that slip through.
fn censor_secrets(text: &str) -> String {
    let creds = match load_credentials_file() {
        Some(c) => c,
        None => return text.to_string(),
    };
    let mut censored = text.to_string();
    for provider in &creds.providers {
        for key in &provider.keys {
            if !key.is_empty() && !is_placeholder_key(key) && key.len() >= 8 {
                censored = censored.replace(key, "[REDACTED]");
            }
        }
    }
    censored
}

/// Format a Temm1eError into a user-friendly message for chat.
///
/// Translates raw error variants into human-readable explanations with
/// actionable suggestions.  Raw JSON bodies and internal details are
/// never exposed to end-users.
fn format_user_error(e: &temm1e_core::types::error::Temm1eError) -> String {
    use temm1e_core::types::error::Temm1eError;
    match e {
        Temm1eError::Provider(msg) => {
            // Detect common sub-categories from the raw message
            if msg.contains("400") || msg.contains("Bad Request") || msg.contains("validation") {
                "The AI provider rejected the request. This can happen when the model \
                 doesn't support certain features (like tool calling). Try switching \
                 models with /model."
                    .to_string()
            } else if msg.contains("500") || msg.contains("502") || msg.contains("503") {
                "The AI provider is experiencing issues. Please try again in a moment.".to_string()
            } else if msg.contains("timeout") || msg.contains("timed out") {
                "The request to the AI provider timed out. Please try again.".to_string()
            } else {
                "An error occurred with the AI provider. Please try again or switch \
                 models with /model."
                    .to_string()
            }
        }
        Temm1eError::Auth(_) => {
            "API key issue — your key may be invalid or expired. Use /addkey to \
             update it."
                .to_string()
        }
        Temm1eError::RateLimited(_) => {
            "Rate limited by the AI provider. Please wait a moment and try again.".to_string()
        }
        Temm1eError::Tool(msg) => {
            format!("A tool encountered an error: {msg}")
        }
        Temm1eError::Memory(_) => {
            "An error occurred accessing conversation memory. Your message wasn't \
             lost — please try again."
                .to_string()
        }
        Temm1eError::Config(_) => {
            "Configuration error. Please check your setup with /status.".to_string()
        }
        _ => {
            // Generic fallback — still never shows raw internals
            "An unexpected error occurred. Please try again.".to_string()
        }
    }
}

// ── OTK key management helpers ────────────────────────────

/// List configured providers (names only, never keys).
fn list_configured_providers() -> String {
    let mut lines = vec![];
    let mut has_providers = false;

    // Check Codex OAuth first
    #[cfg(feature = "codex-oauth")]
    if temm1e_codex_oauth::TokenStore::exists() {
        has_providers = true;
        lines.push("Configured providers:".to_string());
        lines.push("  openai-codex — model: gpt-5.4, OAuth (active)".to_string());
    }

    if let Some(creds) = load_credentials_file() {
        if !creds.providers.is_empty() {
            if !has_providers {
                lines.push("Configured providers:".to_string());
            }
            has_providers = true;
            for p in &creds.providers {
                let key_count = p.keys.iter().filter(|k| !is_placeholder_key(k)).count();
                let active = if p.name == creds.active && !has_providers {
                    " (active)"
                } else {
                    ""
                };
                let proxy = if let Some(ref url) = p.base_url {
                    format!(" via {}", url)
                } else {
                    String::new()
                };
                lines.push(format!(
                    "  {} — model: {}, {} key(s){}{}",
                    p.name, p.model, key_count, proxy, active
                ));
            }
        }
    }

    if !has_providers {
        return "No providers configured. Use /addkey to add one.".to_string();
    }

    lines.push(String::new());
    lines.push("Use /addkey to add a new key, /removekey <provider> to remove one.".to_string());
    lines.join("\n")
}

/// Handle the /model command.
///
/// - `/model` (no args) → show current model + all available models per provider
/// - `/model <exact-name>` → switch to that model on the active provider
fn handle_model_command(args: &str) -> String {
    // Check Codex OAuth first — if active and no args, show Codex model info
    #[cfg(feature = "codex-oauth")]
    {
        let has_creds = load_credentials_file()
            .map(|c| !c.providers.is_empty())
            .unwrap_or(false);
        if !has_creds && temm1e_codex_oauth::TokenStore::exists() {
            if args.is_empty() {
                let codex_models = [
                    "gpt-5.4",
                    "gpt-5.3-codex",
                    "gpt-5.3-codex-spark",
                    "gpt-5.2",
                    "gpt-5.2-codex",
                    "gpt-5.1-codex",
                    "gpt-5.1-codex-mini",
                    "gpt-5",
                    "gpt-5-codex",
                    "gpt-5-codex-mini",
                    "gpt-5-mini",
                    "gpt-4.1",
                    "gpt-4.1-mini",
                    "gpt-4.1-nano",
                    "o4-mini",
                ];
                let mut lines = vec![
                    "Current: gpt-5.4 on openai-codex provider (OAuth)".to_string(),
                    String::new(),
                    "Available Codex models:".to_string(),
                ];
                for m in &codex_models {
                    let current = if *m == "gpt-5.4" { " ← current" } else { "" };
                    lines.push(format!("    {}{}", m, current));
                }
                lines.push(String::new());
                lines.push("Switch model: /model <exact-model-name>".to_string());
                lines.push("Example: /model gpt-5.2-codex".to_string());
                return lines.join("\n");
            } else {
                let target = args.trim();
                // Return "Model switched:" so the caller rebuilds the agent
                return format!("Model switched: codex-oauth → {}\nCodex OAuth", target);
            }
        }
    }

    let creds = match load_credentials_file() {
        Some(c) => c,
        None => return "No providers configured. Use /addkey to add one.".to_string(),
    };

    if creds.providers.is_empty() {
        return "No providers configured. Use /addkey to add one.".to_string();
    }

    // ── No args: show current + available models ──────────────
    if args.is_empty() {
        let mut lines = Vec::new();

        // Current model
        if let Some(active) = creds.providers.iter().find(|p| p.name == creds.active) {
            lines.push(format!(
                "Current: {} on {} provider",
                active.model, active.name
            ));
        }

        lines.push(String::new());
        lines.push("Available models per provider:".to_string());
        for p in &creds.providers {
            let models = available_models_for_provider(&p.name);
            let active_marker = if p.name == creds.active {
                " (active)"
            } else {
                ""
            };
            let is_proxy = p.base_url.is_some() || p.name == "openrouter";
            lines.push(format!("  {}{}:", p.name, active_marker));
            if is_proxy {
                let current_vision = if is_vision_model(&p.model) {
                    " [vision]"
                } else {
                    ""
                };
                lines.push(format!("    {} ← current{}", p.model, current_vision));
                lines.push("    (proxy — any model name accepted)".to_string());
            } else {
                for m in &models {
                    let vision = if is_vision_model(m) { " [vision]" } else { "" };
                    let current = if *m == p.model { " ← current" } else { "" };
                    lines.push(format!("    {}{}{}", m, vision, current));
                }
            }
        }

        lines.push(String::new());
        lines.push("Switch model: /model <exact-model-name>".to_string());
        lines.push("Example: /model claude-sonnet-4-6".to_string());
        return lines.join("\n");
    }

    // ── Switch to specific model ──────────────────────────────
    let target = args.trim();

    // Find active provider
    let active_provider = match creds.providers.iter().find(|p| p.name == creds.active) {
        Some(p) => p.clone(),
        None => return "Active provider not found in credentials.".to_string(),
    };

    if active_provider.model == target {
        return format!("Already using {}.", target);
    }

    // Validate model against known list for the active provider.
    // Skip validation for proxy/OpenRouter providers (custom base_url) — they accept any model.
    let is_proxy = active_provider.base_url.is_some() || active_provider.name == "openrouter";
    let known = available_models_for_provider(&active_provider.name);
    if !is_proxy && !known.is_empty() && !known.contains(&target) {
        let list = known
            .iter()
            .map(|m| {
                let v = if is_vision_model(m) { " [vision]" } else { "" };
                format!("  {}{}", m, v)
            })
            .collect::<Vec<_>>()
            .join("\n");
        return format!(
            "Unknown model '{}' for provider '{}'.\n\nAvailable models:\n{}\n\nUse exact name: /model <model-name>",
            target, active_provider.name, list
        );
    }

    // Update the model in credentials.toml
    let mut updated = creds.clone();
    for p in &mut updated.providers {
        if p.name == creds.active {
            p.model = target.to_string();
        }
    }

    let path = credentials_path();
    match toml::to_string_pretty(&updated) {
        Ok(content) => {
            if let Err(e) = std::fs::write(&path, &content) {
                return format!("Failed to write credentials: {}", e);
            }
            tracing::info!(
                old_model = %active_provider.model,
                new_model = %target,
                "Model switched via /model command"
            );
            format!(
                "Model switched: {} → {}\nHot-reload will apply after this response.",
                active_provider.model, target
            )
        }
        Err(e) => format!("Failed to serialize credentials: {}", e),
    }
}

/// Remove a provider from credentials.
fn remove_provider(provider_name: &str) -> String {
    if provider_name.is_empty() {
        return "Usage: /removekey <provider>\nExample: /removekey openai".to_string();
    }
    let mut creds = match load_credentials_file() {
        Some(c) => c,
        None => return "No providers configured.".to_string(),
    };
    let before = creds.providers.len();
    creds.providers.retain(|p| p.name != provider_name);
    if creds.providers.len() == before {
        return format!(
            "Provider '{}' not found. Use /keys to see configured providers.",
            provider_name
        );
    }
    // If we removed the active provider, switch to first remaining
    if creds.active == provider_name {
        creds.active = creds
            .providers
            .first()
            .map(|p| p.name.clone())
            .unwrap_or_default();
    }
    let path = credentials_path();
    match toml::to_string_pretty(&creds) {
        Ok(content) => {
            if let Err(e) = std::fs::write(&path, content) {
                return format!("Failed to save: {}", e);
            }
        }
        Err(e) => return format!("Failed to serialize: {}", e),
    }
    if creds.providers.is_empty() {
        format!(
            "Removed {}. No providers remaining — send a new API key to configure one.",
            provider_name
        )
    } else {
        format!(
            "Removed {}. Active provider: {} (model: {})",
            provider_name,
            creds.active,
            creds
                .providers
                .first()
                .map(|p| p.model.as_str())
                .unwrap_or("unknown")
        )
    }
}

/// Decrypt an `enc:v1:` blob using the OTK from the setup token store.
async fn decrypt_otk_blob(
    blob_b64: &str,
    store: &temm1e_gateway::SetupTokenStore,
    chat_id: &str,
) -> std::result::Result<String, String> {
    use aes_gcm::aead::{Aead, KeyInit};
    use aes_gcm::{Aes256Gcm, Key, Nonce};

    // Look up OTK for this chat
    let otk = store
        .consume(chat_id)
        .await
        .ok_or_else(|| "No pending setup link for this chat. Run /addkey first.".to_string())?;

    // Base64 decode
    let blob = base64::engine::general_purpose::STANDARD
        .decode(blob_b64.trim())
        .map_err(|e| format!("Invalid base64: {}", e))?;

    // Need at least 12 (IV) + 16 (tag) + 1 (ciphertext) bytes
    if blob.len() < 29 {
        return Err("Encrypted blob too short.".to_string());
    }

    // Split: first 12 bytes = IV, rest = ciphertext + auth tag
    let (iv_bytes, ciphertext) = blob.split_at(12);

    let key = Key::<Aes256Gcm>::from_slice(&otk);
    let cipher = Aes256Gcm::new(key);
    let nonce = Nonce::from_slice(iv_bytes);

    let plaintext = cipher.decrypt(nonce, ciphertext).map_err(|_| {
        "Decryption failed — the setup link may have expired or the data was tampered with."
            .to_string()
    })?;

    String::from_utf8(plaintext).map_err(|_| "Decrypted data is not valid UTF-8.".to_string())
}

// ── Stop-command detection ─────────────────────────────────
// Stop detection is now fully intention-based via the LLM interceptor.
// Only /stop remains as a hardcoded instant-kill command.

/// Retry `send_message` up to 3 times with exponential backoff.
async fn send_with_retry(
    sender: &dyn temm1e_core::Channel,
    reply: temm1e_core::types::message::OutboundMessage,
) {
    let mut attempt = 0u32;
    let msg = reply;
    loop {
        attempt += 1;
        match sender.send_message(msg.clone()).await {
            Ok(_) => return,
            Err(e) => {
                if attempt >= 3 {
                    tracing::error!(error = %e, attempt, "Failed to send reply after 3 attempts — message lost");
                    return;
                }
                tracing::warn!(error = %e, attempt, "Failed to send reply, retrying");
                tokio::time::sleep(std::time::Duration::from_millis(500 * (1 << attempt))).await;
            }
        }
    }
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging — TUI mode writes to a file instead of stderr
    #[cfg(feature = "tui")]
    let _is_tui = matches!(cli.command, Commands::Tui);
    #[cfg(not(feature = "tui"))]
    let _is_tui = false;

    if _is_tui {
        // TUI mode: write logs to ~/.temm1e/tui.log so they don't corrupt the display
        let log_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".temm1e");
        std::fs::create_dir_all(&log_dir).ok();
        if let Ok(log_file) = std::fs::File::create(log_dir.join("tui.log")) {
            tracing_subscriber::fmt()
                .with_env_filter(
                    tracing_subscriber::EnvFilter::try_from_default_env()
                        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
                )
                .with_writer(std::sync::Mutex::new(log_file))
                .with_ansi(false)
                .json()
                .init();
        }
    } else {
        tracing_subscriber::fmt()
            .with_env_filter(
                tracing_subscriber::EnvFilter::try_from_default_env()
                    .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
            )
            .json()
            .init();
    }

    // ── TUI fast path — skip all other init, go straight to TUI ──
    #[cfg(feature = "tui")]
    if _is_tui {
        let config_path = cli.config.as_deref().map(std::path::Path::new);
        let config = temm1e_core::config::load_config(config_path)?;
        return temm1e_tui::launch_tui(config).await;
    }

    // Initialize health endpoint uptime clock
    temm1e_gateway::health::init_start_time();

    // ── Global panic hook — route panics through tracing ─────
    // Without this, panics only write to stderr and are invisible in structured logs.
    std::panic::set_hook(Box::new(|info| {
        let payload = if let Some(s) = info.payload().downcast_ref::<String>() {
            s.clone()
        } else if let Some(s) = info.payload().downcast_ref::<&str>() {
            s.to_string()
        } else {
            "unknown panic payload".to_string()
        };
        let location = info
            .location()
            .map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column()))
            .unwrap_or_else(|| "unknown".to_string());
        tracing::error!(
            panic.payload = %payload,
            panic.location = %location,
            "PANIC caught — task will attempt recovery"
        );
    }));

    // ── Handle Reset before config loading ─────────────────
    // Reset must work even when config is corrupted/poisoned,
    // so we intercept it before load_config() which might fail.
    if let Commands::Reset { confirm } = &cli.command {
        let data_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(".temm1e");

        if !data_dir.exists() {
            println!("Nothing to reset — {} does not exist.", data_dir.display());
            return Ok(());
        }

        // Check if daemon is running
        if let Some(pid) = read_pid_file() {
            if is_process_alive(pid) {
                eprintln!(
                    "TEMM1E daemon is running (PID {}). Stop it first with `temm1e stop`.",
                    pid
                );
                std::process::exit(1);
            }
        }

        // Confirmation gate
        if !confirm {
            println!("This will DELETE all TEMM1E local state:");
            println!("  {}/", data_dir.display());
            println!();
            println!("  - credentials.toml    (saved API keys)");
            println!("  - memory.db           (conversation history)");
            println!("  - allowlist.toml      (user access control)");
            println!("  - mcp.toml            (MCP server configs)");
            println!("  - config.toml         (local config overrides)");
            println!("  - oauth.json          (Codex OAuth tokens)");
            println!("  - custom-tools/       (user-authored tools)");
            println!("  - workspace/          (workspace files)");
            println!();
            println!("A backup will be saved before deletion.");
            println!();
            print!("Type 'reset' to confirm: ");
            use std::io::Write;
            std::io::stdout().flush().ok();

            let mut input = String::new();
            std::io::stdin().read_line(&mut input).ok();
            if input.trim() != "reset" {
                println!("Aborted.");
                return Ok(());
            }
        }

        // Backup before wipe
        let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
        let backup_dir = dirs::home_dir()
            .unwrap_or_else(|| std::path::PathBuf::from("."))
            .join(format!(".temm1e.bak.{}", timestamp));

        // Copy directory tree for backup
        fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
            std::fs::create_dir_all(dst)?;
            for entry in std::fs::read_dir(src)? {
                let entry = entry?;
                let src_path = entry.path();
                let dst_path = dst.join(entry.file_name());
                if src_path.is_dir() {
                    copy_dir_recursive(&src_path, &dst_path)?;
                } else {
                    std::fs::copy(&src_path, &dst_path)?;
                }
            }
            Ok(())
        }

        match copy_dir_recursive(&data_dir, &backup_dir) {
            Ok(()) => {
                println!("Backup saved to {}", backup_dir.display());
            }
            Err(e) => {
                eprintln!("Failed to create backup: {}", e);
                eprintln!("Aborting reset — your data is untouched.");
                std::process::exit(1);
            }
        }

        // Nuke everything
        match std::fs::remove_dir_all(&data_dir) {
            Ok(()) => {
                // Re-create the empty directory so future commands don't fail
                let _ = std::fs::create_dir_all(&data_dir);
                println!("Factory reset complete.");
                println!("Run `temm1e start` for fresh onboarding.");
            }
            Err(e) => {
                eprintln!("Failed to remove {}: {}", data_dir.display(), e);
                eprintln!("Backup is at {}", backup_dir.display());
                std::process::exit(1);
            }
        }

        return Ok(());
    }

    // Load configuration
    let config_path = cli.config.as_ref().map(std::path::Path::new);
    let mut config = temm1e_core::config::load_config(config_path)?;

    if !_is_tui {
        tracing::info!(mode = %cli.mode, "TEMM1E starting");
    }

    match cli.command {
        Commands::Stop => {
            match read_pid_file() {
                Some(pid) if is_process_alive(pid) => {
                    // Send SIGTERM on Unix, taskkill on Windows
                    #[cfg(unix)]
                    {
                        let status = std::process::Command::new("kill")
                            .args(["-TERM", &pid.to_string()])
                            .status();
                        match status {
                            Ok(s) if s.success() => {
                                remove_pid_file();
                                println!("TEMM1E daemon (PID {}) stopped.", pid);
                            }
                            _ => {
                                eprintln!("Failed to stop TEMM1E daemon (PID {}).", pid);
                                std::process::exit(1);
                            }
                        }
                    }
                    #[cfg(windows)]
                    {
                        let status = std::process::Command::new("taskkill")
                            .args(["/PID", &pid.to_string(), "/F"])
                            .status();
                        match status {
                            Ok(s) if s.success() => {
                                remove_pid_file();
                                println!("TEMM1E daemon (PID {}) stopped.", pid);
                            }
                            _ => {
                                eprintln!("Failed to stop TEMM1E daemon (PID {}).", pid);
                                std::process::exit(1);
                            }
                        }
                    }
                }
                Some(pid) => {
                    eprintln!(
                        "TEMM1E daemon (PID {}) is not running. Cleaning up stale PID file.",
                        pid
                    );
                    remove_pid_file();
                }
                None => {
                    eprintln!("No TEMM1E daemon running (no PID file found).");
                    std::process::exit(1);
                }
            }
        }
        Commands::Start {
            daemon,
            log,
            personality,
        } => {
            // ── Parse personality mode ───────────────────────────
            let temm1e_mode = match personality.to_lowercase().as_str() {
                "work" => temm1e_core::types::config::Temm1eMode::Work,
                "pro" => temm1e_core::types::config::Temm1eMode::Pro,
                "none" => temm1e_core::types::config::Temm1eMode::None,
                _ => temm1e_core::types::config::Temm1eMode::Play,
            };
            // Lock mode when user explicitly chose work/pro/none — disables mode_switch tool
            let personality_locked =
                !matches!(temm1e_mode, temm1e_core::types::config::Temm1eMode::Play);
            config.mode = temm1e_mode;
            tracing::info!(personality = %temm1e_mode, locked = personality_locked, "Temm1e personality mode");

            // ── Daemon mode ──────────────────────────────────────
            if daemon {
                let temm1e_dir = dirs::home_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".temm1e");
                let _ = std::fs::create_dir_all(&temm1e_dir);

                // Check for saved credentials — daemon requires prior setup
                let creds_path = temm1e_dir.join("credentials.toml");
                if !creds_path.exists() {
                    eprintln!(
                        "Error: No saved credentials found at {}\n\n\
                         First-time setup requires foreground mode to complete onboarding.\n\
                         Run `temm1e start` (without -d) first, then use -d for subsequent runs.",
                        creds_path.display()
                    );
                    std::process::exit(1);
                }

                // Check if already running
                if let Some(pid) = read_pid_file() {
                    if is_process_alive(pid) {
                        eprintln!(
                            "TEMM1E daemon is already running (PID {}). Use `temm1e stop` first.",
                            pid
                        );
                        std::process::exit(1);
                    }
                    // Stale PID file — clean up
                    remove_pid_file();
                }

                // Resolve log path
                let log_path = log
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| temm1e_dir.join("temm1e.log"));

                // Re-exec ourselves as a detached child
                let exe = std::env::current_exe().expect("cannot resolve own executable path");
                let mut args: Vec<String> = std::env::args().collect();
                // Remove --daemon / -d flag so the child runs in foreground
                args.retain(|a| a != "--daemon" && a != "-d");
                // Remove --log and its value too
                let mut skip_next = false;
                args.retain(|a| {
                    if skip_next {
                        skip_next = false;
                        return false;
                    }
                    if a == "--log" {
                        skip_next = true;
                        return false;
                    }
                    if a.starts_with("--log=") {
                        return false;
                    }
                    true
                });

                let log_file = std::fs::OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&log_path)
                    .unwrap_or_else(|e| {
                        eprintln!("Cannot open log file {}: {}", log_path.display(), e);
                        std::process::exit(1);
                    });
                let log_err = log_file.try_clone().unwrap_or_else(|e| {
                    eprintln!("Cannot clone log file handle: {}", e);
                    std::process::exit(1);
                });

                let child = std::process::Command::new(exe)
                    .args(&args[1..]) // skip argv[0]
                    .stdout(log_file)
                    .stderr(log_err)
                    .stdin(std::process::Stdio::null())
                    .spawn();

                match child {
                    Ok(c) => {
                        // Write child PID
                        let child_pid = c.id();
                        if let Some(path) = pid_file_path() {
                            let _ = std::fs::write(&path, child_pid.to_string());
                        }
                        println!(
                            "TEMM1E daemon started (PID {}).\n  Log: {}\n  Stop: temm1e stop",
                            child_pid,
                            log_path.display()
                        );
                    }
                    Err(e) => {
                        eprintln!("Failed to start daemon: {}", e);
                        std::process::exit(1);
                    }
                }
                return Ok(());
            }

            // ── Normal foreground start ──────────────────────────
            // Write PID file so `temm1e stop` works even in foreground
            write_pid_file();

            tracing::info!("Starting TEMM1E gateway");

            // ── Resolve API credentials ────────────────────────
            // Priority: config file > saved credentials > onboarding
            let credentials: Option<(String, String, String)> = {
                if let Some(ref key) = config.provider.api_key {
                    if !key.is_empty() && !key.starts_with("${") {
                        let name = config
                            .provider
                            .name
                            .clone()
                            .unwrap_or_else(|| "anthropic".to_string());
                        let model = config
                            .provider
                            .model
                            .clone()
                            .unwrap_or_else(|| default_model(&name).to_string());
                        Some((name, key.clone(), model))
                    } else {
                        load_saved_credentials()
                    }
                } else {
                    load_saved_credentials()
                }
            };

            // ── Memory backend ─────────────────────────────────
            let memory_url = config.memory.path.clone().unwrap_or_else(|| {
                let data_dir = dirs::home_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".temm1e");
                if let Err(e) = std::fs::create_dir_all(&data_dir) {
                    tracing::warn!(error = %e, path = %data_dir.display(), "Failed to create directory");
                }
                format!("sqlite:{}/memory.db?mode=rwc", data_dir.display())
            });
            let memory: Arc<dyn temm1e_core::Memory> = Arc::from(
                temm1e_memory::create_memory_backend(&config.memory.backend, &memory_url).await?,
            );
            tracing::info!(backend = %config.memory.backend, "Memory initialized");

            // ── Telegram channel ───────────────────────────────
            let mut channels: Vec<Arc<dyn temm1e_core::Channel>> = Vec::new();
            let mut primary_channel: Option<Arc<dyn temm1e_core::Channel>> = None;
            let mut tg_rx: Option<
                tokio::sync::mpsc::Receiver<temm1e_core::types::message::InboundMessage>,
            > = None;

            // Auto-inject Telegram config from env var when no config entry exists.
            // This enables zero-config VPS deployments: just set TELEGRAM_BOT_TOKEN.
            if !config.channel.contains_key("telegram") {
                if let Ok(token) = std::env::var("TELEGRAM_BOT_TOKEN") {
                    if !token.is_empty() {
                        config.channel.insert(
                            "telegram".to_string(),
                            temm1e_core::types::config::ChannelConfig {
                                enabled: true,
                                token: Some(token),
                                allowlist: vec![],
                                file_transfer: true,
                                max_file_size: None,
                            },
                        );
                        tracing::info!("Auto-configured Telegram from TELEGRAM_BOT_TOKEN env var");
                    }
                }
            }

            if let Some(tg_config) = config.channel.get("telegram") {
                if tg_config.enabled {
                    let mut tg = temm1e_channels::TelegramChannel::new(tg_config)?;
                    tg.start().await?;
                    tg_rx = tg.take_receiver();
                    let tg_arc: Arc<dyn temm1e_core::Channel> = Arc::new(tg);
                    channels.push(tg_arc.clone());
                    primary_channel = Some(tg_arc.clone());
                    tracing::info!("Telegram channel started");
                }
            }

            // ── Pending messages ───────────────────────────────
            let pending_messages: temm1e_tools::PendingMessages =
                Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

            // ── OTK setup token store ───────────────────────────
            let setup_tokens = temm1e_gateway::SetupTokenStore::new();

            // ── Pending raw key pastes (from /addkey unsafe) ────
            let pending_raw_keys: Arc<Mutex<HashSet<String>>> =
                Arc::new(Mutex::new(HashSet::new()));

            // ── Usage store (shares same SQLite DB as memory) ────
            let usage_store: Arc<dyn temm1e_core::UsageStore> =
                Arc::new(temm1e_memory::SqliteUsageStore::new(&memory_url).await?);
            tracing::info!("Usage store initialized");

            // ── Tools (with secret-censoring channel wrapper) ───
            let censored_channel: Option<Arc<dyn Channel>> = primary_channel
                .clone()
                .map(|ch| Arc::new(SecretCensorChannel { inner: ch }) as Arc<dyn Channel>);
            let shared_mode: temm1e_tools::SharedMode =
                Arc::new(tokio::sync::RwLock::new(config.mode));
            let shared_memory_strategy: Arc<
                tokio::sync::RwLock<temm1e_core::types::config::MemoryStrategy>,
            > = Arc::new(tokio::sync::RwLock::new(
                temm1e_core::types::config::MemoryStrategy::Lambda,
            ));
            let mut tools = temm1e_tools::create_tools(
                &config.tools,
                censored_channel,
                Some(pending_messages.clone()),
                Some(memory.clone()),
                Some(Arc::new(setup_tokens.clone()) as Arc<dyn temm1e_core::SetupLinkGenerator>),
                Some(usage_store.clone()),
                // Don't register mode_switch tool when personality is locked (work/pro)
                if personality_locked {
                    None
                } else {
                    Some(shared_mode.clone())
                },
            );
            tracing::info!(count = tools.len(), "Tools initialized");

            // ── Custom script tools (user/agent-authored) ──────
            let custom_tool_registry = Arc::new(temm1e_tools::CustomToolRegistry::new());
            {
                let custom_tools = custom_tool_registry.load_tools();
                if !custom_tools.is_empty() {
                    tracing::info!(count = custom_tools.len(), "Custom script tools loaded");
                    tools.extend(custom_tools);
                }
                tools.push(Arc::new(temm1e_tools::SelfCreateTool::new(
                    custom_tool_registry.clone(),
                )));
            }

            // ── MCP servers (external tool sources) ──────────
            #[cfg(feature = "mcp")]
            let mcp_manager: Arc<temm1e_mcp::McpManager> = {
                let mgr = Arc::new(temm1e_mcp::McpManager::new());
                mgr.connect_all().await;
                let tool_names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
                let mcp_tools = mgr.bridge_tools(&tool_names).await;
                if !mcp_tools.is_empty() {
                    tracing::info!(count = mcp_tools.len(), "MCP bridge tools loaded");
                    tools.extend(mcp_tools);
                }
                // Add MCP agent tools: manage, self-extend (discover), self-add (install)
                tools.push(Arc::new(temm1e_mcp::McpManageTool::new(mgr.clone())));
                tools.push(Arc::new(temm1e_mcp::SelfExtendTool::new()));
                tools.push(Arc::new(temm1e_mcp::SelfAddMcpTool::new(mgr.clone())));
                mgr
            };

            let system_prompt = Some(build_system_prompt());

            // Quick check: is [hive] enabled in config? (just the boolean, full init later)
            let hive_enabled_early = {
                #[derive(serde::Deserialize, Default)]
                struct HiveCheck {
                    #[serde(default)]
                    hive: HiveEnabled,
                }
                #[derive(serde::Deserialize, Default)]
                struct HiveEnabled {
                    #[serde(default)]
                    enabled: bool,
                }

                config_path
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .or_else(|| {
                        dirs::home_dir().and_then(|h| {
                            std::fs::read_to_string(h.join(".temm1e/config.toml")).ok()
                        })
                    })
                    .or_else(|| std::fs::read_to_string("temm1e.toml").ok())
                    .and_then(|content| toml::from_str::<HiveCheck>(&content).ok())
                    .map(|c| c.hive.enabled)
                    .unwrap_or(false)
            };

            // ── Agent state (None during onboarding) ───────────
            let agent_state: Arc<tokio::sync::RwLock<Option<Arc<temm1e_agent::AgentRuntime>>>> =
                Arc::new(tokio::sync::RwLock::new(None));

            if let Some((ref pname, ref key, ref model)) = credentials {
                // Filter out placeholder/invalid keys at startup
                if is_placeholder_key(key) {
                    tracing::warn!(provider = %pname, "Primary API key is a placeholder — starting in onboarding mode");
                    // Fall through to onboarding
                } else {
                    // Load all keys and saved base_url for this provider
                    let (all_keys, saved_base_url) = load_active_provider_keys()
                        .map(|(_, keys, _, burl)| {
                            let valid: Vec<String> = keys
                                .into_iter()
                                .filter(|k| !is_placeholder_key(k))
                                .collect();
                            (valid, burl)
                        })
                        .unwrap_or_else(|| (vec![key.clone()], None));
                    let effective_base_url =
                        saved_base_url.or_else(|| config.provider.base_url.clone());
                    let provider_config = temm1e_core::types::config::ProviderConfig {
                        name: Some(pname.clone()),
                        api_key: Some(key.clone()),
                        keys: all_keys,
                        model: Some(model.clone()),
                        base_url: effective_base_url,
                        extra_headers: config.provider.extra_headers.clone(),
                    };
                    // Create provider — route to Codex OAuth if configured
                    let provider: Arc<dyn temm1e_core::Provider> = {
                        #[cfg(feature = "codex-oauth")]
                        if pname == "openai-codex" {
                            let token_store =
                                std::sync::Arc::new(temm1e_codex_oauth::TokenStore::load()?);
                            Arc::new(temm1e_codex_oauth::CodexResponsesProvider::new(
                                model.clone(),
                                token_store,
                            ))
                        } else {
                            Arc::from(temm1e_providers::create_provider(&provider_config)?)
                        }
                        #[cfg(not(feature = "codex-oauth"))]
                        {
                            if pname == "openai-codex" {
                                return Err(anyhow::anyhow!(
                                    "OpenAI Codex OAuth requires the 'codex-oauth' feature. \
                                     Build with: cargo build --features codex-oauth"
                                ));
                            }
                            Arc::from(temm1e_providers::create_provider(&provider_config)?)
                        }
                    };
                    let agent = Arc::new(
                        temm1e_agent::AgentRuntime::with_limits(
                            provider.clone(),
                            memory.clone(),
                            tools.clone(),
                            model.clone(),
                            system_prompt.clone(),
                            config.agent.max_turns,
                            config.agent.max_context_tokens,
                            config.agent.max_tool_rounds,
                            config.agent.max_task_duration_secs,
                            config.agent.max_spend_usd,
                        )
                        .with_v2_optimizations(config.agent.v2_optimizations)
                        .with_parallel_phases(config.agent.parallel_phases)
                        .with_hive_enabled(hive_enabled_early)
                        .with_shared_mode(shared_mode.clone())
                        .with_shared_memory_strategy(shared_memory_strategy.clone()),
                    );
                    *agent_state.write().await = Some(agent);
                    tracing::info!(provider = %pname, model = %model, "Agent initialized");
                }
            } else {
                // Check if Codex OAuth tokens exist — use those instead of API key
                #[cfg(feature = "codex-oauth")]
                {
                    if temm1e_codex_oauth::TokenStore::exists() {
                        // Always use Codex-compatible model — config model is for API key provider
                        let model = "gpt-5.4".to_string();
                        match temm1e_codex_oauth::TokenStore::load() {
                            Ok(store) => {
                                let token_store = std::sync::Arc::new(store);
                                let provider: Arc<dyn temm1e_core::Provider> =
                                    Arc::new(temm1e_codex_oauth::CodexResponsesProvider::new(
                                        model.clone(),
                                        token_store,
                                    ));
                                let agent = Arc::new(
                                    temm1e_agent::AgentRuntime::with_limits(
                                        provider.clone(),
                                        memory.clone(),
                                        tools.clone(),
                                        model.clone(),
                                        system_prompt.clone(),
                                        config.agent.max_turns,
                                        config.agent.max_context_tokens,
                                        config.agent.max_tool_rounds,
                                        config.agent.max_task_duration_secs,
                                        config.agent.max_spend_usd,
                                    )
                                    .with_v2_optimizations(config.agent.v2_optimizations)
                                    .with_parallel_phases(config.agent.parallel_phases)
                                    .with_shared_mode(shared_mode.clone())
                                    .with_shared_memory_strategy(shared_memory_strategy.clone()),
                                );
                                *agent_state.write().await = Some(agent);
                                tracing::info!(provider = "openai-codex", model = %model, "Agent initialized via Codex OAuth");
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Codex OAuth tokens exist but failed to load — starting in onboarding mode");
                            }
                        }
                    } else {
                        tracing::info!("No API key — starting in onboarding mode");
                    }
                }
                #[cfg(not(feature = "codex-oauth"))]
                {
                    tracing::info!("No API key — starting in onboarding mode");
                }
            }

            // ── Unified message channel ────────────────────────
            let (msg_tx, mut msg_rx) =
                tokio::sync::mpsc::channel::<temm1e_core::types::message::InboundMessage>(32);

            // Track spawned task handles for graceful shutdown
            let mut task_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();

            // Wire Telegram messages into the unified channel
            if let Some(mut tg_rx) = tg_rx {
                let tx = msg_tx.clone();
                task_handles.push(tokio::spawn(async move {
                    while let Some(msg) = tg_rx.recv().await {
                        if tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                }));
            }

            // ── Workspace ──────────────────────────────────────
            let workspace_path = dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".temm1e")
                .join("workspace");
            if let Err(e) = std::fs::create_dir_all(&workspace_path) {
                tracing::warn!(error = %e, path = %workspace_path.display(), "Failed to create directory");
            }

            // ── Heartbeat ──────────────────────────────────────
            if config.heartbeat.enabled {
                let heartbeat_chat_id = config
                    .heartbeat
                    .report_to
                    .clone()
                    .unwrap_or_else(|| "heartbeat".to_string());
                let runner = temm1e_automation::HeartbeatRunner::new(
                    config.heartbeat.clone(),
                    workspace_path.clone(),
                    heartbeat_chat_id,
                );
                let hb_tx = msg_tx.clone();
                task_handles.push(tokio::spawn(async move {
                    runner.run(hb_tx).await;
                }));
                tracing::info!(
                    interval = %config.heartbeat.interval,
                    checklist = %config.heartbeat.checklist,
                    "Heartbeat runner started"
                );
            }

            // ── Hive pack initialization (if enabled) ────────
            let hive_config: temm1e_hive::HiveConfig = {
                // Parse [hive] section from the same config file.
                // If absent or malformed, defaults to enabled=false (inert).
                let hive_toml = config_path
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .or_else(|| {
                        let home = dirs::home_dir()?;
                        std::fs::read_to_string(home.join(".temm1e/config.toml")).ok()
                    })
                    .or_else(|| std::fs::read_to_string("temm1e.toml").ok());
                if let Some(ref content) = hive_toml {
                    #[derive(serde::Deserialize, Default)]
                    struct HiveWrapper {
                        #[serde(default)]
                        hive: temm1e_hive::HiveConfig,
                    }
                    toml::from_str::<HiveWrapper>(content)
                        .map(|w| w.hive)
                        .unwrap_or_default()
                } else {
                    temm1e_hive::HiveConfig::default()
                }
            };

            let hive_instance: Option<Arc<temm1e_hive::Hive>> = if hive_config.enabled {
                let hive_db = dirs::home_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".temm1e/hive.db");
                let hive_url = format!("sqlite:{}?mode=rwc", hive_db.display());
                match temm1e_hive::Hive::new(&hive_config, &hive_url).await {
                    Ok(h) => {
                        tracing::info!(
                            max_workers = hive_config.max_workers,
                            threshold = hive_config.swarm_threshold_speedup,
                            "Many Tems initialized (Swarm Intelligence)"
                        );
                        Some(Arc::new(h))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Hive init failed — pack disabled");
                        None
                    }
                }
            } else {
                None
            };

            let hive_enabled_flag = hive_instance.is_some();

            // ── Per-chat serial executor ───────────────────────

            /// Tracks the active task state for a single chat.
            struct ChatSlot {
                tx: tokio::sync::mpsc::Sender<temm1e_core::types::message::InboundMessage>,
                interrupt: Arc<AtomicBool>,
                is_heartbeat: Arc<AtomicBool>,
                is_busy: Arc<AtomicBool>,
                current_task: Arc<std::sync::Mutex<String>>,
                cancel_token: tokio_util::sync::CancellationToken,
            }

            if let Some(sender) = primary_channel.clone() {
                let agent_state_clone = agent_state.clone();
                let memory_clone = memory.clone();
                let tools_clone = tools.clone();
                let custom_registry_clone = custom_tool_registry.clone();
                #[cfg(feature = "mcp")]
                let mcp_manager_clone = mcp_manager.clone();
                let agent_max_turns = config.agent.max_turns;
                let agent_max_context_tokens = config.agent.max_context_tokens;
                let agent_max_tool_rounds = config.agent.max_tool_rounds;
                let agent_max_task_duration = config.agent.max_task_duration_secs;
                let agent_max_spend_usd = config.agent.max_spend_usd;
                let agent_v2_opt = config.agent.v2_optimizations;
                let agent_parallel_phases = config.agent.parallel_phases;
                let provider_base_url = config.provider.base_url.clone();
                let ws_path = workspace_path.clone();
                let pending_clone = pending_messages.clone();
                let setup_tokens_clone = setup_tokens.clone();
                let pending_raw_keys_clone = pending_raw_keys.clone();
                let usage_store_clone = usage_store.clone();
                let hive_clone = hive_instance.clone();

                let chat_slots: Arc<Mutex<HashMap<String, ChatSlot>>> =
                    Arc::new(Mutex::new(HashMap::new()));

                let msg_tx_redispatch = msg_tx.clone();
                task_handles.push(tokio::spawn(async move {
                    while let Some(inbound) = msg_rx.recv().await {
                        let chat_id = inbound.chat_id.clone();
                        let is_heartbeat_msg = inbound.channel == "heartbeat";

                        let mut slots = chat_slots.lock().await;

                        // Handle user messages while a task is active
                        if !is_heartbeat_msg {
                            if let Some(slot) = slots.get(&chat_id) {
                                if slot.is_heartbeat.load(Ordering::Relaxed) {
                                    tracing::info!(
                                        chat_id = %chat_id,
                                        "User message preempting active heartbeat task"
                                    );
                                    slot.interrupt.store(true, Ordering::Relaxed);
                                    slot.cancel_token.cancel();
                                }

                                // /stop is the only hardcoded instant-kill.
                                // All other cancel intent is handled by the
                                // LLM interceptor (intention-based, not word matching).
                                let is_slash_stop = inbound.text.as_deref()
                                    .map(|t| t.trim().eq_ignore_ascii_case("/stop"))
                                    .unwrap_or(false);

                                if is_slash_stop {
                                    tracing::info!(
                                        chat_id = %chat_id,
                                        "/stop command — interrupting active task"
                                    );
                                    slot.interrupt.store(true, Ordering::Relaxed);
                                    slot.cancel_token.cancel();
                                    continue;
                                }

                                // Only intercept when worker is actively processing.
                                // When idle (waiting on chat_rx), let the message
                                // fall through to the worker channel.
                                if slot.is_busy.load(Ordering::Relaxed) {
                                    // Push to pending queue ONLY when busy — the runtime
                                    // injects these into tool results so the working LLM
                                    // sees them. If not busy, the message goes directly
                                    // to the worker channel below.
                                    if let Some(text) = inbound.text.as_deref() {
                                        if let Ok(mut pq) = pending_clone.lock() {
                                            pq.entry(chat_id.clone())
                                                .or_default()
                                                .push(text.to_string());
                                        }
                                    }
                                    // LLM interceptor — runs on a separate task.
                                    // Can chat, give status, or cancel the active task.
                                    let icpt_sender = sender.clone();
                                    let icpt_chat_id = chat_id.clone();
                                    let icpt_msg_id = inbound.id.clone();
                                    let icpt_msg_text = inbound.text.clone().unwrap_or_default();
                                    let icpt_interrupt = slot.interrupt.clone();
                                    let icpt_cancel = slot.cancel_token.clone();
                                    let icpt_task = slot.current_task.clone();
                                    let icpt_agent_state = agent_state_clone.clone();
                                    tokio::spawn(async move {
                                        let task_desc = icpt_task.lock()
                                            .map(|t| t.clone())
                                            .unwrap_or_default();

                                        // Get provider + model from the active agent
                                        let agent_guard = icpt_agent_state.read().await;
                                        let Some(agent) = agent_guard.as_ref() else { return; };
                                        let provider = agent.provider_arc();
                                        let model = agent.model().to_string();
                                        drop(agent_guard);

                                        let soul = build_system_prompt();
                                        let request = temm1e_core::types::message::CompletionRequest {
                                            model,
                                            system: Some(format!(
                                                "{}\n\n\
                                                 === INTERCEPTOR MODE ===\n\
                                                 You are running as Tem's INTERCEPTOR right now. Your main self is busy \
                                                 working on a task. The user sent a message while that task is running.\n\n\
                                                 Current task: \"{}\"\n\n\
                                                 Interceptor rules:\n\
                                                 - Keep it SHORT (1-3 sentences max)\n\
                                                 - If the user wants to CANCEL/STOP the task, include the exact token [CANCEL] at the very end of your response\n\
                                                 - If the user asks about progress, explain what the task involves based on its description\n\
                                                 - If the user is chatting casually, respond warmly\n\
                                                 - NEVER use [CANCEL] unless the user clearly wants to stop\n\
                                                 === END INTERCEPTOR ===",
                                                soul, task_desc
                                            )),
                                            messages: vec![
                                                temm1e_core::types::message::ChatMessage {
                                                    role: temm1e_core::types::message::Role::User,
                                                    content: temm1e_core::types::message::MessageContent::Text(icpt_msg_text),
                                                },
                                            ],
                                            tools: vec![],
                                            max_tokens: None,
                                            temperature: Some(0.7),
                                        };

                                        match provider.complete(request).await {
                                            Ok(resp) => {
                                                let mut text = resp.content.iter()
                                                    .filter_map(|p| match p {
                                                        temm1e_core::types::message::ContentPart::Text { text } => Some(text.as_str()),
                                                        _ => None,
                                                    })
                                                    .collect::<Vec<_>>()
                                                    .join("");

                                                let should_cancel = text.contains("[CANCEL]");
                                                text = text.replace("[CANCEL]", "").trim().to_string();

                                                if !text.is_empty() {
                                                    let reply = temm1e_core::types::message::OutboundMessage {
                                                        chat_id: icpt_chat_id.clone(),
                                                        text,
                                                        reply_to: Some(icpt_msg_id),
                                                        parse_mode: None,
                                                    };
                                                    let _ = icpt_sender.send_message(reply).await;
                                                }

                                                if should_cancel {
                                                    icpt_interrupt.store(true, Ordering::Relaxed);
                                                    icpt_cancel.cancel();
                                                    tracing::info!(
                                                        chat_id = %icpt_chat_id,
                                                        "Interceptor cancelled active task"
                                                    );
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    error = %e,
                                                    "Interceptor LLM call failed — skipping"
                                                );
                                            }
                                        }
                                    });
                                    continue;
                                }
                            }
                        }

                        // Skip heartbeat if chat is busy
                        if is_heartbeat_msg {
                            if let Some(slot) = slots.get(&chat_id) {
                                if slot.tx.try_send(inbound).is_err() {
                                    tracing::debug!(
                                        chat_id = %chat_id,
                                        "Skipping heartbeat — chat is busy"
                                    );
                                }
                                continue;
                            }
                        }

                        // Ensure a worker exists for this chat_id
                        let shared_mode_for_worker = shared_mode.clone();
                        let shared_memory_strategy_for_worker = shared_memory_strategy.clone();
                        let slot = slots.entry(chat_id.clone()).or_insert_with(|| {
                            let (chat_tx, mut chat_rx) =
                                tokio::sync::mpsc::channel::<temm1e_core::types::message::InboundMessage>(4);

                            let interrupt = Arc::new(AtomicBool::new(false));
                            let is_heartbeat = Arc::new(AtomicBool::new(false));
                            let is_busy = Arc::new(AtomicBool::new(false));
                            let current_task: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));
                            let cancel_token = tokio_util::sync::CancellationToken::new();
                            let is_busy_clone = is_busy.clone();
                            let current_task_clone = current_task.clone();
                            let self_tx = chat_tx.clone();

                            let agent_state = agent_state_clone.clone();
                            let memory = memory_clone.clone();
                            let tools_template = tools_clone.clone();
                            let custom_registry = custom_registry_clone.clone();
                            #[cfg(feature = "mcp")]
                            let mcp_mgr = mcp_manager_clone.clone();
                            let max_turns = agent_max_turns;
                            let max_ctx = agent_max_context_tokens;
                            let max_rounds = agent_max_tool_rounds;
                            let max_task_duration = agent_max_task_duration;
                            let max_spend = agent_max_spend_usd;
                            let v2_opt = agent_v2_opt;
                            let pp_opt = agent_parallel_phases;
                            let hive_on = hive_enabled_flag;
                            let base_url = provider_base_url.clone();
                            let sender = sender.clone();
                            let workspace_path = ws_path.clone();
                            let interrupt_clone = interrupt.clone();
                            let is_heartbeat_clone = is_heartbeat.clone();
                            let cancel_token_clone = cancel_token.clone();
                            let pending_for_worker = pending_clone.clone();
                            let shared_mode = shared_mode_for_worker;
                            let shared_memory_strategy = shared_memory_strategy_for_worker;
                            let setup_tokens_worker = setup_tokens_clone.clone();
                            let pending_raw_keys_worker = pending_raw_keys_clone.clone();
                            let usage_store_worker = usage_store_clone.clone();
                            let hive_worker = hive_clone.clone();
                            let worker_chat_id = chat_id.clone();

                            tokio::spawn(async move {
                                // ── Restore conversation history from memory backend ──
                                let history_key = format!("chat_history:{}", worker_chat_id);
                                let mut persistent_history: Vec<temm1e_core::types::message::ChatMessage> =
                                    match memory.get(&history_key).await {
                                        Ok(Some(entry)) => {
                                            match serde_json::from_str(&entry.content) {
                                                Ok(h) => {
                                                    tracing::info!(
                                                        chat_id = %worker_chat_id,
                                                        messages = %Vec::<temm1e_core::types::message::ChatMessage>::len(&h),
                                                        "Restored conversation history from memory"
                                                    );
                                                    h
                                                }
                                                Err(e) => {
                                                    tracing::warn!(
                                                        chat_id = %worker_chat_id,
                                                        error = %e,
                                                        "Failed to deserialize saved history, starting fresh"
                                                    );
                                                    Vec::new()
                                                }
                                            }
                                        }
                                        Ok(None) => Vec::new(),
                                        Err(e) => {
                                            tracing::warn!(
                                                chat_id = %worker_chat_id,
                                                error = %e,
                                                "Failed to load saved history, starting fresh"
                                            );
                                            Vec::new()
                                        }
                                    };

                                while let Some(mut msg) = chat_rx.recv().await {
                                    // Snapshot for outer panic handler (msg is borrowed by async block)
                                    let panic_chat_id = msg.chat_id.clone();
                                    let panic_msg_id = msg.id.clone();

                                    let outer_catch_result = AssertUnwindSafe(async {
                                    let is_hb = msg.channel == "heartbeat";
                                    is_heartbeat_clone.store(is_hb, Ordering::Relaxed);
                                    interrupt_clone.store(false, Ordering::Relaxed);

                                    let interrupt_flag = Some(interrupt_clone.clone());

                                    // ── Phase 1: status watch + cancel token ──────
                                    // Watch channel created per-message; future phases
                                    // will expose the receiver to observers.
                                    let (status_tx, _status_rx) = tokio::sync::watch::channel(
                                        temm1e_agent::AgentTaskStatus::default(),
                                    );
                                    let cancel = cancel_token_clone.clone();

                                    // ── Commands — intercepted before agent ──────
                                    let msg_text_cmd = msg.text.as_deref().unwrap_or("");
                                    let cmd_lower = msg_text_cmd.trim().to_lowercase();

                                    // /addkey — secure OTK flow
                                    if cmd_lower == "/addkey" {
                                        let otk = setup_tokens_worker.generate(&msg.chat_id).await;
                                        let otk_hex = hex::encode(otk);
                                        let link = format!(
                                            "https://nagisanzenin.github.io/temm1e/setup#{}",
                                            otk_hex
                                        );
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: format!(
                                                "Secure key setup:\n\n\
                                                 1. Open this link:\n{}\n\n\
                                                 2. Paste your API key in the form\n\
                                                 3. Copy the encrypted blob\n\
                                                 4. Paste it back here\n\n\
                                                 Link expires in 10 minutes.\n\n\
                                                 For a quick (less secure) method: /addkey unsafe",
                                                link
                                            ),
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /addkey unsafe — raw key paste mode
                                    if cmd_lower == "/addkey unsafe" {
                                        pending_raw_keys_worker.lock().await.insert(msg.chat_id.clone());
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: "Paste your API key in the next message.\n\n\
                                                   Warning: the key will be visible in chat history.\n\
                                                   For a secure method, use /addkey instead."
                                                .to_string(),
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /keys — list configured providers
                                    if cmd_lower == "/keys" {
                                        let info = list_configured_providers();
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: info,
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /model [model-name] — list or switch models
                                    if cmd_lower == "/model" || cmd_lower.starts_with("/model ") {
                                        let args = if cmd_lower == "/model" {
                                            ""
                                        } else {
                                            msg_text_cmd.trim()["/model".len()..].trim()
                                        };
                                        let result = handle_model_command(args);
                                        let is_switch = result.starts_with("Model switched:");

                                        // If model was switched, reload agent immediately
                                        // (don't wait for file watcher)
                                        let final_text = if is_switch {
                                            // Check if this is a Codex OAuth model switch
                                            #[cfg(feature = "codex-oauth")]
                                            let codex_switch = result.contains("Codex OAuth");
                                            #[cfg(not(feature = "codex-oauth"))]
                                            let codex_switch = false;

                                            if codex_switch {
                                                #[cfg(feature = "codex-oauth")]
                                                {
                                                    // Extract target model from "Model switched: codex-oauth → <model>"
                                                    let new_model = result
                                                        .lines()
                                                        .next()
                                                        .and_then(|l| l.split("→ ").nth(1))
                                                        .unwrap_or("gpt-5.4")
                                                        .trim()
                                                        .to_string();
                                                    match temm1e_codex_oauth::TokenStore::load() {
                                                        Ok(store) => {
                                                            let token_store = std::sync::Arc::new(store);
                                                            let provider: Arc<dyn temm1e_core::Provider> =
                                                                Arc::new(temm1e_codex_oauth::CodexResponsesProvider::new(
                                                                    new_model.clone(),
                                                                    token_store,
                                                                ));
                                                            let new_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                                provider,
                                                                memory.clone(),
                                                                tools_template.clone(),
                                                                new_model.clone(),
                                                                Some(build_system_prompt()),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()));
                                                            *agent_state.write().await = Some(new_agent);
                                                            tracing::info!(
                                                                provider = "openai-codex",
                                                                model = %new_model,
                                                                "Agent reloaded via /model command (Codex OAuth)"
                                                            );
                                                            format!("Model switched → {}\nActive now.", new_model)
                                                        }
                                                        Err(e) => {
                                                            format!("Model switch failed: {}", e)
                                                        }
                                                    }
                                                }
                                                #[cfg(not(feature = "codex-oauth"))]
                                                { result }
                                            } else if let Some(creds) = load_credentials_file() {
                                                if let Some(prov) = creds.providers.iter().find(|p| p.name == creds.active) {
                                                    let valid_keys: Vec<String> = prov.keys.iter()
                                                        .filter(|k| !is_placeholder_key(k))
                                                        .cloned()
                                                        .collect();
                                                    let effective_base_url = prov.base_url.clone().or_else(|| base_url.clone());
                                                    let reload_config = temm1e_core::types::config::ProviderConfig {
                                                        name: Some(creds.active.clone()),
                                                        api_key: valid_keys.first().cloned(),
                                                        keys: valid_keys,
                                                        model: Some(prov.model.clone()),
                                                        base_url: effective_base_url,
                                                        extra_headers: std::collections::HashMap::new(),
                                                    };
                                                    match validate_provider_key(&reload_config).await {
                                                        Ok(validated_provider) => {
                                                            let new_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                                validated_provider,
                                                                memory.clone(),
                                                                tools_template.clone(),
                                                                prov.model.clone(),
                                                                Some(build_system_prompt()),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()));
                                                            *agent_state.write().await = Some(new_agent);
                                                            tracing::info!(
                                                                provider = %creds.active,
                                                                model = %prov.model,
                                                                "Agent reloaded via /model command"
                                                            );
                                                            format!("{}\nActive now.", result)
                                                        }
                                                        Err(err) => {
                                                            tracing::warn!(error = %err, "Model switch failed validation");
                                                            // Revert credentials
                                                            if let Some(old_agent) = agent_state.read().await.as_ref() {
                                                                let old_model = old_agent.model().to_string();
                                                                let mut rev = creds.clone();
                                                                for p in &mut rev.providers {
                                                                    if p.name == creds.active {
                                                                        p.model = old_model.clone();
                                                                    }
                                                                }
                                                                if let Ok(content) = toml::to_string_pretty(&rev) {
                                                                    let _ = std::fs::write(credentials_path(), &content);
                                                                }
                                                            }
                                                            format!("Model switch failed: {}\nReverted to previous model.", err)
                                                        }
                                                    }
                                                } else {
                                                    result
                                                }
                                            } else {
                                                result
                                            }
                                        } else {
                                            result
                                        };

                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: final_text,
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /removekey <provider>
                                    if cmd_lower.starts_with("/removekey") {
                                        let provider_arg = msg_text_cmd.trim()["/removekey".len()..].trim();
                                        let result = remove_provider(provider_arg);
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: result,
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;

                                        // If provider was removed, check if agent needs to go offline
                                        if !provider_arg.is_empty() && load_active_provider_keys().is_none() {
                                            *agent_state.write().await = None;
                                            tracing::info!("All providers removed — agent offline");
                                        }

                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /usage — show usage summary
                                    if cmd_lower == "/usage" {
                                        let summary_text = match usage_store_worker.usage_summary(&msg.chat_id).await {
                                            Ok(summary) => {
                                                if summary.turn_count == 0 {
                                                    "No usage records for this chat yet.".to_string()
                                                } else {
                                                    format!(
                                                        "Usage Summary\nTurns: {}\nAPI Calls: {}\nInput Tokens: {}\nOutput Tokens: {}\nCombined Tokens: {}\nTools Used: {}\nTotal Cost: ${:.4}",
                                                        summary.turn_count,
                                                        summary.total_api_calls,
                                                        summary.total_input_tokens,
                                                        summary.total_output_tokens,
                                                        summary.combined_tokens(),
                                                        summary.total_tools_used,
                                                        summary.total_cost_usd,
                                                    )
                                                }
                                            }
                                            Err(e) => format!("Failed to query usage: {}", e),
                                        };
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: summary_text,
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /help — list available commands
                                    if cmd_lower == "/help" {
                                        let help_text = format!("\
temm1e {} — commit: {} — date: {}\n\n\
Available commands:\n\n\
/help — Show this help message\n\
/addkey — Securely add an API key (encrypted OTK flow)\n\
/addkey unsafe — Add an API key by pasting directly\n\
/keys — List configured providers and active model\n\
/model — Show current model and available models\n\
/model <name> — Switch to a different model\n\
/removekey <provider> — Remove a provider's API key\n\
/usage — Show token usage and cost summary\n\
/memory — Show current memory strategy\n\
/memory lambda — Switch to λ-Memory (decay + persistence)\n\
/memory echo — Switch to Echo Memory (context window only)\n\
/mcp — List connected MCP servers and tools\n\
/mcp add <name> <command-or-url> — Connect a new MCP server\n\
/mcp remove <name> — Disconnect an MCP server\n\
/mcp restart <name> — Restart an MCP server\n\
/reload — Hot-reload config and agent (admin)\n\
/reset — Factory reset all local state (admin)\n\
/restart — Restart TEMM1E process (admin)\n\n\
Just type a message to chat with the AI agent.",
                                            env!("CARGO_PKG_VERSION"),
                                            env!("GIT_HASH"),
                                            env!("BUILD_DATE"),
                                        );
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: help_text.to_string(),
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /memory — switch memory strategy
                                    if cmd_lower == "/memory" || cmd_lower.starts_with("/memory ") {
                                        let args = if cmd_lower == "/memory" { "" } else { msg_text_cmd.trim()["/memory".len()..].trim() };
                                        let args_lower = args.to_lowercase();
                                        let response = if args_lower.is_empty() || args_lower == "status" {
                                            let current = shared_memory_strategy.read().await;
                                            format!(
                                                "Memory Strategy: {}\n\n\
                                                 Available strategies:\n\
                                                 • /memory lambda — λ-Memory: decay-scored, cross-session persistence, hash-based recall (default)\n\
                                                 • /memory echo — Echo Memory: keyword search over current context window, no persistence",
                                                *current,
                                            )
                                        } else if args_lower == "lambda" || args_lower == "λ" {
                                            *shared_memory_strategy.write().await = temm1e_core::types::config::MemoryStrategy::Lambda;
                                            "Switched to λ-Memory\nDecay-scored fidelity tiers • cross-session persistence • hash-based recall".to_string()
                                        } else if args_lower == "echo" {
                                            *shared_memory_strategy.write().await = temm1e_core::types::config::MemoryStrategy::Echo;
                                            "Switched to Echo Memory\nKeyword search over context window • no persistence between sessions".to_string()
                                        } else {
                                            "Unknown strategy. Use: /memory lambda or /memory echo".to_string()
                                        };
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: response,
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /mcp — manage MCP servers
                                    #[cfg(feature = "mcp")]
                                    if cmd_lower == "/mcp" || cmd_lower.starts_with("/mcp ") || cmd_lower.starts_with("/mcp@") {
                                        // Extract args: strip "/mcp" and optional "@botname" suffix
                                        let mcp_args = {
                                            let raw = msg_text_cmd.trim();
                                            let after_cmd = if raw.len() > 4 {
                                                &raw[4..] // skip "/mcp"
                                            } else {
                                                ""
                                            };
                                            // Strip @botname if present (e.g., "/mcp@my_bot add ...")
                                            let after_bot = if let Some(space_pos) = after_cmd.find(' ') {
                                                if after_cmd.starts_with('@') {
                                                    &after_cmd[space_pos..]
                                                } else {
                                                    after_cmd
                                                }
                                            } else if after_cmd.starts_with('@') {
                                                "" // just "/mcp@botname" with no args
                                            } else {
                                                after_cmd
                                            };
                                            after_bot.trim()
                                        };
                                        let mcp_args_lower = mcp_args.to_lowercase();
                                        tracing::debug!(mcp_args = %mcp_args, "Parsing /mcp command");

                                        let mcp_reply = if mcp_args.is_empty() || mcp_args_lower == "list" {
                                            mcp_mgr.list_servers().await
                                        } else if mcp_args_lower.starts_with("add ") {
                                            let add_rest = mcp_args["add ".len()..].trim();
                                            let parts: Vec<&str> = add_rest.splitn(2, ' ').collect();
                                            if parts.len() < 2 || parts[1].trim().is_empty() {
                                                "Usage: /mcp add <name> <command-or-url>\n\n\
                                                 Examples:\n\
                                                 • /mcp add playwright npx @playwright/mcp@latest\n\
                                                 • /mcp add filesystem npx -y @modelcontextprotocol/server-filesystem /path\n\
                                                 • /mcp add myapi https://mcp.example.com/sse\n\n\
                                                 Note: The command must be an MCP server (not a GitHub URL).\n\
                                                 For Playwright: npx @playwright/mcp@latest\n\
                                                 For other servers: check the package's README for the MCP command.".to_string()
                                            } else {
                                                let name = parts[0];
                                                let target = parts[1].trim();

                                                // Warn if target looks like a GitHub repo URL (not an MCP endpoint)
                                                if target.contains("github.com/") && !target.contains("/sse") && !target.contains("/mcp") {
                                                    format!(
                                                        "That looks like a GitHub repository URL, not an MCP server endpoint.\n\n\
                                                         To use an MCP server, you need the command to run it. For example:\n\
                                                         • /mcp add {} npx @playwright/mcp@latest\n\
                                                         • /mcp add {} npx -y @modelcontextprotocol/server-filesystem /path\n\n\
                                                         Check the repo's README for the correct MCP server command.",
                                                        name, name
                                                    )
                                                } else {
                                                    let config = if target.starts_with("http://") || target.starts_with("https://") {
                                                        temm1e_mcp::McpServerConfig::http(name, target)
                                                    } else {
                                                        let cmd_parts: Vec<&str> = target.split_whitespace().collect();
                                                        let command = cmd_parts[0];
                                                        let args: Vec<String> = cmd_parts[1..].iter().map(|s| s.to_string()).collect();
                                                        temm1e_mcp::McpServerConfig::stdio(name, command, args)
                                                    };
                                                    match mcp_mgr.add_server(config).await {
                                                        Ok(count) => {
                                                            if let Some(agent) = agent_state.read().await.as_ref() {
                                                                let tool_names: Vec<String> = tools_template.iter().map(|t| t.name().to_string()).collect();
                                                                let mut new_tools = tools_template.clone();
                                                                let mcp_tools = mcp_mgr.bridge_tools(&tool_names).await;
                                                                new_tools.extend(mcp_tools);
                                                                new_tools.push(Arc::new(temm1e_mcp::McpManageTool::new(mcp_mgr.clone())));
                                                                new_tools.push(Arc::new(temm1e_mcp::SelfExtendTool::new()));
                                                                new_tools.push(Arc::new(temm1e_mcp::SelfAddMcpTool::new(mcp_mgr.clone())));
                                                                let new_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                                    agent.provider_arc(),
                                                                    memory.clone(),
                                                                    new_tools,
                                                                    agent.model().to_string(),
                                                                    Some(build_system_prompt()),
                                                                    max_turns, max_ctx, max_rounds, max_task_duration, max_spend,
                                                                ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()));
                                                                *agent_state.write().await = Some(new_agent);
                                                            }
                                                            mcp_mgr.take_tools_changed();
                                                            format!("MCP server '{}' connected with {} tools. New tools are now available.", name, count)
                                                        }
                                                        Err(e) => format!("Failed to add MCP server: {}", e),
                                                    }
                                                }
                                            }
                                        } else if mcp_args_lower.starts_with("remove ") {
                                            let name = mcp_args["remove ".len()..].trim();
                                            match mcp_mgr.remove_server(name).await {
                                                Ok(()) => {
                                                    if let Some(agent) = agent_state.read().await.as_ref() {
                                                        let tool_names: Vec<String> = tools_template.iter().map(|t| t.name().to_string()).collect();
                                                        let mut new_tools = tools_template.clone();
                                                        let mcp_tools = mcp_mgr.bridge_tools(&tool_names).await;
                                                        new_tools.extend(mcp_tools);
                                                        new_tools.push(Arc::new(temm1e_mcp::McpManageTool::new(mcp_mgr.clone())));
                                                                new_tools.push(Arc::new(temm1e_mcp::SelfExtendTool::new()));
                                                                new_tools.push(Arc::new(temm1e_mcp::SelfAddMcpTool::new(mcp_mgr.clone())));
                                                        let new_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                            agent.provider_arc(),
                                                            memory.clone(),
                                                            new_tools,
                                                            agent.model().to_string(),
                                                            Some(build_system_prompt()),
                                                            max_turns, max_ctx, max_rounds, max_task_duration, max_spend,
                                                        ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()));
                                                        *agent_state.write().await = Some(new_agent);
                                                    }
                                                    mcp_mgr.take_tools_changed();
                                                    format!("MCP server '{}' removed.", name)
                                                }
                                                Err(e) => format!("Failed to remove MCP server: {}", e),
                                            }
                                        } else if mcp_args_lower.starts_with("restart ") {
                                            let name = mcp_args["restart ".len()..].trim();
                                            match mcp_mgr.restart_server(name).await {
                                                Ok(count) => {
                                                    if let Some(agent) = agent_state.read().await.as_ref() {
                                                        let tool_names: Vec<String> = tools_template.iter().map(|t| t.name().to_string()).collect();
                                                        let mut new_tools = tools_template.clone();
                                                        let mcp_tools = mcp_mgr.bridge_tools(&tool_names).await;
                                                        new_tools.extend(mcp_tools);
                                                        new_tools.push(Arc::new(temm1e_mcp::McpManageTool::new(mcp_mgr.clone())));
                                                                new_tools.push(Arc::new(temm1e_mcp::SelfExtendTool::new()));
                                                                new_tools.push(Arc::new(temm1e_mcp::SelfAddMcpTool::new(mcp_mgr.clone())));
                                                        let new_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                            agent.provider_arc(),
                                                            memory.clone(),
                                                            new_tools,
                                                            agent.model().to_string(),
                                                            Some(build_system_prompt()),
                                                            max_turns, max_ctx, max_rounds, max_task_duration, max_spend,
                                                        ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()));
                                                        *agent_state.write().await = Some(new_agent);
                                                    }
                                                    mcp_mgr.take_tools_changed();
                                                    format!("MCP server '{}' restarted with {} tools.", name, count)
                                                }
                                                Err(e) => format!("Failed to restart MCP server: {}", e),
                                            }
                                        } else {
                                            "Usage: /mcp [list|add|remove|restart]\n\n\
                                             /mcp — List all MCP servers\n\
                                             /mcp add <name> <command> — Add a stdio MCP server\n\
                                             /mcp add <name> <url> — Add an HTTP MCP server\n\
                                             /mcp remove <name> — Remove a server\n\
                                             /mcp restart <name> — Restart a server\n\n\
                                             Examples:\n\
                                             /mcp add playwright npx @playwright/mcp@latest\n\
                                             /mcp add myapi https://mcp.example.com/sse".to_string()
                                        };
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: mcp_reply,
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /reload — hot-reload config and rebuild agent (admin only)
                                    if cmd_lower == "/reload" {
                                        if !is_admin_user(&msg.user_id) {
                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                chat_id: msg.chat_id.clone(),
                                                text: "Only the admin can use /reload.".to_string(),
                                                reply_to: Some(msg.id.clone()),
                                                parse_mode: None,
                                            };
                                            send_with_retry(&*sender, reply).await;
                                            is_heartbeat_clone.store(false, Ordering::Relaxed);
                                            return;
                                        }

                                        tracing::info!(chat_id = %msg.chat_id, "Reload requested via /reload command");

                                        let reload_result: String = if let Some(creds) = load_credentials_file() {
                                            if let Some(prov) = creds.providers.iter().find(|p| p.name == creds.active).or_else(|| creds.providers.first()) {
                                                let valid_keys: Vec<String> = prov.keys.iter()
                                                    .filter(|k| !is_placeholder_key(k))
                                                    .cloned()
                                                    .collect();
                                                if valid_keys.is_empty() {
                                                    "Reload failed: no valid API keys found.".to_string()
                                                } else {
                                                    let effective_base_url = prov.base_url.clone().or_else(|| base_url.clone());
                                                    let reload_config = temm1e_core::types::config::ProviderConfig {
                                                        name: Some(prov.name.clone()),
                                                        api_key: valid_keys.first().cloned(),
                                                        keys: valid_keys,
                                                        model: Some(prov.model.clone()),
                                                        base_url: effective_base_url,
                                                        extra_headers: std::collections::HashMap::new(),
                                                    };
                                                    match validate_provider_key(&reload_config).await {
                                                        Ok(validated_provider) => {
                                                            let new_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                                validated_provider,
                                                                memory.clone(),
                                                                tools_template.clone(),
                                                                prov.model.clone(),
                                                                Some(build_system_prompt()),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()));
                                                            *agent_state.write().await = Some(new_agent);
                                                            tracing::info!(
                                                                provider = %prov.name,
                                                                model = %prov.model,
                                                                "Agent reloaded via /reload command"
                                                            );
                                                            format!(
                                                                "Reloaded successfully.\n  Provider: {}\n  Model: {}",
                                                                prov.name, prov.model
                                                            )
                                                        }
                                                        Err(err) => {
                                                            tracing::warn!(error = %err, "Reload failed — agent unchanged");
                                                            format!("Reload failed: {}\nAgent unchanged — still running on previous config.", err)
                                                        }
                                                    }
                                                }
                                            } else {
                                                "Reload failed: no providers configured.".to_string()
                                            }
                                        } else {
                                            "Reload failed: no credentials file found.".to_string()
                                        };

                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: reload_result,
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /reset — factory reset from messaging (admin only)
                                    if cmd_lower == "/reset" {
                                        if !is_admin_user(&msg.user_id) {
                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                chat_id: msg.chat_id.clone(),
                                                text: "Only the admin can use /reset.".to_string(),
                                                reply_to: Some(msg.id.clone()),
                                                parse_mode: None,
                                            };
                                            send_with_retry(&*sender, reply).await;
                                            is_heartbeat_clone.store(false, Ordering::Relaxed);
                                            return;
                                        }

                                        tracing::info!(chat_id = %msg.chat_id, "Factory reset requested via /reset command");

                                        let data_dir = dirs::home_dir()
                                            .unwrap_or_else(|| std::path::PathBuf::from("."))
                                            .join(".temm1e");

                                        let reset_result = if !data_dir.exists() {
                                            "Nothing to reset — no local state found.".to_string()
                                        } else {
                                            // Backup before wipe
                                            let timestamp = chrono::Utc::now().format("%Y%m%d_%H%M%S");
                                            let backup_dir = dirs::home_dir()
                                                .unwrap_or_else(|| std::path::PathBuf::from("."))
                                                .join(format!(".temm1e.bak.{}", timestamp));

                                            fn copy_dir_recursive(src: &std::path::Path, dst: &std::path::Path) -> std::io::Result<()> {
                                                std::fs::create_dir_all(dst)?;
                                                for entry in std::fs::read_dir(src)? {
                                                    let entry = entry?;
                                                    let src_path = entry.path();
                                                    let dst_path = dst.join(entry.file_name());
                                                    if src_path.is_dir() {
                                                        copy_dir_recursive(&src_path, &dst_path)?;
                                                    } else {
                                                        std::fs::copy(&src_path, &dst_path)?;
                                                    }
                                                }
                                                Ok(())
                                            }

                                            match copy_dir_recursive(&data_dir, &backup_dir) {
                                                Ok(()) => {
                                                    match std::fs::remove_dir_all(&data_dir) {
                                                        Ok(()) => {
                                                            let _ = std::fs::create_dir_all(&data_dir);
                                                            *agent_state.write().await = None;
                                                            format!(
                                                                "Factory reset complete.\nBackup: {}\n\nUse /restart to reboot, or send /addkey to reconfigure.",
                                                                backup_dir.display()
                                                            )
                                                        }
                                                        Err(e) => format!("Reset failed: {}\nBackup at: {}", e, backup_dir.display()),
                                                    }
                                                }
                                                Err(e) => format!("Reset aborted — backup failed: {}\nYour data is untouched.", e),
                                            }
                                        };

                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: reset_result,
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /restart — restart the TEMM1E process, server mode (admin only)
                                    if cmd_lower == "/restart" {
                                        if !is_admin_user(&msg.user_id) {
                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                chat_id: msg.chat_id.clone(),
                                                text: "Only the admin can use /restart.".to_string(),
                                                reply_to: Some(msg.id.clone()),
                                                parse_mode: None,
                                            };
                                            send_with_retry(&*sender, reply).await;
                                            is_heartbeat_clone.store(false, Ordering::Relaxed);
                                            return;
                                        }

                                        tracing::info!(
                                            chat_id = %msg.chat_id,
                                            "Restart requested via /restart command"
                                        );
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: "Restarting TEMM1E... I'll be back in a few seconds.".to_string(),
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;

                                        // Spawn a delayed restart: wait for this process to exit,
                                        // then start a new one. Cross-platform.
                                        let exe = std::env::current_exe()
                                            .unwrap_or_else(|_| std::path::PathBuf::from("temm1e"));
                                        let exe_str = exe.to_string_lossy().to_string();

                                        #[cfg(unix)]
                                        {
                                            let _ = std::process::Command::new("sh")
                                                .arg("-c")
                                                .arg(format!("sleep 2 && \"{}\" start", exe_str))
                                                .stdin(std::process::Stdio::null())
                                                .stdout(std::process::Stdio::null())
                                                .stderr(std::process::Stdio::null())
                                                .spawn();
                                        }
                                        #[cfg(windows)]
                                        {
                                            use std::os::windows::process::CommandExt;
                                            let _ = std::process::Command::new("cmd")
                                                .args(["/C", &format!("timeout /t 2 /nobreak >nul & \"{}\" start", exe_str)])
                                                .stdin(std::process::Stdio::null())
                                                .stdout(std::process::Stdio::null())
                                                .stderr(std::process::Stdio::null())
                                                .creation_flags(0x00000008) // DETACHED_PROCESS
                                                .spawn();
                                        }

                                        // Give the reply a moment to flush, then exit
                                        tokio::time::sleep(std::time::Duration::from_millis(500)).await;
                                        tracing::info!("Exiting for restart");
                                        std::process::exit(0);
                                    }

                                    // enc:v1: — encrypted blob from OTK flow
                                    if msg_text_cmd.trim().starts_with("enc:v1:") {
                                        let blob_b64 = &msg_text_cmd.trim()["enc:v1:".len()..];
                                        match decrypt_otk_blob(blob_b64, &setup_tokens_worker, &msg.chat_id).await {
                                            Ok(api_key_text) => {
                                                // Treat the decrypted text as an API key
                                                if let Some(cred) = detect_api_key(&api_key_text) {
                                                    let model = default_model(cred.provider).to_string();
                                                    let effective_base_url = cred.base_url.clone().or_else(|| base_url.clone());
                                                    let test_config = temm1e_core::types::config::ProviderConfig {
                                                        name: Some(cred.provider.to_string()),
                                                        api_key: Some(cred.api_key.clone()),
                                                        keys: vec![cred.api_key.clone()],
                                                        model: Some(model.clone()),
                                                        base_url: effective_base_url,
                                                        extra_headers: std::collections::HashMap::new(),
                                                    };
                                                    match validate_provider_key(&test_config).await {
                                                        Ok(validated_provider) => {
                                                            if let Err(e) = save_credentials(cred.provider, &cred.api_key, &model, cred.base_url.as_deref()).await {
                                                                tracing::error!(error = %e, "Failed to save credentials from OTK flow");
                                                            }
                                                            let new_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                                validated_provider,
                                                                memory.clone(),
                                                                tools_template.clone(),
                                                                model.clone(),
                                                                Some(build_system_prompt()),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()));
                                                            *agent_state.write().await = Some(new_agent);
                                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                                chat_id: msg.chat_id.clone(),
                                                                text: format!(
                                                                    "API key securely received and verified! Configured {} with model {}.\n\nTEMM1E is online.",
                                                                    cred.provider, model
                                                                ),
                                                                reply_to: Some(msg.id.clone()),
                                                                parse_mode: None,
                                                            };
                                                            send_with_retry(&*sender, reply).await;
                                                            tracing::info!(provider = %cred.provider, "OTK key validated — agent online");
                                                        }
                                                        Err(err) => {
                                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                                chat_id: msg.chat_id.clone(),
                                                                text: format!(
                                                                    "Key decrypted but validation failed — {} returned:\n{}\n\nCheck the key and try /addkey again.",
                                                                    cred.provider, err
                                                                ),
                                                                reply_to: Some(msg.id.clone()),
                                                                parse_mode: None,
                                                            };
                                                            send_with_retry(&*sender, reply).await;
                                                        }
                                                    }
                                                } else {
                                                    let reply = temm1e_core::types::message::OutboundMessage {
                                                        chat_id: msg.chat_id.clone(),
                                                        text: "Decrypted successfully but couldn't detect the provider. \
                                                               Make sure you pasted a valid API key in the setup page."
                                                            .to_string(),
                                                        reply_to: Some(msg.id.clone()),
                                                        parse_mode: None,
                                                    };
                                                    send_with_retry(&*sender, reply).await;
                                                }
                                            }
                                            Err(err) => {
                                                let reply = temm1e_core::types::message::OutboundMessage {
                                                    chat_id: msg.chat_id.clone(),
                                                    text: err,
                                                    reply_to: Some(msg.id.clone()),
                                                    parse_mode: None,
                                                };
                                                send_with_retry(&*sender, reply).await;
                                            }
                                        }
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        if let Ok(mut pq) = pending_for_worker.lock() {
                                            pq.remove(&worker_chat_id);
                                        }
                                        return;
                                    }

                                    // Pending raw key paste (from /addkey unsafe)
                                    if pending_raw_keys_worker.lock().await.remove(&msg.chat_id) {
                                        // Treat the message as a raw API key — falls through
                                        // to the normal detect_api_key path below
                                    }

                                    // Check if agent is available
                                    let agent = {
                                        let guard = agent_state.read().await;
                                        guard.as_ref().cloned()
                                    };

                                    if let Some(agent) = agent {
                                        // ── Detect new API key mid-conversation ────
                                        let msg_text_peek = msg.text.as_deref().unwrap_or("");
                                        if let Some(cred) = detect_api_key(msg_text_peek) {
                                            let model = default_model(cred.provider).to_string();
                                            let effective_base_url = cred.base_url.clone().or_else(|| base_url.clone());

                                            // Validate the key BEFORE saving — don't brick the agent
                                            let test_config = temm1e_core::types::config::ProviderConfig {
                                                name: Some(cred.provider.to_string()),
                                                api_key: Some(cred.api_key.clone()),
                                                keys: vec![cred.api_key.clone()],
                                                model: Some(model.clone()),
                                                base_url: effective_base_url,
                                                extra_headers: std::collections::HashMap::new(),
                                            };

                                            match validate_provider_key(&test_config).await {
                                                Ok(_validated_provider) => {
                                                    // Key is valid — now save and reload with all keys
                                                    if let Err(e) = save_credentials(cred.provider, &cred.api_key, &model, cred.base_url.as_deref()).await {
                                                        tracing::error!(error = %e, "Failed to save new key");
                                                    } else if let Some((name, keys, mdl, saved_base_url)) = load_active_provider_keys() {
                                                        let reload_base_url = saved_base_url.or_else(|| base_url.clone());
                                                        let reload_config = temm1e_core::types::config::ProviderConfig {
                                                            name: Some(name.clone()),
                                                            api_key: keys.first().cloned(),
                                                            keys: keys.clone(),
                                                            model: Some(mdl.clone()),
                                                            base_url: reload_base_url,
                                                            extra_headers: std::collections::HashMap::new(),
                                                        };
                                                        if let Ok(new_provider) = temm1e_providers::create_provider(&reload_config) {
                                                            let new_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                                Arc::from(new_provider),
                                                                memory.clone(),
                                                                tools_template.clone(),
                                                                mdl.clone(),
                                                                Some(build_system_prompt()),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()));
                                                            *agent_state.write().await = Some(new_agent);
                                                            let key_count = keys.len();
                                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                                chat_id: msg.chat_id.clone(),
                                                                text: format!(
                                                                    "Key verified and added for {}! Now using {} key{} with model {}.",
                                                                    name, key_count,
                                                                    if key_count > 1 { "s (rotation on error)" } else { "" },
                                                                    mdl
                                                                ),
                                                                reply_to: Some(msg.id.clone()),
                                                                parse_mode: None,
                                                            };
                                                            send_with_retry(&*sender, reply).await;
                                                            tracing::info!(
                                                                provider = %name,
                                                                key_count = key_count,
                                                                "Mid-conversation key validated and added — agent reloaded"
                                                            );
                                                        }
                                                    }
                                                }
                                                Err(err) => {
                                                    // Key is invalid — DO NOT save, DO NOT switch
                                                    let reply = temm1e_core::types::message::OutboundMessage {
                                                        chat_id: msg.chat_id.clone(),
                                                        text: format!(
                                                            "Invalid API key — {} returned an error:\n{}\n\nThe current provider is still active. Check the key and try again.",
                                                            cred.provider, err
                                                        ),
                                                        reply_to: Some(msg.id.clone()),
                                                        parse_mode: None,
                                                    };
                                                    send_with_retry(&*sender, reply).await;
                                                    tracing::warn!(
                                                        provider = %cred.provider,
                                                        error = %err,
                                                        "Mid-conversation key rejected — validation failed"
                                                    );
                                                }
                                            }

                                            // Skip processing the key message as a normal prompt
                                            is_heartbeat_clone.store(false, Ordering::Relaxed);
                                            interrupt_clone.store(false, Ordering::Relaxed);
                                            if let Ok(mut pq) = pending_for_worker.lock() {
                                                pq.remove(&worker_chat_id);
                                            }
                                            return;
                                        }

                                        // ── Normal mode: process with agent ────

                                        // Download attachments
                                        if !msg.attachments.is_empty() {
                                            if let Some(ft) = sender.file_transfer() {
                                                match ft.receive_file(&msg).await {
                                                    Ok(files) => {
                                                        let mut file_notes = Vec::new();
                                                        for file in &files {
                                                            let save_path = workspace_path.join(&file.name);
                                                            if let Err(e) = tokio::fs::write(&save_path, &file.data).await {
                                                                tracing::error!(error = %e, file = %file.name, "Failed to save attachment");
                                                            } else {
                                                                tracing::info!(file = %file.name, size = file.size, "Saved attachment to workspace");
                                                                file_notes.push(format!(
                                                                    "[File received: {} ({}, {} bytes) — saved to workspace/{}]",
                                                                    file.name, file.mime_type, file.size, file.name
                                                                ));
                                                            }
                                                        }
                                                        if !file_notes.is_empty() {
                                                            let prefix = file_notes.join("\n");
                                                            let existing = msg.text.take().unwrap_or_default();
                                                            msg.text = Some(format!("{}\n{}", prefix, existing));
                                                        }
                                                    }
                                                    Err(e) => {
                                                        tracing::error!(error = %e, "Failed to download attachments");
                                                    }
                                                }
                                            }
                                        }

                                        let mut session = temm1e_core::types::session::SessionContext {
                                            session_id: format!("{}-{}", msg.channel, msg.chat_id),
                                            user_id: msg.user_id.clone(),
                                            channel: msg.channel.clone(),
                                            chat_id: msg.chat_id.clone(),
                                            history: persistent_history.clone(),
                                            workspace_path: workspace_path.clone(),
                                        };

                                        // ── Early reply channel for LLM classifier ────
                                        // When V2 classifies a message as "order", it sends
                                        // an immediate acknowledgment through this channel
                                        // so the user sees a response while the pipeline runs.
                                        let (early_tx, mut early_rx) = tokio::sync::mpsc::unbounded_channel::<temm1e_core::types::message::OutboundMessage>();
                                        let sender_for_early = sender.clone();
                                        tokio::spawn(async move {
                                            while let Some(mut early_msg) = early_rx.recv().await {
                                                early_msg.text = censor_secrets(&early_msg.text);
                                                send_with_retry(&*sender_for_early, early_msg).await;
                                            }
                                        });

                                        // ── Panic-guarded message processing ─────────
                                        // Mark worker as busy AFTER command interception.
                                        // Commands handled above use `continue` and never
                                        // reach here, so is_busy stays false for them.
                                        is_busy_clone.store(true, Ordering::Relaxed);
                                        if let Ok(mut ct) = current_task_clone.lock() {
                                            *ct = msg.text.as_deref().unwrap_or("").to_string();
                                        }

                                        // Wraps process_message in catch_unwind so a panic
                                        // in context building, tool execution, or provider
                                        // parsing doesn't kill the per-chat worker loop.
                                        // The worker survives and continues processing the
                                        // next message — the user gets an error reply
                                        // instead of permanent silence.
                                        let process_result = AssertUnwindSafe(
                                            agent.process_message(&msg, &mut session, interrupt_flag, Some(pending_for_worker.clone()), Some(early_tx), Some(status_tx), Some(cancel))
                                        )
                                        .catch_unwind()
                                        .await;

                                        match process_result {
                                            Ok(Ok((mut reply, turn_usage))) => {
                                                reply.text = censor_secrets(&reply.text);
                                                if !reply.text.trim().is_empty() {
                                                    send_with_retry(&*sender, reply).await;
                                                }

                                                // Record usage
                                                let record = temm1e_core::UsageRecord {
                                                    id: uuid::Uuid::new_v4().to_string(),
                                                    chat_id: msg.chat_id.clone(),
                                                    session_id: format!("{}-{}", msg.channel, msg.chat_id),
                                                    timestamp: chrono::Utc::now(),
                                                    api_calls: turn_usage.api_calls,
                                                    input_tokens: turn_usage.input_tokens,
                                                    output_tokens: turn_usage.output_tokens,
                                                    tools_used: turn_usage.tools_used,
                                                    total_cost_usd: turn_usage.total_cost_usd,
                                                    provider: turn_usage.provider.clone(),
                                                    model: turn_usage.model.clone(),
                                                };
                                                if let Err(e) = usage_store_worker.record_usage(record).await {
                                                    tracing::error!(error = %e, "Failed to record usage");
                                                }

                                                // Display usage summary if enabled
                                                if turn_usage.api_calls > 0 {
                                                    if let Ok(enabled) = usage_store_worker.is_usage_display_enabled(&msg.chat_id).await {
                                                        if enabled {
                                                            let usage_msg = temm1e_core::types::message::OutboundMessage {
                                                                chat_id: msg.chat_id.clone(),
                                                                text: turn_usage.format_summary(),
                                                                reply_to: None,
                                                                parse_mode: None,
                                                            };
                                                            send_with_retry(&*sender, usage_msg).await;
                                                        }
                                                    }
                                                }
                                            }
                                            Ok(Err(temm1e_core::types::error::Temm1eError::HiveRoute(hive_msg))) => {
                                                // ── Classifier said Order+Complex, hive enabled → pack ──
                                                if let Some(ref hive) = hive_worker {
                                                    if let Some(ref agent) = agent_state.read().await.as_ref().cloned() {
                                                        let provider = agent.provider_arc();
                                                        let model = agent.model().to_string();
                                                        let hive = Arc::clone(hive);
                                                        let chat_id = msg.chat_id.clone();

                                                        tracing::info!(chat = %chat_id, "Many Tems: classifier routed Order+Complex to pack");

                                                        // Send immediate ack so the user knows pack is working
                                                        let ack = temm1e_core::types::message::OutboundMessage {
                                                            chat_id: msg.chat_id.clone(),
                                                            text: "Alpha decomposing into pack tasks...".to_string(),
                                                            reply_to: Some(msg.id.clone()),
                                                            parse_mode: None,
                                                        };
                                                        send_with_retry(&*sender, ack).await;

                                                        let decompose_result = hive.maybe_decompose(
                                                            &hive_msg, &chat_id,
                                                            |prompt| {
                                                                let p = provider.clone();
                                                                let m = model.clone();
                                                                async move {
                                                                    let resp = p.complete(temm1e_core::types::message::CompletionRequest {
                                                                        model: m,
                                                                        messages: vec![temm1e_core::types::message::ChatMessage {
                                                                            role: temm1e_core::types::message::Role::User,
                                                                            content: temm1e_core::types::message::MessageContent::Text(prompt),
                                                                        }],
                                                                        tools: vec![],
                                                                        max_tokens: None,
                                                                        temperature: Some(0.3),
                                                                        system: None,
                                                                    }).await?;
                                                                    let text: String = resp.content.iter().filter_map(|p| match p {
                                                                        temm1e_core::types::message::ContentPart::Text { text } => Some(text.clone()),
                                                                        _ => None,
                                                                    }).collect();
                                                                    let tokens = (resp.usage.input_tokens + resp.usage.output_tokens) as u64;
                                                                    Ok((text, tokens))
                                                                }
                                                            },
                                                        ).await;

                                                        if let Ok(Some(order_id)) = decompose_result {
                                                            tracing::info!(order_id = %order_id, "Pack: executing order");
                                                            let swarm_ack = temm1e_core::types::message::OutboundMessage {
                                                                chat_id: msg.chat_id.clone(),
                                                                text: "Pack activated — Tems working in parallel...".to_string(),
                                                                reply_to: None,
                                                                parse_mode: None,
                                                            };
                                                            send_with_retry(&*sender, swarm_ack).await;
                                                            let cancel = cancel_token_clone.clone();
                                                            let provider = agent.provider_arc();
                                                            let tools_h = tools_template.clone();
                                                            let memory_h = memory.clone();
                                                            let model_h = agent.model().to_string();

                                                            let swarm_result = hive.execute_order(
                                                                &order_id, cancel,
                                                                move |task, deps| {
                                                                    let p = provider.clone();
                                                                    let t = tools_h.clone();
                                                                    let m_clone = memory_h.clone();
                                                                    let mdl = model_h.clone();
                                                                    async move {
                                                                        let scoped = temm1e_hive::worker::build_scoped_context(&task, &deps);
                                                                        let mini = temm1e_agent::AgentRuntime::with_limits(
                                                                            p, m_clone, t, mdl, None, 10, 30000, 50, 300, 0.0,
                                                                        );
                                                                        let mini_msg = temm1e_core::types::message::InboundMessage {
                                                                            id: uuid::Uuid::new_v4().to_string(),
                                                                            chat_id: "hive".into(), user_id: "hive".into(),
                                                                            username: None, channel: "hive".into(),
                                                                            text: Some(scoped), attachments: vec![],
                                                                            reply_to: None, timestamp: chrono::Utc::now(),
                                                                        };
                                                                        let mut s = temm1e_core::types::session::SessionContext {
                                                                            session_id: format!("hive-{}", task.id),
                                                                            user_id: "hive".into(), channel: "hive".into(),
                                                                            chat_id: "hive".into(), history: vec![],
                                                                            workspace_path: std::path::PathBuf::from("."),
                                                                        };
                                                                        match mini.process_message(&mini_msg, &mut s, None, None, None, None, None).await {
                                                                            Ok((r, u)) => Ok(temm1e_hive::worker::TaskResult {
                                                                                summary: r.text, tokens_used: u.combined_tokens(),
                                                                                artifacts: vec![], success: true, error: None,
                                                                            }),
                                                                            Err(e) => Ok(temm1e_hive::worker::TaskResult {
                                                                                summary: String::new(), tokens_used: 0,
                                                                                artifacts: vec![], success: false, error: Some(e.to_string()),
                                                                            }),
                                                                        }
                                                                    }
                                                                },
                                                            ).await;

                                                            match swarm_result {
                                                                Ok(result) => {
                                                                    let full_text = censor_secrets(&format!(
                                                                        "{}\n\n---\nPack: {} tasks, {} Tems, {}ms, {} tokens",
                                                                        result.text, result.tasks_completed,
                                                                        result.workers_used, result.wall_clock_ms,
                                                                        result.total_tokens,
                                                                    ));

                                                                    // Split into chunks for Telegram's 4096 char limit
                                                                    let max_chunk = 4000; // leave margin
                                                                    let chunks: Vec<&str> = if full_text.len() <= max_chunk {
                                                                        vec![&full_text]
                                                                    } else {
                                                                        // Split on double-newlines (task boundaries) or at max_chunk
                                                                        let mut parts = Vec::new();
                                                                        let mut remaining = full_text.as_str();
                                                                        while !remaining.is_empty() {
                                                                            if remaining.len() <= max_chunk {
                                                                                parts.push(remaining);
                                                                                break;
                                                                            }
                                                                            // Find a good split point (double newline near the limit)
                                                                            let search_end = remaining.len().min(max_chunk);
                                                                            let split_at = remaining[..search_end]
                                                                                .rfind("\n\n")
                                                                                .unwrap_or_else(|| {
                                                                                    // Find safe char boundary near max_chunk
                                                                                    remaining.char_indices()
                                                                                        .take_while(|(i, _)| *i <= max_chunk)
                                                                                        .last()
                                                                                        .map(|(i, c)| i + c.len_utf8())
                                                                                        .unwrap_or(max_chunk)
                                                                                });
                                                                            parts.push(&remaining[..split_at]);
                                                                            remaining = remaining[split_at..].trim_start();
                                                                        }
                                                                        parts
                                                                    };

                                                                    for (i, chunk) in chunks.iter().enumerate() {
                                                                        let reply = temm1e_core::types::message::OutboundMessage {
                                                                            chat_id: msg.chat_id.clone(),
                                                                            text: chunk.to_string(),
                                                                            reply_to: if i == 0 { Some(msg.id.clone()) } else { None },
                                                                            parse_mode: None,
                                                                        };
                                                                        send_with_retry(&*sender, reply).await;
                                                                    }
                                                                }
                                                                Err(e) => {
                                                                    tracing::error!(error = %e, "Hive execution failed");
                                                                    let reply = temm1e_core::types::message::OutboundMessage {
                                                                        chat_id: msg.chat_id.clone(),
                                                                        text: format!("Pack execution failed: {e}"),
                                                                        reply_to: Some(msg.id.clone()),
                                                                        parse_mode: None,
                                                                    };
                                                                    send_with_retry(&*sender, reply).await;
                                                                }
                                                            }
                                                        } else {
                                                            tracing::info!("Alpha: decomposition failed or not worth it, no pack");
                                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                                chat_id: msg.chat_id.clone(),
                                                                text: "Task classified as complex but decomposition wasn't viable. Please try rephrasing or breaking it down.".into(),
                                                                reply_to: Some(msg.id.clone()),
                                                                parse_mode: None,
                                                            };
                                                            send_with_retry(&*sender, reply).await;
                                                        }
                                                    }
                                                }
                                            }
                                            Ok(Err(e)) => {
                                                tracing::error!(error = %e, "Agent processing error");
                                                let user_msg = format_user_error(&e);
                                                let error_reply = temm1e_core::types::message::OutboundMessage {
                                                    chat_id: msg.chat_id.clone(),
                                                    text: censor_secrets(&user_msg),
                                                    reply_to: Some(msg.id.clone()),
                                                    parse_mode: None,
                                                };
                                                send_with_retry(&*sender, error_reply).await;
                                            }
                                            Err(panic_info) => {
                                                // ── Panic recovered — worker stays alive ────
                                                let panic_msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                                                    s.clone()
                                                } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                                                    s.to_string()
                                                } else {
                                                    "internal error".to_string()
                                                };
                                                tracing::error!(
                                                    chat_id = %msg.chat_id,
                                                    panic = %panic_msg,
                                                    "PANIC RECOVERED in message processing — worker continues"
                                                );
                                                let error_reply = temm1e_core::types::message::OutboundMessage {
                                                    chat_id: msg.chat_id.clone(),
                                                    text: "An internal error occurred while processing your message. I've recovered and am ready for your next message.".to_string(),
                                                    reply_to: Some(msg.id.clone()),
                                                    parse_mode: None,
                                                };
                                                send_with_retry(&*sender, error_reply).await;
                                                // Session history may be corrupted after a panic.
                                                // Trim the last entry if it was partially added.
                                                if persistent_history.len() < session.history.len() {
                                                    // Panic happened after adding user msg but before
                                                    // assistant reply — rollback to pre-message state.
                                                    session.history = persistent_history.clone();
                                                }
                                            }
                                        }

                                        // ── Persist session history for next message ────
                                        // Cap to last 200 messages to prevent unbounded memory growth
                                        persistent_history = session.history;
                                        if persistent_history.len() > 200 {
                                            let drain_count = persistent_history.len() - 200;
                                            persistent_history.drain(..drain_count);
                                        }

                                        // ── Save conversation history to memory backend ──
                                        if let Ok(json) = serde_json::to_string(&persistent_history) {
                                            let entry = temm1e_core::MemoryEntry {
                                                id: history_key.clone(),
                                                content: json,
                                                metadata: serde_json::json!({"chat_id": worker_chat_id}),
                                                timestamp: chrono::Utc::now(),
                                                session_id: Some(worker_chat_id.clone()),
                                                entry_type: temm1e_core::MemoryEntryType::Conversation,
                                            };
                                            if let Err(e) = memory.store(entry).await {
                                                tracing::warn!(
                                                    chat_id = %worker_chat_id,
                                                    error = %e,
                                                    "Failed to persist conversation history"
                                                );
                                            }
                                        }

                                        // ── Hot-reload: check if credentials changed ────
                                        if let Some((new_name, new_keys, new_model, saved_base_url)) = load_active_provider_keys() {
                                            let current_model = agent.model().to_string();
                                            if new_model != current_model || new_keys.len() > 1 {
                                                // Filter out placeholder keys before reloading
                                                let valid_keys: Vec<String> = new_keys.into_iter()
                                                    .filter(|k| !is_placeholder_key(k))
                                                    .collect();
                                                if valid_keys.is_empty() {
                                                    tracing::warn!(
                                                        provider = %new_name,
                                                        "Hot-reload skipped — all keys are placeholders"
                                                    );
                                                } else {
                                                    tracing::info!(
                                                        old_model = %current_model,
                                                        new_model = %new_model,
                                                        key_count = valid_keys.len(),
                                                        "Credentials changed — validating before hot-reload"
                                                    );
                                                    let effective_base_url = saved_base_url.or_else(|| base_url.clone());
                                                    let reload_config = temm1e_core::types::config::ProviderConfig {
                                                        name: Some(new_name.clone()),
                                                        api_key: valid_keys.first().cloned(),
                                                        keys: valid_keys,
                                                        model: Some(new_model.clone()),
                                                        base_url: effective_base_url,
                                                        extra_headers: std::collections::HashMap::new(),
                                                    };
                                                    match validate_provider_key(&reload_config).await {
                                                        Ok(validated_provider) => {
                                                            let new_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                                validated_provider,
                                                                memory.clone(),
                                                                tools_template.clone(),
                                                                new_model.clone(),
                                                                Some(build_system_prompt()),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()));
                                                            *agent_state.write().await = Some(new_agent);
                                                            tracing::info!(provider = %new_name, model = %new_model, "Agent hot-reloaded (key validated)");
                                                        }
                                                        Err(err) => {
                                                            tracing::warn!(
                                                                provider = %new_name,
                                                                error = %err,
                                                                "Hot-reload aborted — validation failed, reverting model"
                                                            );
                                                            // Revert credentials.toml to the working model
                                                            if let Some(mut creds) = load_credentials_file() {
                                                                for p in &mut creds.providers {
                                                                    if p.name == new_name {
                                                                        p.model = current_model.clone();
                                                                    }
                                                                }
                                                                if let Ok(content) = toml::to_string_pretty(&creds) {
                                                                    let _ = std::fs::write(credentials_path(), &content);
                                                                    tracing::info!(
                                                                        model = %current_model,
                                                                        "Reverted credentials.toml to working model"
                                                                    );
                                                                }
                                                            }
                                                        }
                                                    }
                                                }
                                            }
                                        }

                                        // ── Hot-reload: check if MCP tools changed ────
                                        #[cfg(feature = "mcp")]
                                        if mcp_mgr.take_tools_changed() {
                                            tracing::info!("MCP tools changed — rebuilding agent");
                                            let tool_names: Vec<String> = tools_template.iter().map(|t| t.name().to_string()).collect();
                                            let mut new_tools = tools_template.clone();
                                            let mcp_tools = mcp_mgr.bridge_tools(&tool_names).await;
                                            new_tools.extend(mcp_tools);
                                            new_tools.push(std::sync::Arc::new(temm1e_mcp::McpManageTool::new(mcp_mgr.clone())));
                                            new_tools.push(std::sync::Arc::new(temm1e_mcp::SelfExtendTool::new()));
                                            new_tools.push(std::sync::Arc::new(temm1e_mcp::SelfAddMcpTool::new(mcp_mgr.clone())));
                                            let new_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                agent.provider_arc(),
                                                memory.clone(),
                                                new_tools,
                                                agent.model().to_string(),
                                                Some(build_system_prompt()),
                                                max_turns, max_ctx, max_rounds, max_task_duration, max_spend,
                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()));
                                            *agent_state.write().await = Some(new_agent);
                                            tracing::info!("Agent rebuilt with updated MCP tools");
                                        }

                                        // ── Hot-reload: check if custom tools changed ────
                                        if custom_registry.take_tools_changed() {
                                            tracing::info!("Custom tools changed — rebuilding agent");
                                            let mut new_tools = tools_template.clone();
                                            let custom_tools = custom_registry.load_tools();
                                            if !custom_tools.is_empty() {
                                                tracing::info!(count = custom_tools.len(), "Reloaded custom tools");
                                                new_tools.extend(custom_tools);
                                            }
                                            new_tools.push(std::sync::Arc::new(temm1e_tools::SelfCreateTool::new(custom_registry.clone())));
                                            #[cfg(feature = "mcp")]
                                            {
                                                let tool_names: Vec<String> = new_tools.iter().map(|t| t.name().to_string()).collect();
                                                let mcp_tools = mcp_mgr.bridge_tools(&tool_names).await;
                                                new_tools.extend(mcp_tools);
                                                new_tools.push(std::sync::Arc::new(temm1e_mcp::McpManageTool::new(mcp_mgr.clone())));
                                                new_tools.push(std::sync::Arc::new(temm1e_mcp::SelfExtendTool::new()));
                                                new_tools.push(std::sync::Arc::new(temm1e_mcp::SelfAddMcpTool::new(mcp_mgr.clone())));
                                            }
                                            let new_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                agent.provider_arc(),
                                                memory.clone(),
                                                new_tools,
                                                agent.model().to_string(),
                                                Some(build_system_prompt()),
                                                max_turns, max_ctx, max_rounds, max_task_duration, max_spend,
                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()));
                                            *agent_state.write().await = Some(new_agent);
                                            tracing::info!("Agent rebuilt with updated custom tools");
                                        }
                                    } else {
                                        // ── Onboarding / add-key mode: detect API key ────
                                        let msg_text = msg.text.as_deref().unwrap_or("");

                                        if let Some(cred) = detect_api_key(msg_text) {
                                            let provider_name = cred.provider;
                                            let api_key = cred.api_key;
                                            let custom_base_url = cred.base_url;
                                            let model = default_model(provider_name).to_string();
                                            // Load existing keys for this provider (if any)
                                            let mut all_keys = vec![api_key.clone()];
                                            if let Some(creds) = load_credentials_file() {
                                                if let Some(existing) = creds.providers.iter().find(|p| p.name == provider_name) {
                                                    for k in &existing.keys {
                                                        if !all_keys.contains(k) {
                                                            all_keys.push(k.clone());
                                                        }
                                                    }
                                                }
                                            }
                                            let effective_base_url = custom_base_url.clone().or_else(|| base_url.clone());
                                            let provider_config = temm1e_core::types::config::ProviderConfig {
                                                name: Some(provider_name.to_string()),
                                                api_key: Some(api_key.clone()),
                                                keys: all_keys,
                                                model: Some(model.clone()),
                                                base_url: effective_base_url,
                                                extra_headers: std::collections::HashMap::new(),
                                            };

                                            match temm1e_providers::create_provider(&provider_config) {
                                                Ok(_provider) => {
                                                    // Use shared validation (handles auth vs non-auth errors)
                                                    match validate_provider_key(&provider_config).await {
                                                        Ok(validated_provider) => {
                                                            // Key is valid — create agent and go online
                                                            let new_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                                validated_provider,
                                                                memory.clone(),
                                                                tools_template.clone(),
                                                                model.clone(),
                                                                Some(build_system_prompt()),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()));
                                                            *agent_state.write().await = Some(new_agent);

                                                            if let Err(e) = save_credentials(provider_name, &api_key, &model, custom_base_url.as_deref()).await {
                                                                tracing::error!(error = %e, "Failed to save credentials");
                                                            }

                                                            let proxy_note = if custom_base_url.is_some() {
                                                                " (via proxy)"
                                                            } else {
                                                                ""
                                                            };
                                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                                chat_id: msg.chat_id.clone(),
                                                                text: format!(
                                                                    "API key verified! Configured {}{} with model {}.\n\nTEMM1E is online! You can:\n- Add more keys anytime (just paste them)\n- Use a proxy: \"proxy openai https://your-proxy/v1 your-key\"\n- Change settings in natural language\n\nHow can I help?",
                                                                    provider_name, proxy_note, model
                                                                ),
                                                                reply_to: Some(msg.id.clone()),
                                                                parse_mode: None,
                                                            };
                                                            send_with_retry(&*sender, reply).await;
                                                            tracing::info!(provider = %provider_name, model = %model, "API key validated — agent online");
                                                        }
                                                        Err(e) => {
                                                            // Key failed auth validation
                                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                                chat_id: msg.chat_id.clone(),
                                                                text: format!(
                                                                    "Invalid API key — the {} API returned an error:\n{}\n\nPlease check your key and paste it again.",
                                                                    provider_name, e
                                                                ),
                                                                reply_to: Some(msg.id.clone()),
                                                                parse_mode: None,
                                                            };
                                                            send_with_retry(&*sender, reply).await;
                                                            tracing::warn!(provider = %provider_name, error = %e, "API key validation failed");
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    let reply = temm1e_core::types::message::OutboundMessage {
                                                        chat_id: msg.chat_id.clone(),
                                                        text: format!("Failed to configure provider: {}", e),
                                                        reply_to: Some(msg.id.clone()),
                                                        parse_mode: None,
                                                    };
                                                    send_with_retry(&*sender, reply).await;
                                                }
                                            }
                                        } else {
                                            // Auto-generate OTK and send onboarding with setup link
                                            let otk = setup_tokens_worker.generate(&msg.chat_id).await;
                                            let otk_hex = hex::encode(otk);
                                            let link = format!(
                                                "https://nagisanzenin.github.io/temm1e/setup#{}",
                                                otk_hex
                                            );
                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                chat_id: msg.chat_id.clone(),
                                                text: onboarding_message_with_link(&link),
                                                reply_to: Some(msg.id.clone()),
                                                parse_mode: None,
                                            };
                                            send_with_retry(&*sender, reply).await;

                                            // Send format reference as separate message for easy copy-paste
                                            let ref_msg = temm1e_core::types::message::OutboundMessage {
                                                chat_id: msg.chat_id.clone(),
                                                text: ONBOARDING_REFERENCE.to_string(),
                                                reply_to: None,
                                                parse_mode: None,
                                            };
                                            send_with_retry(&*sender, ref_msg).await;
                                        }
                                    }

                                    // Re-queue any unconsumed pending messages as
                                    // standalone requests, then clear active state.
                                    if let Ok(mut pq) = pending_for_worker.lock() {
                                        if let Some(pending_msgs) = pq.remove(&worker_chat_id) {
                                            if !pending_msgs.is_empty() {
                                                tracing::info!(
                                                    count = pending_msgs.len(),
                                                    chat_id = %worker_chat_id,
                                                    "Re-queuing unconsumed pending messages"
                                                );
                                                for text in pending_msgs {
                                                    let synthetic = temm1e_core::types::message::InboundMessage {
                                                        id: uuid::Uuid::new_v4().to_string(),
                                                        channel: msg.channel.clone(),
                                                        chat_id: worker_chat_id.clone(),
                                                        user_id: msg.user_id.clone(),
                                                        username: None,
                                                        text: Some(text),
                                                        timestamp: chrono::Utc::now(),
                                                        reply_to: None,
                                                        attachments: vec![],
                                                    };
                                                    if self_tx.try_send(synthetic).is_err() {
                                                        tracing::warn!(
                                                            chat_id = %worker_chat_id,
                                                            "Failed to re-queue pending message — channel full"
                                                        );
                                                    }
                                                }
                                            }
                                        }
                                    }
                                    is_heartbeat_clone.store(false, Ordering::Relaxed);
                                    is_busy_clone.store(false, Ordering::Relaxed);
                                    interrupt_clone.store(false, Ordering::Relaxed);
                                    }).catch_unwind().await;

                                    // ── Outer panic safety net ─────────────────
                                    // If ANYTHING in the loop body panicked
                                    // (command handling, key detection, session
                                    // setup, etc.), recover here so the worker
                                    // loop survives.  The inner catch_unwind on
                                    // process_message provides more specific
                                    // recovery with usage tracking; this outer
                                    // one is a last-resort safety net.
                                    if let Err(panic_info) = outer_catch_result {
                                        let panic_msg = if let Some(s) = panic_info.downcast_ref::<&str>() {
                                            s.to_string()
                                        } else if let Some(s) = panic_info.downcast_ref::<String>() {
                                            s.clone()
                                        } else {
                                            "unknown panic".to_string()
                                        };
                                        tracing::error!(
                                            panic = %panic_msg,
                                            chat_id = %panic_chat_id,
                                            "Worker panic caught in outer safety net — recovering"
                                        );
                                        // Best-effort notification to the user
                                        let error_reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: panic_chat_id.clone(),
                                            text: "An internal error occurred. Please try again.".to_string(),
                                            reply_to: Some(panic_msg_id.clone()),
                                            parse_mode: None,
                                        };
                                        let _ = sender.send_message(error_reply).await;
                                        // Ensure cleanup in case the panic skipped it
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        is_busy_clone.store(false, Ordering::Relaxed);
                                        interrupt_clone.store(false, Ordering::Relaxed);
                                        if let Ok(mut pq) = pending_for_worker.lock() {
                                            pq.remove(&worker_chat_id);
                                        }
                                    }
                                }
                            });

                            ChatSlot { tx: chat_tx, interrupt, is_heartbeat, is_busy, current_task, cancel_token }
                        });

                        // Send message into the chat's dedicated queue.
                        // Clone the sender to release the borrow on slots, so we
                        // can remove the dead slot if the send fails.
                        if !is_heartbeat_msg {
                            let tx = slot.tx.clone();
                            drop(slots); // release Mutex guard before await
                            let inbound_backup = inbound.clone();
                            if let Err(e) = tx.send(inbound).await {
                                tracing::error!(
                                    chat_id = %chat_id,
                                    error = %e,
                                    "Chat worker dead — removing slot and re-dispatching"
                                );
                                let mut slots = chat_slots.lock().await;
                                slots.remove(&chat_id);
                                drop(slots); // release lock before re-dispatch
                                // Re-send through the unified channel so the
                                // dispatcher loop creates a fresh worker for
                                // this chat_id — zero messages lost.
                                if let Err(e2) = msg_tx_redispatch.send(inbound_backup).await {
                                    tracing::error!(
                                        chat_id = %chat_id,
                                        error = %e2,
                                        "Failed to re-dispatch message after worker death"
                                    );
                                }
                            }
                        }
                    }
                }));
            }

            // ── Start gateway + block ──────────────────────────
            println!("TEMM1E gateway starting...");
            println!("  Mode: {}", cli.mode);

            if let Some(agent) = agent_state.read().await.as_ref().cloned() {
                let gate = temm1e_gateway::SkyGate::new(channels, agent, config.gateway.clone());
                task_handles.push(tokio::spawn(async move {
                    if let Err(e) = gate.start().await {
                        tracing::error!(error = %e, "Gateway error");
                    }
                }));
                println!("  Status: Online");
                println!(
                    "  Gateway: http://{}:{}",
                    config.gateway.host, config.gateway.port
                );
                println!(
                    "  Health: http://{}:{}/health",
                    config.gateway.host, config.gateway.port
                );
            } else {
                println!("  Status: Onboarding — send your API key via Telegram");
            }

            // Block until Ctrl+C, then drain gracefully
            tokio::signal::ctrl_c().await?;
            println!("\nTEMM1E shutting down gracefully...");

            // Drop the inbound message sender so the dispatcher loop exits
            // when its receiver sees the channel closed.
            drop(msg_tx);

            // Wait for spawned tasks with a timeout
            let drain_timeout = tokio::time::timeout(
                std::time::Duration::from_secs(5),
                futures::future::join_all(task_handles),
            );
            match drain_timeout.await {
                Ok(_) => println!("All tasks drained cleanly."),
                Err(_) => println!("Drain timeout — forcing exit."),
            }

            // Clean up PID file on graceful shutdown
            remove_pid_file();
        }
        Commands::Chat => {
            println!("TEMM1E interactive chat");
            println!("Type '/quit' or '/exit' to quit.\n");

            // Check hive config for CLI chat path
            let hive_enabled_early = {
                #[derive(serde::Deserialize, Default)]
                struct HC {
                    #[serde(default)]
                    hive: HE,
                }
                #[derive(serde::Deserialize, Default)]
                struct HE {
                    #[serde(default)]
                    enabled: bool,
                }
                config_path
                    .and_then(|p| std::fs::read_to_string(p).ok())
                    .or_else(|| {
                        dirs::home_dir().and_then(|h| {
                            std::fs::read_to_string(h.join(".temm1e/config.toml")).ok()
                        })
                    })
                    .or_else(|| std::fs::read_to_string("temm1e.toml").ok())
                    .and_then(|c| toml::from_str::<HC>(&c).ok())
                    .map(|c| c.hive.enabled)
                    .unwrap_or(false)
            };

            // ── Resolve API credentials ────────────────────────
            let credentials: Option<(String, String, String)> = {
                if let Some(ref key) = config.provider.api_key {
                    if !key.is_empty() && !key.starts_with("${") {
                        let name = config
                            .provider
                            .name
                            .clone()
                            .unwrap_or_else(|| "anthropic".to_string());
                        let model = config
                            .provider
                            .model
                            .clone()
                            .unwrap_or_else(|| default_model(&name).to_string());
                        Some((name, key.clone(), model))
                    } else {
                        load_saved_credentials()
                    }
                } else {
                    load_saved_credentials()
                }
            };

            // ── Memory backend ─────────────────────────────────
            let memory_url = config.memory.path.clone().unwrap_or_else(|| {
                let data_dir = dirs::home_dir()
                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                    .join(".temm1e");
                if let Err(e) = std::fs::create_dir_all(&data_dir) {
                    tracing::warn!(error = %e, path = %data_dir.display(), "Failed to create directory");
                }
                format!("sqlite:{}/memory.db?mode=rwc", data_dir.display())
            });
            let memory: Arc<dyn temm1e_core::Memory> = Arc::from(
                temm1e_memory::create_memory_backend(&config.memory.backend, &memory_url).await?,
            );

            // ── CLI channel ────────────────────────────────────
            let workspace = dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".temm1e")
                .join("workspace");
            if let Err(e) = std::fs::create_dir_all(&workspace) {
                tracing::warn!(error = %e, path = %workspace.display(), "Failed to create directory");
            }
            let mut cli_channel = temm1e_channels::CliChannel::new(workspace.clone());
            let cli_rx = cli_channel.take_receiver();
            cli_channel.start().await?;
            let cli_arc: Arc<dyn temm1e_core::Channel> = Arc::new(cli_channel);

            // ── OTK state ──────────────────────────────────────
            let setup_tokens = temm1e_gateway::SetupTokenStore::new();

            // ── Usage store ──────────────────────────────────────
            let usage_store: Arc<dyn temm1e_core::UsageStore> =
                Arc::new(temm1e_memory::SqliteUsageStore::new(&memory_url).await?);

            // ── Tools ──────────────────────────────────────────
            let pending_messages: temm1e_tools::PendingMessages =
                Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));
            let censored_cli: Arc<dyn Channel> = Arc::new(SecretCensorChannel {
                inner: cli_arc.clone(),
            });
            let shared_mode: temm1e_tools::SharedMode =
                Arc::new(tokio::sync::RwLock::new(config.mode));
            let shared_memory_strategy: Arc<
                tokio::sync::RwLock<temm1e_core::types::config::MemoryStrategy>,
            > = Arc::new(tokio::sync::RwLock::new(
                temm1e_core::types::config::MemoryStrategy::Lambda,
            ));
            let mut tools_template = temm1e_tools::create_tools(
                &config.tools,
                Some(censored_cli),
                Some(pending_messages.clone()),
                Some(memory.clone()),
                Some(Arc::new(setup_tokens.clone()) as Arc<dyn temm1e_core::SetupLinkGenerator>),
                Some(usage_store.clone()),
                Some(shared_mode.clone()),
            );

            // ── Custom script tools (user/agent-authored) ──────
            let custom_tool_registry = Arc::new(temm1e_tools::CustomToolRegistry::new());
            {
                let custom_tools = custom_tool_registry.load_tools();
                if !custom_tools.is_empty() {
                    tracing::info!(count = custom_tools.len(), "Custom script tools loaded");
                    tools_template.extend(custom_tools);
                }
                tools_template.push(Arc::new(temm1e_tools::SelfCreateTool::new(
                    custom_tool_registry.clone(),
                )));
            }

            // ── MCP servers (external tool sources) ──────────
            #[cfg(feature = "mcp")]
            let mcp_manager: Arc<temm1e_mcp::McpManager> = {
                let mgr = Arc::new(temm1e_mcp::McpManager::new());
                mgr.connect_all().await;
                let tool_names: Vec<String> = tools_template
                    .iter()
                    .map(|t| t.name().to_string())
                    .collect();
                let mcp_tools = mgr.bridge_tools(&tool_names).await;
                if !mcp_tools.is_empty() {
                    tracing::info!(count = mcp_tools.len(), "MCP bridge tools loaded");
                    tools_template.extend(mcp_tools);
                }
                tools_template.push(Arc::new(temm1e_mcp::McpManageTool::new(mgr.clone())));
                tools_template.push(Arc::new(temm1e_mcp::SelfExtendTool::new()));
                tools_template.push(Arc::new(temm1e_mcp::SelfAddMcpTool::new(mgr.clone())));
                mgr
            };

            let base_url = config.provider.base_url.clone();

            // ── Build agent (if credentials available) ─────────
            let max_turns = config.agent.max_turns;
            let max_ctx = config.agent.max_context_tokens;
            let max_rounds = config.agent.max_tool_rounds;
            let max_task_duration = config.agent.max_task_duration_secs;
            let max_spend = config.agent.max_spend_usd;
            let v2_opt = config.agent.v2_optimizations;
            let pp_opt = config.agent.parallel_phases;

            let mut agent_opt: Option<temm1e_agent::AgentRuntime> = None;

            if let Some((pname, key, model)) = credentials {
                if !is_placeholder_key(&key) {
                    let (all_keys, saved_base_url) = load_active_provider_keys()
                        .map(|(_, keys, _, burl)| {
                            let valid: Vec<String> = keys
                                .into_iter()
                                .filter(|k| !is_placeholder_key(k))
                                .collect();
                            (valid, burl)
                        })
                        .unwrap_or_else(|| (vec![key.clone()], None));
                    let effective_base_url =
                        saved_base_url.or_else(|| config.provider.base_url.clone());
                    let provider_config = temm1e_core::types::config::ProviderConfig {
                        name: Some(pname.clone()),
                        api_key: Some(key.clone()),
                        keys: all_keys,
                        model: Some(model.clone()),
                        base_url: effective_base_url,
                        extra_headers: config.provider.extra_headers.clone(),
                    };
                    // Create provider — route to Codex OAuth if configured
                    let provider_result: Result<Arc<dyn temm1e_core::Provider>, String> = {
                        #[cfg(feature = "codex-oauth")]
                        if pname == "openai-codex" {
                            match temm1e_codex_oauth::TokenStore::load() {
                                Ok(store) => Ok(Arc::new(
                                    temm1e_codex_oauth::CodexResponsesProvider::new(
                                        model.clone(),
                                        std::sync::Arc::new(store),
                                    ),
                                )),
                                Err(e) => Err(format!(
                                    "Codex OAuth not configured: {}. Run `temm1e auth login` first.",
                                    e
                                )),
                            }
                        } else {
                            temm1e_providers::create_provider(&provider_config)
                                .map(|p| Arc::from(p) as Arc<dyn temm1e_core::Provider>)
                                .map_err(|e| e.to_string())
                        }
                        #[cfg(not(feature = "codex-oauth"))]
                        {
                            temm1e_providers::create_provider(&provider_config)
                                .map(|p| Arc::from(p) as Arc<dyn temm1e_core::Provider>)
                                .map_err(|e| e.to_string())
                        }
                    };
                    match provider_result {
                        Ok(provider) => {
                            let system_prompt = Some(build_system_prompt());
                            agent_opt = Some(
                                temm1e_agent::AgentRuntime::with_limits(
                                    provider,
                                    memory.clone(),
                                    tools_template.clone(),
                                    model.clone(),
                                    system_prompt,
                                    max_turns,
                                    max_ctx,
                                    max_rounds,
                                    max_task_duration,
                                    max_spend,
                                )
                                .with_v2_optimizations(v2_opt)
                                .with_parallel_phases(pp_opt)
                                .with_hive_enabled(hive_enabled_early)
                                .with_shared_mode(shared_mode.clone())
                                .with_shared_memory_strategy(shared_memory_strategy.clone()),
                            );
                            println!("Connected to {} (model: {})", pname, model);
                            if max_spend > 0.0 {
                                println!("Budget: ${:.2} per session", max_spend);
                            } else {
                                println!("Budget: unlimited");
                            }
                        }
                        Err(e) => {
                            eprintln!("Failed to create provider: {}", e);
                        }
                    }
                }
            }

            if agent_opt.is_none() {
                // Check if Codex OAuth tokens exist — use those instead of API key
                #[cfg(feature = "codex-oauth")]
                {
                    if temm1e_codex_oauth::TokenStore::exists() {
                        // Always use Codex-compatible model — config model is for API key provider
                        let model = "gpt-5.4".to_string();
                        match temm1e_codex_oauth::TokenStore::load() {
                            Ok(store) => {
                                let token_store = std::sync::Arc::new(store);
                                let provider: Arc<dyn temm1e_core::Provider> =
                                    Arc::new(temm1e_codex_oauth::CodexResponsesProvider::new(
                                        model.clone(),
                                        token_store,
                                    ));
                                let system_prompt = Some(build_system_prompt());
                                agent_opt = Some(
                                    temm1e_agent::AgentRuntime::with_limits(
                                        provider,
                                        memory.clone(),
                                        tools_template.clone(),
                                        model.clone(),
                                        system_prompt,
                                        max_turns,
                                        max_ctx,
                                        max_rounds,
                                        max_task_duration,
                                        max_spend,
                                    )
                                    .with_v2_optimizations(v2_opt)
                                    .with_parallel_phases(pp_opt)
                                    .with_shared_mode(shared_mode.clone())
                                    .with_shared_memory_strategy(shared_memory_strategy.clone()),
                                );
                                println!(
                                    "Connected to openai-codex via Codex OAuth (model: {})",
                                    model
                                );
                            }
                            Err(e) => {
                                tracing::warn!(error = %e, "Codex OAuth tokens exist but failed to load");
                            }
                        }
                    }
                }
            }
            if agent_opt.is_none() {
                println!("No API key configured — running in onboarding mode.");
                // Auto-generate OTK and show setup link immediately
                let otk = setup_tokens.generate("cli").await;
                let otk_hex = hex::encode(otk);
                let link = format!("https://nagisanzenin.github.io/temm1e/setup#{}", otk_hex);
                println!("\n{}", onboarding_message_with_link(&link));
                println!("\n{}", ONBOARDING_REFERENCE);
            }
            println!("---\n");

            // ── Message loop ───────────────────────────────────
            let Some(mut rx) = cli_rx else {
                eprintln!("CLI channel receiver unavailable");
                return Ok(());
            };
            // ── Restore CLI conversation history from memory backend ──
            let cli_history_key = "chat_history:cli".to_string();
            let mut history: Vec<temm1e_core::types::message::ChatMessage> =
                match memory.get(&cli_history_key).await {
                    Ok(Some(entry)) => match serde_json::from_str(&entry.content) {
                        Ok(h) => {
                            let count = Vec::<temm1e_core::types::message::ChatMessage>::len(&h);
                            if count > 0 {
                                println!("  Restored {} messages from previous session.", count);
                            }
                            h
                        }
                        Err(_) => Vec::new(),
                    },
                    _ => Vec::new(),
                };

            while let Some(msg) = rx.recv().await {
                let msg_text = msg.text.as_deref().unwrap_or("");
                let cmd_lower = msg_text.trim().to_lowercase();

                // ── Command interception (same as gateway) ─────
                // /addkey — secure OTK flow
                if cmd_lower == "/addkey" {
                    let otk = setup_tokens.generate(&msg.chat_id).await;
                    let otk_hex = hex::encode(otk);
                    let link = format!("https://nagisanzenin.github.io/temm1e/setup#{}", otk_hex);
                    println!(
                        "\nSecure key setup:\n\n\
                         1. Open this link:\n{}\n\n\
                         2. Paste your API key in the form\n\
                         3. Copy the encrypted blob\n\
                         4. Paste it back here\n\n\
                         Link expires in 10 minutes.\n\n\
                         For a quick (less secure) method: /addkey unsafe\n",
                        link
                    );
                    eprint!("temm1e> ");
                    continue;
                }

                // /addkey unsafe
                if cmd_lower == "/addkey unsafe" {
                    println!("\nPaste your API key below.");
                    println!("Warning: the key will be visible in terminal history.");
                    println!("For a secure method, use /addkey instead.\n");
                    eprint!("temm1e> ");
                    continue;
                }

                // /keys
                if cmd_lower == "/keys" {
                    println!("\n{}\n", list_configured_providers());
                    eprint!("temm1e> ");
                    continue;
                }

                // /removekey <provider>
                if cmd_lower.starts_with("/removekey") {
                    let provider_arg = msg_text.trim()["/removekey".len()..].trim();
                    println!("\n{}\n", remove_provider(provider_arg));
                    if !provider_arg.is_empty() && load_active_provider_keys().is_none() {
                        agent_opt = None;
                        println!("All providers removed — agent offline.\n");
                    }
                    eprint!("temm1e> ");
                    continue;
                }

                // /usage — show usage summary
                if cmd_lower == "/usage" {
                    match usage_store.usage_summary(&msg.chat_id).await {
                        Ok(summary) => {
                            if summary.turn_count == 0 {
                                println!("\nNo usage records for this chat yet.\n");
                            } else {
                                println!(
                                    "\nUsage Summary\nTurns: {}\nAPI Calls: {}\nInput Tokens: {}\nOutput Tokens: {}\nCombined Tokens: {}\nTools Used: {}\nTotal Cost: ${:.4}\n",
                                    summary.turn_count,
                                    summary.total_api_calls,
                                    summary.total_input_tokens,
                                    summary.total_output_tokens,
                                    summary.combined_tokens(),
                                    summary.total_tools_used,
                                    summary.total_cost_usd,
                                );
                            }
                        }
                        Err(e) => eprintln!("Failed to query usage: {}", e),
                    }
                    eprint!("temm1e> ");
                    continue;
                }

                // /help — list available commands
                if cmd_lower == "/help" {
                    println!(
                        "\ntemm1e {} — commit: {} — date: {}\n\n\
                         Available commands:\n\n\
                         /help — Show this help message\n\
                         /addkey — Securely add an API key (encrypted OTK flow)\n\
                         /addkey unsafe — Add an API key by pasting directly\n\
                         /keys — List configured providers and active model\n\
                         /model — Show current model and available models\n\
                         /model <name> — Switch to a different model\n\
                         /removekey <provider> — Remove a provider's API key\n\
                         /usage — Show token usage and cost summary\n\
                         /memory — Show current memory strategy\n\
                         /memory lambda — Switch to λ-Memory (decay + persistence)\n\
                         /memory echo — Switch to Echo Memory (context window only)\n\
                         /mcp — List connected MCP servers and tools\n\
                         /mcp add <name> <command-or-url> — Connect a new MCP server\n\
                         /mcp remove <name> — Disconnect an MCP server\n\
                         /mcp restart <name> — Restart an MCP server\n\
                         /quit — Exit the CLI chat\n\n\
                         Just type a message to chat with the AI agent.\n",
                        env!("CARGO_PKG_VERSION"),
                        env!("GIT_HASH"),
                        env!("BUILD_DATE"),
                    );
                    eprint!("temm1e> ");
                    continue;
                }

                // /memory — switch memory strategy
                if cmd_lower == "/memory" || cmd_lower.starts_with("/memory ") {
                    let args = if cmd_lower == "/memory" {
                        ""
                    } else {
                        msg_text.trim()["/memory".len()..].trim()
                    };
                    let args_lower = args.to_lowercase();
                    if args_lower.is_empty() || args_lower == "status" {
                        let current = shared_memory_strategy.read().await;
                        println!(
                            "\nMemory Strategy: {}\n\n\
                             Available strategies:\n\
                             • /memory lambda — λ-Memory: decay-scored, cross-session persistence, hash-based recall (default)\n\
                             • /memory echo — Echo Memory: keyword search over current context window, no persistence\n",
                            *current,
                        );
                    } else if args_lower == "lambda" || args_lower == "λ" {
                        *shared_memory_strategy.write().await =
                            temm1e_core::types::config::MemoryStrategy::Lambda;
                        println!("\nSwitched to λ-Memory\nDecay-scored fidelity tiers • cross-session persistence • hash-based recall\n");
                    } else if args_lower == "echo" {
                        *shared_memory_strategy.write().await =
                            temm1e_core::types::config::MemoryStrategy::Echo;
                        println!("\nSwitched to Echo Memory\nKeyword search over context window • no persistence between sessions\n");
                    } else {
                        println!("\nUnknown strategy. Use: /memory lambda or /memory echo\n");
                    }
                    eprint!("temm1e> ");
                    continue;
                }

                // /mcp — manage MCP servers
                #[cfg(feature = "mcp")]
                if cmd_lower == "/mcp" || cmd_lower.starts_with("/mcp ") {
                    let mcp_args = if cmd_lower == "/mcp" {
                        ""
                    } else {
                        msg_text.trim()["/mcp".len()..].trim()
                    };
                    let mcp_args_lower = mcp_args.to_lowercase();
                    if mcp_args.is_empty() || mcp_args_lower == "list" {
                        println!("\n{}\n", mcp_manager.list_servers().await);
                    } else if mcp_args_lower.starts_with("add ") {
                        let add_rest = mcp_args["add ".len()..].trim();
                        let parts: Vec<&str> = add_rest.splitn(2, ' ').collect();
                        if parts.len() < 2 || parts[1].trim().is_empty() {
                            println!(
                                "\nUsage: /mcp add <name> <command-or-url>\n\n\
                                 Examples:\n\
                                 • /mcp add playwright npx @playwright/mcp@latest\n\
                                 • /mcp add filesystem npx -y @modelcontextprotocol/server-filesystem /path\n\
                                 • /mcp add myapi https://mcp.example.com/sse\n"
                            );
                        } else {
                            let name = parts[0];
                            let target = parts[1].trim();

                            // Warn if target looks like a GitHub repo URL
                            if target.contains("github.com/")
                                && !target.contains("/sse")
                                && !target.contains("/mcp")
                            {
                                println!(
                                    "\nThat looks like a GitHub repository URL, not an MCP server endpoint.\n\n\
                                     To use an MCP server, you need the command to run it. For example:\n\
                                     • /mcp add {} npx @playwright/mcp@latest\n\
                                     • /mcp add {} npx -y @modelcontextprotocol/server-filesystem /path\n\n\
                                     Check the repo's README for the correct MCP server command.\n",
                                    name, name
                                );
                            } else {
                                let config = if target.starts_with("http://")
                                    || target.starts_with("https://")
                                {
                                    temm1e_mcp::McpServerConfig::http(name, target)
                                } else {
                                    let cmd_parts: Vec<&str> = target.split_whitespace().collect();
                                    let command = cmd_parts[0];
                                    let args: Vec<String> =
                                        cmd_parts[1..].iter().map(|s| s.to_string()).collect();
                                    temm1e_mcp::McpServerConfig::stdio(name, command, args)
                                };
                                match mcp_manager.add_server(config).await {
                                    Ok(count) => {
                                        if let Some(ref mut agent) = agent_opt {
                                            let tool_names: Vec<String> = tools_template
                                                .iter()
                                                .map(|t| t.name().to_string())
                                                .collect();
                                            let mut new_tools = tools_template.clone();
                                            let mcp_tools =
                                                mcp_manager.bridge_tools(&tool_names).await;
                                            new_tools.extend(mcp_tools);
                                            new_tools.push(Arc::new(
                                                temm1e_mcp::McpManageTool::new(mcp_manager.clone()),
                                            ));
                                            new_tools
                                                .push(Arc::new(temm1e_mcp::SelfExtendTool::new()));
                                            new_tools.push(Arc::new(
                                                temm1e_mcp::SelfAddMcpTool::new(
                                                    mcp_manager.clone(),
                                                ),
                                            ));
                                            agent_opt = Some(
                                                temm1e_agent::AgentRuntime::with_limits(
                                                    agent.provider_arc(),
                                                    memory.clone(),
                                                    new_tools,
                                                    agent.model().to_string(),
                                                    Some(build_system_prompt()),
                                                    max_turns,
                                                    max_ctx,
                                                    max_rounds,
                                                    max_task_duration,
                                                    max_spend,
                                                )
                                                .with_v2_optimizations(v2_opt)
                                                .with_parallel_phases(pp_opt)
                                                .with_shared_mode(shared_mode.clone())
                                                .with_shared_memory_strategy(
                                                    shared_memory_strategy.clone(),
                                                ),
                                            );
                                        }
                                        mcp_manager.take_tools_changed();
                                        println!(
                                            "\nMCP server '{}' connected with {} tools.\n",
                                            name, count
                                        );
                                    }
                                    Err(e) => println!("\nFailed to add MCP server: {}\n", e),
                                }
                            }
                        }
                    } else if mcp_args_lower.starts_with("remove ") {
                        let name = mcp_args["remove ".len()..].trim();
                        match mcp_manager.remove_server(name).await {
                            Ok(()) => {
                                if let Some(ref mut agent) = agent_opt {
                                    let tool_names: Vec<String> = tools_template
                                        .iter()
                                        .map(|t| t.name().to_string())
                                        .collect();
                                    let mut new_tools = tools_template.clone();
                                    let mcp_tools = mcp_manager.bridge_tools(&tool_names).await;
                                    new_tools.extend(mcp_tools);
                                    new_tools.push(Arc::new(temm1e_mcp::McpManageTool::new(
                                        mcp_manager.clone(),
                                    )));
                                    new_tools.push(Arc::new(temm1e_mcp::SelfExtendTool::new()));
                                    new_tools.push(Arc::new(temm1e_mcp::SelfAddMcpTool::new(
                                        mcp_manager.clone(),
                                    )));
                                    agent_opt = Some(
                                        temm1e_agent::AgentRuntime::with_limits(
                                            agent.provider_arc(),
                                            memory.clone(),
                                            new_tools,
                                            agent.model().to_string(),
                                            Some(build_system_prompt()),
                                            max_turns,
                                            max_ctx,
                                            max_rounds,
                                            max_task_duration,
                                            max_spend,
                                        )
                                        .with_v2_optimizations(v2_opt)
                                        .with_parallel_phases(pp_opt)
                                        .with_shared_mode(shared_mode.clone())
                                        .with_shared_memory_strategy(
                                            shared_memory_strategy.clone(),
                                        ),
                                    );
                                }
                                mcp_manager.take_tools_changed();
                                println!("\nMCP server '{}' removed.\n", name);
                            }
                            Err(e) => println!("\nFailed to remove MCP server: {}\n", e),
                        }
                    } else if mcp_args_lower.starts_with("restart ") {
                        let name = mcp_args["restart ".len()..].trim();
                        match mcp_manager.restart_server(name).await {
                            Ok(count) => {
                                if let Some(ref mut agent) = agent_opt {
                                    let tool_names: Vec<String> = tools_template
                                        .iter()
                                        .map(|t| t.name().to_string())
                                        .collect();
                                    let mut new_tools = tools_template.clone();
                                    let mcp_tools = mcp_manager.bridge_tools(&tool_names).await;
                                    new_tools.extend(mcp_tools);
                                    new_tools.push(Arc::new(temm1e_mcp::McpManageTool::new(
                                        mcp_manager.clone(),
                                    )));
                                    new_tools.push(Arc::new(temm1e_mcp::SelfExtendTool::new()));
                                    new_tools.push(Arc::new(temm1e_mcp::SelfAddMcpTool::new(
                                        mcp_manager.clone(),
                                    )));
                                    agent_opt = Some(
                                        temm1e_agent::AgentRuntime::with_limits(
                                            agent.provider_arc(),
                                            memory.clone(),
                                            new_tools,
                                            agent.model().to_string(),
                                            Some(build_system_prompt()),
                                            max_turns,
                                            max_ctx,
                                            max_rounds,
                                            max_task_duration,
                                            max_spend,
                                        )
                                        .with_v2_optimizations(v2_opt)
                                        .with_parallel_phases(pp_opt)
                                        .with_shared_mode(shared_mode.clone())
                                        .with_shared_memory_strategy(
                                            shared_memory_strategy.clone(),
                                        ),
                                    );
                                }
                                mcp_manager.take_tools_changed();
                                println!(
                                    "\nMCP server '{}' restarted with {} tools.\n",
                                    name, count
                                );
                            }
                            Err(e) => println!("\nFailed to restart MCP server: {}\n", e),
                        }
                    } else {
                        println!(
                            "\nUsage: /mcp [list|add|remove|restart]\n\n\
                             /mcp — List all MCP servers\n\
                             /mcp add <name> <command> — Add a stdio MCP server\n\
                             /mcp add <name> <url> — Add an HTTP MCP server\n\
                             /mcp remove <name> — Remove a server\n\
                             /mcp restart <name> — Restart a server\n\n\
                             Examples:\n\
                             /mcp add playwright npx @playwright/mcp@latest\n\
                             /mcp add myapi https://mcp.example.com/sse\n"
                        );
                    }
                    eprint!("temm1e> ");
                    continue;
                }

                // /restart — not applicable in CLI mode
                if cmd_lower == "/restart" {
                    println!("\n/restart is only available in server mode (temm1e start).");
                    println!("In CLI mode, just exit and re-run: temm1e chat\n");
                    eprint!("temm1e> ");
                    continue;
                }

                // enc:v1: — encrypted blob from OTK flow
                if msg_text.trim().starts_with("enc:v1:") {
                    let blob_b64 = &msg_text.trim()["enc:v1:".len()..];
                    match decrypt_otk_blob(blob_b64, &setup_tokens, &msg.chat_id).await {
                        Ok(api_key_text) => {
                            if let Some(cred) = detect_api_key(&api_key_text) {
                                let model = default_model(cred.provider).to_string();
                                let effective_base_url =
                                    cred.base_url.clone().or_else(|| base_url.clone());
                                let test_config = temm1e_core::types::config::ProviderConfig {
                                    name: Some(cred.provider.to_string()),
                                    api_key: Some(cred.api_key.clone()),
                                    keys: vec![cred.api_key.clone()],
                                    model: Some(model.clone()),
                                    base_url: effective_base_url,
                                    extra_headers: std::collections::HashMap::new(),
                                };
                                match validate_provider_key(&test_config).await {
                                    Ok(validated_provider) => {
                                        if let Err(e) = save_credentials(
                                            cred.provider,
                                            &cred.api_key,
                                            &model,
                                            cred.base_url.as_deref(),
                                        )
                                        .await
                                        {
                                            eprintln!("Failed to save credentials: {}", e);
                                        }
                                        let system_prompt = Some(build_system_prompt());
                                        agent_opt = Some(
                                            temm1e_agent::AgentRuntime::with_limits(
                                                validated_provider,
                                                memory.clone(),
                                                tools_template.clone(),
                                                model.clone(),
                                                system_prompt,
                                                max_turns,
                                                max_ctx,
                                                max_rounds,
                                                max_task_duration,
                                                max_spend,
                                            )
                                            .with_v2_optimizations(v2_opt)
                                            .with_parallel_phases(pp_opt)
                                            .with_shared_mode(shared_mode.clone())
                                            .with_shared_memory_strategy(
                                                shared_memory_strategy.clone(),
                                            ),
                                        );
                                        println!(
                                            "\nAPI key securely received and verified! Configured {} with model {}.",
                                            cred.provider, model
                                        );
                                        println!("TEMM1E is online.\n");
                                    }
                                    Err(err) => {
                                        eprintln!(
                                            "\nKey decrypted but validation failed — {} returned:\n{}\nCheck the key and try /addkey again.\n",
                                            cred.provider, err
                                        );
                                    }
                                }
                            } else {
                                eprintln!(
                                    "\nDecrypted successfully but couldn't detect the provider.\nMake sure you pasted a valid API key in the setup page.\n"
                                );
                            }
                        }
                        Err(err) => {
                            eprintln!("\n{}\n", err);
                        }
                    }
                    eprint!("temm1e> ");
                    continue;
                }

                // Detect raw API key paste
                if let Some(cred) = detect_api_key(msg_text) {
                    let model = default_model(cred.provider).to_string();
                    let effective_base_url = cred.base_url.clone().or_else(|| base_url.clone());
                    let test_config = temm1e_core::types::config::ProviderConfig {
                        name: Some(cred.provider.to_string()),
                        api_key: Some(cred.api_key.clone()),
                        keys: vec![cred.api_key.clone()],
                        model: Some(model.clone()),
                        base_url: effective_base_url,
                        extra_headers: std::collections::HashMap::new(),
                    };
                    match validate_provider_key(&test_config).await {
                        Ok(validated_provider) => {
                            if let Err(e) = save_credentials(
                                cred.provider,
                                &cred.api_key,
                                &model,
                                cred.base_url.as_deref(),
                            )
                            .await
                            {
                                eprintln!("Failed to save credentials: {}", e);
                            }
                            let system_prompt = Some(build_system_prompt());
                            agent_opt = Some(
                                temm1e_agent::AgentRuntime::with_limits(
                                    validated_provider,
                                    memory.clone(),
                                    tools_template.clone(),
                                    model.clone(),
                                    system_prompt,
                                    max_turns,
                                    max_ctx,
                                    max_rounds,
                                    max_task_duration,
                                    max_spend,
                                )
                                .with_v2_optimizations(v2_opt)
                                .with_parallel_phases(pp_opt)
                                .with_hive_enabled(hive_enabled_early)
                                .with_shared_mode(shared_mode.clone())
                                .with_shared_memory_strategy(shared_memory_strategy.clone()),
                            );
                            println!(
                                "\nAPI key verified! Configured {} with model {}.",
                                cred.provider, model
                            );
                            println!("TEMM1E is online.\n");
                        }
                        Err(err) => {
                            eprintln!(
                                "\nInvalid API key — {} returned:\n{}\nCheck the key and try again.\n",
                                cred.provider, err
                            );
                        }
                    }
                    eprint!("temm1e> ");
                    continue;
                }

                // ── Normal agent processing ────────────────────
                if let Some(ref agent) = agent_opt {
                    let mut session = temm1e_core::types::session::SessionContext {
                        session_id: "cli-cli".to_string(),
                        user_id: msg.user_id.clone(),
                        channel: msg.channel.clone(),
                        chat_id: msg.chat_id.clone(),
                        history: history.clone(),
                        workspace_path: workspace.clone(),
                    };

                    // Early reply channel for LLM classifier (order acknowledgments)
                    let (early_tx, mut early_rx) = tokio::sync::mpsc::unbounded_channel::<
                        temm1e_core::types::message::OutboundMessage,
                    >();
                    let cli_for_early = cli_arc.clone();
                    tokio::spawn(async move {
                        while let Some(mut early_msg) = early_rx.recv().await {
                            early_msg.text = censor_secrets(&early_msg.text);
                            cli_for_early.send_message(early_msg).await.ok();
                        }
                    });

                    let process_result = AssertUnwindSafe(agent.process_message(
                        &msg,
                        &mut session,
                        None,
                        None,
                        Some(early_tx),
                        None,
                        None,
                    ))
                    .catch_unwind()
                    .await;

                    match process_result {
                        Ok(Ok((mut reply, turn_usage))) => {
                            reply.text = censor_secrets(&reply.text);
                            cli_arc.send_message(reply).await.ok();

                            // Record usage
                            let record = temm1e_core::UsageRecord {
                                id: uuid::Uuid::new_v4().to_string(),
                                chat_id: msg.chat_id.clone(),
                                session_id: "cli-cli".to_string(),
                                timestamp: chrono::Utc::now(),
                                api_calls: turn_usage.api_calls,
                                input_tokens: turn_usage.input_tokens,
                                output_tokens: turn_usage.output_tokens,
                                tools_used: turn_usage.tools_used,
                                total_cost_usd: turn_usage.total_cost_usd,
                                provider: turn_usage.provider.clone(),
                                model: turn_usage.model.clone(),
                            };
                            if let Err(e) = usage_store.record_usage(record).await {
                                tracing::error!(error = %e, "Failed to record usage");
                            }

                            // Display usage summary if enabled
                            if turn_usage.api_calls > 0 {
                                if let Ok(enabled) =
                                    usage_store.is_usage_display_enabled(&msg.chat_id).await
                                {
                                    if enabled {
                                        println!("\n{}", turn_usage.format_summary());
                                    }
                                }
                            }
                        }
                        Ok(Err(temm1e_core::types::error::Temm1eError::HiveRoute(hive_msg))) => {
                            // CLI pack path — simplified version
                            println!("  [Many Tems: Alpha decomposing into pack tasks...]");
                            // For CLI, fall back to single-agent since the hive
                            // infrastructure needs the full dispatcher (Start command).
                            // Re-process as a normal message without hive.
                            if let Some(ref mut agent) = agent_opt {
                                let non_hive = temm1e_agent::AgentRuntime::with_limits(
                                    agent.provider_arc(),
                                    agent.memory_arc(),
                                    agent.tools().to_vec(),
                                    agent.model().to_string(),
                                    None,
                                    max_turns,
                                    max_ctx,
                                    max_rounds,
                                    max_task_duration,
                                    max_spend,
                                )
                                .with_v2_optimizations(v2_opt)
                                .with_parallel_phases(pp_opt);
                                let re_msg = temm1e_core::types::message::InboundMessage {
                                    id: uuid::Uuid::new_v4().to_string(),
                                    channel: "cli".into(),
                                    chat_id: "cli".into(),
                                    user_id: "local".into(),
                                    username: None,
                                    text: Some(hive_msg),
                                    attachments: vec![],
                                    reply_to: None,
                                    timestamp: chrono::Utc::now(),
                                };
                                match non_hive
                                    .process_message(
                                        &re_msg,
                                        &mut session,
                                        None,
                                        None,
                                        None,
                                        None,
                                        None,
                                    )
                                    .await
                                {
                                    Ok((reply, _usage)) => {
                                        if !reply.text.trim().is_empty() {
                                            println!("\n{}\n", reply.text);
                                        }
                                    }
                                    Err(e) => eprintln!("  [{}]", format_user_error(&e)),
                                }
                            }
                            eprint!("temm1e> ");
                        }
                        Ok(Err(e)) => {
                            tracing::error!(error = %e, "CLI agent processing error");
                            eprintln!("  [{}]", format_user_error(&e));
                            eprint!("temm1e> ");
                        }
                        Err(panic_info) => {
                            let panic_msg = if let Some(s) = panic_info.downcast_ref::<String>() {
                                s.clone()
                            } else if let Some(s) = panic_info.downcast_ref::<&str>() {
                                s.to_string()
                            } else {
                                "internal error".to_string()
                            };
                            eprintln!("  [panic recovered: {}]", panic_msg);
                            tracing::error!(panic = %panic_msg, "PANIC RECOVERED in CLI processing");
                            // Rollback session to pre-message state
                            session.history = history.clone();
                        }
                    }

                    history = session.history;

                    // ── Save CLI conversation history to memory backend ──
                    if let Ok(json) = serde_json::to_string(&history) {
                        let entry = temm1e_core::MemoryEntry {
                            id: cli_history_key.clone(),
                            content: json,
                            metadata: serde_json::json!({"chat_id": "cli"}),
                            timestamp: chrono::Utc::now(),
                            session_id: Some("cli".to_string()),
                            entry_type: temm1e_core::MemoryEntryType::Conversation,
                        };
                        if let Err(e) = memory.store(entry).await {
                            tracing::warn!(error = %e, "Failed to persist CLI conversation history");
                        }
                    }
                } else {
                    // Auto-generate fresh OTK for onboarding
                    let otk = setup_tokens.generate("cli").await;
                    let otk_hex = hex::encode(otk);
                    let link = format!("https://nagisanzenin.github.io/temm1e/setup#{}", otk_hex);
                    println!("\n{}", onboarding_message_with_link(&link));
                    println!("\n{}\n", ONBOARDING_REFERENCE);
                    eprint!("temm1e> ");
                }
            }

            println!("\nTEMM1E chat ended.");
        }
        Commands::Status => {
            println!("TEMM1E Status");
            println!("  Mode: {}", config.temm1e.mode);
            println!("  Gateway: {}:{}", config.gateway.host, config.gateway.port);
            println!(
                "  Provider: {}",
                config.provider.name.as_deref().unwrap_or("not configured")
            );
            println!("  Memory: {}", config.memory.backend);
            println!("  Vault: {}", config.vault.backend);
        }
        Commands::Skill { command } => match command {
            SkillCommands::List => {
                println!("Installed skills:");
            }
            SkillCommands::Info { name } => {
                println!("Skill info: {}", name);
            }
            SkillCommands::Install { path } => {
                println!("Installing skill from: {}", path);
            }
        },
        Commands::Config { command } => match command {
            ConfigCommands::Validate => {
                println!("Configuration valid.");
                println!("  Gateway: {}:{}", config.gateway.host, config.gateway.port);
                println!(
                    "  Provider: {}",
                    config.provider.name.as_deref().unwrap_or("none")
                );
                println!("  Memory backend: {}", config.memory.backend);
                println!("  Channels: {}", config.channel.len());
            }
            ConfigCommands::Show => {
                let output = toml::to_string_pretty(&config)?;
                println!("{}", output);
            }
        },
        Commands::Update => {
            println!("TEMM1E Update");
            println!("Current version: {}\n", env!("CARGO_PKG_VERSION"));

            // 1. Check if we're in a git repo
            let git_check = std::process::Command::new("git")
                .args(["rev-parse", "--is-inside-work-tree"])
                .output();
            match git_check {
                Ok(out) if out.status.success() => {}
                _ => {
                    eprintln!("Error: Not a git repository. Run `temm1e update` from the cloned repo directory.");
                    std::process::exit(1);
                }
            }

            // 2. Fetch remote
            println!("Fetching latest changes...");
            let fetch = std::process::Command::new("git")
                .args(["fetch", "origin"])
                .output();
            if let Err(e) = fetch {
                eprintln!("Error: Failed to fetch from remote: {}", e);
                std::process::exit(1);
            }

            // 3. Compare local vs remote
            let local_head = std::process::Command::new("git")
                .args(["rev-parse", "HEAD"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();

            // Detect the default remote branch (main or master)
            let remote_branch = {
                let check_main = std::process::Command::new("git")
                    .args(["rev-parse", "--verify", "origin/main"])
                    .output();
                if check_main.is_ok_and(|o| o.status.success()) {
                    "origin/main"
                } else {
                    "origin/master"
                }
            };

            let remote_head = std::process::Command::new("git")
                .args(["rev-parse", remote_branch])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
                .unwrap_or_default();

            if local_head == remote_head {
                println!("Already up to date.");
                return Ok(());
            }

            // 4. Show what's new
            let log_range = format!("HEAD..{}", remote_branch);
            let log_output = std::process::Command::new("git")
                .args(["log", "--oneline", "--no-decorate", &log_range])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();

            let commit_count = log_output.lines().count();
            println!("{} new commit(s):\n", commit_count);
            for line in log_output.lines().take(20) {
                println!("  {}", line);
            }
            if commit_count > 20 {
                println!("  ... and {} more", commit_count - 20);
            }
            println!();

            // 5. Check for dirty working tree
            let status = std::process::Command::new("git")
                .args(["status", "--porcelain"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();
            if !status.trim().is_empty() {
                eprintln!("Warning: You have uncommitted changes. Stashing before update...");
                let stash = std::process::Command::new("git")
                    .args(["stash", "push", "-m", "temm1e-update-autostash"])
                    .output();
                if stash.map_or(true, |o| !o.status.success()) {
                    eprintln!("Error: Failed to stash changes. Commit or stash manually first.");
                    std::process::exit(1);
                }
                println!("Changes stashed.\n");
            }

            // 6. Pull
            let branch = remote_branch.strip_prefix("origin/").unwrap_or("main");
            println!("Pulling from origin/{}...", branch);
            let pull = std::process::Command::new("git")
                .args(["pull", "origin", branch])
                .output();
            match pull {
                Ok(out) if out.status.success() => {
                    println!("{}", String::from_utf8_lossy(&out.stdout));
                }
                Ok(out) => {
                    eprintln!(
                        "Error: git pull failed:\n{}",
                        String::from_utf8_lossy(&out.stderr)
                    );
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("Error: git pull failed: {}", e);
                    std::process::exit(1);
                }
            }

            // 7. Build release binary
            println!("Building release binary... (this may take a few minutes)");
            let build = std::process::Command::new("cargo")
                .args(["build", "--release", "--bin", "temm1e"])
                .status();
            match build {
                Ok(s) if s.success() => {
                    println!("\nUpdate complete!");
                    println!("Restart with: temm1e start");
                }
                Ok(s) => {
                    eprintln!("\nBuild failed with exit code: {:?}", s.code());
                    eprintln!("The source was updated but the binary was not rebuilt.");
                    eprintln!("Run `cargo build --release --bin temm1e` manually to retry.");
                    std::process::exit(1);
                }
                Err(e) => {
                    eprintln!("\nBuild failed: {}", e);
                    eprintln!("The source was updated but the binary was not rebuilt.");
                    std::process::exit(1);
                }
            }

            // 8. Pop stash if we stashed earlier
            let stash_list = std::process::Command::new("git")
                .args(["stash", "list"])
                .output()
                .map(|o| String::from_utf8_lossy(&o.stdout).to_string())
                .unwrap_or_default();
            if stash_list.contains("temm1e-update-autostash") {
                println!("Restoring stashed changes...");
                let _ = std::process::Command::new("git")
                    .args(["stash", "pop"])
                    .output();
            }
        }
        Commands::Version => {
            println!(
                "temm1e {} — commit: {} — date: {}",
                env!("CARGO_PKG_VERSION"),
                env!("GIT_HASH"),
                env!("BUILD_DATE")
            );
            println!("Cloud-native Rust AI agent runtime — Telegram-native");
        }
        #[cfg(feature = "codex-oauth")]
        Commands::Auth { command } => match command {
            AuthCommands::Login { headless, output } => {
                println!("TEMM1E — OpenAI Codex OAuth Login");
                println!("Authenticating with your ChatGPT subscription...\n");

                match temm1e_codex_oauth::login(headless).await {
                    Ok(store) => {
                        let email = store.email().await;
                        let expires = store.expires_in().await;
                        println!("\n  Authenticated successfully!");
                        println!("  Email:   {}", email);
                        println!("  Expires: {}", expires);
                        println!("  Model:   gpt-5.4 (default)");

                        // Export to custom path if --output was specified
                        if let Some(ref out_path) = output {
                            let path = std::path::PathBuf::from(out_path);
                            let tokens = store.get_tokens().await;
                            let dir = path.parent().unwrap_or(std::path::Path::new("."));
                            if let Err(e) = std::fs::create_dir_all(dir) {
                                eprintln!("Failed to create directory {}: {}", dir.display(), e);
                                std::process::exit(1);
                            }
                            let content = serde_json::to_string_pretty(&tokens).unwrap();
                            if let Err(e) = std::fs::write(&path, content) {
                                eprintln!("Failed to write {}: {}", path.display(), e);
                                std::process::exit(1);
                            }
                            println!("  Exported: {}", path.display());
                        }

                        println!("\n  Run `temm1e start` to go online.");
                    }
                    Err(e) => {
                        eprintln!("Authentication failed: {}", e);
                        std::process::exit(1);
                    }
                }
            }
            AuthCommands::Status => {
                if !temm1e_codex_oauth::TokenStore::exists() {
                    println!("Not authenticated. Run `temm1e auth login` to connect your ChatGPT account.");
                    return Ok(());
                }
                match temm1e_codex_oauth::TokenStore::load() {
                    Ok(store) => {
                        let email = store.email().await;
                        let account = store.account_id().await;
                        let expires = store.expires_in().await;
                        let expired = store.is_expired().await;
                        println!("TEMM1E — Codex OAuth Status");
                        println!("  Email:      {}", email);
                        println!("  Account:    {}", account);
                        println!(
                            "  Token:      {}",
                            if expired { "expired" } else { "valid" }
                        );
                        println!("  Expires in: {}", expires);
                    }
                    Err(e) => {
                        eprintln!("Failed to read OAuth tokens: {}", e);
                    }
                }
            }
            AuthCommands::Logout => match temm1e_codex_oauth::TokenStore::delete() {
                Ok(()) => {
                    println!("Logged out. OAuth tokens removed.");
                }
                Err(e) => {
                    eprintln!("Failed to remove tokens: {}", e);
                }
            },
        },
        // Reset is handled before config loading — this arm is unreachable
        Commands::Reset { .. } => unreachable!(),
        #[cfg(feature = "tui")]
        Commands::Tui => {
            temm1e_tui::launch_tui(config).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // ── detect_api_key: auto-detect from prefix ──────────────────────

    #[test]
    fn detect_anthropic_key() {
        let result = detect_api_key("sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(result.unwrap().provider, "anthropic");
    }

    #[test]
    fn detect_openai_key() {
        let result = detect_api_key("sk-proj-abcdefghijklmnopqrstuv");
        assert_eq!(result.unwrap().provider, "openai");
    }

    #[test]
    fn detect_openrouter_key() {
        let result = detect_api_key("sk-or-v1-abcdefghijklmnopqrstuv");
        assert_eq!(result.unwrap().provider, "openrouter");
    }

    #[test]
    fn detect_grok_key() {
        let result = detect_api_key("xai-abcdefghijklmnopqrstuvwxyz");
        assert_eq!(result.unwrap().provider, "grok");
    }

    #[test]
    fn detect_gemini_key() {
        let result = detect_api_key("AIzaSyA-abcdefghijklmnopqrstu");
        assert_eq!(result.unwrap().provider, "gemini");
    }

    #[test]
    fn detect_unknown_key_returns_none() {
        assert!(detect_api_key("unknown-key-format-here").is_none());
    }

    // ── detect_api_key: explicit provider:key format ─────────────────

    #[test]
    fn explicit_minimax_key() {
        let result = detect_api_key("minimax:eyJhbGciOiJSUzI1NiIsInR5cCI6").unwrap();
        assert_eq!(result.provider, "minimax");
        assert_eq!(result.api_key, "eyJhbGciOiJSUzI1NiIsInR5cCI6");
    }

    #[test]
    fn explicit_openrouter_key() {
        let result = detect_api_key("openrouter:sk-or-v1-abcdefghijklm").unwrap();
        assert_eq!(result.provider, "openrouter");
        assert_eq!(result.api_key, "sk-or-v1-abcdefghijklm");
    }

    #[test]
    fn explicit_grok_with_xai_alias() {
        let result = detect_api_key("xai:some-long-api-key-value").unwrap();
        assert_eq!(result.provider, "grok");
        assert_eq!(result.api_key, "some-long-api-key-value");
    }

    #[test]
    fn explicit_ollama_key() {
        let result = detect_api_key("ollama:some-long-ollama-api-key").unwrap();
        assert_eq!(result.provider, "ollama");
        assert_eq!(result.api_key, "some-long-ollama-api-key");
    }

    #[test]
    fn explicit_zai_key() {
        let result =
            detect_api_key("zai:24f7a8ebaa2f4cb1866b82b0670a5e6c.rPGt3alOjwddy4l1").unwrap();
        assert_eq!(result.provider, "zai");
        assert_eq!(
            result.api_key,
            "24f7a8ebaa2f4cb1866b82b0670a5e6c.rPGt3alOjwddy4l1"
        );
    }

    #[test]
    fn explicit_zhipu_key() {
        let result =
            detect_api_key("zhipu:24f7a8ebaa2f4cb1866b82b0670a5e6c.rPGt3alOjwddy4l1").unwrap();
        assert_eq!(result.provider, "zai");
    }

    #[test]
    fn explicit_format_case_insensitive() {
        let result = detect_api_key("MiniMax:eyJhbGciOiJSUzI1NiIsInR5cCI6");
        assert_eq!(result.unwrap().provider, "minimax");
    }

    #[test]
    fn explicit_format_short_key_rejected() {
        assert!(detect_api_key("minimax:short").is_none());
    }

    #[test]
    fn explicit_unknown_provider_falls_through() {
        assert!(detect_api_key("fakeprovider:some-key-value").is_none());
    }

    // ── detect_api_key: ordering (specific before generic) ───────────

    #[test]
    fn openrouter_not_misdetected_as_openai() {
        let result = detect_api_key("sk-or-v1-abcdefghijklmnopqrstuv");
        assert_eq!(result.unwrap().provider, "openrouter");
    }

    #[test]
    fn anthropic_not_misdetected_as_openai() {
        let result = detect_api_key("sk-ant-api03-AAAAAAAAAAAAAAAAAAAAAA");
        assert_eq!(result.unwrap().provider, "anthropic");
    }

    // ── detect_api_key: proxy format ────────────────────────────────

    #[test]
    fn proxy_with_key_value_format() {
        let result = detect_api_key(
            "proxy provider:openai base_url:https://my-proxy.com/v1 key:sk-test-key-12345678",
        )
        .unwrap();
        assert_eq!(result.provider, "openai");
        assert_eq!(result.api_key, "sk-test-key-12345678");
        assert_eq!(result.base_url.unwrap(), "https://my-proxy.com/v1");
    }

    #[test]
    fn proxy_with_positional_format() {
        let result =
            detect_api_key("proxy openai https://my-proxy.com/v1 sk-test-key-12345678").unwrap();
        assert_eq!(result.provider, "openai");
        assert_eq!(result.api_key, "sk-test-key-12345678");
        assert_eq!(result.base_url.unwrap(), "https://my-proxy.com/v1");
    }

    #[test]
    fn proxy_with_url_alias() {
        let result = detect_api_key(
            "proxy provider:anthropic url:https://claude-proxy.com/v1 key:sk-ant-test1234",
        )
        .unwrap();
        assert_eq!(result.provider, "anthropic");
        assert_eq!(result.base_url.unwrap(), "https://claude-proxy.com/v1");
    }

    #[test]
    fn proxy_defaults_to_openai() {
        let result = detect_api_key("proxy https://my-proxy.com/v1 sk-test-key-12345678").unwrap();
        assert_eq!(result.provider, "openai");
        assert_eq!(result.base_url.unwrap(), "https://my-proxy.com/v1");
    }

    #[test]
    fn proxy_too_few_tokens_returns_none() {
        assert!(detect_api_key("proxy openai").is_none());
    }

    // ── default_model ────────────────────────────────────────────────

    #[test]
    fn default_models_all_providers() {
        assert_eq!(default_model("anthropic"), "claude-sonnet-4-6");
        assert_eq!(default_model("openai"), "gpt-5.2");
        assert_eq!(default_model("openai-codex"), "gpt-5.4");
        assert_eq!(default_model("gemini"), "gemini-3-flash-preview");
        assert_eq!(default_model("grok"), "grok-4-1-fast-non-reasoning");
        assert_eq!(default_model("xai"), "grok-4-1-fast-non-reasoning");
        assert_eq!(default_model("openrouter"), "anthropic/claude-sonnet-4-6");
        assert_eq!(default_model("minimax"), "MiniMax-M2.5");
        assert_eq!(default_model("zai"), "glm-4.7-flash");
        assert_eq!(default_model("ollama"), "llama3.3");
    }

    #[test]
    fn default_model_unknown_falls_back() {
        assert_eq!(default_model("unknown"), "claude-sonnet-4-6");
    }

    // ── is_placeholder_key ────────────────────────────────────────────

    #[test]
    fn placeholder_key_rejects_common_fakes() {
        assert!(is_placeholder_key("PASTE_YOUR_KEY_HERE"));
        assert!(is_placeholder_key("your_api_key"));
        assert!(is_placeholder_key("your-key-goes-here"));
        assert!(is_placeholder_key("insert_your_key_here"));
        assert!(is_placeholder_key("replace_with_your_key"));
        assert!(is_placeholder_key("placeholder_key_value"));
        assert!(is_placeholder_key("enter_your_api_key_here"));
        assert!(is_placeholder_key("xxxxxxxxxx"));
        assert!(is_placeholder_key("your_token_goes_here"));
    }

    #[test]
    fn placeholder_key_rejects_too_short() {
        assert!(is_placeholder_key("sk-abc"));
        assert!(is_placeholder_key("short"));
        assert!(is_placeholder_key(""));
    }

    #[test]
    fn placeholder_key_rejects_all_same_char() {
        assert!(is_placeholder_key("aaaaaaaaaa"));
        assert!(is_placeholder_key("0000000000"));
    }

    #[test]
    fn placeholder_key_accepts_real_keys() {
        assert!(!is_placeholder_key(
            "sk-ant-api03-abc123def456ghi789jkl012mno345pqr678stu"
        ));
        assert!(!is_placeholder_key("sk-proj-abcdefghijklmnopqrstuv"));
        assert!(!is_placeholder_key("sk-or-v1-abcdefghijklmnopqrstuv"));
        assert!(!is_placeholder_key("xai-abcdefghijklmnopqrstuvwxyz"));
        assert!(!is_placeholder_key("AIzaSyA-abcdefghijklmnopqrstu"));
        assert!(!is_placeholder_key("sk-test-key-12345678")); // valid test fixture format
    }

    #[test]
    fn detect_rejects_placeholder_auto_format() {
        // These match sk- prefix but are obvious placeholders
        assert!(detect_api_key("sk-your_key_here_12345").is_none());
        assert!(detect_api_key("sk-ant-paste_your_key_here").is_none());
    }

    // ── OTK: AES-256-GCM encrypt/decrypt round-trip ──────────────────

    #[tokio::test]
    async fn otk_encrypt_decrypt_round_trip() {
        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes256Gcm, Key, Nonce};

        let store = temm1e_gateway::SetupTokenStore::new();
        let otk = store.generate("test-chat").await;

        // Simulate browser-side encryption
        let api_key = "sk-ant-api03-realkey1234567890abcdef";
        let key = Key::<Aes256Gcm>::from_slice(&otk);
        let cipher = Aes256Gcm::new(key);

        let mut iv = [0u8; 12];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut iv);
        let nonce = Nonce::from_slice(&iv);

        let ciphertext = cipher
            .encrypt(nonce, api_key.as_bytes())
            .expect("encryption failed");

        // Concatenate IV + ciphertext (matches WebCrypto format)
        let mut blob = Vec::with_capacity(12 + ciphertext.len());
        blob.extend_from_slice(&iv);
        blob.extend_from_slice(&ciphertext);

        let b64 = base64::engine::general_purpose::STANDARD.encode(&blob);

        // Decrypt using the OTK flow
        let result = decrypt_otk_blob(&b64, &store, "test-chat").await;
        assert_eq!(result.unwrap(), api_key);
    }

    #[tokio::test]
    async fn otk_decrypt_wrong_chat_id_fails() {
        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes256Gcm, Key, Nonce};

        let store = temm1e_gateway::SetupTokenStore::new();
        let otk = store.generate("chat-a").await;

        let api_key = "sk-ant-api03-testkey123456789";
        let key = Key::<Aes256Gcm>::from_slice(&otk);
        let cipher = Aes256Gcm::new(key);
        let iv = [1u8; 12];
        let nonce = Nonce::from_slice(&iv);
        let ciphertext = cipher
            .encrypt(nonce, api_key.as_bytes())
            .expect("encryption failed");

        let mut blob = Vec::new();
        blob.extend_from_slice(&iv);
        blob.extend_from_slice(&ciphertext);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&blob);

        // Try to decrypt with wrong chat_id — should fail (no OTK)
        let result = decrypt_otk_blob(&b64, &store, "chat-b").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No pending setup link"));
    }

    #[tokio::test]
    async fn otk_decrypt_expired_token_fails() {
        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes256Gcm, Key, Nonce};

        let store = temm1e_gateway::SetupTokenStore::with_ttl(std::time::Duration::from_millis(1));
        let otk = store.generate("chat-expire").await;

        let api_key = "sk-ant-api03-testkey123456789";
        let key = Key::<Aes256Gcm>::from_slice(&otk);
        let cipher = Aes256Gcm::new(key);
        let iv = [2u8; 12];
        let nonce = Nonce::from_slice(&iv);
        let ciphertext = cipher
            .encrypt(nonce, api_key.as_bytes())
            .expect("encryption failed");

        let mut blob = Vec::new();
        blob.extend_from_slice(&iv);
        blob.extend_from_slice(&ciphertext);
        let b64 = base64::engine::general_purpose::STANDARD.encode(&blob);

        tokio::time::sleep(std::time::Duration::from_millis(10)).await;

        let result = decrypt_otk_blob(&b64, &store, "chat-expire").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("No pending setup link"));
    }

    #[tokio::test]
    async fn otk_decrypt_tampered_blob_fails() {
        let store = temm1e_gateway::SetupTokenStore::new();
        let _otk = store.generate("chat-tamper").await;

        // Tampered blob — valid base64 but wrong ciphertext
        let fake_blob = base64::engine::general_purpose::STANDARD.encode([0u8; 64]); // random bytes

        let result = decrypt_otk_blob(&fake_blob, &store, "chat-tamper").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Decryption failed"));
    }

    #[tokio::test]
    async fn otk_decrypt_invalid_base64_fails() {
        let store = temm1e_gateway::SetupTokenStore::new();
        let _otk = store.generate("chat-b64").await;

        let result = decrypt_otk_blob("not!valid!base64!!!", &store, "chat-b64").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("Invalid base64"));
    }

    #[tokio::test]
    async fn otk_decrypt_too_short_blob_fails() {
        let store = temm1e_gateway::SetupTokenStore::new();
        let _otk = store.generate("chat-short").await;

        let short_blob = base64::engine::general_purpose::STANDARD.encode([0u8; 10]);

        let result = decrypt_otk_blob(&short_blob, &store, "chat-short").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("too short"));
    }

    #[test]
    fn enc_v1_prefix_detection() {
        assert!("enc:v1:SGVsbG8gV29ybGQ=".starts_with("enc:v1:"));
        assert!(!"sk-ant-api03-abc".starts_with("enc:v1:"));
        assert!(!"enc:v2:something".starts_with("enc:v1:"));
    }

    // ── Command parsing ──────────────────────────────────────────────

    #[test]
    fn command_addkey_detection() {
        assert_eq!("/addkey".trim().to_lowercase(), "/addkey");
        assert_eq!("/addkey ".trim().to_lowercase(), "/addkey");
        assert_eq!("  /addkey  ".trim().to_lowercase(), "/addkey");
    }

    #[test]
    fn command_addkey_unsafe_detection() {
        assert_eq!("/addkey unsafe".trim().to_lowercase(), "/addkey unsafe");
        assert_eq!("  /addkey unsafe  ".trim().to_lowercase(), "/addkey unsafe");
    }

    #[test]
    fn command_keys_detection() {
        assert_eq!("/keys".trim().to_lowercase(), "/keys");
    }

    #[test]
    fn command_removekey_detection() {
        let cmd = "/removekey openai";
        let lower = cmd.trim().to_lowercase();
        assert!(lower.starts_with("/removekey"));
        let provider = cmd.trim()["/removekey".len()..].trim();
        assert_eq!(provider, "openai");
    }

    #[test]
    fn command_removekey_no_provider() {
        let cmd = "/removekey";
        let provider = cmd.trim()["/removekey".len()..].trim();
        assert!(provider.is_empty());
    }

    // ── list/remove helpers ──────────────────────────────────────────

    #[test]
    fn list_providers_no_credentials() {
        // When no credentials file exists, returns helpful message
        let result = list_configured_providers();
        // Either returns provider list or "no providers" message — both valid
        assert!(!result.is_empty());
    }

    #[test]
    fn remove_provider_empty_name() {
        let result = remove_provider("");
        assert!(result.contains("Usage"));
    }

    // ── OTK hex encoding ─────────────────────────────────────────────

    #[test]
    fn otk_hex_encoding_format() {
        let bytes = [0xab_u8; 32];
        let hex_str = hex::encode(bytes);
        assert_eq!(hex_str.len(), 64); // 32 bytes = 64 hex chars
        assert!(hex_str.chars().all(|c| c.is_ascii_hexdigit()));
    }

    /// Full end-to-end: generate OTK → hex encode (like /addkey) → decode hex
    /// (like browser) → encrypt (like browser) → format as enc:v1: → decrypt
    /// (like server). Verifies the entire chain is consistent.
    #[tokio::test]
    async fn otk_full_e2e_hex_roundtrip() {
        use aes_gcm::aead::{Aead, KeyInit};
        use aes_gcm::{Aes256Gcm, Key, Nonce};

        let store = temm1e_gateway::SetupTokenStore::new();
        let otk = store.generate("e2e-chat").await;

        // Step 1: Server encodes OTK as hex (what goes into the URL fragment)
        let otk_hex = hex::encode(otk);
        assert_eq!(otk_hex.len(), 64);

        // Step 2: Browser decodes hex back to bytes (simulating JS hex decode)
        let browser_otk = hex::decode(&otk_hex).expect("hex decode failed");
        assert_eq!(browser_otk.len(), 32);
        assert_eq!(&browser_otk[..], &otk[..]);

        // Step 3: Browser encrypts with AES-256-GCM (simulating WebCrypto)
        let api_key = "sk-ant-api03-test1234567890abcdefghijklmnopqrs";
        let key = Key::<Aes256Gcm>::from_slice(&browser_otk);
        let cipher = Aes256Gcm::new(key);
        let mut iv = [0u8; 12];
        use rand::RngCore;
        rand::thread_rng().fill_bytes(&mut iv);
        let nonce = Nonce::from_slice(&iv);
        let ciphertext = cipher
            .encrypt(nonce, api_key.as_bytes())
            .expect("encryption failed");

        // Step 4: Browser builds "enc:v1:" blob
        let mut blob = Vec::with_capacity(12 + ciphertext.len());
        blob.extend_from_slice(&iv);
        blob.extend_from_slice(&ciphertext);
        let enc_blob = format!(
            "enc:v1:{}",
            base64::engine::general_purpose::STANDARD.encode(&blob)
        );

        // Step 5: Server detects prefix and decrypts
        assert!(enc_blob.starts_with("enc:v1:"));
        let blob_b64 = &enc_blob["enc:v1:".len()..];
        let result = decrypt_otk_blob(blob_b64, &store, "e2e-chat").await;
        assert_eq!(result.unwrap(), api_key);
    }

    /// Verify that detect_api_key works on decrypted OTK output for all providers.
    #[test]
    fn otk_decrypted_key_detection() {
        // These are the key formats users would paste into the setup page
        assert!(detect_api_key("sk-ant-api03-abcdefghijklmnop").is_some());
        assert!(detect_api_key("sk-proj-abcdefghijklmnop1234").is_some());
        assert!(detect_api_key("AIzaSyA-abcdefghijklmnopqrstu").is_some());
        assert!(detect_api_key("xai-abcdefghijklmnopqrstuvwxyz").is_some());
        assert!(detect_api_key("sk-or-v1-abcdefghijklmnopqrstu").is_some());
    }

    // ── censor_secrets: output filter ──────────────────────────────

    #[test]
    fn censor_no_credentials_file_returns_unchanged() {
        // When there are no credentials, text passes through unchanged
        let text = "Here is your key: sk-ant-test123456789";
        let result = censor_secrets(text);
        // Without credentials file, nothing to censor — returns as-is
        assert_eq!(result, text);
    }

    #[test]
    fn censor_replaces_known_key_in_text() {
        // Write a temporary credentials file for the test
        let path = credentials_path();
        let dir = path.parent().unwrap();
        std::fs::create_dir_all(dir).ok();

        // Save current file if exists, restore after test
        let backup = std::fs::read_to_string(&path).ok();

        let test_key = "sk-ant-test-SUPERSECRETKEY12345678";
        let creds_content = format!(
            "active = \"anthropic\"\n\n\
             [[providers]]\n\
             name = \"anthropic\"\n\
             keys = [\"{}\"]\n\
             model = \"claude-sonnet-4-6\"\n",
            test_key
        );
        std::fs::write(&path, &creds_content).expect("test: write credential file");

        let text = format!("Your API key is {} and it works great!", test_key);
        let censored = censor_secrets(&text);
        assert!(
            !censored.contains(test_key),
            "Key should be censored from output"
        );
        assert!(
            censored.contains("[REDACTED]"),
            "Should contain [REDACTED] placeholder"
        );
        assert_eq!(censored, "Your API key is [REDACTED] and it works great!");

        // Restore
        match backup {
            Some(content) => std::fs::write(&path, content).expect("test: restore credential file"),
            None => {
                std::fs::remove_file(&path).ok();
            }
        };
    }

    #[test]
    fn censor_ignores_placeholder_keys() {
        let text = "Your key is placeholder_or_empty";
        let result = censor_secrets(text);
        // Placeholder keys should not cause censoring
        assert_eq!(result, text);
    }
}
