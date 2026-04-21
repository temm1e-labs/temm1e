use std::collections::{HashMap, HashSet};
use std::panic::AssertUnwindSafe;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

mod search_install;

use anyhow::Result;
use async_trait::async_trait;
use base64::Engine as _;
use clap::{Parser, Subcommand};
use futures::FutureExt;
use temm1e_core::config::credentials::{
    credentials_path, detect_api_key, is_placeholder_key, is_placeholder_key_lenient,
    load_active_provider_keys, load_credentials_file, load_saved_credentials, save_credentials,
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
    /// Interactive setup wizard — guides you through first-time configuration
    Setup,
    /// Manage Eigen-Tune (self-tuning knowledge distillation)
    Eigentune {
        #[command(subcommand)]
        command: EigentuneCommands,
    },
    /// Manage web search integrations (install SearXNG, list backends)
    Search {
        #[command(subcommand)]
        command: SearchCommands,
    },
}

#[derive(Subcommand)]
enum SearchCommands {
    /// Install and configure SearXNG locally via Docker/Podman for unlimited general web search
    Install,
}

#[derive(Subcommand)]
enum EigentuneCommands {
    /// Show training status, prerequisites, and tier metrics
    Status,
    /// Print setup instructions for the local training stack
    Setup,
    /// Show or set the base model for fine-tuning
    Model { name: Option<String> },
    /// Manually trigger a state machine tick (advances tier transitions)
    Tick,
    /// Force-demote a graduated tier back to Collecting (Gate 7 emergency kill switch)
    Demote { tier: String },
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

    // Custom endpoint (LM Studio, Ollama, vLLM, custom proxy, …) — we can't
    // reliably validate the key via a test completion because the proxy's
    // model catalog is unknown to us. Trust the user's setup; the first
    // real message will surface any issue with a clear provider error,
    // rather than our misleading "Invalid API key" wrapper (which fires
    // on 404 "model not found" responses because the error classifier
    // below matches 404 as an auth failure).
    if config.base_url.is_some() {
        tracing::debug!(
            base_url = ?config.base_url,
            model = ?config.model,
            "Skipping validate_provider_key test call — custom base_url set"
        );
        return Ok(provider_arc);
    }

    let test_req = temm1e_core::types::message::CompletionRequest {
        model: config.model.clone().unwrap_or_default(),
        messages: vec![temm1e_core::types::message::ChatMessage {
            role: temm1e_core::types::message::Role::User,
            content: temm1e_core::types::message::MessageContent::Text("Hi".to_string()),
        }],
        tools: Vec::new(),
        // Per project rule (feedback_no_max_tokens): no hardcoded output caps.
        max_tokens: None,
        temperature: Some(0.0),
        system: None,
        system_volatile: None,
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
/// Format a capture timestamp into a human-readable age string.
///
/// Takes an ISO 8601 timestamp and returns e.g. "2h ago", "5m ago", "1d ago".
#[cfg(feature = "browser")]
fn format_capture_age(captured_at: &str) -> String {
    let captured = match chrono::DateTime::parse_from_rfc3339(captured_at) {
        Ok(dt) => dt.with_timezone(&chrono::Utc),
        Err(_) => return "unknown".to_string(),
    };
    let elapsed = chrono::Utc::now().signed_duration_since(captured);
    let secs = elapsed.num_seconds();
    if secs < 0 {
        "just now".to_string()
    } else if secs < 60 {
        format!("{}s ago", secs)
    } else if secs < 3600 {
        format!("{}m ago", secs / 60)
    } else if secs < 86400 {
        format!("{}h ago", secs / 3600)
    } else {
        format!("{}d ago", secs / 86400)
    }
}

/// Get the user's role from the role file for a specific channel.
/// Returns Admin if no file, user not found, or on error (safe default).
fn get_user_role(channel: &str, user_id: &str) -> temm1e_core::types::rbac::Role {
    temm1e_core::types::rbac::load_role_file(channel)
        .and_then(|rf| rf.role_of(user_id))
        .unwrap_or(temm1e_core::types::rbac::Role::Admin)
}

/// Check if a slash command is allowed for the user's role.
/// Returns true if allowed, false if blocked.
fn is_command_allowed_for_user(channel: &str, user_id: &str, command: &str) -> bool {
    let role = get_user_role(channel, user_id);
    role.is_command_allowed(command)
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

/// Format a Cambium session report for chat display.
fn format_cambium_report(report: &temm1e_cambium::session::CambiumSessionReport) -> String {
    let mut out = String::new();
    if report.success {
        out.push_str("Cambium session SUCCEEDED\n");
    } else {
        out.push_str("Cambium session FAILED\n");
    }
    out.push_str(&format!("Task:    {}\n", report.task));
    out.push_str(&format!("Model:   {}\n", report.model));
    out.push_str(&format!("Elapsed: {} ms\n", report.elapsed_ms));
    out.push_str(&format!(
        "Stages:  cargo check {} | cargo clippy {} | cargo test {}\n",
        if report.cargo_check_pass {
            "OK"
        } else {
            "FAIL"
        },
        if report.cargo_clippy_pass {
            "OK"
        } else {
            "FAIL"
        },
        if report.cargo_test_pass { "OK" } else { "FAIL" },
    ));
    if let Some(summary) = &report.test_summary {
        out.push_str(&format!("Tests:   {}\n", summary.trim()));
    }
    if let Some(reason) = &report.failure_reason {
        out.push_str(&format!("Reason:  {}\n", reason));
    }
    if let Some((path, content)) = report.files_generated.first() {
        let preview_lines = content.lines().take(40).collect::<Vec<_>>().join("\n");
        out.push_str(&format!(
            "\nGenerated {} (preview, first 40 lines):\n---\n{}\n---\n",
            path.display(),
            preview_lines
        ));
        if content.lines().count() > 40 {
            out.push_str("(truncated; see full output in tempdir)\n");
        }
    }
    out
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
stepfun:YOUR_KEY\n\
openrouter:YOUR_KEY\n\
ollama:YOUR_KEY\n\n\
3\u{fe0f}\u{20e3} Proxy / custom endpoint:\n\
proxy <provider> <base_url> <api_key>\n\n\
Example:\n\
proxy openai https://my-proxy.com/v1 sk-xxx\n\
proxy anthropic https://gateway.ai/v1/anthropic sk-ant-xxx\n\
proxy ollama https://ollama.com/v1 your-ollama-key";

const SYSTEM_PROMPT_BODY: &str = "\
You have full access to these tools:\n\
- shell: run any command\n\
- file_read / file_write / file_list: filesystem operations\n\
- web_fetch: HTTP GET requests against a known URL\n\
- web_search: search the web across 13 backends (hackernews, wikipedia, github, \
  stackoverflow, reddit, marginalia, arxiv, pubmed, duckduckgo — all free, no key; \
  plus searxng/exa/brave/tavily if user opts in). ONE tool, auto-picks default mix, \
  retry with backends=[...] for specific sources. Use for finding current info, \
  documentation, code, or facts not in training data.\n\
- browser: control a real Chrome browser (navigate, click, type, screenshot, \
  get_text, evaluate JS, get_html) — use this for any website interaction\n\
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
  USE THE BROWSER TOOL. Do not refuse or explain why you can't — just do it.\n\
- Do NOT close the browser after finishing a task. The browser stays open so \
  sessions persist (logged-in sites stay logged in). The user controls the \
  browser lifecycle with /browser close.\n\
- When using browser observe/accessibility_tree, share key findings with the user. \
Show them what elements you found (e.g., 'I can see a search box, login button, \
and 3 article links'). Don't just silently process the tree — the user wants to \
know what you see.\n\
- SECURITY: NEVER ask users to send passwords or credentials in chat.\n\
- LOGIN FLOW (follow this order):\n\
  1. First try browser action 'restore_web_session' with the service name — the user \
     may already have a saved session.\n\
  2. If restore fails, navigate to the login page and take a screenshot.\n\
  3. If you see a QR code on the page — send the screenshot to the user and say \
     'Scan this QR code to log in. Tell me when done.' Then wait for the user. \
     After they confirm, observe the page to verify login succeeded.\n\
  4. If you see a password form (no QR) — tell the user to use /login <service> \
     to enter credentials securely. NEVER type passwords yourself.\n\
  5. Once logged in, the session auto-saves. Future tasks restore automatically.\n\
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
/// Uses `personality` for the identity section instead of hardcoded Tem identity.
fn build_system_prompt(personality: &temm1e_anima::personality::PersonalityConfig) -> String {
    let identity = personality.generate_identity_section();
    let mut prompt = format!("{identity}\n\n{SYSTEM_PROMPT_BODY}");

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
    prompt.push_str("- stepfun: step-3.5-flash, step-3\n");
    prompt.push_str("- lmstudio: local models via http://localhost:1234/v1 — register your downloaded model with /addmodel (e.g. qwen3.5-7b-instruct, llama-3.3-70b-instruct)\n");
    prompt.push_str("- openai-codex: gpt-5.4 (recommended), gpt-5.3-codex, gpt-5.2-codex (OAuth subscription)\n");

    // ── Vision capability ──────────────────────────────────────
    prompt.push_str(
        "\nVISION (IMAGE) SUPPORT:\n\
         Models that can see images: all claude-*, all gpt-4o/gpt-4.1/gpt-5.*, all gemini-*, \
         grok-3/grok-4, glm-*v* (V-suffix only, e.g. glm-4.6v-flash), step-3.\n\
         Text-only (NO vision): gpt-3.5-turbo, glm-4.7-flash, glm-4.7, glm-5, glm-5-code, \
         glm-4.5-flash, all MiniMax models, step-3.5-flash.\n\
         If the user sends an image on a text-only model, images are auto-stripped and \
         the user is notified. Suggest switching to a vision model.\n",
    );

    // ── Current configuration ─────────────────────────────────
    if let Some(creds) = load_credentials_file() {
        prompt.push_str("\nCURRENT CONFIGURATION:\n");
        prompt.push_str(&format!("Active provider: {}\n", creds.active));
        for p in &creds.providers {
            // Proxy providers (custom base_url) use lenient placeholder check
            // so short LM Studio / Ollama keys are counted correctly.
            let has_custom = p.base_url.is_some();
            let key_count = p
                .keys
                .iter()
                .filter(|k| {
                    if has_custom {
                        !is_placeholder_key_lenient(k)
                    } else {
                        !is_placeholder_key(k)
                    }
                })
                .count();
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

    // ── TemDOS: Specialist cores ──────────────────────────────
    // Loaded dynamically — only inject if cores are available.
    // The core listing is injected at runtime from the registry;
    // this section provides the usage guidelines.
    prompt.push_str(
        "\n\n\
TEMDOS — SPECIALIST CORES:\n\
You have access to specialist cores via the invoke_core tool. Each core is an \
independent AI agent with full tool access that runs until completion and returns \
a detailed answer.\n\n\
WHEN TO USE CORES:\n\
- USE a core when a task requires deep, focused analysis that would take many \
  tool rounds (architecture review, security audit, test generation, debugging).\n\
- DO NOT use a core for simple tasks you can handle in 1-2 tool calls.\n\
- INVOKE MULTIPLE cores in parallel when you need independent analyses \
  (e.g., architecture AND security simultaneously).\n\n\
HOW CORES WORK:\n\
- Cores share your budget — they deduct from the same spending pool.\n\
- Cores have full tool access (file, shell, git, browser, etc.).\n\
- Cores run in isolation — they have their own context, not yours.\n\
- Cores CANNOT call other cores — they work alone and report back.\n\
- Be SPECIFIC in your task description — the core cannot ask follow-up questions.\n\
- Use the 'context' parameter to pass relevant info: previous findings, \
  constraints, or pre-read file contents. This reduces the core's cold start.\n\n\
GOOD INVOCATION:\n\
  invoke_core(core='architecture', task='Map all files that import from \
  temm1e-providers and list which types they use. I need the blast radius \
  of changing CompletionResponse.', context='Adding a cache_hit field')\n\n\
BAD INVOCATION:\n\
  invoke_core(core='architecture', task='look at the code')\n\n\
PARALLEL PATTERN:\n\
  When you need independent analyses, invoke multiple cores in a single response. \
  They run concurrently and you receive all results together.",
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
                // Proxy providers use lenient placeholder check so short
                // LM Studio / Ollama keys are counted correctly.
                let has_custom = p.base_url.is_some();
                let key_count = p
                    .keys
                    .iter()
                    .filter(|k| {
                        if has_custom {
                            !is_placeholder_key_lenient(k)
                        } else {
                            !is_placeholder_key(k)
                        }
                    })
                    .count();
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
    // Accept either hardcoded models OR user-registered custom models
    // (via /addmodel). Custom names extend the valid-target set for the
    // active provider — they never shadow hardcoded first-party names
    // unless the user explicitly opts in.
    let custom_names: Vec<String> =
        temm1e_core::config::custom_models::custom_models_for_provider(&active_provider.name)
            .into_iter()
            .map(|m| m.name)
            .collect();
    let in_custom = custom_names.iter().any(|n| n == target);
    if !is_proxy && !known.is_empty() && !known.contains(&target) && !in_custom {
        let list = known
            .iter()
            .map(|m| {
                let v = if is_vision_model(m) { " [vision]" } else { "" };
                format!("  {}{}", m, v)
            })
            .collect::<Vec<_>>()
            .join("\n");
        let custom_note = if custom_names.is_empty() {
            "\n\nTip: register custom models with /addmodel".to_string()
        } else {
            format!(
                "\n\nCustom models for {}:\n  {}",
                active_provider.name,
                custom_names.join("\n  ")
            )
        };
        return format!(
            "Unknown model '{}' for provider '{}'.\n\nAvailable models:\n{}\n\nUse exact name: /model <model-name>{}",
            target, active_provider.name, list, custom_note
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

// ── Custom model commands (/addmodel, /listmodels, /removemodel) ────
//
// Users running LM Studio / Ollama / vLLM / custom proxies register their
// local models via these commands. Storage lives in a separate file
// (`~/.temm1e/custom_models.toml`) so credentials.toml format is untouched.

/// Handle `/addmodel <name> context:<int> output:<int> [input_price:<float>] [output_price:<float>]`.
fn handle_addmodel_command(args: &str) -> String {
    use temm1e_core::config::custom_models::{upsert_custom_model, CustomModel};

    let trimmed = args.trim();
    if trimmed.is_empty() {
        return "Usage: /addmodel <name> context:<int> output:<int> \
                [input_price:<float>] [output_price:<float>]\n\n\
                Example: /addmodel qwen3-coder-30b-a3b context:262144 output:65536\n\
                Example: /addmodel glm-4.7 context:200000 output:131072 \
                input_price:0.5 output_price:2.0"
            .to_string();
    }

    // Require an active provider so we know which provider to scope the model to.
    let active_provider = match load_credentials_file() {
        Some(c) if !c.providers.is_empty() => c.active.clone(),
        _ => {
            return "No active provider. Configure one first with /addkey or `proxy …`, \
                    then /addmodel to register a custom model."
                .to_string();
        }
    };

    // First whitespace-delimited token is the name; the rest are k/v pairs.
    let mut tokens = trimmed.split_whitespace();
    let name = match tokens.next() {
        Some(n) if !n.contains(':') => n.to_string(),
        _ => {
            return "Missing model name. First argument must be the model name \
                    (before any k:v pairs).\nUsage: /addmodel <name> \
                    context:<int> output:<int> [input_price:<float>] \
                    [output_price:<float>]"
                .to_string();
        }
    };

    let mut context_window: Option<usize> = None;
    let mut max_output_tokens: Option<usize> = None;
    let mut input_price_per_1m: f64 = 0.0;
    let mut output_price_per_1m: f64 = 0.0;

    for token in tokens {
        let Some((k, v)) = token.split_once(':') else {
            return format!(
                "Unexpected token `{}`. All arguments after the model name \
                 must be k:v pairs (context:, output:, input_price:, output_price:).",
                token
            );
        };
        match k.to_lowercase().as_str() {
            "context" | "context_window" | "ctx" => match v.parse::<usize>() {
                Ok(n) => context_window = Some(n),
                Err(_) => return format!("Invalid context value `{}` — expected integer.", v),
            },
            "output" | "max_output" | "max_output_tokens" | "out" => match v.parse::<usize>() {
                Ok(n) => max_output_tokens = Some(n),
                Err(_) => return format!("Invalid output value `{}` — expected integer.", v),
            },
            "input_price" | "input_price_per_1m" | "in_price" => match v.parse::<f64>() {
                Ok(p) if p >= 0.0 => input_price_per_1m = p,
                _ => {
                    return format!(
                        "Invalid input_price value `{}` — expected non-negative float.",
                        v
                    )
                }
            },
            "output_price" | "output_price_per_1m" | "out_price" => match v.parse::<f64>() {
                Ok(p) if p >= 0.0 => output_price_per_1m = p,
                _ => {
                    return format!(
                        "Invalid output_price value `{}` — expected non-negative float.",
                        v
                    )
                }
            },
            other => {
                return format!(
                    "Unknown key `{}`. Accepted keys: context:, output:, \
                     input_price:, output_price:",
                    other
                );
            }
        }
    }

    let context_window = match context_window {
        Some(c) if c > 0 => c,
        Some(_) => return "context: must be greater than 0.".to_string(),
        None => {
            return "Missing required `context:<int>`. See `/addmodel` for usage.".to_string();
        }
    };
    let max_output_tokens = match max_output_tokens {
        Some(o) if o > 0 => o,
        Some(_) => return "output: must be greater than 0.".to_string(),
        None => {
            return "Missing required `output:<int>`. See `/addmodel` for usage.".to_string();
        }
    };

    let model = CustomModel {
        provider: active_provider.clone(),
        name: name.clone(),
        context_window,
        max_output_tokens,
        input_price_per_1m,
        output_price_per_1m,
    };

    match upsert_custom_model(model) {
        Ok(()) => {
            let price_note = if input_price_per_1m == 0.0 && output_price_per_1m == 0.0 {
                " (free / local inference)".to_string()
            } else {
                format!(
                    " (pricing: ${:.2}/1M in · ${:.2}/1M out)",
                    input_price_per_1m, output_price_per_1m
                )
            };
            format!(
                "Added custom model `{}` for provider `{}`:\n  context_window:   {}\n  max_output:       {}{}\n\nSwitch to it with: /model {}",
                name, active_provider, context_window, max_output_tokens, price_note, name
            )
        }
        Err(e) => format!("Failed to save custom model: {}", e),
    }
}

/// Handle `/listmodels` — show hardcoded + custom models grouped by provider.
fn handle_listmodels_command() -> String {
    use temm1e_core::config::custom_models::custom_models_for_provider;

    let creds = match load_credentials_file() {
        Some(c) => c,
        None => return "No providers configured. Use /addkey or `proxy …` first.".to_string(),
    };
    if creds.providers.is_empty() {
        return "No providers configured. Use /addkey or `proxy …` first.".to_string();
    }

    let mut lines = Vec::new();
    for p in &creds.providers {
        let active_marker = if p.name == creds.active {
            " (active)"
        } else {
            ""
        };
        lines.push(format!("Provider: {}{}", p.name, active_marker));

        // Hardcoded models from the static registry
        let hardcoded = available_models_for_provider(&p.name);
        if !hardcoded.is_empty() {
            lines.push("  Hardcoded:".to_string());
            for m in &hardcoded {
                let (ctx, out) = temm1e_core::types::model_registry::model_limits(m);
                let current = if *m == p.model { " ← current" } else { "" };
                lines.push(format!(
                    "    {} — {}K ctx · {}K out{}",
                    m,
                    ctx / 1000,
                    out / 1000,
                    current
                ));
            }
        }

        // Custom models for this provider
        let custom = custom_models_for_provider(&p.name);
        if custom.is_empty() {
            lines.push("  Custom: (none)".to_string());
        } else {
            lines.push("  Custom:".to_string());
            for m in &custom {
                let current = if m.name == p.model {
                    " ← current"
                } else {
                    ""
                };
                let price = if m.input_price_per_1m == 0.0 && m.output_price_per_1m == 0.0 {
                    " · free".to_string()
                } else {
                    format!(
                        " · ${:.2}/1M in · ${:.2}/1M out",
                        m.input_price_per_1m, m.output_price_per_1m
                    )
                };
                lines.push(format!(
                    "    {} — {}K ctx · {}K out{}{}",
                    m.name,
                    m.context_window / 1000,
                    m.max_output_tokens / 1000,
                    price,
                    current
                ));
            }
        }
        lines.push(String::new());
    }

    lines.push(
        "Add a custom model: /addmodel <name> context:<int> output:<int> \
         [input_price:<float>] [output_price:<float>]"
            .to_string(),
    );
    lines.push("Remove a custom model: /removemodel <name>".to_string());
    lines.join("\n")
}

/// Handle `/removemodel <name>` — remove a custom model from the active provider.
fn handle_removemodel_command(args: &str) -> String {
    use temm1e_core::config::custom_models::remove_custom_model;

    let name = args.trim();
    if name.is_empty() {
        return "Usage: /removemodel <name>\nExample: /removemodel qwen3-coder-30b-a3b\n\n\
                Only affects the active provider. Use /listmodels to see current entries."
            .to_string();
    }

    let active_provider = match load_credentials_file() {
        Some(c) if !c.providers.is_empty() => c.active.clone(),
        _ => {
            return "No active provider. Nothing to remove.".to_string();
        }
    };

    match remove_custom_model(name, Some(&active_provider)) {
        Ok(0) => format!(
            "No custom model `{}` found for provider `{}`. Use /listmodels to see current entries.",
            name, active_provider
        ),
        Ok(n) => format!(
            "Removed {} custom model entry for `{}` (provider `{}`).",
            n, name, active_provider
        ),
        Err(e) => format!("Failed to remove custom model: {}", e),
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
/// Does not retry deterministic (permanent) failures.
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
                let err_str = e.to_string();
                // Don't retry permanent failures — they'll fail identically every time.
                if err_str.contains("message is too long")
                    || err_str.contains("can't parse")
                    || err_str.contains("chat not found")
                    || err_str.contains("bot was blocked")
                    || err_str.contains("CHAT_WRITE_FORBIDDEN")
                {
                    tracing::error!(
                        error = %e,
                        "Non-retryable send failure — message lost"
                    );
                    return;
                }
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

/// Interactive setup wizard — guides first-time users through configuration.
///
/// Steps:
/// 1. Channel selection (Telegram, Discord, or skip)
/// 2. AI provider authentication
/// 3. Writes a minimal config and prints next steps
async fn run_setup_wizard() -> Result<()> {
    use std::io::{self, BufRead, Write};

    let home = dirs::home_dir().ok_or_else(|| anyhow::anyhow!("Cannot find home directory"))?;
    let temm1e_dir = home.join(".temm1e");
    std::fs::create_dir_all(&temm1e_dir)?;

    println!();
    println!("  ╭──────────────────────────────────────╮");
    println!("  │       TEMM1E — Setup Wizard          │");
    println!("  ╰──────────────────────────────────────╯");
    println!();

    let stdin = io::stdin();
    let mut input = String::new();

    // ── Step 1: Channel ─────────────────────────────────────────────
    println!("  How do you want to talk to me?");
    println!();
    println!("    1) Telegram bot  (most popular)");
    println!("    2) Discord bot");
    println!("    3) CLI only      (no messaging app needed)");
    println!();
    print!("  Choose [1/2/3]: ");
    io::stdout().flush()?;
    input.clear();
    stdin.lock().read_line(&mut input)?;
    let channel_choice = input.trim();

    let mut config_lines: Vec<String> = Vec::new();
    let mut env_hint = String::new();

    match channel_choice {
        "1" | "telegram" | "" => {
            println!();
            println!("  Telegram! Great choice.");
            println!();
            println!("  If you don't have a bot yet:");
            println!("    1. Open Telegram, search @BotFather");
            println!("    2. Send /newbot, follow the prompts");
            println!("    3. Copy the bot token");
            println!();
            print!("  Paste your Telegram bot token (or press Enter to skip): ");
            io::stdout().flush()?;
            input.clear();
            stdin.lock().read_line(&mut input)?;
            let token = input.trim();
            if token.is_empty() {
                println!(
                    "  Skipped — set TELEGRAM_BOT_TOKEN env var before running `temm1e start`."
                );
                env_hint = "export TELEGRAM_BOT_TOKEN=\"your-token-here\"".to_string();
            } else {
                config_lines.push("[channel.telegram]".to_string());
                config_lines.push("enabled = true".to_string());
                config_lines.push(format!("token = \"{}\"", token));
                config_lines.push("allowlist = []".to_string());
                config_lines.push("file_transfer = true".to_string());
            }
        }
        "2" | "discord" => {
            println!();
            println!("  Discord! Nice.");
            println!();
            print!("  Paste your Discord bot token (or press Enter to skip): ");
            io::stdout().flush()?;
            input.clear();
            stdin.lock().read_line(&mut input)?;
            let token = input.trim();
            if token.is_empty() {
                println!(
                    "  Skipped — set DISCORD_BOT_TOKEN env var before running `temm1e start`."
                );
                env_hint = "export DISCORD_BOT_TOKEN=\"your-token-here\"".to_string();
            } else {
                config_lines.push("[channel.discord]".to_string());
                config_lines.push("enabled = true".to_string());
                config_lines.push(format!("token = \"{}\"", token));
                config_lines.push("allowlist = []".to_string());
            }
        }
        "3" | "cli" => {
            println!();
            println!("  CLI mode — run `temm1e chat` to talk to me directly.");
        }
        _ => {
            println!("  Defaulting to CLI mode.");
        }
    }

    // ── Step 2: AI Provider ─────────────────────────────────────────
    println!();
    println!("  How do you want to power my brain?");
    println!();
    println!("    1) Paste an API key (Anthropic, OpenAI, Gemini, Grok, etc.)");
    println!("    2) Use ChatGPT Plus/Pro (OAuth login)");
    println!("    3) Skip for now (set up in chat later)");
    println!();
    print!("  Choose [1/2/3]: ");
    io::stdout().flush()?;
    input.clear();
    stdin.lock().read_line(&mut input)?;
    let provider_choice = input.trim();

    // Detect if user pasted an API key directly instead of choosing 1/2/3
    let key_at_choice = if provider_choice.starts_with("sk-")
        || provider_choice.starts_with("AIzaSy")
        || provider_choice.starts_with("xai-")
    {
        Some(provider_choice.to_string())
    } else {
        None
    };

    match provider_choice {
        _ if key_at_choice.is_some() => {
            // User pasted API key directly at the choice prompt
            let key = key_at_choice.unwrap();
            if !key.is_empty() {
                // Auto-detect provider
                let provider = if key.starts_with("sk-ant-") {
                    "anthropic"
                } else if key.starts_with("AIzaSy") {
                    "gemini"
                } else if key.starts_with("xai-") {
                    "grok"
                } else if key.starts_with("sk-or-") {
                    "openrouter"
                } else {
                    // Default: OpenAI-compatible (sk- prefix or unknown)
                    "openai"
                };
                println!("  Detected provider: {provider}");

                // Save via the canonical credential system (correct TOML format)
                let model = temm1e_core::types::model_registry::default_model(provider);
                save_credentials(provider, &key, model, None).await?;
                println!("  Saved to ~/.temm1e/credentials.toml");
            } else {
                println!("  Skipped — you can paste your API key in chat later.");
            }
        }
        "1" | "api" | "" => {
            println!();
            print!("  Paste your API key: ");
            io::stdout().flush()?;
            input.clear();
            stdin.lock().read_line(&mut input)?;
            let key = input.trim().to_string();
            if !key.is_empty() {
                let provider = if key.starts_with("sk-ant-") {
                    "anthropic"
                } else if key.starts_with("AIzaSy") {
                    "gemini"
                } else if key.starts_with("xai-") {
                    "grok"
                } else if key.starts_with("sk-or-") {
                    "openrouter"
                } else {
                    "openai"
                };
                println!("  Detected provider: {provider}");
                let model = temm1e_core::types::model_registry::default_model(provider);
                save_credentials(provider, &key, model, None).await?;
                println!("  Saved to ~/.temm1e/credentials.toml");
            } else {
                println!("  Skipped — you can paste your API key in chat later.");
            }
        }
        "2" | "oauth" | "chatgpt" => {
            println!();
            println!("  Run `temm1e auth login` to authenticate with ChatGPT.");
        }
        _ => {
            println!("  Skipped — you can set up a provider in chat later.");
        }
    }

    // ── Step 3: Write config ────────────────────────────────────────
    // Must match the config loader's search path: ~/.temm1e/config.toml
    let config_path = temm1e_dir.join("config.toml");
    if !config_lines.is_empty() {
        // Build minimal config
        let mut full_config = String::new();
        full_config.push_str("# TEMM1E configuration — generated by `temm1e setup`\n\n");
        full_config.push_str("[memory]\nbackend = \"sqlite\"\n\n");
        full_config.push_str(&config_lines.join("\n"));
        full_config.push('\n');

        std::fs::write(&config_path, &full_config)?;
        println!();
        println!("  Config written to ~/.temm1e/config.toml");
    }

    // ── Done ────────────────────────────────────────────────────────
    println!();
    println!("  ╭──────────────────────────────────────╮");
    println!("  │         Setup complete!               │");
    println!("  ╰──────────────────────────────────────╯");
    println!();
    if !env_hint.is_empty() {
        println!("  Before starting, set your token:");
        println!("    {}", env_hint);
        println!();
    }
    println!("  Next steps:");
    println!("    temm1e start       Start the bot");
    println!("    temm1e chat        Chat in CLI mode");
    println!("    temm1e status      Check health");
    println!();

    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging — TUI mode writes to a file instead of stderr
    #[cfg(feature = "tui")]
    let _is_tui = matches!(cli.command, Commands::Tui);
    #[cfg(not(feature = "tui"))]
    let _is_tui = false;

    // Clean up old log files (7-day retention, 100 MB budget)
    temm1e_observable::file_logger::cleanup_logs(7);

    // Set up tracing: stdout + persistent log file
    let env_filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let file_appender = temm1e_observable::file_logger::create_file_appender();
    let (file_writer, _log_guard) = tracing_appender::non_blocking(file_appender);

    if _is_tui {
        // TUI mode: file only (no stdout — would corrupt the display)
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        tracing_subscriber::registry()
            .with(env_filter)
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_ansi(false)
                    .with_writer(file_writer),
            )
            .init();
    } else {
        // CLI/daemon mode: stdout + file
        use tracing_subscriber::layer::SubscriberExt;
        use tracing_subscriber::util::SubscriberInitExt;
        tracing_subscriber::registry()
            .with(env_filter)
            .with(tracing_subscriber::fmt::layer().json())
            .with(
                tracing_subscriber::fmt::layer()
                    .json()
                    .with_ansi(false)
                    .with_writer(file_writer),
            )
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

    // Initialize file safety guards (captures running binary path for self-protection)
    temm1e_tools::file_safety::init();

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

            // ── Self-learning maintenance (startup GC) ────────────
            {
                let gc_count = temm1e_agent::blueprint::blueprint_gc(&*memory).await;
                if gc_count > 0 {
                    tracing::info!(pruned = gc_count, "Blueprint GC completed at startup");
                }

                // Lambda memory dedup: merge near-duplicate entries before GC
                let lambda_config = &config.memory.lambda;
                if lambda_config.enabled {
                    if let Ok(candidates) = memory
                        .lambda_query_candidates(lambda_config.candidate_limit)
                        .await
                    {
                        let merges = temm1e_agent::lambda_memory::dedup_candidates(&candidates);
                        let mut merged_count = 0usize;
                        for (keep_idx, absorb_idx) in &merges {
                            let merged = temm1e_agent::lambda_memory::merge_entries(
                                &candidates[*keep_idx],
                                &candidates[*absorb_idx],
                            );
                            if memory.lambda_update_entry(&merged).await.is_ok()
                                && memory
                                    .lambda_delete(&candidates[*absorb_idx].hash)
                                    .await
                                    .is_ok()
                            {
                                merged_count += 1;
                            }
                        }
                        if merged_count > 0 {
                            tracing::info!(
                                merged = merged_count,
                                "λ-Memory dedup completed at startup"
                            );
                        }
                    }
                }
            }

            // ── Messaging channels ────────────────────────────────
            let mut channels: Vec<Arc<dyn temm1e_core::Channel>> = Vec::new();
            let mut channel_map: HashMap<String, Arc<dyn temm1e_core::Channel>> = HashMap::new();
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
                    channel_map.insert("telegram".to_string(), tg_arc.clone());
                    primary_channel = Some(tg_arc.clone());
                    tracing::info!("Telegram channel started");
                }
            }

            // ── Discord channel ───────────────────────────────
            #[cfg(feature = "discord")]
            let mut discord_rx: Option<
                tokio::sync::mpsc::Receiver<temm1e_core::types::message::InboundMessage>,
            > = None;

            #[cfg(feature = "discord")]
            {
                // Auto-inject Discord config from env var when no config entry exists.
                // Mirrors the Telegram zero-config pattern.
                if !config.channel.contains_key("discord") {
                    if let Ok(token) = std::env::var("DISCORD_BOT_TOKEN") {
                        if !token.is_empty() {
                            config.channel.insert(
                                "discord".to_string(),
                                temm1e_core::types::config::ChannelConfig {
                                    enabled: true,
                                    token: Some(token),
                                    allowlist: vec![],
                                    file_transfer: true,
                                    max_file_size: None,
                                },
                            );
                            tracing::info!(
                                "Auto-configured Discord from DISCORD_BOT_TOKEN env var"
                            );
                        }
                    }
                }

                if let Some(discord_config) = config.channel.get("discord") {
                    if discord_config.enabled {
                        let mut discord = temm1e_channels::DiscordChannel::new(discord_config)?;
                        discord.start().await?;
                        discord_rx = discord.take_receiver();
                        let discord_arc: Arc<dyn temm1e_core::Channel> = Arc::new(discord);
                        channels.push(discord_arc.clone());
                        channel_map.insert("discord".to_string(), discord_arc.clone());
                        if primary_channel.is_none() {
                            primary_channel = Some(discord_arc.clone());
                        }
                        tracing::info!("Discord channel started");
                    }
                }
            }

            // ── WhatsApp Web channel ─────────────────────────
            #[cfg(feature = "whatsapp-web")]
            let mut whatsapp_web_rx: Option<
                tokio::sync::mpsc::Receiver<temm1e_core::types::message::InboundMessage>,
            > = None;

            #[cfg(feature = "whatsapp-web")]
            {
                if let Some(wa_config) = config
                    .channel
                    .get("whatsapp_web")
                    .or_else(|| config.channel.get("whatsapp-web"))
                {
                    if wa_config.enabled {
                        let mut wa = temm1e_channels::WhatsAppWebChannel::new(wa_config)?;
                        wa.start().await?;
                        whatsapp_web_rx = wa.take_receiver();
                        let wa_arc: Arc<dyn temm1e_core::Channel> = Arc::new(wa);
                        channels.push(wa_arc.clone());
                        channel_map.insert("whatsapp-web".to_string(), wa_arc.clone());
                        if primary_channel.is_none() {
                            primary_channel = Some(wa_arc.clone());
                        }
                        tracing::info!("WhatsApp Web channel started");
                    }
                }
            }

            // ── Channel map zero-channels guard ──────────────────
            if channel_map.is_empty() {
                tracing::warn!(
                    "No messaging channels configured. Set TELEGRAM_BOT_TOKEN or DISCORD_BOT_TOKEN, \
                     or add [channel.telegram] / [channel.discord] to config."
                );
            }
            let channel_map: Arc<HashMap<String, Arc<dyn temm1e_core::Channel>>> =
                Arc::new(channel_map);

            // ── Pending messages ───────────────────────────────
            let pending_messages: temm1e_tools::PendingMessages =
                Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

            // ── OTK setup token store ───────────────────────────
            let setup_tokens = temm1e_gateway::SetupTokenStore::new();

            // ── Pending raw key pastes (from /addkey unsafe) ────
            let pending_raw_keys: Arc<Mutex<HashSet<String>>> =
                Arc::new(Mutex::new(HashSet::new()));

            // ── Active login sessions (OTK Prowl — per-chat interactive browser sessions) ────
            #[cfg(feature = "browser")]
            let login_sessions: Arc<
                Mutex<HashMap<String, temm1e_tools::browser_session::InteractiveBrowseSession>>,
            > = Arc::new(Mutex::new(HashMap::new()));

            // ── Usage store (shares same SQLite DB as memory) ────
            let usage_store: Arc<dyn temm1e_core::UsageStore> =
                Arc::new(temm1e_memory::SqliteUsageStore::new(&memory_url).await?);
            tracing::info!("Usage store initialized");

            // ── Vault (encrypted credential store) ───────────────
            let vault: Option<Arc<dyn temm1e_core::Vault>> = match temm1e_vault::LocalVault::new()
                .await
            {
                Ok(v) => {
                    tracing::info!("Vault initialized");
                    Some(Arc::new(v) as Arc<dyn temm1e_core::Vault>)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Vault initialization failed — browser authenticate disabled");
                    None
                }
            };

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
            // ── Social intelligence: personality + storage ──────────
            let personality =
                std::sync::Arc::new(temm1e_anima::personality::PersonalityConfig::load(
                    &dirs::home_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                        .join(".temm1e"),
                ));
            let social_storage: Option<std::sync::Arc<temm1e_anima::SocialStorage>> = if config
                .social
                .enabled
            {
                let social_db_url = format!(
                    "sqlite:{}/social.db?mode=rwc",
                    dirs::home_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                        .join(".temm1e")
                        .display()
                );
                match temm1e_anima::SocialStorage::new(&social_db_url).await {
                    Ok(s) => {
                        tracing::info!("Social intelligence initialized");
                        Some(std::sync::Arc::new(s))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Social intelligence disabled: DB init failed");
                        None
                    }
                }
            } else {
                None
            };
            // Pre-capture social config for use in inner closures where `config` may be shadowed
            let social_config_captured = config.social.clone();

            // ── Skills: load registry from global + workspace dirs ─────
            let skill_registry = {
                let workspace =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let mut reg = temm1e_skills::SkillRegistry::new(workspace);
                if let Err(e) = reg.load_skills().await {
                    tracing::warn!(error = %e, "Failed to load skills");
                }
                let count = reg.list_skills().len();
                if count > 0 {
                    tracing::info!(count, "Skills loaded");
                }
                std::sync::Arc::new(tokio::sync::RwLock::new(reg))
            };

            // Use create_tools_with_browser to get a separate BrowserTool reference
            // for /browser command handling.
            #[cfg(feature = "browser")]
            let (mut tools, browser_tool_ref) = temm1e_tools::create_tools_with_browser(
                &config.tools,
                censored_channel.clone(),
                Some(pending_messages.clone()),
                Some(memory.clone()),
                Some(Arc::new(setup_tokens.clone()) as Arc<dyn temm1e_core::SetupLinkGenerator>),
                Some(usage_store.clone()),
                if personality_locked {
                    None
                } else {
                    Some(shared_mode.clone())
                },
                vault.clone(),
                Some(skill_registry.clone()),
            );
            #[cfg(not(feature = "browser"))]
            let mut tools = temm1e_tools::create_tools(
                &config.tools,
                censored_channel,
                Some(pending_messages.clone()),
                Some(memory.clone()),
                Some(Arc::new(setup_tokens.clone()) as Arc<dyn temm1e_core::SetupLinkGenerator>),
                Some(usage_store.clone()),
                if personality_locked {
                    None
                } else {
                    Some(shared_mode.clone())
                },
                vault.clone(),
                Some(skill_registry.clone()),
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

            // ── TemDOS: Load core registry ──────────────────
            let core_registry = {
                let mut registry = temm1e_cores::CoreRegistry::new();
                let ws_path = dirs::home_dir()
                    .map(|h| h.join(".temm1e"))
                    .unwrap_or_default();
                registry
                    .load(Some(ws_path.as_path()))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "Failed to load TemDOS cores");
                    });
                if !registry.is_empty() {
                    tracing::info!(count = registry.len(), "TemDOS cores loaded");
                }
                Arc::new(tokio::sync::RwLock::new(registry))
            };

            // Perpetuum state (initialized lazily after provider is ready)
            let perpetuum: Arc<tokio::sync::RwLock<Option<Arc<temm1e_perpetuum::Perpetuum>>>> =
                Arc::new(tokio::sync::RwLock::new(None));
            let perpetuum_temporal: Arc<tokio::sync::RwLock<String>> =
                Arc::new(tokio::sync::RwLock::new(String::new()));

            let system_prompt = Some(build_system_prompt(&personality));

            // Quick check: is [hive] enabled in config? (just the boolean, full init later)
            let hive_enabled_early = {
                // v5.5.0: default-ON to match `HiveConfig::default()` at
                // `temm1e_hive/src/config.rs:64` (comment: "Enabled by
                // default since v3.0.0"). Previously this shadow struct
                // silently overrode the crate default with bool::default()
                // = false, keeping JIT Swarm dormant for upgrading users
                // despite v5.4.0 having shipped it. Explicit opt-out via
                // `[hive] enabled = false` still works.
                #[derive(serde::Deserialize, Default)]
                struct HiveCheck {
                    #[serde(default)]
                    hive: HiveEnabled,
                }
                #[derive(serde::Deserialize)]
                struct HiveEnabled {
                    #[serde(default = "hive_default_enabled")]
                    enabled: bool,
                }
                impl Default for HiveEnabled {
                    fn default() -> Self {
                        Self {
                            enabled: hive_default_enabled(),
                        }
                    }
                }
                fn hive_default_enabled() -> bool {
                    true
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
                    .unwrap_or(true)
            };

            // ── Witness attachments (built once, reused at every runtime
            // rebuild — initial + ~20 provider/model switches). Returns None
            // when [witness] enabled=false so wiring is a no-op for opt-out.
            let witness_attachments: Option<temm1e_agent::witness_init::WitnessAttachments> =
                match temm1e_agent::witness_init::build_witness_attachments(&config.witness).await {
                    Ok(a) => {
                        if a.is_some() {
                            tracing::info!(
                                strictness = %config.witness.strictness,
                                auto_planner_oath = config.witness.auto_planner_oath,
                                "Witness enabled — attachments built"
                            );
                        }
                        a
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Witness init failed — continuing without Witness");
                        None
                    }
                };

            // ── Agent state (None during onboarding) ───────────
            let agent_state: Arc<tokio::sync::RwLock<Option<Arc<temm1e_agent::AgentRuntime>>>> =
                Arc::new(tokio::sync::RwLock::new(None));

            // ── JIT spawn_swarm tool: register early with a deferred handle ──
            // The tool is in tools_template from the start so every agent sees
            // it; the handle is populated later, after Hive + agent are ready.
            // When the handle is still empty at execute time, the tool returns
            // a graceful "not available yet" message.
            let swarm_handle: Option<temm1e_agent::spawn_swarm::SwarmHandle> = if hive_enabled_early
            {
                let h = temm1e_agent::spawn_swarm::SpawnSwarmTool::fresh_handle();
                tools.push(Arc::new(temm1e_agent::spawn_swarm::SpawnSwarmTool::new(
                    h.clone(),
                )));
                tracing::info!("JIT spawn_swarm tool registered (context deferred)");
                Some(h)
            } else {
                None
            };

            // ── Eigen-Tune: load [eigentune] config + instantiate engine ──
            // Hoisted to outer scope so both the agent construction (inside
            // the credentials block below) and the periodic tick task
            // (after the credentials block) can see them.
            //
            // Plan §A1: avoid the temm1e-core ↔ temm1e-distill circular dep
            // by parsing the same TOML twice. Temm1eConfig already silently
            // ignores unknown sections; here we pull only [eigentune].
            let eigentune_cfg: temm1e_distill::config::EigenTuneConfig = {
                #[derive(serde::Deserialize, Default)]
                struct EigenRoot {
                    #[serde(default)]
                    eigentune: temm1e_distill::config::EigenTuneConfig,
                }
                let raw_path = config_path
                    .map(std::path::PathBuf::from)
                    .unwrap_or_else(|| {
                        dirs::home_dir()
                            .map(|h| h.join(".temm1e/config.toml"))
                            .unwrap_or_else(|| std::path::PathBuf::from("temm1e.toml"))
                    });
                let raw = std::fs::read_to_string(&raw_path).unwrap_or_default();
                let expanded = temm1e_core::config::expand_env_vars(&raw);
                toml::from_str::<EigenRoot>(&expanded)
                    .map(|r| r.eigentune)
                    .unwrap_or_default()
            };

            let eigen_tune_engine: Option<Arc<temm1e_distill::EigenTuneEngine>> =
                if eigentune_cfg.enabled {
                    let db_path = dirs::home_dir()
                        .map(|h| h.join(".temm1e").join("eigentune.db"))
                        .unwrap_or_else(|| std::path::PathBuf::from("eigentune.db"));
                    if let Some(parent) = db_path.parent() {
                        let _ = std::fs::create_dir_all(parent);
                    }
                    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
                    match temm1e_distill::EigenTuneEngine::new(&eigentune_cfg, &db_url).await {
                        Ok(engine) => {
                            tracing::info!(
                                db = %db_path.display(),
                                enable_local_routing = eigentune_cfg.enable_local_routing,
                                "Eigen-Tune: engine initialized"
                            );
                            Some(Arc::new(engine))
                        }
                        Err(e) => {
                            tracing::error!(
                                error = %e,
                                "Eigen-Tune: failed to initialize, continuing without"
                            );
                            None
                        }
                    }
                } else {
                    None
                };

            if let Some((ref pname, ref key, ref model)) = credentials {
                // Filter out placeholder/invalid keys at startup. Use lenient
                // mode for custom-endpoint providers so short LM Studio / Ollama
                // keys pass — otherwise this check would wrongly reject keys
                // that load_saved_credentials already approved via lenient filter.
                let has_custom_endpoint = load_credentials_file()
                    .and_then(|c| {
                        c.providers
                            .iter()
                            .find(|p| p.name == *pname)
                            .and_then(|p| p.base_url.clone())
                    })
                    .is_some();
                let is_placeholder_start = if has_custom_endpoint {
                    is_placeholder_key_lenient(key)
                } else {
                    is_placeholder_key(key)
                };
                if is_placeholder_start {
                    tracing::warn!(provider = %pname, "Primary API key is a placeholder — starting in onboarding mode");
                    // Fall through to onboarding
                } else {
                    // Load all keys and saved base_url for this provider.
                    // Inside the filter closure, gate lenient vs strict on the
                    // saved base_url so proxy providers keep their short keys.
                    let (all_keys, saved_base_url) = load_active_provider_keys()
                        .map(|(_, keys, _, burl)| {
                            let has_custom = burl.is_some();
                            let valid: Vec<String> = keys
                                .into_iter()
                                .filter(|k| {
                                    if has_custom {
                                        !is_placeholder_key_lenient(k)
                                    } else {
                                        !is_placeholder_key(k)
                                    }
                                })
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
                    // TemDOS: register invoke_core tool now that provider is available
                    if !core_registry.read().await.is_empty() {
                        // Custom-model aware pricing lookup — tries the active
                        // provider's custom_models.toml first, falls back to
                        // hardcoded substring pricing.
                        let model_pricing =
                            temm1e_agent::budget::get_pricing_with_custom(pname, model);
                        let invoke_core = temm1e_cores::InvokeCoreTool::new(
                            core_registry.clone(),
                            provider.clone(),
                            tools.clone(), // all tools — invoke_core filters itself out
                            // Note: this is a SEPARATE budget for core tracking.
                            // The main agent's budget is inside AgentRuntime.
                            // Both ultimately deduct from the user's wallet via provider calls.
                            Arc::new(temm1e_agent::budget::BudgetTracker::new(
                                config.agent.max_spend_usd,
                            )),
                            model_pricing,
                            model.clone(),
                            config.agent.max_context_tokens,
                            memory.clone(), // v4.6.0: core stats persistence
                        );
                        tools.push(Arc::new(invoke_core));
                        tracing::info!("TemDOS invoke_core tool registered");
                    }

                    let mut runtime = temm1e_agent::AgentRuntime::with_limits(
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
                    .with_shared_memory_strategy(shared_memory_strategy.clone())
                    .with_personality(personality.clone())
                    .with_social(social_storage.clone(), Some(social_config_captured.clone()));
                    // Tem Conscious: enable consciousness if configured
                    if config.consciousness.enabled {
                        let aware_config = temm1e_agent::consciousness::ConsciousnessConfig {
                            enabled: true,
                            confidence_threshold: config.consciousness.confidence_threshold,
                            max_interventions_per_session: config
                                .consciousness
                                .max_interventions_per_session,
                            observation_mode: config.consciousness.observation_mode.clone(),
                        };
                        runtime = runtime.with_consciousness(
                            temm1e_agent::consciousness_engine::ConsciousnessEngine::new(
                                aware_config,
                                provider.clone(),
                                model.clone(),
                            ),
                        );
                    }
                    // Wire Perpetuum temporal context into agent
                    runtime = runtime.with_perpetuum_temporal(perpetuum_temporal.clone());

                    // Wire Eigen-Tune engine into agent (Phase 9)
                    if let Some(et) = eigen_tune_engine.clone() {
                        runtime = runtime.with_eigen_tune(et, eigentune_cfg.enable_local_routing);
                    }

                    // Wire Witness attachments (no-op when disabled)
                    runtime = runtime.with_witness_attachments(witness_attachments.as_ref());

                    let agent = Arc::new(runtime);
                    *agent_state.write().await = Some(agent);
                    tracing::info!(provider = %pname, model = %model, "Agent initialized");

                    // ── Perpetuum lazy init (needs provider) ──────
                    if config.perpetuum.enabled && perpetuum.read().await.is_none() {
                        let perpetuum_db = dirs::home_dir()
                            .unwrap_or_else(|| std::path::PathBuf::from("."))
                            .join(".temm1e/perpetuum.db");
                        let db_url = format!("sqlite:{}?mode=rwc", perpetuum_db.display());

                        let perp_config = temm1e_perpetuum::PerpetualConfig {
                            enabled: true,
                            timezone: config.perpetuum.timezone.clone(),
                            max_concerns: config.perpetuum.max_concerns,
                            conscience: temm1e_perpetuum::ConscienceConfig {
                                idle_threshold_secs: config
                                    .perpetuum
                                    .conscience_idle_threshold_secs
                                    .unwrap_or(900),
                                dream_threshold_secs: config
                                    .perpetuum
                                    .conscience_dream_threshold_secs
                                    .unwrap_or(3600),
                            },
                            cognitive: temm1e_perpetuum::CognitiveConfig {
                                review_every_n_checks: config.perpetuum.review_every_n_checks,
                                interpret_changes: true,
                            },
                            volition: temm1e_perpetuum::VolitionConfig {
                                enabled: config.perpetuum.volition_enabled,
                                interval_secs: config.perpetuum.volition_interval_secs,
                                max_actions_per_cycle: config.perpetuum.volition_max_actions,
                                event_triggered: true,
                            },
                        };

                        match temm1e_perpetuum::Perpetuum::new(
                            perp_config,
                            provider.clone(),
                            model.clone(),
                            channel_map.clone(),
                            &db_url,
                        )
                        .await
                        {
                            Ok(p) => {
                                let p = Arc::new(p);
                                p.start();
                                *perpetuum.write().await = Some(p);
                                tracing::info!("Perpetuum runtime started");
                            }
                            Err(e) => {
                                tracing::error!(error = %e, "Failed to initialize Perpetuum");
                            }
                        }
                    }
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
                                    .with_shared_memory_strategy(shared_memory_strategy.clone())
                                    .with_personality(personality.clone())
                                    .with_social(
                                        social_storage.clone(),
                                        Some(social_config_captured.clone()),
                                    )
                                    .with_witness_attachments(witness_attachments.as_ref()),
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
                tokio::sync::mpsc::channel::<temm1e_core::types::message::InboundMessage>(128);

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

            // Wire Discord messages into the unified channel
            #[cfg(feature = "discord")]
            if let Some(mut discord_rx) = discord_rx {
                let tx = msg_tx.clone();
                task_handles.push(tokio::spawn(async move {
                    while let Some(msg) = discord_rx.recv().await {
                        if tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                }));
            }

            // Wire WhatsApp Web messages into the unified channel
            #[cfg(feature = "whatsapp-web")]
            if let Some(mut wa_rx) = whatsapp_web_rx {
                let tx = msg_tx.clone();
                task_handles.push(tokio::spawn(async move {
                    while let Some(msg) = wa_rx.recv().await {
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

            // ── Eigen-Tune periodic state-machine tick (Phase 8) ──
            // Runs every 60s to advance tier transitions. When a tier
            // enters Training, the trainer is spawned as a child task so
            // the tick loop is not blocked by a multi-minute training run.
            if let Some(et_engine) = eigen_tune_engine.clone() {
                task_handles.push(tokio::spawn(async move {
                    let mut interval =
                        tokio::time::interval(std::time::Duration::from_secs(60));
                    interval.set_missed_tick_behavior(
                        tokio::time::MissedTickBehavior::Skip,
                    );
                    loop {
                        interval.tick().await;
                        let transitions: Vec<(
                            temm1e_distill::types::EigenTier,
                            temm1e_distill::types::TierState,
                            temm1e_distill::types::TierState,
                        )> = et_engine.tick().await;
                        for (tier, from, to) in transitions {
                            tracing::info!(
                                tier = %tier.as_str(),
                                from = %from.as_str(),
                                to = %to.as_str(),
                                "Eigen-Tune: tier transition"
                            );
                            if to == temm1e_distill::types::TierState::Training {
                                let engine = et_engine.clone();
                                tokio::spawn(async move {
                                    if let Err(e) = engine.train(tier).await {
                                        tracing::warn!(
                                            error = %e,
                                            tier = %tier.as_str(),
                                            "Eigen-Tune: training cycle failed (tier reverts to Collecting)"
                                        );
                                    }
                                });
                            }
                        }
                    }
                }));
                tracing::info!("Eigen-Tune: periodic tick task spawned (60s interval)");
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

            // ── JIT spawn_swarm tool: fill in the handle now that Hive + agent
            // are both ready. The tool was registered earlier with an empty
            // handle; we populate it here using the active agent's provider,
            // model, memory, and a snapshot of tools (excluding spawn_swarm
            // itself via the recursion-filter inside the tool).
            if let (Some(hive), Some(handle)) = (hive_instance.as_ref(), swarm_handle.as_ref()) {
                let agent_opt = agent_state.read().await.clone();
                if let Some(agent) = agent_opt {
                    let ctx = temm1e_agent::spawn_swarm::SpawnSwarmContext {
                        hive: Arc::clone(hive),
                        provider: agent.provider_arc(),
                        memory: memory.clone(),
                        tools_template: tools.clone(),
                        model: agent.model().to_string(),
                        parent_budget: Arc::new(temm1e_agent::budget::BudgetTracker::new(
                            config.agent.max_spend_usd,
                        )),
                        cancel: tokio_util::sync::CancellationToken::new(),
                        workspace_path: std::env::current_dir()
                            .unwrap_or_else(|_| std::path::PathBuf::from(".")),
                        witness_attachments: witness_attachments.clone(),
                    };
                    *handle.write().await = Some(ctx);
                    tracing::info!("JIT spawn_swarm context wired");
                } else {
                    tracing::warn!(
                        "Hive initialized but no agent yet — spawn_swarm context deferred"
                    );
                }
            }

            // ── Per-chat serial executor ───────────────────────

            /// A user order queued for processing after the current task.
            struct QueuedOrder {
                original_msg: temm1e_core::types::message::InboundMessage,
                #[allow(dead_code)]
                queued_at: std::time::Instant,
            }

            type OrderQueue = Arc<std::sync::Mutex<std::collections::VecDeque<QueuedOrder>>>;

            /// Tracks the active task state for a single chat.
            #[allow(dead_code)]
            struct ChatSlot {
                tx: tokio::sync::mpsc::Sender<temm1e_core::types::message::InboundMessage>,
                interrupt: Arc<AtomicBool>,
                is_heartbeat: Arc<AtomicBool>,
                is_busy: Arc<AtomicBool>,
                current_task: Arc<std::sync::Mutex<String>>,
                cancel_token: tokio_util::sync::CancellationToken,
                // ── Mission Control ──
                status_tx: tokio::sync::watch::Sender<temm1e_agent::AgentTaskStatus>,
                order_queue: OrderQueue,
                active_cancel: Arc<std::sync::Mutex<tokio_util::sync::CancellationToken>>,
            }

            if !channel_map.is_empty() {
                let channel_map_arc = channel_map.clone();
                let primary_fallback = primary_channel.clone();
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
                #[cfg(feature = "browser")]
                let login_sessions_clone = login_sessions.clone();
                let usage_store_clone = usage_store.clone();
                let hive_clone = hive_instance.clone();

                let chat_slots: Arc<Mutex<HashMap<String, ChatSlot>>> =
                    Arc::new(Mutex::new(HashMap::new()));

                let msg_tx_redispatch = msg_tx.clone();
                task_handles.push(tokio::spawn(async move {
                    while let Some(inbound) = msg_rx.recv().await {
                        let chat_id = inbound.chat_id.clone();
                        let is_heartbeat_msg = inbound.channel == "heartbeat";

                        // ── Perpetuum: record interaction + refresh temporal context ──
                        if !is_heartbeat_msg {
                            if let Some(ref perp) = *perpetuum.read().await {
                                perp.record_user_interaction().await;
                                let temporal = perp.temporal_injection("standard").await;
                                *perpetuum_temporal.write().await = temporal;
                            }
                        }

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
                                    if let Ok(ct) = slot.active_cancel.lock() { ct.cancel(); }
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
                                    if let Ok(ct) = slot.active_cancel.lock() { ct.cancel(); }
                                    continue;
                                }

                                // ── Fast-path: /status (no LLM call) ──────
                                let is_slash_status = inbound.text.as_deref()
                                    .map(|t| t.trim().eq_ignore_ascii_case("/status"))
                                    .unwrap_or(false);

                                if is_slash_status {
                                    let status_snap = slot.status_tx.borrow().clone();
                                    let is_busy_now = slot.is_busy.load(Ordering::Relaxed);
                                    let is_hb_now = slot.is_heartbeat.load(Ordering::Relaxed);
                                    let oq_len = slot.order_queue.lock()
                                        .map(|q| q.len()).unwrap_or(0);
                                    let task_desc = slot.current_task.lock()
                                        .map(|t| t.clone()).unwrap_or_default();

                                    let mut status_text = if !is_busy_now {
                                        "Idle — no active task.".to_string()
                                    } else if is_hb_now {
                                        "Running background heartbeat check.".to_string()
                                    } else {
                                        let elapsed = status_snap.started_at.elapsed().as_secs();
                                        format!(
                                            "Active task: {}\nRequest: \"{}\"\nRounds: {} | Tools: {} | {}s elapsed | ${:.4}",
                                            status_snap.phase, task_desc,
                                            status_snap.rounds_completed, status_snap.tools_executed,
                                            elapsed, status_snap.cost_usd
                                        )
                                    };

                                    let temporal = perpetuum_temporal.read().await;
                                    if !temporal.is_empty() {
                                        status_text.push_str(&format!("\n\nBackground:\n{}", &*temporal));
                                    }

                                    if oq_len > 0 {
                                        status_text.push_str(&format!("\n\nQueued orders: {}", oq_len));
                                    }

                                    let icpt_sender = channel_map_arc
                                        .get(&inbound.channel)
                                        .cloned()
                                        .or_else(|| primary_fallback.clone())
                                        .expect("channel_map non-empty");
                                    let reply = temm1e_core::types::message::OutboundMessage {
                                        chat_id: chat_id.clone(),
                                        text: status_text,
                                        reply_to: Some(inbound.id.clone()),
                                        parse_mode: None,
                                    };
                                    let _ = icpt_sender.send_message(reply).await;
                                    continue;
                                }

                                // ── Fast-path: /queue (no LLM call) ──────
                                let is_slash_queue = inbound.text.as_deref()
                                    .map(|t| t.trim().eq_ignore_ascii_case("/queue"))
                                    .unwrap_or(false);

                                if is_slash_queue {
                                    let status_text = if let Ok(oq) = slot.order_queue.lock() {
                                        if oq.is_empty() {
                                            "No queued orders.".to_string()
                                        } else {
                                            let mut lines = format!("Queued orders ({}):\n", oq.len());
                                            for (i, order) in oq.iter().enumerate() {
                                                let text = order.original_msg.text.as_deref().unwrap_or("(no text)");
                                                let ago = order.queued_at.elapsed().as_secs();
                                                lines.push_str(&format!("  {}. \"{}\" ({}s ago)\n", i + 1, text, ago));
                                            }
                                            lines
                                        }
                                    } else {
                                        "Could not read order queue.".to_string()
                                    };

                                    let icpt_sender = channel_map_arc
                                        .get(&inbound.channel)
                                        .cloned()
                                        .or_else(|| primary_fallback.clone())
                                        .expect("channel_map non-empty");
                                    let reply = temm1e_core::types::message::OutboundMessage {
                                        chat_id: chat_id.clone(),
                                        text: status_text,
                                        reply_to: Some(inbound.id.clone()),
                                        parse_mode: None,
                                    };
                                    let _ = icpt_sender.send_message(reply).await;
                                    continue;
                                }

                                // ── Mission Control: intercept when busy (skip heartbeats) ──
                                // When idle (waiting on chat_rx), let the message
                                // fall through to the worker channel.
                                // Skip interceptor for heartbeat tasks — user message
                                // preempts heartbeat (lines above), falls through naturally.
                                if slot.is_busy.load(Ordering::Relaxed)
                                    && !slot.is_heartbeat.load(Ordering::Relaxed)
                                {
                                    // DO NOT push to pending queue here — Mission Control
                                    // classifies first, then routes to pending ([AMEND])
                                    // or order queue ([QUEUE]).
                                    let icpt_sender = channel_map_arc
                                        .get(&inbound.channel)
                                        .cloned()
                                        .or_else(|| primary_fallback.clone())
                                        .expect("channel_map non-empty");
                                    let icpt_chat_id = chat_id.clone();
                                    let icpt_msg_id = inbound.id.clone();
                                    let icpt_msg_text = inbound.text.clone().unwrap_or_default();
                                    let icpt_inbound = inbound.clone();
                                    let icpt_interrupt = slot.interrupt.clone();
                                    let icpt_active_cancel = slot.active_cancel.clone();
                                    let icpt_task = slot.current_task.clone();
                                    let icpt_status_tx = slot.status_tx.clone();
                                    let icpt_order_queue = slot.order_queue.clone();
                                    let icpt_pending = pending_clone.clone();
                                    let icpt_perpetuum_temporal = perpetuum_temporal.clone();
                                    let icpt_agent_state = agent_state_clone.clone();
                                    let icpt_personality = personality.clone();
                                    tokio::spawn(async move {
                                        let task_desc = icpt_task.lock()
                                            .map(|t| t.clone())
                                            .unwrap_or_default();

                                        // Read real-time phase from status watch channel
                                        let status_snap = icpt_status_tx.borrow().clone();
                                        let elapsed = status_snap.started_at.elapsed().as_secs();
                                        let phase_str = format!("{}", status_snap.phase);

                                        // Read Perpetuum background context (cached, zero DB cost)
                                        let perpetuum_ctx = icpt_perpetuum_temporal.read().await.clone();
                                        let perpetuum_section = if perpetuum_ctx.is_empty() {
                                            "None active".to_string()
                                        } else {
                                            perpetuum_ctx
                                        };

                                        // Read order queue count
                                        let oq_count = icpt_order_queue.lock()
                                            .map(|q| q.len()).unwrap_or(0);

                                        // Get provider + model from the active agent
                                        let agent_guard = icpt_agent_state.read().await;
                                        let Some(agent) = agent_guard.as_ref() else { return; };
                                        let provider = agent.provider_arc();
                                        let model = agent.model().to_string();
                                        drop(agent_guard);

                                        let soul = build_system_prompt(&icpt_personality);
                                        let request = temm1e_core::types::message::CompletionRequest {
                                            model,
                                            system: Some(format!(
                                                "{soul}\n\n\
                                                 === MISSION CONTROL ===\n\
                                                 You are Tem's MISSION CONTROL. Your main self is busy working.\n\n\
                                                 FOREGROUND TASK:\n\
                                                   Request: \"{task_desc}\"\n\
                                                   Phase: {phase_str}\n\
                                                   Elapsed: {elapsed}s | Rounds: {} | Tools run: {} | Cost: ${:.4}\n\n\
                                                 BACKGROUND (Perpetuum):\n\
                                                   {perpetuum_section}\n\n\
                                                 QUEUED ORDERS: {oq_count}\n\n\
                                                 The user says: \"{icpt_msg_text}\"\n\n\
                                                 Classify and respond (1-3 sentences max). End with EXACTLY ONE token:\n\
                                                 [AMEND] — user is correcting/adding to the CURRENT task\n\
                                                 [QUEUE] — user wants something NEW done AFTER the current task\n\
                                                 [CANCEL] — user wants to STOP the current task\n\
                                                 [CHAT] — user is chatting or asking about status\n\n\
                                                 Rules:\n\
                                                 - For status questions: describe what you're doing using the phase info, then end with [CHAT]\n\
                                                 - For [QUEUE]: confirm the order is queued\n\
                                                 - For [AMEND]: acknowledge the update\n\
                                                 - NEVER use [CANCEL] unless the user clearly wants to stop\n\
                                                 === END MISSION CONTROL ===",
                                                status_snap.rounds_completed,
                                                status_snap.tools_executed,
                                                status_snap.cost_usd,
                                            )),
                                            messages: vec![
                                                temm1e_core::types::message::ChatMessage {
                                                    role: temm1e_core::types::message::Role::User,
                                                    content: temm1e_core::types::message::MessageContent::Text(icpt_msg_text.clone()),
                                                },
                                            ],
                                            tools: vec![],
                                            max_tokens: None,
                                            temperature: Some(0.7),
                                            system_volatile: None,
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

                                                // Parse classification token
                                                let classification = if text.contains("[CANCEL]") {
                                                    "cancel"
                                                } else if text.contains("[QUEUE]") {
                                                    "queue"
                                                } else if text.contains("[AMEND]") {
                                                    "amend"
                                                } else {
                                                    "chat"
                                                };

                                                // Strip all tokens from response
                                                for token in &["[CANCEL]", "[QUEUE]", "[AMEND]", "[CHAT]"] {
                                                    text = text.replace(token, "");
                                                }
                                                text = text.trim().to_string();

                                                // Send response to user
                                                if !text.is_empty() {
                                                    let reply = temm1e_core::types::message::OutboundMessage {
                                                        chat_id: icpt_chat_id.clone(),
                                                        text,
                                                        reply_to: Some(icpt_msg_id),
                                                        parse_mode: None,
                                                    };
                                                    let _ = icpt_sender.send_message(reply).await;
                                                }

                                                // Route based on classification
                                                match classification {
                                                    "cancel" => {
                                                        icpt_interrupt.store(true, Ordering::Relaxed);
                                                        if let Ok(ct) = icpt_active_cancel.lock() {
                                                            ct.cancel();
                                                        }
                                                        tracing::info!(
                                                            chat_id = %icpt_chat_id,
                                                            "Mission Control cancelled active task"
                                                        );
                                                    }
                                                    "queue" => {
                                                        if let Ok(mut oq) = icpt_order_queue.lock() {
                                                            oq.push_back(QueuedOrder {
                                                                original_msg: icpt_inbound,
                                                                queued_at: std::time::Instant::now(),
                                                            });
                                                        }
                                                        tracing::info!(
                                                            chat_id = %icpt_chat_id,
                                                            "Mission Control queued new order"
                                                        );
                                                    }
                                                    "amend" => {
                                                        if let Ok(mut pq) = icpt_pending.lock() {
                                                            pq.entry(icpt_chat_id.clone())
                                                                .or_default()
                                                                .push(icpt_msg_text);
                                                        }
                                                        tracing::info!(
                                                            chat_id = %icpt_chat_id,
                                                            "Mission Control routed amendment to pending"
                                                        );
                                                    }
                                                    _ => {
                                                        // [CHAT] — message consumed by response
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                tracing::warn!(
                                                    error = %e,
                                                    "Mission Control LLM call failed — fallback to pending"
                                                );
                                                // Conservative: treat as amendment
                                                if let Ok(mut pq) = icpt_pending.lock() {
                                                    pq.entry(icpt_chat_id.clone())
                                                        .or_default()
                                                        .push(icpt_msg_text);
                                                }
                                                // Send hardcoded ack
                                                let ack = temm1e_core::types::message::OutboundMessage {
                                                    chat_id: icpt_chat_id,
                                                    text: "Got your message \u{2014} I'll look at it when I finish what I'm working on.".to_string(),
                                                    reply_to: Some(icpt_msg_id),
                                                    parse_mode: None,
                                                };
                                                let _ = icpt_sender.send_message(ack).await;
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
                        let personality_for_worker = personality.clone();
                        let social_storage_for_worker = social_storage.clone();
                        let social_config_for_worker = social_config_captured.clone();
                        let witness_attachments_for_worker = witness_attachments.clone();
                        let slot = slots.entry(chat_id.clone()).or_insert_with(|| {
                            let (chat_tx, mut chat_rx) =
                                tokio::sync::mpsc::channel::<temm1e_core::types::message::InboundMessage>(32);

                            let interrupt = Arc::new(AtomicBool::new(false));
                            let is_heartbeat = Arc::new(AtomicBool::new(false));
                            let is_busy = Arc::new(AtomicBool::new(false));
                            let current_task: Arc<std::sync::Mutex<String>> = Arc::new(std::sync::Mutex::new(String::new()));
                            let cancel_token = tokio_util::sync::CancellationToken::new();
                            // ── Mission Control state ──
                            let (slot_status_tx, _) = tokio::sync::watch::channel(
                                temm1e_agent::AgentTaskStatus::default(),
                            );
                            let order_queue: OrderQueue = Arc::new(std::sync::Mutex::new(std::collections::VecDeque::new()));
                            let active_cancel: Arc<std::sync::Mutex<tokio_util::sync::CancellationToken>> =
                                Arc::new(std::sync::Mutex::new(cancel_token.child_token()));
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
                            let channel_map_worker = channel_map_arc.clone();
                            let primary_fallback_worker = primary_fallback.clone();
                            let workspace_path = ws_path.clone();
                            let interrupt_clone = interrupt.clone();
                            let is_heartbeat_clone = is_heartbeat.clone();
                            let cancel_token_clone = cancel_token.clone();
                            let status_tx_clone = slot_status_tx.clone();
                            let order_queue_worker = order_queue.clone();
                            let active_cancel_clone = active_cancel.clone();
                            let pending_for_worker = pending_clone.clone();
                            let shared_mode = shared_mode_for_worker;
                            let shared_memory_strategy = shared_memory_strategy_for_worker;
                            let personality = personality_for_worker;
                            let social_storage = social_storage_for_worker;
                            let social_config_captured = social_config_for_worker;
                            let witness_attachments = witness_attachments_for_worker;
                            let setup_tokens_worker = setup_tokens_clone.clone();
                            let pending_raw_keys_worker = pending_raw_keys_clone.clone();
                            #[cfg(feature = "browser")]
                            let login_sessions_worker = login_sessions_clone.clone();
                            #[cfg(feature = "browser")]
                            let vault_for_login = vault.clone();
                            #[cfg(feature = "browser")]
                            let browser_ref_worker = browser_tool_ref.clone();
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
                                    // Resolve sender per-message from channel map
                                    let sender: Arc<dyn temm1e_core::Channel> = channel_map_worker
                                        .get(&msg.channel)
                                        .cloned()
                                        .or_else(|| primary_fallback_worker.clone())
                                        .expect("channel_map is non-empty, checked at gate");

                                    // Snapshot for outer panic handler (msg is borrowed by async block)
                                    let panic_chat_id = msg.chat_id.clone();
                                    let panic_msg_id = msg.id.clone();

                                    let outer_catch_result = AssertUnwindSafe(async {
                                    let is_hb = msg.channel == "heartbeat";
                                    is_heartbeat_clone.store(is_hb, Ordering::Relaxed);
                                    interrupt_clone.store(false, Ordering::Relaxed);

                                    let interrupt_flag = Some(interrupt_clone.clone());

                                    // ── Mission Control: reset status + fresh cancel token ──
                                    status_tx_clone.send_modify(|s| *s = temm1e_agent::AgentTaskStatus::default());
                                    let status_tx = status_tx_clone.clone();
                                    let task_cancel = cancel_token_clone.child_token();
                                    if let Ok(mut ac) = active_cancel_clone.lock() {
                                        *ac = task_cancel.clone();
                                    }
                                    let cancel = task_cancel;

                                    // ── Commands — intercepted before agent ──────
                                    let msg_text_cmd = msg.text.as_deref().unwrap_or("");
                                    let cmd_lower = msg_text_cmd.trim().to_lowercase();

                                    // ── RBAC: centralized command gate ────────────
                                    // Block admin-only slash commands for User role.
                                    if cmd_lower.starts_with('/')
                                        && !is_command_allowed_for_user(
                                            &msg.channel,
                                            &msg.user_id,
                                            &cmd_lower,
                                        )
                                    {
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: "You don't have permission to use this command."
                                                .to_string(),
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        if let Err(e) = sender.send_message(reply).await {
                                            tracing::error!(error = %e, "Failed to send permission denied reply");
                                        }
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /eigentune — Eigen-Tune slash dispatch
                                    if cmd_lower.starts_with("/eigentune") {
                                        let arg = msg_text_cmd.trim()
                                            ["/eigentune".len()..]
                                            .trim()
                                            .to_string();
                                        let reply_text = handle_eigentune_slash(&arg).await;
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: reply_text,
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /addkey — secure OTK flow
                                    if cmd_lower == "/addkey" {
                                        let otk = setup_tokens_worker.generate(&msg.chat_id).await;
                                        let otk_hex = hex::encode(otk);
                                        let link = format!(
                                            "https://temm1e-labs.github.io/temm1e/setup#{}",
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

                                    // /addkey github — GitHub PAT for vigil
                                    if cmd_lower == "/addkey github" {
                                        pending_raw_keys_worker.lock().await.insert(msg.chat_id.clone());
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: "Paste your GitHub Personal Access Token.\n\n\
                                                   Create one at: github.com/settings/tokens/new\n\
                                                   Select ONLY the `public_repo` scope.\n\n\
                                                   This lets me report bugs I find in myself to the TEMM1E developers."
                                                .to_string(),
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

                                    // /addmodel — register a custom model for the active provider
                                    if cmd_lower.starts_with("/addmodel") {
                                        let args = msg_text_cmd.trim()["/addmodel".len()..].trim();
                                        let info = handle_addmodel_command(args);
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

                                    // /listmodels — show all hardcoded + custom models
                                    if cmd_lower == "/listmodels" {
                                        let info = handle_listmodels_command();
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

                                    // /removemodel — drop a custom model for the active provider
                                    if cmd_lower.starts_with("/removemodel") {
                                        let args = msg_text_cmd.trim()["/removemodel".len()..].trim();
                                        let info = handle_removemodel_command(args);
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

                                    // /vigil — self-diagnosis vigil commands
                                    if cmd_lower.starts_with("/vigil") {
                                        let subcmd = cmd_lower.strip_prefix("/vigil").unwrap_or("").trim();
                                        let reply_text = match subcmd {
                                            "disable" => {
                                                // Persist to config file
                                                let config_path = dirs::home_dir()
                                                    .unwrap_or_default()
                                                    .join(".temm1e")
                                                    .join("vigil.toml");
                                                std::fs::write(&config_path, "enabled = false\nconsent_given = false\nauto_report = false\n").ok();
                                                "Vigil disabled. Re-enable by deleting ~/.temm1e/vigil.toml.".to_string()
                                            }
                                            "auto" => {
                                                let config_path = dirs::home_dir()
                                                    .unwrap_or_default()
                                                    .join(".temm1e")
                                                    .join("vigil.toml");
                                                std::fs::write(&config_path, "enabled = true\nconsent_given = true\nauto_report = true\n").ok();
                                                "Vigil auto-reporting enabled. I'll show a 60-second window before each report.".to_string()
                                            }
                                            "status" => {
                                                let has_github = load_credentials_file()
                                                    .is_some_and(|c| c.providers.iter().any(|p| p.name == "github"));
                                                let consent_path = dirs::home_dir()
                                                    .unwrap_or_default()
                                                    .join(".temm1e")
                                                    .join("vigil.toml");
                                                let consent = std::fs::read_to_string(&consent_path)
                                                    .unwrap_or_default()
                                                    .contains("consent_given = true");
                                                format!(
                                                    "Tem Vigil Status:\n\
                                                     - GitHub PAT: {}\n\
                                                     - Consent: {}\n\
                                                     - Log file: {}\n\n\
                                                     Commands: /vigil auto, /vigil disable, /vigil status",
                                                    if has_github { "configured" } else { "not set — run /addkey github" },
                                                    if consent { "granted" } else { "not yet" },
                                                    temm1e_observable::file_logger::current_log_path().display(),
                                                )
                                            }
                                            _ => {
                                                "Tem Vigil Commands:\n\
                                                 - /vigil status — show current configuration\n\
                                                 - /vigil auto — enable auto-reporting (with 60s review window)\n\
                                                 - /vigil disable — disable all vigil\n\
                                                 - /addkey github — add GitHub PAT for issue creation".to_string()
                                            }
                                        };
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: reply_text,
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
                                                                Some(build_system_prompt(&personality)),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
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
                                                    // Proxy providers use lenient placeholder check so short
                                                    // LM Studio / Ollama keys survive /model reload.
                                                    let has_custom = prov.base_url.is_some();
                                                    let valid_keys: Vec<String> = prov.keys.iter()
                                                        .filter(|k| {
                                                            if has_custom {
                                                                !is_placeholder_key_lenient(k)
                                                            } else {
                                                                !is_placeholder_key(k)
                                                            }
                                                        })
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
                                                                Some(build_system_prompt(&personality)),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
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

                                    // /timelimit [seconds] — view or set hive task time limit
                                    if cmd_lower == "/timelimit"
                                        || cmd_lower.starts_with("/timelimit ")
                                    {
                                        let args = if cmd_lower == "/timelimit" {
                                            ""
                                        } else {
                                            msg_text_cmd.trim()["/timelimit".len()..].trim()
                                        };
                                        let response = if args.is_empty() {
                                            // Show current limit
                                            if let Some(ref hive) = hive_worker {
                                                let secs = hive.max_task_duration_secs();
                                                format!(
                                                    "Current task time limit: {}s ({}m {}s)",
                                                    secs,
                                                    secs / 60,
                                                    secs % 60
                                                )
                                            } else {
                                                "Hive (swarm) is not enabled.".to_string()
                                            }
                                        } else if let Ok(secs) = args.parse::<u64>() {
                                            if secs < 30 {
                                                "Time limit must be at least 30 seconds.".to_string()
                                            } else if let Some(ref hive) = hive_worker {
                                                hive.set_max_task_duration_secs(secs);
                                                format!(
                                                    "Task time limit set to {}s ({}m {}s)",
                                                    secs,
                                                    secs / 60,
                                                    secs % 60
                                                )
                                            } else {
                                                "Hive (swarm) is not enabled.".to_string()
                                            }
                                        } else {
                                            "Usage: /timelimit [seconds]\nExample: /timelimit 1800"
                                                .to_string()
                                        };
                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: response,
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_busy_clone.store(false, Ordering::Relaxed);
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
/addmodel <name> context:<int> output:<int> [input_price:<float>] [output_price:<float>] — Register a custom model (LM Studio, Ollama, vLLM, …)\n\
/listmodels — Show hardcoded + custom models grouped by provider\n\
/removemodel <name> — Remove a custom model from the active provider\n\
/usage — Show token usage and cost summary\n\
/memory — Show current memory strategy\n\
/memory lambda — Switch to λ-Memory (decay + persistence)\n\
/memory echo — Switch to Echo Memory (context window only)\n\
/cambium — Cambium status (gap-driven self-grow)\n\
/cambium on — Enable cambium growth\n\
/cambium off — Disable cambium growth\n\
/eigentune — Eigen-Tune status (self-tuning distillation)\n\
/eigentune setup — Show prerequisites + setup guide\n\
/eigentune model — Show base model + recommendations\n\
/eigentune tick — Manually advance state machine\n\
/eigentune demote <tier> — Force-revert a graduated tier (kill switch)\n\
/mcp — List connected MCP servers and tools\n\
/mcp add <name> <command-or-url> — Connect a new MCP server\n\
/mcp remove <name> — Disconnect an MCP server\n\
/mcp restart <name> — Restart an MCP server\n\
/browser — Browser status, sessions, and lifecycle\n\
/browser close — Save sessions and close browser\n\
/browser sessions — List saved web sessions\n\
/browser forget <service> — Delete a saved session\n\
/timelimit — Show current task time limit\n\
/timelimit <seconds> — Set task time limit (e.g. /timelimit 1800)\n\
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

                                    // /cambium — gap-driven self-grow toggle
                                    if cmd_lower == "/cambium" || cmd_lower.starts_with("/cambium ") {
                                        // Read the original (case-preserving) text for the grow task
                                        let original_args = msg_text_cmd
                                            .trim()
                                            .strip_prefix("/cambium")
                                            .or_else(|| msg_text_cmd.trim().strip_prefix("/CAMBIUM"))
                                            .unwrap_or("")
                                            .trim();
                                        let subcmd = cmd_lower
                                            .strip_prefix("/cambium")
                                            .unwrap_or("")
                                            .trim();
                                        let cambium_path = dirs::home_dir()
                                            .unwrap_or_default()
                                            .join(".temm1e")
                                            .join("cambium.toml");
                                        let current_enabled = std::fs::read_to_string(&cambium_path)
                                            .ok()
                                            .and_then(|s| {
                                                s.lines()
                                                    .find(|l| l.trim().starts_with("enabled"))
                                                    .map(|l| !l.contains("false"))
                                            })
                                            .unwrap_or(true);

                                        // /cambium grow <task> — spawn an async growth session
                                        if subcmd.starts_with("grow") {
                                            let task = original_args
                                                .strip_prefix("grow")
                                                .or_else(|| original_args.strip_prefix("GROW"))
                                                .unwrap_or("")
                                                .trim()
                                                .to_string();
                                            if task.is_empty() {
                                                let reply = temm1e_core::types::message::OutboundMessage {
                                                    chat_id: msg.chat_id.clone(),
                                                    text: "Usage: /cambium grow <description>\n\nExample: /cambium grow add a function that converts celsius to fahrenheit with tests".to_string(),
                                                    reply_to: Some(msg.id.clone()),
                                                    parse_mode: None,
                                                };
                                                send_with_retry(&*sender, reply).await;
                                                is_heartbeat_clone.store(false, Ordering::Relaxed);
                                                return;
                                            }
                                            if !current_enabled {
                                                let reply = temm1e_core::types::message::OutboundMessage {
                                                    chat_id: msg.chat_id.clone(),
                                                    text: "Cambium is DISABLED. Run /cambium on to enable, then try again.".to_string(),
                                                    reply_to: Some(msg.id.clone()),
                                                    parse_mode: None,
                                                };
                                                send_with_retry(&*sender, reply).await;
                                                is_heartbeat_clone.store(false, Ordering::Relaxed);
                                                return;
                                            }
                                            // Acquire the agent's provider + model
                                            let agent_guard = agent_state.read().await;
                                            let Some(agent) = agent_guard.as_ref() else {
                                                drop(agent_guard);
                                                let reply = temm1e_core::types::message::OutboundMessage {
                                                    chat_id: msg.chat_id.clone(),
                                                    text: "Cambium needs an active provider. Set up an API key with /addkey first.".to_string(),
                                                    reply_to: Some(msg.id.clone()),
                                                    parse_mode: None,
                                                };
                                                send_with_retry(&*sender, reply).await;
                                                is_heartbeat_clone.store(false, Ordering::Relaxed);
                                                return;
                                            };
                                            let provider = agent.provider_arc();
                                            let model = agent.model().to_string();
                                            drop(agent_guard);

                                            // Send acknowledgement
                                            let ack = temm1e_core::types::message::OutboundMessage {
                                                chat_id: msg.chat_id.clone(),
                                                text: format!(
                                                    "Cambium session started.\nTask: {task}\nModel: {model}\nThis runs in an isolated tempdir — production code is never touched.\nProgress will follow shortly..."
                                                ),
                                                reply_to: Some(msg.id.clone()),
                                                parse_mode: None,
                                            };
                                            send_with_retry(&*sender, ack).await;

                                            // Spawn the cambium session asynchronously
                                            let sender_for_session = sender.clone();
                                            let chat_id_for_session = msg.chat_id.clone();
                                            let reply_to_for_session = msg.id.clone();
                                            tokio::spawn(async move {
                                                let cfg = temm1e_cambium::session::CambiumSessionConfig::new(
                                                    task.clone(),
                                                    model.clone(),
                                                );
                                                let report = match temm1e_cambium::session::run_minimal_session(
                                                    provider,
                                                    cfg,
                                                    None,
                                                )
                                                .await
                                                {
                                                    Ok(r) => r,
                                                    Err(e) => {
                                                        let err_reply = temm1e_core::types::message::OutboundMessage {
                                                            chat_id: chat_id_for_session.clone(),
                                                            text: format!("Cambium session failed to start: {e}"),
                                                            reply_to: Some(reply_to_for_session.clone()),
                                                            parse_mode: None,
                                                        };
                                                        send_with_retry(&*sender_for_session, err_reply).await;
                                                        return;
                                                    }
                                                };
                                                let summary = format_cambium_report(&report);
                                                let final_reply = temm1e_core::types::message::OutboundMessage {
                                                    chat_id: chat_id_for_session,
                                                    text: summary,
                                                    reply_to: Some(reply_to_for_session),
                                                    parse_mode: None,
                                                };
                                                send_with_retry(&*sender_for_session, final_reply).await;
                                            });
                                            is_heartbeat_clone.store(false, Ordering::Relaxed);
                                            return;
                                        }

                                        let response = match subcmd {
                                            "on" | "enable" | "enabled" => {
                                                if let Some(parent) = cambium_path.parent() {
                                                    let _ = std::fs::create_dir_all(parent);
                                                }
                                                let _ = std::fs::write(
                                                    &cambium_path,
                                                    "# Cambium runtime config — toggled via /cambium\nenabled = true\n",
                                                );
                                                "Cambium ENABLED. Tem may grow new capabilities at the cambium layer (heartwood stays immutable).".to_string()
                                            }
                                            "off" | "disable" | "disabled" => {
                                                if let Some(parent) = cambium_path.parent() {
                                                    let _ = std::fs::create_dir_all(parent);
                                                }
                                                let _ = std::fs::write(
                                                    &cambium_path,
                                                    "# Cambium runtime config — toggled via /cambium\nenabled = false\n",
                                                );
                                                "Cambium DISABLED. Tem will not grow new capabilities until you run /cambium on.".to_string()
                                            }
                                            "" | "status" => format!(
                                                "Cambium status: {}\n\n\
                                                 Cambium is the layer where Tem grows new capabilities at the edge while the heartwood (immutable kernel: vault, core traits, security) stays stable. Named after the biological cambium — the growth tissue under tree bark where rings are added each year.\n\n\
                                                 Commands:\n\
                                                 /cambium on — enable cambium growth\n\
                                                 /cambium off — disable cambium growth\n\
                                                 /cambium status — show current state (this view)\n\n\
                                                 Default: enabled. Persisted to ~/.temm1e/cambium.toml",
                                                if current_enabled { "ENABLED" } else { "DISABLED" }
                                            ),
                                            other => format!(
                                                "Unknown subcommand: {other}\nTry: /cambium on | /cambium off | /cambium status"
                                            ),
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
                                                                    Some(build_system_prompt(&personality)),
                                                                    max_turns, max_ctx, max_rounds, max_task_duration, max_spend,
                                                                ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
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
                                                            Some(build_system_prompt(&personality)),
                                                            max_turns, max_ctx, max_rounds, max_task_duration, max_spend,
                                                        ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
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
                                                            Some(build_system_prompt(&personality)),
                                                            max_turns, max_ctx, max_rounds, max_task_duration, max_spend,
                                                        ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
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
                                        if !is_command_allowed_for_user(&msg.channel, &msg.user_id, &cmd_lower) {
                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                chat_id: msg.chat_id.clone(),
                                                text: "You don't have permission to use this command.".to_string(),
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
                                                // Proxy providers use lenient placeholder check so short
                                                // LM Studio / Ollama keys survive /reload.
                                                let has_custom = prov.base_url.is_some();
                                                let valid_keys: Vec<String> = prov.keys.iter()
                                                    .filter(|k| {
                                                        if has_custom {
                                                            !is_placeholder_key_lenient(k)
                                                        } else {
                                                            !is_placeholder_key(k)
                                                        }
                                                    })
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
                                                                Some(build_system_prompt(&personality)),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
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

                                    // /login <service> <url> — OTK Prowl interactive login session
                                    #[cfg(feature = "browser")]
                                    if cmd_lower.starts_with("/login ") || cmd_lower == "/login" {
                                        let args = msg_text_cmd.trim().strip_prefix("/login").unwrap_or("").trim();
                                        if args.is_empty() {
                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                chat_id: msg.chat_id.clone(),
                                                text: "Usage: /login <service>\nExamples:\n  /login facebook\n  /login github\n  /login https://mysite.com/login\n  /login myapp https://myapp.com/auth\n\n100+ services supported: facebook, google, github, slack, discord, amazon, netflix, spotify...".to_string(),
                                                reply_to: Some(msg.id.clone()),
                                                parse_mode: None,
                                            };
                                            send_with_retry(&*sender, reply).await;
                                            is_heartbeat_clone.store(false, Ordering::Relaxed);
                                            return;
                                        }

                                        // Resolve service name → login URL using registry
                                        let (service_name, login_url) = match temm1e_tools::prowl_blueprints::login_registry::resolve_login_args(args) {
                                            Some((s, u)) => (s, u),
                                            None => {
                                                let reply = temm1e_core::types::message::OutboundMessage {
                                                    chat_id: msg.chat_id.clone(),
                                                    text: "Could not parse login target. Try: /login facebook".to_string(),
                                                    reply_to: Some(msg.id.clone()),
                                                    parse_mode: None,
                                                };
                                                send_with_retry(&*sender, reply).await;
                                                is_heartbeat_clone.store(false, Ordering::Relaxed);
                                                return;
                                            }
                                        };

                                        tracing::info!(
                                            service = %service_name,
                                            url = %login_url,
                                            chat_id = %msg.chat_id,
                                            "OTK Prowl login session starting"
                                        );

                                        // Launch browser and create session via convenience API
                                        match temm1e_tools::browser_session::InteractiveBrowseSession::launch(
                                            &service_name, &login_url
                                        ).await {
                                            Ok(mut session) => {
                                                // Capture first annotated screenshot
                                                match session.capture_annotated().await {
                                                    Ok((_png, description)) => {
                                                        let text = format!(
                                                            "🔐 Login session for '{}'\n\n{}",
                                                            service_name, description
                                                        );
                                                        let reply = temm1e_core::types::message::OutboundMessage {
                                                            chat_id: msg.chat_id.clone(),
                                                            text,
                                                            reply_to: Some(msg.id.clone()),
                                                            parse_mode: None,
                                                        };
                                                        send_with_retry(&*sender, reply).await;

                                                        // Store session for this chat
                                                        login_sessions_worker.lock().await.insert(
                                                            msg.chat_id.clone(), session
                                                        );
                                                    }
                                                    Err(e) => {
                                                        let reply = temm1e_core::types::message::OutboundMessage {
                                                            chat_id: msg.chat_id.clone(),
                                                            text: format!("Failed to scan page: {}", e),
                                                            reply_to: Some(msg.id.clone()),
                                                            parse_mode: None,
                                                        };
                                                        send_with_retry(&*sender, reply).await;
                                                    }
                                                }
                                            }
                                            Err(e) => {
                                                let reply = temm1e_core::types::message::OutboundMessage {
                                                    chat_id: msg.chat_id.clone(),
                                                    text: format!("Login session failed: {}", e),
                                                    reply_to: Some(msg.id.clone()),
                                                    parse_mode: None,
                                                };
                                                send_with_retry(&*sender, reply).await;
                                            }
                                        }

                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // ── Active login session interceptor ──────
                                    // If this chat has an active login session, route messages there
                                    // instead of the agent
                                    #[cfg(feature = "browser")]
                                    {
                                        let has_session = login_sessions_worker.lock().await.contains_key(&msg.chat_id);
                                        if has_session {
                                            let input = msg_text_cmd.trim();
                                            let mut sessions = login_sessions_worker.lock().await;
                                            if let Some(session) = sessions.get_mut(&msg.chat_id) {
                                                match session.handle_input(input).await {
                                                    Ok(temm1e_tools::browser_session::SessionAction::Continue) => {
                                                        // Re-capture and send updated page
                                                        match session.capture_annotated().await {
                                                            Ok((_png, description)) => {
                                                                let reply = temm1e_core::types::message::OutboundMessage {
                                                                    chat_id: msg.chat_id.clone(),
                                                                    text: format!("✅ Done\n\n{}", description),
                                                                    reply_to: Some(msg.id.clone()),
                                                                    parse_mode: None,
                                                                };
                                                                send_with_retry(&*sender, reply).await;
                                                            }
                                                            Err(e) => {
                                                                let reply = temm1e_core::types::message::OutboundMessage {
                                                                    chat_id: msg.chat_id.clone(),
                                                                    text: format!("Page scan error: {}", e),
                                                                    reply_to: Some(msg.id.clone()),
                                                                    parse_mode: None,
                                                                };
                                                                send_with_retry(&*sender, reply).await;
                                                            }
                                                        }
                                                    }
                                                    Ok(temm1e_tools::browser_session::SessionAction::Done) => {
                                                        // Capture session to vault
                                                        if let Some(ref v) = vault_for_login {
                                                            match session.capture_session(v.as_ref()).await {
                                                                Ok(()) => {
                                                                    let svc = session.service().to_string();
                                                                    sessions.remove(&msg.chat_id);
                                                                    let reply = temm1e_core::types::message::OutboundMessage {
                                                                        chat_id: msg.chat_id.clone(),
                                                                        text: format!("🔒 Session for '{}' saved securely! I can now browse {} for you.", svc, svc),
                                                                        reply_to: Some(msg.id.clone()),
                                                                        parse_mode: None,
                                                                    };
                                                                    send_with_retry(&*sender, reply).await;
                                                                }
                                                                Err(e) => {
                                                                    let reply = temm1e_core::types::message::OutboundMessage {
                                                                        chat_id: msg.chat_id.clone(),
                                                                        text: format!("Session save failed: {}", e),
                                                                        reply_to: Some(msg.id.clone()),
                                                                        parse_mode: None,
                                                                    };
                                                                    send_with_retry(&*sender, reply).await;
                                                                }
                                                            }
                                                        } else {
                                                            sessions.remove(&msg.chat_id);
                                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                                chat_id: msg.chat_id.clone(),
                                                                text: "Login complete but vault not available — session not saved.".to_string(),
                                                                reply_to: Some(msg.id.clone()),
                                                                parse_mode: None,
                                                            };
                                                            send_with_retry(&*sender, reply).await;
                                                        }
                                                    }
                                                    Err(e) => {
                                                        let reply = temm1e_core::types::message::OutboundMessage {
                                                            chat_id: msg.chat_id.clone(),
                                                            text: format!("⚠️ {}", e),
                                                            reply_to: Some(msg.id.clone()),
                                                            parse_mode: None,
                                                        };
                                                        send_with_retry(&*sender, reply).await;
                                                    }
                                                }
                                            }
                                            is_heartbeat_clone.store(false, Ordering::Relaxed);
                                            return;
                                        }
                                    }

                                    // /browser — browser lifecycle management (V2)
                                    #[cfg(feature = "browser")]
                                    if cmd_lower == "/browser" || cmd_lower.starts_with("/browser ") {
                                        let browser_args = if cmd_lower == "/browser" {
                                            ""
                                        } else {
                                            msg_text_cmd.trim().strip_prefix("/browser").unwrap_or("").trim()
                                        };
                                        let browser_args_lower = browser_args.to_lowercase();

                                        let response_text = if browser_args_lower.is_empty() || browser_args_lower == "status" {
                                            // /browser or /browser status — report state
                                            match &browser_ref_worker {
                                                Some(bt) => {
                                                    if bt.is_running() {
                                                        let domains = bt.get_active_domains();
                                                        let sessions_str = if domains.is_empty() {
                                                            "none".to_string()
                                                        } else {
                                                            let mut sorted: Vec<_> = domains.into_iter().collect();
                                                            sorted.sort();
                                                            sorted.join(", ")
                                                        };
                                                        let uptime = bt.uptime().unwrap_or_else(|| "unknown".to_string());
                                                        format!(
                                                            "\u{1f310} Browser: Active\nSessions: {}\nUptime: {}",
                                                            sessions_str, uptime
                                                        )
                                                    } else {
                                                        "\u{1f310} Browser: Inactive. Will start on next web task.".to_string()
                                                    }
                                                }
                                                None => "\u{1f310} Browser: Not configured.".to_string(),
                                            }
                                        } else if browser_args_lower == "close" {
                                            // /browser close — auto-capture and close
                                            match &browser_ref_worker {
                                                Some(bt) => {
                                                    let (msg, saved) = bt.close_with_capture().await;
                                                    if saved.is_empty() {
                                                        format!("\u{1f512} {}", msg)
                                                    } else {
                                                        format!(
                                                            "\u{1f4be} Sessions saved: {}\n\u{1f512} {}",
                                                            saved.join(", "), msg
                                                        )
                                                    }
                                                }
                                                None => "Browser not configured.".to_string(),
                                            }
                                        } else if browser_args_lower == "sessions" {
                                            // /browser sessions — list saved vault sessions
                                            match &browser_ref_worker {
                                                Some(bt) => {
                                                    let sessions = bt.list_saved_sessions().await;
                                                    if sessions.is_empty() {
                                                        "\u{1f4cb} No saved sessions.".to_string()
                                                    } else {
                                                        let mut lines = vec!["\u{1f4cb} Saved sessions:".to_string()];
                                                        for (service, captured_at) in &sessions {
                                                            let age = format_capture_age(captured_at);
                                                            lines.push(format!("- {} (captured {})", service, age));
                                                        }
                                                        lines.join("\n")
                                                    }
                                                }
                                                None => "Browser not configured.".to_string(),
                                            }
                                        } else if browser_args_lower.starts_with("forget ") {
                                            // /browser forget <service>
                                            let service = browser_args["forget ".len()..].trim();
                                            if service.is_empty() {
                                                "Usage: /browser forget <service>".to_string()
                                            } else {
                                                match &browser_ref_worker {
                                                    Some(bt) => match bt.forget_session(service).await {
                                                        Ok(()) => format!("\u{1f5d1}\u{fe0f} Session for '{}' deleted.", service),
                                                        Err(e) => format!("Failed: {}", e),
                                                    },
                                                    None => "Browser not configured.".to_string(),
                                                }
                                            }
                                        } else {
                                            "Usage: /browser [status|close|sessions|forget <service>]".to_string()
                                        };

                                        let reply = temm1e_core::types::message::OutboundMessage {
                                            chat_id: msg.chat_id.clone(),
                                            text: response_text,
                                            reply_to: Some(msg.id.clone()),
                                            parse_mode: None,
                                        };
                                        send_with_retry(&*sender, reply).await;
                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                        return;
                                    }

                                    // /reset — factory reset from messaging (admin only)
                                    if cmd_lower == "/reset" {
                                        if !is_command_allowed_for_user(&msg.channel, &msg.user_id, &cmd_lower) {
                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                chat_id: msg.chat_id.clone(),
                                                text: "You don't have permission to use this command.".to_string(),
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
                                        if !is_command_allowed_for_user(&msg.channel, &msg.user_id, &cmd_lower) {
                                            let reply = temm1e_core::types::message::OutboundMessage {
                                                chat_id: msg.chat_id.clone(),
                                                text: "You don't have permission to use this command.".to_string(),
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
                                                .args(["/C", &format!("timeout /t 2 /nobreak >nul && \"{}\" start", exe_str)])
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
                                                    // GitHub PAT — not an LLM provider, handle separately
                                                    if cred.provider == "github" {
                                                        match temm1e_perpetuum::bug_reporter::check_pat_scopes(
                                                            &reqwest::Client::new(), &cred.api_key,
                                                        ).await {
                                                            Ok((_, dangerous)) => {
                                                                let mut reply_text = String::from("GitHub connected! I can now report bugs I find in myself.");
                                                                if !dangerous.is_empty() {
                                                                    reply_text = format!(
                                                                        "Warning: this token has more permissions than needed: {}\n\
                                                                         I only need `public_repo` scope.\n\
                                                                         Create a minimal token at: github.com/settings/tokens/new\n\n\
                                                                         Saved for now — I recommend replacing it with a minimal one.",
                                                                        dangerous.join(", ")
                                                                    );
                                                                }
                                                                if let Err(e) = save_credentials("github", &cred.api_key, "github", None).await {
                                                                    tracing::error!(error = %e, "Failed to save GitHub PAT");
                                                                }
                                                                let reply = temm1e_core::types::message::OutboundMessage {
                                                                    chat_id: msg.chat_id.clone(),
                                                                    text: reply_text,
                                                                    reply_to: Some(msg.id.clone()),
                                                                    parse_mode: None,
                                                                };
                                                                send_with_retry(&*sender, reply).await;
                                                            }
                                                            Err(e) => {
                                                                let reply = temm1e_core::types::message::OutboundMessage {
                                                                    chat_id: msg.chat_id.clone(),
                                                                    text: format!("GitHub PAT validation failed: {}", e),
                                                                    reply_to: Some(msg.id.clone()),
                                                                    parse_mode: None,
                                                                };
                                                                send_with_retry(&*sender, reply).await;
                                                            }
                                                        }
                                                        is_heartbeat_clone.store(false, Ordering::Relaxed);
                                                        if let Ok(mut pq) = pending_for_worker.lock() { pq.remove(&worker_chat_id); }
                                                        return;
                                                    }
                                                    // Honor user-specified `model:` from proxy command;
                                                    // fall back to provider default otherwise. Same
                                                    // pattern as the main onboarding flow so LM Studio /
                                                    // Ollama users can pick their local model at setup.
                                                    let model = cred
                                                        .model
                                                        .clone()
                                                        .unwrap_or_else(|| default_model(cred.provider).to_string());
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
                                                                Some(build_system_prompt(&personality)),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
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
                                            // GitHub PAT — handle separately (not an LLM provider)
                                            if cred.provider == "github" {
                                                match temm1e_perpetuum::bug_reporter::check_pat_scopes(
                                                    &reqwest::Client::new(), &cred.api_key,
                                                ).await {
                                                    Ok((_, dangerous)) => {
                                                        let mut reply_text = String::from("GitHub connected! I can now report bugs I find in myself.");
                                                        if !dangerous.is_empty() {
                                                            reply_text = format!(
                                                                "Warning: this token has more permissions than needed: {}\n\
                                                                 I only need `public_repo` scope.\n\n\
                                                                 Saved — but I recommend creating a minimal token.",
                                                                dangerous.join(", ")
                                                            );
                                                        }
                                                        save_credentials("github", &cred.api_key, "github", None).await.ok();
                                                        let reply = temm1e_core::types::message::OutboundMessage {
                                                            chat_id: msg.chat_id.clone(),
                                                            text: reply_text,
                                                            reply_to: Some(msg.id.clone()),
                                                            parse_mode: None,
                                                        };
                                                        send_with_retry(&*sender, reply).await;
                                                    }
                                                    Err(e) => {
                                                        let reply = temm1e_core::types::message::OutboundMessage {
                                                            chat_id: msg.chat_id.clone(),
                                                            text: format!("GitHub PAT validation failed: {}", e),
                                                            reply_to: Some(msg.id.clone()),
                                                            parse_mode: None,
                                                        };
                                                        send_with_retry(&*sender, reply).await;
                                                    }
                                                }
                                                is_heartbeat_clone.store(false, Ordering::Relaxed);
                                                if let Ok(mut pq) = pending_for_worker.lock() { pq.remove(&worker_chat_id); }
                                                return;
                                            }
                                            // Honor user-specified `model:` from proxy command;
                                            // fall back to provider default otherwise.
                                            let model = cred
                                                .model
                                                .clone()
                                                .unwrap_or_else(|| default_model(cred.provider).to_string());
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
                                                                Some(build_system_prompt(&personality)),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
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

                                        // Resolve user role from channel's role file
                                        let user_role = temm1e_core::types::rbac::load_role_file(&msg.channel)
                                            .and_then(|rf| rf.role_of(&msg.user_id))
                                            .unwrap_or(temm1e_core::types::rbac::Role::Admin);

                                        let mut session = temm1e_core::types::session::SessionContext {
                                            session_id: format!("{}-{}", msg.channel, msg.chat_id),
                                            user_id: msg.user_id.clone(),
                                            channel: msg.channel.clone(),
                                            chat_id: msg.chat_id.clone(),
                                            role: user_role,
                                            history: persistent_history.clone(),
                                            workspace_path: workspace_path.clone(),
                                            read_tracker: std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new())),
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
                                        // Only set current_task for user messages, not heartbeats.
                                        // Heartbeat text would poison the Mission Control interceptor.
                                        if !is_hb {
                                            if let Ok(mut ct) = current_task_clone.lock() {
                                                *ct = msg.text.as_deref().unwrap_or("").to_string();
                                            }
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
                                                                        system_volatile: None,
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
                                                            // Hive worker witness wiring — ACTIVE mode (v5.5.0).
                                                            // Workers now inherit the parent session's workspace_path
                                                            // (propagated via workspace_for_hive → workspace_for_worker
                                                            // below) so the Planner's file-path postconditions target
                                                            // the user's real workspace, not the process cwd. This
                                                            // closes the audit-trail gap where delegated work escaped
                                                            // Witness oversight in passive mode.
                                                            let witness_h = witness_attachments.clone();
                                                            let workspace_for_hive = workspace_path.clone();

                                                            let swarm_result = hive.execute_order(
                                                                &order_id, cancel,
                                                                move |task, deps| {
                                                                    let p = provider.clone();
                                                                    let t = tools_h.clone();
                                                                    let m_clone = memory_h.clone();
                                                                    let mdl = model_h.clone();
                                                                    let witness_for_worker = witness_h.clone();
                                                                    let workspace_for_worker = workspace_for_hive.clone();
                                                                    async move {
                                                                        let scoped = temm1e_hive::worker::build_scoped_context(&task, &deps);
                                                                        let mini = temm1e_agent::AgentRuntime::with_limits(
                                                                            p, m_clone, t, mdl, None, 10, 30000, 50, 300, 0.0,
                                                                        )
                                                                        .with_witness_attachments(
                                                                            witness_for_worker.as_ref(),
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
                                                                            chat_id: "hive".into(),
                                                                            role: temm1e_core::types::rbac::Role::Admin,
                                                                            history: vec![],
                                                                            workspace_path: workspace_for_worker,
                                                                            read_tracker: std::sync::Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new())),
                                                                        };
                                                                        match mini.process_message(&mini_msg, &mut s, None, None, None, None, None).await {
                                                                            Ok((r, u)) => {
                                                                                // P3: read worker's isolated budget for
                                                                                // accurate per-task accounting.
                                                                                let snap = mini.budget_snapshot();
                                                                                Ok(temm1e_hive::worker::TaskResult {
                                                                                    summary: r.text,
                                                                                    tokens_used: u.combined_tokens(),
                                                                                    input_tokens: snap.input_tokens,
                                                                                    output_tokens: snap.output_tokens,
                                                                                    cost_usd: snap.cost_usd,
                                                                                    artifacts: vec![], success: true, error: None,
                                                                                })
                                                                            }
                                                                            Err(e) => Ok(temm1e_hive::worker::TaskResult {
                                                                                summary: String::new(),
                                                                                tokens_used: 0,
                                                                                input_tokens: 0,
                                                                                output_tokens: 0,
                                                                                cost_usd: 0.0,
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
                                                            // Decomposition wasn't viable — fall back to single-agent processing
                                                            tracing::info!("Alpha: decomposition failed or not worth it, falling back to single-agent");
                                                            let fallback_agent = Arc::new(temm1e_agent::AgentRuntime::with_limits(
                                                                agent.provider_arc(),
                                                                memory.clone(),
                                                                tools_template.clone(),
                                                                agent.model().to_string(),
                                                                Some(build_system_prompt(&personality)),
                                                                max_turns, max_ctx, max_rounds, max_task_duration, max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
                                                            let fallback_cancel = cancel_token_clone.clone();
                                                            match fallback_agent.process_message(&msg, &mut session, Some(interrupt_clone.clone()), Some(pending_for_worker.clone()), None, None, Some(fallback_cancel)).await {
                                                                Ok((mut reply, _usage)) => {
                                                                    reply.text = censor_secrets(&reply.text);
                                                                    if !reply.text.trim().is_empty() {
                                                                        send_with_retry(&*sender, reply).await;
                                                                    }
                                                                }
                                                                Err(e) => {
                                                                    tracing::error!(error = %e, "Single-agent fallback failed");
                                                                    let reply = temm1e_core::types::message::OutboundMessage {
                                                                        chat_id: msg.chat_id.clone(),
                                                                        text: censor_secrets(&format_user_error(&e)),
                                                                        reply_to: Some(msg.id.clone()),
                                                                        parse_mode: None,
                                                                    };
                                                                    send_with_retry(&*sender, reply).await;
                                                                }
                                                            }
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
                                                // Filter out placeholder keys before reloading. Use lenient
                                                // mode when base_url is set so short LM Studio / Ollama keys
                                                // survive hot-reload.
                                                let has_custom_hot = saved_base_url.is_some();
                                                let valid_keys: Vec<String> = new_keys.into_iter()
                                                    .filter(|k| {
                                                        if has_custom_hot {
                                                            !is_placeholder_key_lenient(k)
                                                        } else {
                                                            !is_placeholder_key(k)
                                                        }
                                                    })
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
                                                                Some(build_system_prompt(&personality)),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
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
                                                Some(build_system_prompt(&personality)),
                                                max_turns, max_ctx, max_rounds, max_task_duration, max_spend,
                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
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
                                                Some(build_system_prompt(&personality)),
                                                max_turns, max_ctx, max_rounds, max_task_duration, max_spend,
                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
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
                                            // If the user passed `model:NAME` in a proxy command,
                                            // honor it. Otherwise fall back to the provider's
                                            // hardcoded default. This is what makes
                                            // `proxy openai http://lm/v1 sk-lm-xxx model:qwen3-coder`
                                            // work end-to-end for LM Studio / Ollama / vLLM.
                                            let model = cred
                                                .model
                                                .clone()
                                                .unwrap_or_else(|| default_model(provider_name).to_string());
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
                                                                Some(build_system_prompt(&personality)),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                                max_spend,
                                                            ).with_v2_optimizations(v2_opt).with_parallel_phases(pp_opt).with_hive_enabled(hive_on).with_shared_mode(shared_mode.clone()).with_shared_memory_strategy(shared_memory_strategy.clone()).with_personality(personality.clone()).with_social(social_storage.clone(), Some(social_config_captured.clone())).with_witness_attachments(witness_attachments.as_ref()));
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
                                                "https://temm1e-labs.github.io/temm1e/setup#{}",
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
                                    // ── Mission Control: dispatch next queued order ──
                                    if let Ok(mut oq) = order_queue_worker.lock() {
                                        if let Some(next_order) = oq.pop_front() {
                                            tracing::info!(
                                                chat_id = %worker_chat_id,
                                                remaining = oq.len(),
                                                "Dispatching next queued order"
                                            );
                                            if self_tx.try_send(next_order.original_msg).is_err() {
                                                tracing::warn!(
                                                    chat_id = %worker_chat_id,
                                                    "Failed to dispatch queued order — channel full"
                                                );
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

                            ChatSlot { tx: chat_tx, interrupt, is_heartbeat, is_busy, current_task, cancel_token, status_tx: slot_status_tx, order_queue, active_cancel }
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
                let channel_names: Vec<&str> = channel_map.keys().map(|s| s.as_str()).collect();
                if channel_names.is_empty() {
                    println!("  Status: No channels configured — set TELEGRAM_BOT_TOKEN or DISCORD_BOT_TOKEN");
                } else {
                    println!(
                        "  Status: Onboarding — send your API key via {}",
                        channel_names.join(" or ")
                    );
                }
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
            // v5.5.0: default-ON — same rationale as the Start path above.
            let hive_enabled_early = {
                #[derive(serde::Deserialize, Default)]
                struct HC {
                    #[serde(default)]
                    hive: HE,
                }
                #[derive(serde::Deserialize)]
                struct HE {
                    #[serde(default = "hive_default_enabled_cli")]
                    enabled: bool,
                }
                impl Default for HE {
                    fn default() -> Self {
                        Self {
                            enabled: hive_default_enabled_cli(),
                        }
                    }
                }
                fn hive_default_enabled_cli() -> bool {
                    true
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
                    .unwrap_or(true)
            };

            // ── Witness attachments (built once, reused across CLI chat
            // rebuilds). None when [witness] enabled=false — wiring no-ops.
            let witness_attachments: Option<temm1e_agent::witness_init::WitnessAttachments> =
                match temm1e_agent::witness_init::build_witness_attachments(&config.witness).await {
                    Ok(a) => {
                        if a.is_some() {
                            tracing::info!(
                                strictness = %config.witness.strictness,
                                auto_planner_oath = config.witness.auto_planner_oath,
                                "Witness enabled for CLI chat"
                            );
                        }
                        a
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Witness init failed — continuing without Witness");
                        None
                    }
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

            // ── Vault (encrypted credential store) ───────────────
            let vault: Option<Arc<dyn temm1e_core::Vault>> = match temm1e_vault::LocalVault::new()
                .await
            {
                Ok(v) => {
                    tracing::info!("Vault initialized (CLI)");
                    Some(Arc::new(v) as Arc<dyn temm1e_core::Vault>)
                }
                Err(e) => {
                    tracing::warn!(error = %e, "Vault initialization failed — browser authenticate disabled");
                    None
                }
            };

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
            // ── Social intelligence: personality + storage (CLI) ──────
            let personality =
                std::sync::Arc::new(temm1e_anima::personality::PersonalityConfig::load(
                    &dirs::home_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                        .join(".temm1e"),
                ));
            let social_storage: Option<std::sync::Arc<temm1e_anima::SocialStorage>> = if config
                .social
                .enabled
            {
                let social_db_url = format!(
                    "sqlite:{}/social.db?mode=rwc",
                    dirs::home_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                        .join(".temm1e")
                        .display()
                );
                match temm1e_anima::SocialStorage::new(&social_db_url).await {
                    Ok(s) => {
                        tracing::info!("Social intelligence initialized (CLI)");
                        Some(std::sync::Arc::new(s))
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Social intelligence disabled (CLI): DB init failed");
                        None
                    }
                }
            } else {
                None
            };
            // Pre-capture social config for use in inner closures where `config` may be shadowed
            let social_config_captured = config.social.clone();

            // ── Skills: load registry (CLI) ─────
            let skill_registry = {
                let workspace =
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
                let mut reg = temm1e_skills::SkillRegistry::new(workspace);
                if let Err(e) = reg.load_skills().await {
                    tracing::warn!(error = %e, "Failed to load skills (CLI)");
                }
                let count = reg.list_skills().len();
                if count > 0 {
                    tracing::info!(count, "Skills loaded (CLI)");
                }
                std::sync::Arc::new(tokio::sync::RwLock::new(reg))
            };

            #[cfg(feature = "browser")]
            let (mut tools_template, cli_browser_ref) = temm1e_tools::create_tools_with_browser(
                &config.tools,
                Some(censored_cli),
                Some(pending_messages.clone()),
                Some(memory.clone()),
                Some(Arc::new(setup_tokens.clone()) as Arc<dyn temm1e_core::SetupLinkGenerator>),
                Some(usage_store.clone()),
                Some(shared_mode.clone()),
                vault.clone(),
                Some(skill_registry.clone()),
            );
            #[cfg(not(feature = "browser"))]
            let mut tools_template = temm1e_tools::create_tools(
                &config.tools,
                Some(censored_cli),
                Some(pending_messages.clone()),
                Some(memory.clone()),
                Some(Arc::new(setup_tokens.clone()) as Arc<dyn temm1e_core::SetupLinkGenerator>),
                Some(usage_store.clone()),
                Some(shared_mode.clone()),
                vault.clone(),
                Some(skill_registry.clone()),
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

            // ── TemDOS: Load core registry (CLI) ──────────────
            let cli_core_registry = {
                let mut registry = temm1e_cores::CoreRegistry::new();
                let ws_path = dirs::home_dir()
                    .map(|h| h.join(".temm1e"))
                    .unwrap_or_default();
                registry
                    .load(Some(ws_path.as_path()))
                    .await
                    .unwrap_or_else(|e| {
                        tracing::warn!(error = %e, "Failed to load TemDOS cores (CLI)");
                    });
                if !registry.is_empty() {
                    tracing::info!(count = registry.len(), "TemDOS cores loaded (CLI)");
                }
                Arc::new(tokio::sync::RwLock::new(registry))
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
            let cli_perp_instance: Arc<
                tokio::sync::RwLock<Option<Arc<temm1e_perpetuum::Perpetuum>>>,
            > = Arc::new(tokio::sync::RwLock::new(None));
            let cli_perp_temporal: Arc<tokio::sync::RwLock<String>> =
                Arc::new(tokio::sync::RwLock::new(String::new()));

            tracing::info!(
                has_credentials = credentials.is_some(),
                "CLI Chat: checking credentials for agent init"
            );
            if let Some((pname, key, model)) = credentials {
                // Filter out placeholder/invalid keys at startup. Use lenient
                // mode for custom-endpoint providers so short LM Studio / Ollama
                // keys pass — otherwise this check would wrongly reject keys
                // that load_saved_credentials already approved via lenient filter.
                let has_custom_endpoint = load_credentials_file()
                    .and_then(|c| {
                        c.providers
                            .iter()
                            .find(|p| p.name == pname)
                            .and_then(|p| p.base_url.clone())
                    })
                    .is_some();
                let is_placeholder_start = if has_custom_endpoint {
                    is_placeholder_key_lenient(&key)
                } else {
                    is_placeholder_key(&key)
                };
                if !is_placeholder_start {
                    let (all_keys, saved_base_url) = load_active_provider_keys()
                        .map(|(_, keys, _, burl)| {
                            let has_custom = burl.is_some();
                            let valid: Vec<String> = keys
                                .into_iter()
                                .filter(|k| {
                                    if has_custom {
                                        !is_placeholder_key_lenient(k)
                                    } else {
                                        !is_placeholder_key(k)
                                    }
                                })
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
                            // TemDOS: register invoke_core tool for CLI chat
                            if !cli_core_registry.read().await.is_empty() {
                                // Custom-model aware pricing lookup.
                                let model_pricing =
                                    temm1e_agent::budget::get_pricing_with_custom(&pname, &model);
                                let invoke_core = temm1e_cores::InvokeCoreTool::new(
                                    cli_core_registry.clone(),
                                    provider.clone(),
                                    tools_template.clone(),
                                    Arc::new(temm1e_agent::budget::BudgetTracker::new(max_spend)),
                                    model_pricing,
                                    model.clone(),
                                    max_ctx,
                                    memory.clone(), // v4.6.0: core stats persistence
                                );
                                tools_template.push(Arc::new(invoke_core));
                                tracing::info!("TemDOS invoke_core tool registered (CLI)");
                            }

                            // ── JIT spawn_swarm tool registration (CLI chat path) ──
                            // Register the tool into the CLI agent's toolset with a
                            // deferred SwarmHandle. Hive is initialized below and the
                            // context is filled afterward. The tool snapshot captured
                            // at handle-fill time is the set BEFORE spawn_swarm is
                            // pushed, so workers physically cannot see it (recursion
                            // filter is belt-and-suspenders).
                            let cli_swarm_snapshot = tools_template.clone();
                            let cli_swarm_handle: Option<temm1e_agent::spawn_swarm::SwarmHandle> =
                                if hive_enabled_early {
                                    let h =
                                        temm1e_agent::spawn_swarm::SpawnSwarmTool::fresh_handle();
                                    tools_template.push(Arc::new(
                                        temm1e_agent::spawn_swarm::SpawnSwarmTool::new(h.clone()),
                                    ));
                                    tracing::info!(
                                        "JIT spawn_swarm tool registered (CLI, context deferred)"
                                    );
                                    Some(h)
                                } else {
                                    None
                                };

                            // ── Hive pack initialization for CLI chat ──
                            // Required for both dispatch-time HiveRoute and JIT
                            // spawn_swarm. Fetches the [hive] section from the active
                            // config; opens the same SQLite db as the start command.
                            let cli_hive_instance: Option<Arc<temm1e_hive::Hive>> =
                                if hive_enabled_early {
                                    let hive_config: temm1e_hive::HiveConfig = {
                                        #[derive(serde::Deserialize, Default)]
                                        struct HW {
                                            #[serde(default)]
                                            hive: temm1e_hive::HiveConfig,
                                        }
                                        config_path
                                            .and_then(|p| std::fs::read_to_string(p).ok())
                                            .or_else(|| {
                                                dirs::home_dir().and_then(|h| {
                                                    std::fs::read_to_string(
                                                        h.join(".temm1e/config.toml"),
                                                    )
                                                    .ok()
                                                })
                                            })
                                            .or_else(|| std::fs::read_to_string("temm1e.toml").ok())
                                            .and_then(|c| toml::from_str::<HW>(&c).ok())
                                            .map(|w| w.hive)
                                            .unwrap_or_default()
                                    };
                                    let hive_db = dirs::home_dir()
                                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                                        .join(".temm1e/hive.db");
                                    let hive_url = format!("sqlite:{}?mode=rwc", hive_db.display());
                                    match temm1e_hive::Hive::new(&hive_config, &hive_url).await {
                                        Ok(h) => {
                                            tracing::info!(
                                                max_workers = hive_config.max_workers,
                                                threshold = hive_config.swarm_threshold_speedup,
                                                "Many Tems initialized (CLI Swarm Intelligence)"
                                            );
                                            Some(Arc::new(h))
                                        }
                                        Err(e) => {
                                            tracing::warn!(error = %e, "CLI Hive init failed — JIT swarm disabled");
                                            None
                                        }
                                    }
                                } else {
                                    None
                                };

                            // ── Eigen-Tune: load + instantiate engine for CLI chat ──
                            let cli_eigentune_cfg = load_eigentune_config_from_path(config_path);
                            let cli_eigen_tune_engine: Option<
                                Arc<temm1e_distill::EigenTuneEngine>,
                            > = if cli_eigentune_cfg.enabled {
                                match open_eigentune_engine(&cli_eigentune_cfg).await {
                                    Ok(engine) => {
                                        tracing::info!(
                                            enable_local_routing =
                                                cli_eigentune_cfg.enable_local_routing,
                                            "Eigen-Tune: engine initialized (CLI chat)"
                                        );
                                        Some(Arc::new(engine))
                                    }
                                    Err(e) => {
                                        tracing::error!(error = %e, "Eigen-Tune: failed to initialize (CLI chat), continuing without");
                                        None
                                    }
                                }
                            } else {
                                None
                            };

                            let system_prompt = Some(build_system_prompt(&personality));
                            let consciousness_provider = provider.clone();
                            let mut rt = temm1e_agent::AgentRuntime::with_limits(
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
                            .with_shared_memory_strategy(shared_memory_strategy.clone())
                            .with_personality(personality.clone())
                            .with_social(
                                social_storage.clone(),
                                Some(social_config_captured.clone()),
                            );
                            if let Some(et) = cli_eigen_tune_engine.clone() {
                                rt = rt.with_eigen_tune(et, cli_eigentune_cfg.enable_local_routing);
                            }
                            // Tem Conscious: enable consciousness for CLI chat
                            tracing::info!(
                                consciousness_enabled = config.consciousness.enabled,
                                "Checking consciousness config"
                            );
                            if config.consciousness.enabled {
                                let consciousness_cfg =
                                    temm1e_agent::consciousness::ConsciousnessConfig {
                                        enabled: true,
                                        confidence_threshold: config
                                            .consciousness
                                            .confidence_threshold,
                                        max_interventions_per_session: config
                                            .consciousness
                                            .max_interventions_per_session,
                                        observation_mode: config
                                            .consciousness
                                            .observation_mode
                                            .clone(),
                                    };
                                rt = rt.with_consciousness(
                                    temm1e_agent::consciousness_engine::ConsciousnessEngine::new(
                                        consciousness_cfg,
                                        consciousness_provider.clone(),
                                        model.clone(),
                                    ),
                                );
                            }
                            // ── Perpetuum: init for CLI chat ──────────
                            if config.perpetuum.enabled {
                                let perpetuum_db = dirs::home_dir()
                                    .unwrap_or_else(|| std::path::PathBuf::from("."))
                                    .join(".temm1e/perpetuum.db");
                                let db_url = format!("sqlite:{}?mode=rwc", perpetuum_db.display());

                                let perp_config = temm1e_perpetuum::PerpetualConfig {
                                    enabled: true,
                                    timezone: config.perpetuum.timezone.clone(),
                                    max_concerns: config.perpetuum.max_concerns,
                                    conscience: temm1e_perpetuum::ConscienceConfig {
                                        idle_threshold_secs: config
                                            .perpetuum
                                            .conscience_idle_threshold_secs
                                            .unwrap_or(900),
                                        dream_threshold_secs: config
                                            .perpetuum
                                            .conscience_dream_threshold_secs
                                            .unwrap_or(3600),
                                    },
                                    cognitive: temm1e_perpetuum::CognitiveConfig {
                                        review_every_n_checks: config
                                            .perpetuum
                                            .review_every_n_checks,
                                        interpret_changes: true,
                                    },
                                    volition: temm1e_perpetuum::VolitionConfig {
                                        enabled: config.perpetuum.volition_enabled,
                                        interval_secs: config.perpetuum.volition_interval_secs,
                                        max_actions_per_cycle: config
                                            .perpetuum
                                            .volition_max_actions,
                                        event_triggered: true,
                                    },
                                };

                                // Register CLI channel for Perpetuum notifications
                                let mut cli_ch_map: HashMap<String, Arc<dyn temm1e_core::Channel>> =
                                    HashMap::new();
                                cli_ch_map.insert("cli".to_string(), cli_arc.clone());
                                let cli_channel_map = Arc::new(cli_ch_map);

                                match temm1e_perpetuum::Perpetuum::new(
                                    perp_config,
                                    consciousness_provider.clone(),
                                    model.clone(),
                                    cli_channel_map,
                                    &db_url,
                                )
                                .await
                                {
                                    Ok(p) => {
                                        let p = Arc::new(p);
                                        let perp_tools = p.tools();
                                        tracing::info!(
                                            count = perp_tools.len(),
                                            "Perpetuum tools loaded"
                                        );
                                        tools_template.extend(perp_tools);
                                        // Re-create the agent runtime with updated tools
                                        let mut rt2 = temm1e_agent::AgentRuntime::with_limits(
                                            consciousness_provider.clone(),
                                            memory.clone(),
                                            tools_template.clone(),
                                            model.clone(),
                                            Some(build_system_prompt(&personality)),
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
                                        .with_shared_memory_strategy(shared_memory_strategy.clone())
                                        .with_personality(personality.clone())
                                        .with_social(
                                            social_storage.clone(),
                                            Some(social_config_captured.clone()),
                                        )
                                        .with_perpetuum_temporal(cli_perp_temporal.clone());
                                        if config.consciousness.enabled {
                                            rt2 = rt2.with_consciousness(
                                                temm1e_agent::consciousness_engine::ConsciousnessEngine::new(
                                                    temm1e_agent::consciousness::ConsciousnessConfig {
                                                        enabled: true,
                                                        confidence_threshold: config.consciousness.confidence_threshold,
                                                        max_interventions_per_session: config.consciousness.max_interventions_per_session,
                                                        observation_mode: config.consciousness.observation_mode.clone(),
                                                    },
                                                    consciousness_provider.clone(),
                                                    model.clone(),
                                                ),
                                            );
                                        }
                                        // Re-inject eigen-tune into the rebuilt runtime
                                        if let Some(et) = cli_eigen_tune_engine.clone() {
                                            rt2 = rt2.with_eigen_tune(
                                                et,
                                                cli_eigentune_cfg.enable_local_routing,
                                            );
                                        }
                                        rt = rt2;
                                        p.start();
                                        // Populate temporal context immediately so the first
                                        // message has time awareness
                                        let initial_temporal =
                                            p.temporal_injection("standard").await;
                                        *cli_perp_temporal.write().await = initial_temporal;
                                        *cli_perp_instance.write().await = Some(p.clone());
                                        tracing::info!("Perpetuum runtime started (CLI chat)");
                                    }
                                    Err(e) => {
                                        tracing::error!(error = %e, "Failed to init Perpetuum for CLI chat");
                                    }
                                }
                            }
                            rt = rt.with_perpetuum_temporal(cli_perp_temporal.clone());

                            // ── Fill JIT spawn_swarm handle (CLI chat path) ──
                            // Context uses the tool snapshot captured before
                            // spawn_swarm was pushed — workers never see it.
                            if let (Some(hive), Some(handle)) =
                                (cli_hive_instance.as_ref(), cli_swarm_handle.as_ref())
                            {
                                let ctx = temm1e_agent::spawn_swarm::SpawnSwarmContext {
                                    hive: Arc::clone(hive),
                                    provider: rt.provider_arc(),
                                    memory: memory.clone(),
                                    tools_template: cli_swarm_snapshot.clone(),
                                    model: rt.model().to_string(),
                                    parent_budget: Arc::new(
                                        temm1e_agent::budget::BudgetTracker::new(max_spend),
                                    ),
                                    cancel: tokio_util::sync::CancellationToken::new(),
                                    workspace_path: std::env::current_dir()
                                        .unwrap_or_else(|_| std::path::PathBuf::from(".")),
                                    witness_attachments: witness_attachments.clone(),
                                };
                                *handle.write().await = Some(ctx);
                                tracing::info!("JIT spawn_swarm context wired (CLI)");
                            }

                            // Wire Witness attachments (CLI chat, no-op if disabled)
                            rt = rt.with_witness_attachments(witness_attachments.as_ref());

                            agent_opt = Some(rt);
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
                                let system_prompt = Some(build_system_prompt(&personality));
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
                                    .with_shared_memory_strategy(shared_memory_strategy.clone())
                                    .with_personality(personality.clone())
                                    .with_social(
                                        social_storage.clone(),
                                        Some(social_config_captured.clone()),
                                    )
                                    .with_witness_attachments(witness_attachments.as_ref()),
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
                let link = format!("https://temm1e-labs.github.io/temm1e/setup#{}", otk_hex);
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
                // /eigentune — Eigen-Tune slash dispatch
                if cmd_lower.starts_with("/eigentune") {
                    let arg = msg_text.trim()["/eigentune".len()..].trim().to_string();
                    let reply_text = handle_eigentune_slash(&arg).await;
                    println!("\n{}\n", reply_text);
                    eprint!("temm1e> ");
                    continue;
                }

                // /addkey — secure OTK flow
                if cmd_lower == "/addkey" {
                    let otk = setup_tokens.generate(&msg.chat_id).await;
                    let otk_hex = hex::encode(otk);
                    let link = format!("https://temm1e-labs.github.io/temm1e/setup#{}", otk_hex);
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

                // /addmodel — register a custom model for the active provider
                if cmd_lower.starts_with("/addmodel") {
                    let args = msg_text.trim()["/addmodel".len()..].trim();
                    println!("\n{}\n", handle_addmodel_command(args));
                    eprint!("temm1e> ");
                    continue;
                }

                // /listmodels — show hardcoded + custom models
                if cmd_lower == "/listmodels" {
                    println!("\n{}\n", handle_listmodels_command());
                    eprint!("temm1e> ");
                    continue;
                }

                // /removemodel — drop a custom model for the active provider
                if cmd_lower.starts_with("/removemodel") {
                    let args = msg_text.trim()["/removemodel".len()..].trim();
                    println!("\n{}\n", handle_removemodel_command(args));
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
                         /addmodel <name> context:<int> output:<int> [input_price:<float>] [output_price:<float>] — Register a custom model\n\
                         /listmodels — Show hardcoded + custom models grouped by provider\n\
                         /removemodel <name> — Remove a custom model from the active provider\n\
                         /usage — Show token usage and cost summary\n\
                         /memory — Show current memory strategy\n\
                         /memory lambda — Switch to λ-Memory (decay + persistence)\n\
                         /memory echo — Switch to Echo Memory (context window only)\n\
                         /cambium — Cambium status (gap-driven self-grow)\n\
                         /cambium on — Enable cambium growth\n\
                         /cambium off — Disable cambium growth\n\
                         /eigentune — Eigen-Tune status (self-tuning distillation)\n\
                         /eigentune setup — Show prerequisites + setup guide\n\
                         /eigentune model — Show base model + recommendations\n\
                         /eigentune tick — Manually advance state machine\n\
                         /eigentune demote <tier> — Force-revert a graduated tier (kill switch)\n\
                         /mcp — List connected MCP servers and tools\n\
                         /mcp add <name> <command-or-url> — Connect a new MCP server\n\
                         /mcp remove <name> — Disconnect an MCP server\n\
                         /mcp restart <name> — Restart an MCP server\n\
                         /browser — Browser status, sessions, and lifecycle\n\
                         /browser close — Save sessions and close browser\n\
                         /browser sessions — List saved web sessions\n\
                         /browser forget <service> — Delete a saved session\n\
                         /vigil — Bug reporter status and configuration\n\
                         /addkey github — Add GitHub PAT for auto vigil\n\
                         /quit — Exit the CLI chat\n\n\
                         Just type a message to chat with the AI agent.\n",
                        env!("CARGO_PKG_VERSION"),
                        env!("GIT_HASH"),
                        env!("BUILD_DATE"),
                    );
                    eprint!("temm1e> ");
                    continue;
                }

                // /vigil — self-diagnosis vigil
                if cmd_lower.starts_with("/vigil") {
                    let subcmd = cmd_lower.strip_prefix("/vigil").unwrap_or("").trim();
                    match subcmd {
                        "disable" => {
                            let config_path = dirs::home_dir()
                                .unwrap_or_default()
                                .join(".temm1e")
                                .join("vigil.toml");
                            std::fs::write(
                                &config_path,
                                "enabled = false\nconsent_given = false\nauto_report = false\n",
                            )
                            .ok();
                            println!("Vigil disabled.");
                        }
                        "auto" => {
                            let config_path = dirs::home_dir()
                                .unwrap_or_default()
                                .join(".temm1e")
                                .join("vigil.toml");
                            std::fs::write(
                                &config_path,
                                "enabled = true\nconsent_given = true\nauto_report = true\n",
                            )
                            .ok();
                            println!("Vigil auto-reporting enabled.");
                        }
                        "status" => {
                            let has_github = load_credentials_file()
                                .is_some_and(|c| c.providers.iter().any(|p| p.name == "github"));
                            let consent_path = dirs::home_dir()
                                .unwrap_or_default()
                                .join(".temm1e")
                                .join("vigil.toml");
                            let consent = std::fs::read_to_string(&consent_path)
                                .unwrap_or_default()
                                .contains("consent_given = true");
                            println!(
                                "\nTem Vigil Status:\n\
                                 - GitHub PAT: {}\n\
                                 - Consent: {}\n\
                                 - Log file: {}\n",
                                if has_github {
                                    "configured"
                                } else {
                                    "not set — run /addkey github"
                                },
                                if consent {
                                    "granted"
                                } else {
                                    "not yet — run /vigil auto"
                                },
                                temm1e_observable::file_logger::current_log_path().display(),
                            );
                        }
                        _ => {
                            println!(
                                "\nTem Vigil Commands:\n\
                                 /vigil status — show current configuration\n\
                                 /vigil auto — enable auto-reporting\n\
                                 /vigil disable — disable all vigil\n\
                                 /addkey github — add GitHub PAT for issue creation\n"
                            );
                        }
                    }
                    eprint!("temm1e> ");
                    continue;
                }

                // /cambium — gap-driven self-grow toggle
                if cmd_lower == "/cambium" || cmd_lower.starts_with("/cambium ") {
                    let original_args = msg_text
                        .trim()
                        .strip_prefix("/cambium")
                        .or_else(|| msg_text.trim().strip_prefix("/CAMBIUM"))
                        .unwrap_or("")
                        .trim();
                    let subcmd = cmd_lower.strip_prefix("/cambium").unwrap_or("").trim();
                    let cambium_path = dirs::home_dir()
                        .unwrap_or_default()
                        .join(".temm1e")
                        .join("cambium.toml");
                    let current_enabled = std::fs::read_to_string(&cambium_path)
                        .ok()
                        .and_then(|s| {
                            s.lines()
                                .find(|l| l.trim().starts_with("enabled"))
                                .map(|l| !l.contains("false"))
                        })
                        .unwrap_or(true); // default enabled

                    // /cambium grow <task> — synchronous in CLI mode (we wait for the result)
                    if subcmd.starts_with("grow") {
                        let task = original_args
                            .strip_prefix("grow")
                            .or_else(|| original_args.strip_prefix("GROW"))
                            .unwrap_or("")
                            .trim()
                            .to_string();
                        if task.is_empty() {
                            println!(
                                "\nUsage: /cambium grow <description>\n\nExample: /cambium grow add a function that converts celsius to fahrenheit with tests\n"
                            );
                            eprint!("temm1e> ");
                            continue;
                        }
                        if !current_enabled {
                            println!(
                                "\nCambium is DISABLED. Run /cambium on to enable, then try again.\n"
                            );
                            eprint!("temm1e> ");
                            continue;
                        }
                        let Some(agent) = agent_opt.as_ref() else {
                            println!(
                                "\nCambium needs an active provider. Set up an API key with /addkey first.\n"
                            );
                            eprint!("temm1e> ");
                            continue;
                        };
                        let provider = agent.provider_arc();
                        let model = agent.model().to_string();

                        println!("\nCambium session started.");
                        println!("Task: {task}");
                        println!("Model: {model}");
                        println!("(this runs in an isolated tempdir; production code is never touched)\n");

                        let cfg = temm1e_cambium::session::CambiumSessionConfig::new(
                            task.clone(),
                            model.clone(),
                        );
                        match temm1e_cambium::session::run_minimal_session(provider, cfg, None)
                            .await
                        {
                            Ok(report) => {
                                println!("{}", format_cambium_report(&report));
                            }
                            Err(e) => {
                                println!("\nCambium session failed to start: {e}\n");
                            }
                        }
                        eprint!("temm1e> ");
                        continue;
                    }

                    match subcmd {
                        "on" | "enable" | "enabled" => {
                            if let Some(parent) = cambium_path.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            std::fs::write(
                                &cambium_path,
                                "# Cambium runtime config — toggled via /cambium\nenabled = true\n",
                            )
                            .ok();
                            println!(
                                "\nCambium ENABLED. Tem may grow new capabilities at the\n\
                                 cambium layer (heartwood stays immutable).\n"
                            );
                        }
                        "off" | "disable" | "disabled" => {
                            if let Some(parent) = cambium_path.parent() {
                                let _ = std::fs::create_dir_all(parent);
                            }
                            std::fs::write(
                                &cambium_path,
                                "# Cambium runtime config — toggled via /cambium\nenabled = false\n",
                            )
                            .ok();
                            println!(
                                "\nCambium DISABLED. Tem will not grow new capabilities\n\
                                 until you run /cambium on.\n"
                            );
                        }
                        "" | "status" => {
                            println!(
                                "\nCambium status: {}\n\n\
                                 Cambium is the layer where Tem grows new capabilities at\n\
                                 the edge while the heartwood (immutable kernel: vault, core\n\
                                 traits, security) stays stable. Named after the biological\n\
                                 cambium — the growth tissue under tree bark where rings are\n\
                                 added each year.\n\n\
                                 Commands:\n\
                                 /cambium on    — enable cambium growth\n\
                                 /cambium off   — disable cambium growth\n\
                                 /cambium status — show current state (this view)\n\n\
                                 Default: enabled. Persisted to ~/.temm1e/cambium.toml\n",
                                if current_enabled {
                                    "ENABLED"
                                } else {
                                    "DISABLED"
                                }
                            );
                        }
                        other => {
                            println!(
                                "\nUnknown subcommand: {other}\n\
                                 Try: /cambium on | /cambium off | /cambium status\n"
                            );
                        }
                    }
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
                                                    Some(build_system_prompt(&personality)),
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
                                                )
                                                .with_personality(personality.clone())
                                                .with_social(
                                                    social_storage.clone(),
                                                    Some(social_config_captured.clone()),
                                                )
                                                .with_witness_attachments(
                                                    witness_attachments.as_ref(),
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
                                            Some(build_system_prompt(&personality)),
                                            max_turns,
                                            max_ctx,
                                            max_rounds,
                                            max_task_duration,
                                            max_spend,
                                        )
                                        .with_v2_optimizations(v2_opt)
                                        .with_parallel_phases(pp_opt)
                                        .with_shared_mode(shared_mode.clone())
                                        .with_shared_memory_strategy(shared_memory_strategy.clone())
                                        .with_personality(personality.clone())
                                        .with_social(
                                            social_storage.clone(),
                                            Some(social_config_captured.clone()),
                                        )
                                        .with_witness_attachments(witness_attachments.as_ref()),
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
                                            Some(build_system_prompt(&personality)),
                                            max_turns,
                                            max_ctx,
                                            max_rounds,
                                            max_task_duration,
                                            max_spend,
                                        )
                                        .with_v2_optimizations(v2_opt)
                                        .with_parallel_phases(pp_opt)
                                        .with_shared_mode(shared_mode.clone())
                                        .with_shared_memory_strategy(shared_memory_strategy.clone())
                                        .with_personality(personality.clone())
                                        .with_social(
                                            social_storage.clone(),
                                            Some(social_config_captured.clone()),
                                        )
                                        .with_witness_attachments(witness_attachments.as_ref()),
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

                // /browser — browser lifecycle management (V2)
                #[cfg(feature = "browser")]
                if cmd_lower == "/browser" || cmd_lower.starts_with("/browser ") {
                    let browser_args = if cmd_lower == "/browser" {
                        ""
                    } else {
                        msg_text
                            .trim()
                            .strip_prefix("/browser")
                            .unwrap_or("")
                            .trim()
                    };
                    let browser_args_lower = browser_args.to_lowercase();

                    if browser_args_lower.is_empty() || browser_args_lower == "status" {
                        match &cli_browser_ref {
                            Some(bt) => {
                                if bt.is_running() {
                                    let domains = bt.get_active_domains();
                                    let sessions_str = if domains.is_empty() {
                                        "none".to_string()
                                    } else {
                                        let mut sorted: Vec<_> = domains.into_iter().collect();
                                        sorted.sort();
                                        sorted.join(", ")
                                    };
                                    let uptime =
                                        bt.uptime().unwrap_or_else(|| "unknown".to_string());
                                    println!(
                                        "\nBrowser: Active\nSessions: {}\nUptime: {}\n",
                                        sessions_str, uptime
                                    );
                                } else {
                                    println!("\nBrowser: Inactive. Will start on next web task.\n");
                                }
                            }
                            None => println!("\nBrowser: Not configured.\n"),
                        }
                    } else if browser_args_lower == "close" {
                        match &cli_browser_ref {
                            Some(bt) => {
                                let (msg, saved) = bt.close_with_capture().await;
                                if !saved.is_empty() {
                                    println!("\nSessions saved: {}", saved.join(", "));
                                }
                                println!("{}\n", msg);
                            }
                            None => println!("\nBrowser not configured.\n"),
                        }
                    } else if browser_args_lower == "sessions" {
                        match &cli_browser_ref {
                            Some(bt) => {
                                let sessions = bt.list_saved_sessions().await;
                                if sessions.is_empty() {
                                    println!("\nNo saved sessions.\n");
                                } else {
                                    println!("\nSaved sessions:");
                                    for (service, captured_at) in &sessions {
                                        let age = format_capture_age(captured_at);
                                        println!("  - {} (captured {})", service, age);
                                    }
                                    println!();
                                }
                            }
                            None => println!("\nBrowser not configured.\n"),
                        }
                    } else if browser_args_lower.starts_with("forget ") {
                        let service = browser_args["forget ".len()..].trim();
                        if service.is_empty() {
                            println!("\nUsage: /browser forget <service>\n");
                        } else {
                            match &cli_browser_ref {
                                Some(bt) => match bt.forget_session(service).await {
                                    Ok(()) => println!("\nSession for \'{}\' deleted.\n", service),
                                    Err(e) => println!("\nFailed: {}\n", e),
                                },
                                None => println!("\nBrowser not configured.\n"),
                            }
                        }
                    } else {
                        println!("\nUsage: /browser [status|close|sessions|forget <service>]\n");
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

                // /login <service> <url> — interactive OTK browser login session
                #[cfg(feature = "browser")]
                if cmd_lower.starts_with("/login ") {
                    let login_args = msg_text.trim()["/login".len()..].trim();
                    let parts: Vec<&str> = login_args.splitn(2, ' ').collect();
                    if parts.len() < 2 || parts[1].trim().is_empty() {
                        println!(
                            "\nUsage: /login <service> <url>\n\n\
                             Start an interactive browser login session.\n\
                             You'll see numbered interactive elements — type a number to click,\n\
                             type text to fill a focused field, or type 'done' to finish.\n\n\
                             Example: /login github https://github.com/login\n"
                        );
                    } else {
                        let service = parts[0];
                        let url = parts[1].trim();
                        match &vault {
                            None => {
                                println!(
                                    "\nVault not available — cannot store session credentials.\n"
                                );
                            }
                            Some(vault_ref) => {
                                println!(
                                    "\nStarting interactive login for '{}' at {}\n\
                                     Launching browser...\n",
                                    service, url
                                );
                                // Launch browser and create session
                                match temm1e_tools::browser_session_login(
                                    service,
                                    url,
                                    vault_ref.as_ref(),
                                )
                                .await
                                {
                                    Ok(summary) => {
                                        println!("\n{}\n", summary);
                                    }
                                    Err(e) => {
                                        println!("\nLogin session failed: {}\n", e);
                                    }
                                }
                            }
                        }
                    }
                    eprint!("temm1e> ");
                    continue;
                }

                // enc:v1: — encrypted blob from OTK flow
                if msg_text.trim().starts_with("enc:v1:") {
                    let blob_b64 = &msg_text.trim()["enc:v1:".len()..];
                    match decrypt_otk_blob(blob_b64, &setup_tokens, &msg.chat_id).await {
                        Ok(api_key_text) => {
                            if let Some(cred) = detect_api_key(&api_key_text) {
                                // Honor user-specified `model:` from proxy command
                                // (if OTK blob contained a proxy-style payload);
                                // fall back to provider default otherwise.
                                let model = cred
                                    .model
                                    .clone()
                                    .unwrap_or_else(|| default_model(cred.provider).to_string());
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
                                        let system_prompt = Some(build_system_prompt(&personality));
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
                                            )
                                            .with_personality(personality.clone())
                                            .with_social(
                                                social_storage.clone(),
                                                Some(social_config_captured.clone()),
                                            )
                                            .with_witness_attachments(witness_attachments.as_ref()),
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

                // Detect raw API key paste (CLI chat onboarding path)
                if let Some(cred) = detect_api_key(msg_text) {
                    // Honor user-specified `model:` from proxy command;
                    // fall back to provider default otherwise. This is the
                    // CLI-chat onboarding path — critical for LM Studio /
                    // Ollama / vLLM users who type `proxy openai <url> <key>
                    // model:<local-model-name>` at the temm1e> prompt.
                    let model = cred
                        .model
                        .clone()
                        .unwrap_or_else(|| default_model(cred.provider).to_string());
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
                            let system_prompt = Some(build_system_prompt(&personality));
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
                                .with_shared_memory_strategy(shared_memory_strategy.clone())
                                .with_personality(personality.clone())
                                .with_social(
                                    social_storage.clone(),
                                    Some(social_config_captured.clone()),
                                )
                                .with_witness_attachments(witness_attachments.as_ref()),
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
                // Perpetuum: refresh temporal context before each turn
                if let Some(ref perp) = *cli_perp_instance.read().await {
                    perp.record_user_interaction().await;
                    let temporal = perp.temporal_injection("standard").await;
                    *cli_perp_temporal.write().await = temporal;
                }
                if let Some(ref agent) = agent_opt {
                    let mut session = temm1e_core::types::session::SessionContext {
                        session_id: "cli-cli".to_string(),
                        user_id: msg.user_id.clone(),
                        channel: msg.channel.clone(),
                        chat_id: msg.chat_id.clone(),
                        role: temm1e_core::types::rbac::Role::Admin,
                        history: history.clone(),
                        workspace_path: workspace.clone(),
                        read_tracker: std::sync::Arc::new(tokio::sync::RwLock::new(
                            std::collections::HashSet::new(),
                        )),
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
                                .with_parallel_phases(pp_opt)
                                .with_witness_attachments(witness_attachments.as_ref());
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
                    let link = format!("https://temm1e-labs.github.io/temm1e/setup#{}", otk_hex);
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
        Commands::Skill { command } => {
            let workspace =
                std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."));
            let mut registry = temm1e_skills::SkillRegistry::new(workspace);
            if let Err(e) = registry.load_skills().await {
                eprintln!("Failed to load skills: {}", e);
            }

            match command {
                SkillCommands::List => {
                    let skills = registry.list_skills();
                    if skills.is_empty() {
                        println!("No skills installed.");
                        println!(
                            "\nPlace .md skill files in ~/.temm1e/skills/ or <workspace>/skills/"
                        );
                    } else {
                        println!("{} skill(s) installed:\n", skills.len());
                        for s in skills {
                            println!("  {} (v{}) — {}", s.name, s.version, s.description);
                            println!("    capabilities: {}", s.capabilities.join(", "));
                            println!("    source: {}", s.source_path.display());
                        }
                    }
                }
                SkillCommands::Info { name } => match registry.get_skill(&name) {
                    Some(skill) => {
                        println!("Skill: {} (v{})", skill.name, skill.version);
                        println!("Description: {}", skill.description);
                        println!("Capabilities: {}", skill.capabilities.join(", "));
                        println!("Source: {}", skill.source_path.display());
                        println!("\n--- Instructions ---\n{}", skill.instructions);
                    }
                    None => {
                        eprintln!("Skill '{}' not found.", name);
                        let skills = registry.list_skills();
                        if !skills.is_empty() {
                            let names: Vec<&str> = skills.iter().map(|s| s.name.as_str()).collect();
                            eprintln!("Available: {}", names.join(", "));
                        }
                        std::process::exit(1);
                    }
                },
                SkillCommands::Install { path } => {
                    let src = std::path::Path::new(&path);
                    if !src.exists() {
                        eprintln!("File not found: {}", path);
                        std::process::exit(1);
                    }
                    let dest_dir = dirs::home_dir()
                        .unwrap_or_else(|| std::path::PathBuf::from("."))
                        .join(".temm1e")
                        .join("skills");
                    if let Err(e) = std::fs::create_dir_all(&dest_dir) {
                        eprintln!("Failed to create skills directory: {}", e);
                        std::process::exit(1);
                    }
                    let filename = src
                        .file_name()
                        .unwrap_or_else(|| std::ffi::OsStr::new("skill.md"));
                    let dest = dest_dir.join(filename);
                    match std::fs::copy(src, &dest) {
                        Ok(_) => println!("Installed skill to {}", dest.display()),
                        Err(e) => {
                            eprintln!("Failed to install skill: {}", e);
                            std::process::exit(1);
                        }
                    }
                }
            }
        }
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

            // 1. Check if we're in a git repo — if not, do binary self-update
            let git_check = std::process::Command::new("git")
                .args(["rev-parse", "--is-inside-work-tree"])
                .output();
            let in_git_repo = git_check.is_ok_and(|o| o.status.success());

            if !in_git_repo {
                // Binary self-update: download latest release from GitHub
                println!("Not in a git repo — updating via GitHub Releases...\n");

                // Fetch latest release tag
                let client = reqwest::blocking::Client::builder()
                    .user_agent("temm1e-updater")
                    .build()
                    .unwrap_or_else(|_| reqwest::blocking::Client::new());

                let api_url = format!(
                    "https://api.github.com/repos/{}/releases/latest",
                    "temm1e-labs/temm1e"
                );
                let release: serde_json::Value = match client.get(&api_url).send() {
                    Ok(resp) if resp.status().is_success() => match resp.json() {
                        Ok(v) => v,
                        Err(e) => {
                            eprintln!("Error: Failed to parse release info: {}", e);
                            std::process::exit(1);
                        }
                    },
                    Ok(resp) => {
                        eprintln!("Error: GitHub API returned status {}", resp.status());
                        std::process::exit(1);
                    }
                    Err(e) => {
                        eprintln!("Error: Failed to reach GitHub: {}", e);
                        eprintln!("Check your internet connection and try again.");
                        std::process::exit(1);
                    }
                };

                let latest_tag = release["tag_name"]
                    .as_str()
                    .unwrap_or("unknown")
                    .trim_start_matches('v');
                let current = env!("CARGO_PKG_VERSION");

                if latest_tag == current {
                    println!("Already up to date (v{}).", current);
                    return Ok(());
                }

                println!("New version available: v{} → v{}", current, latest_tag);

                // Detect platform
                let os = std::env::consts::OS;
                let arch = std::env::consts::ARCH;
                let target = match (os, arch) {
                    ("macos", "aarch64") => "aarch64-apple-darwin",
                    ("macos", "x86_64") => "x86_64-apple-darwin",
                    ("linux", "x86_64") => "x86_64-unknown-linux-musl",
                    ("linux", "aarch64") => "aarch64-unknown-linux-musl",
                    _ => {
                        eprintln!(
                            "Error: No pre-built binary for {}-{}. Build from source instead.",
                            os, arch
                        );
                        std::process::exit(1);
                    }
                };

                let asset_name = format!("temm1e-{}", target);

                // Find the matching asset URL
                let assets = release["assets"].as_array();
                let download_url = assets
                    .and_then(|arr| {
                        arr.iter().find(|a| {
                            a["name"].as_str().is_some_and(|n| {
                                n.starts_with(&asset_name) && !n.ends_with(".sha256")
                            })
                        })
                    })
                    .and_then(|a| a["browser_download_url"].as_str());

                let url = match download_url {
                    Some(u) => u.to_string(),
                    None => {
                        eprintln!(
                            "Error: No binary found for {} in release v{}",
                            target, latest_tag
                        );
                        std::process::exit(1);
                    }
                };

                // Download binary
                println!("Downloading {}...", asset_name);
                let binary_data = match client.get(&url).send() {
                    Ok(resp) if resp.status().is_success() => match resp.bytes() {
                        Ok(b) => b,
                        Err(e) => {
                            eprintln!("Error: Failed to download binary: {}", e);
                            std::process::exit(1);
                        }
                    },
                    _ => {
                        eprintln!("Error: Failed to download from {}", url);
                        std::process::exit(1);
                    }
                };

                // Find current binary location and replace
                let current_exe = std::env::current_exe().unwrap_or_else(|_| {
                    // Fallback: check common install locations
                    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
                    let local_bin = home.join(".local/bin/temm1e");
                    if local_bin.exists() {
                        local_bin
                    } else {
                        home.join("bin/temm1e")
                    }
                });

                // Atomic replace: write to .tmp, then rename
                let tmp_path = current_exe.with_extension("tmp");
                if let Err(e) = std::fs::write(&tmp_path, &binary_data) {
                    eprintln!("Error: Failed to write temporary binary: {}", e);
                    std::process::exit(1);
                }

                // Make executable on Unix
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    let _ =
                        std::fs::set_permissions(&tmp_path, std::fs::Permissions::from_mode(0o755));
                }

                // Replace current binary
                if let Err(e) = std::fs::rename(&tmp_path, &current_exe) {
                    eprintln!("Error: Failed to replace binary: {}", e);
                    eprintln!(
                        "You may need to run with sudo or manually move {} to {}",
                        tmp_path.display(),
                        current_exe.display()
                    );
                    let _ = std::fs::remove_file(&tmp_path);
                    std::process::exit(1);
                }

                println!("\nUpdate complete! v{} → v{}", current, latest_tag);
                println!("Binary: {}", current_exe.display());
                println!("\nRestart with: temm1e start");
                println!("\nNote: Your data in ~/.temm1e/ is untouched (keys, memory, config).");
                return Ok(());
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
        Commands::Setup => {
            run_setup_wizard().await?;
        }
        Commands::Eigentune { command } => {
            handle_eigentune_command(config_path, command).await?;
        }
        Commands::Search { command } => match command {
            SearchCommands::Install => {
                search_install::run_install().await?;
            }
        },
    }

    Ok(())
}

/// Load the [eigentune] section from a TOML config path (two-pass parse).
/// Returns Default::default() on any error.
fn load_eigentune_config_from_path(
    config_path: Option<&std::path::Path>,
) -> temm1e_distill::config::EigenTuneConfig {
    #[derive(serde::Deserialize, Default)]
    struct EigenRoot {
        #[serde(default)]
        eigentune: temm1e_distill::config::EigenTuneConfig,
    }
    let raw_path: std::path::PathBuf = config_path.map(|p| p.to_path_buf()).unwrap_or_else(|| {
        dirs::home_dir()
            .map(|h| h.join(".temm1e/config.toml"))
            .unwrap_or_else(|| std::path::PathBuf::from("temm1e.toml"))
    });
    let raw = std::fs::read_to_string(&raw_path).unwrap_or_default();
    let expanded = temm1e_core::config::expand_env_vars(&raw);
    toml::from_str::<EigenRoot>(&expanded)
        .map(|r| r.eigentune)
        .unwrap_or_default()
}

/// Open the Eigen-Tune SQLite store at the standard path.
async fn open_eigentune_engine(
    cfg: &temm1e_distill::config::EigenTuneConfig,
) -> anyhow::Result<temm1e_distill::EigenTuneEngine> {
    let db_path = dirs::home_dir()
        .map(|h| h.join(".temm1e").join("eigentune.db"))
        .unwrap_or_else(|| std::path::PathBuf::from("eigentune.db"));
    if let Some(parent) = db_path.parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
    let engine = temm1e_distill::EigenTuneEngine::new(cfg, &db_url)
        .await
        .map_err(|e| anyhow::anyhow!("eigentune store: {e}"))?;
    Ok(engine)
}

/// Handle a `/eigentune ...` slash command from chat (gateway or CLI).
///
/// Opens its own EigenTune store on demand. Returns the reply text.
/// Slash commands are user-initiated and infrequent, so the per-call
/// SQLite connection setup cost (~50ms) is acceptable.
async fn handle_eigentune_slash(arg: &str) -> String {
    // Find the config path the same way the daemon does
    let config_path: std::path::PathBuf = dirs::home_dir()
        .map(|h| h.join(".temm1e/config.toml"))
        .unwrap_or_else(|| std::path::PathBuf::from("temm1e.toml"));
    let cfg = load_eigentune_config_from_path(Some(&config_path));

    if !cfg.enabled {
        return "Eigen-Tune is not enabled. To activate:\n  1. Edit temm1e.toml\n  2. Add [eigentune]\\nenabled = true\n  3. Restart the daemon".to_string();
    }

    let engine = match open_eigentune_engine(&cfg).await {
        Ok(e) => e,
        Err(e) => return format!("Eigen-Tune: failed to open store: {e}"),
    };

    let parts: Vec<&str> = arg.split_whitespace().collect();
    match parts.as_slice() {
        [] | ["status"] => {
            let mut out = engine.format_status().await;
            out.push_str("\n\nMaster switches:\n");
            out.push_str(&format!("  enabled              = {}\n", cfg.enabled));
            out.push_str(&format!(
                "  enable_local_routing = {}\n",
                cfg.enable_local_routing
            ));
            out
        }
        ["setup"] => {
            let prereqs = engine.check_prerequisites().await;
            let mut out = String::from("EIGEN-TUNE SETUP\n\n");
            out.push_str(&format!(
                "Ollama: {}\n",
                if prereqs.ollama_running {
                    "running ✓"
                } else {
                    "not running"
                }
            ));
            out.push_str(&format!(
                "Python: {}\n",
                prereqs.python_version.as_deref().unwrap_or("not found")
            ));
            out.push_str(&format!(
                "Can collect: {}\nCan train:   {}\nCan serve:   {}\n",
                prereqs.can_collect, prereqs.can_train, prereqs.can_serve
            ));
            out
        }
        ["model"] => engine.format_model_status().await,
        ["model", name] => format!(
            "To change the base model, edit [eigentune] base_model = \"{name}\" in temm1e.toml and restart."
        ),
        ["tick"] => {
            let transitions: Vec<(
                temm1e_distill::types::EigenTier,
                temm1e_distill::types::TierState,
                temm1e_distill::types::TierState,
            )> = engine.tick().await;
            if transitions.is_empty() {
                "Eigen-Tune: no tier transitions".to_string()
            } else {
                let mut out = String::new();
                for (tier, from, to) in transitions {
                    out.push_str(&format!(
                        "Eigen-Tune: {} {} → {}\n",
                        tier.as_str(),
                        from.as_str(),
                        to.as_str()
                    ));
                }
                out
            }
        }
        ["demote", tier] => {
            let tier_lower = tier.to_lowercase();
            if !["simple", "standard", "complex"].contains(&tier_lower.as_str()) {
                return format!(
                    "Eigen-Tune: invalid tier '{tier}'. Must be one of: simple, standard, complex"
                );
            }
            let db_path = dirs::home_dir()
                .map(|h| h.join(".temm1e").join("eigentune.db"))
                .unwrap_or_else(|| std::path::PathBuf::from("eigentune.db"));
            let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
            let store = match temm1e_distill::store::EigenTuneStore::new(&db_url).await {
                Ok(s) => std::sync::Arc::new(s),
                Err(e) => return format!("Eigen-Tune: store error: {e}"),
            };
            let mut record = match store.get_tier(&tier_lower).await {
                Ok(r) => r,
                Err(e) => return format!("Eigen-Tune: get_tier error: {e}"),
            };
            let from = record.state;
            record.state = temm1e_distill::types::TierState::Collecting;
            record.last_demoted_at = Some(chrono::Utc::now());
            record.serving_run_id = None;
            record.serving_since = None;
            record.sprt_lambda = 0.0;
            record.sprt_n = 0;
            record.cusum_s = 0.0;
            record.cusum_n = 0;
            if let Err(e) = store.update_tier(&record).await {
                return format!("Eigen-Tune: update_tier error: {e}");
            }
            format!(
                "Eigen-Tune: tier {tier_lower} demoted (was: {} → now: collecting)",
                from.as_str()
            )
        }
        _ => "Eigen-Tune: usage:\n  /eigentune status\n  /eigentune setup\n  /eigentune model [name]\n  /eigentune tick\n  /eigentune demote <tier>".to_string(),
    }
}

/// Handle a `temm1e eigentune <subcommand>` invocation.
async fn handle_eigentune_command(
    config_path: Option<&std::path::Path>,
    command: EigentuneCommands,
) -> anyhow::Result<()> {
    let cfg = load_eigentune_config_from_path(config_path);
    let engine = match open_eigentune_engine(&cfg).await {
        Ok(e) => e,
        Err(e) => {
            eprintln!("Eigen-Tune: failed to open store: {e}");
            std::process::exit(1);
        }
    };

    match command {
        EigentuneCommands::Status => {
            println!("{}", engine.format_status().await);
            // Show both opt-in switches explicitly so the user can audit
            // exactly what's enabled.
            println!();
            println!("Master switches:");
            println!("  enabled              = {}", cfg.enabled);
            println!("  enable_local_routing = {}", cfg.enable_local_routing);
        }
        EigentuneCommands::Setup => {
            let prereqs = engine.check_prerequisites().await;
            println!("EIGEN-TUNE SETUP\n");
            println!(
                "Ollama: {}",
                if prereqs.ollama_running {
                    "running ✓"
                } else {
                    "not running — brew install ollama && ollama serve"
                }
            );
            println!(
                "Python: {}",
                prereqs.python_version.as_deref().unwrap_or("not found")
            );
            if cfg!(all(target_os = "macos", target_arch = "aarch64")) {
                println!(
                    "MLX: {}",
                    if prereqs.mlx_installed {
                        "installed ✓"
                    } else {
                        "not found — pip install mlx-lm"
                    }
                );
            } else {
                println!(
                    "Unsloth: {}",
                    if prereqs.unsloth_installed {
                        "installed ✓"
                    } else {
                        "not found — pip install unsloth"
                    }
                );
            }
            println!("Can collect: {}", prereqs.can_collect);
            println!("Can train:   {}", prereqs.can_train);
            println!("Can serve:   {}", prereqs.can_serve);
            println!();
            println!("To enable Eigen-Tune, set in temm1e.toml:");
            println!("  [eigentune]");
            println!("  enabled = true");
            println!("  # enable_local_routing = true   # second opt-in for serving from local");
        }
        EigentuneCommands::Model { name } => {
            if let Some(name) = name {
                println!(
                    "Eigen-Tune: to change the base model, edit [eigentune] base_model = \"{name}\" in temm1e.toml and restart."
                );
            } else {
                println!("{}", engine.format_model_status().await);
            }
        }
        EigentuneCommands::Tick => {
            let transitions: Vec<(
                temm1e_distill::types::EigenTier,
                temm1e_distill::types::TierState,
                temm1e_distill::types::TierState,
            )> = engine.tick().await;
            if transitions.is_empty() {
                println!("Eigen-Tune: no tier transitions");
            } else {
                for (tier, from, to) in transitions {
                    println!(
                        "Eigen-Tune: {} {} → {}",
                        tier.as_str(),
                        from.as_str(),
                        to.as_str()
                    );
                }
            }
        }
        EigentuneCommands::Demote { tier } => {
            // Gate 7 emergency kill switch — directly transition the tier
            // back to Collecting via raw store update.
            let tier_lower = tier.to_lowercase();
            if !["simple", "standard", "complex"].contains(&tier_lower.as_str()) {
                eprintln!(
                    "Eigen-Tune: invalid tier '{tier}'. Must be one of: simple, standard, complex"
                );
                std::process::exit(2);
            }
            // We don't have direct store access from EigenTuneEngine in the
            // public API, so we use the engine's tick to query state and
            // call the graduation manager via store.
            // For now, we open a fresh store connection and demote directly.
            let db_path = dirs::home_dir()
                .map(|h| h.join(".temm1e").join("eigentune.db"))
                .unwrap_or_else(|| std::path::PathBuf::from("eigentune.db"));
            let db_url = format!("sqlite:{}?mode=rwc", db_path.display());
            let store =
                std::sync::Arc::new(temm1e_distill::store::EigenTuneStore::new(&db_url).await?);
            let mut record = store.get_tier(&tier_lower).await?;
            let from = record.state;
            record.state = temm1e_distill::types::TierState::Collecting;
            record.last_demoted_at = Some(chrono::Utc::now());
            record.serving_run_id = None;
            record.serving_since = None;
            record.sprt_lambda = 0.0;
            record.sprt_n = 0;
            record.cusum_s = 0.0;
            record.cusum_n = 0;
            store.update_tier(&record).await?;
            println!(
                "Eigen-Tune: tier {tier_lower} demoted (was: {} → now: collecting)",
                from.as_str()
            );
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
        assert_eq!(default_model("lmstudio"), "qwen3.5-7b-instruct");
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
