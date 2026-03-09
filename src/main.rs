use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use anyhow::Result;
use clap::{Parser, Subcommand};
use skyclaw_core::Channel;
use tokio::sync::Mutex;

#[derive(Parser)]
#[command(name = "skyclaw")]
#[command(about = "Cloud-native Rust AI agent runtime — Telegram-native")]
#[command(version)]
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
    /// Start the SkyClaw gateway daemon
    Start,
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

/// Result of credential detection from user input.
struct DetectedCredential {
    provider: &'static str,
    api_key: String,
    base_url: Option<String>,
}

/// Reject obviously fake / placeholder API keys before they reach any provider.
/// This prevents bricking the agent by saving a dummy key to credentials.toml.
fn is_placeholder_key(key: &str) -> bool {
    let k = key.trim().to_lowercase();
    // Too short to be any real API key
    if k.len() < 10 {
        return true;
    }
    // Common placeholders users might paste from docs/examples/READMEs
    let placeholders = [
        "paste_your", "your_key", "your_api", "your-key", "your-api",
        "insert_your", "insert-your", "put_your", "put-your",
        "replace_with", "replace-with", "enter_your", "enter-your",
        "placeholder", "xxxxxxxx", "your_token", "your-token",
        "_here",  // catches PASTE_YOUR_KEY_HERE, PUT_KEY_HERE, etc.
    ];
    for p in &placeholders {
        if k.contains(p) {
            return true;
        }
    }
    // All same character (e.g. "aaaaaaaaaa")
    if k.len() >= 10 && k.chars().all(|c| c == k.chars().next().unwrap_or('a')) {
        return true;
    }
    false
}

/// Validate a provider key by making a minimal API call.
/// Returns Ok(provider_arc) if the key works, Err(message) if not.
async fn validate_provider_key(
    config: &skyclaw_core::types::config::ProviderConfig,
) -> Result<Arc<dyn skyclaw_core::Provider>, String> {
    let provider = skyclaw_providers::create_provider(config)
        .map_err(|e| format!("Failed to create provider: {}", e))?;
    let provider_arc: Arc<dyn skyclaw_core::Provider> = Arc::from(provider);

    let test_req = skyclaw_core::types::message::CompletionRequest {
        model: config.model.clone().unwrap_or_default(),
        messages: vec![skyclaw_core::types::message::ChatMessage {
            role: skyclaw_core::types::message::Role::User,
            content: skyclaw_core::types::message::MessageContent::Text("Hi".to_string()),
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
            // Auth errors mean the key is invalid — reject
            if err_lower.contains("401")
                || err_lower.contains("403")
                || err_lower.contains("unauthorized")
                || err_lower.contains("invalid api key")
                || err_lower.contains("invalid x-api-key")
                || err_lower.contains("authentication")
                || err_lower.contains("permission")
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

/// Detect API provider from user input. Supports multiple formats:
///
/// 1. Raw key (auto-detect): `sk-ant-xxx`
/// 2. Explicit provider:key: `minimax:eyJhbG...`
/// 3. Proxy config (key:value pairs on one or multiple lines):
///    `proxy provider:openai base_url:https://my-proxy/v1 key:sk-xxx`
///    or `proxy openai https://my-proxy/v1 sk-xxx` (positional shorthand)
fn detect_api_key(text: &str) -> Option<DetectedCredential> {
    let trimmed = text.trim();

    // ── Format 3: Proxy config ──────────────────────────────
    // Detect "proxy" keyword (case-insensitive)
    let lower = trimmed.to_lowercase();
    if lower.starts_with("proxy") {
        let result = parse_proxy_config(trimmed);
        // Validate proxy key isn't a placeholder
        if let Some(ref cred) = result {
            if is_placeholder_key(&cred.api_key) {
                return None;
            }
        }
        return result;
    }

    // ── Format 2: Explicit provider:key ─────────────────────
    if let Some((provider, key)) = trimmed.split_once(':') {
        // Don't match "http:" or "https:" as provider:key
        let p = provider.to_lowercase();
        if p != "http" && p != "https" {
            match p.as_str() {
                "anthropic" | "openai" | "gemini" | "grok" | "xai" | "openrouter" | "minimax" => {
                    if key.len() >= 8 && !is_placeholder_key(key) {
                        return Some(DetectedCredential {
                            provider: match p.as_str() {
                                "anthropic" => "anthropic",
                                "openai" => "openai",
                                "gemini" => "gemini",
                                "grok" | "xai" => "grok",
                                "openrouter" => "openrouter",
                                "minimax" => "minimax",
                                _ => unreachable!(),
                            },
                            api_key: key.to_string(),
                            base_url: None,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    // ── Format 1: Auto-detect from key prefix ───────────────
    // Reject placeholders before accepting
    if is_placeholder_key(trimmed) {
        return None;
    }
    if trimmed.starts_with("sk-ant-") {
        Some(DetectedCredential { provider: "anthropic", api_key: trimmed.to_string(), base_url: None })
    } else if trimmed.starts_with("sk-or-") {
        Some(DetectedCredential { provider: "openrouter", api_key: trimmed.to_string(), base_url: None })
    } else if trimmed.starts_with("xai-") {
        Some(DetectedCredential { provider: "grok", api_key: trimmed.to_string(), base_url: None })
    } else if trimmed.starts_with("sk-") {
        Some(DetectedCredential { provider: "openai", api_key: trimmed.to_string(), base_url: None })
    } else if trimmed.starts_with("AIzaSy") {
        Some(DetectedCredential { provider: "gemini", api_key: trimmed.to_string(), base_url: None })
    } else {
        None
    }
}

/// Parse proxy configuration from user input.
///
/// Supports flexible formats:
///   `proxy provider:openai base_url:https://... key:sk-xxx`
///   `proxy provider:openai url:https://... key:sk-xxx`
///   `proxy openai https://my-proxy.com/v1 sk-xxx`  (positional shorthand)
///
/// Also handles multi-line input (Telegram sends line breaks).
fn parse_proxy_config(text: &str) -> Option<DetectedCredential> {
    // Normalize: join all lines, split by whitespace
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.len() < 3 {
        return None; // Need at least "proxy <provider> <key>"
    }

    let mut provider: Option<&'static str> = None;
    let mut base_url: Option<String> = None;
    let mut api_key: Option<String> = None;

    // Skip the "proxy" token
    let mut i = 1;
    while i < tokens.len() {
        let token = tokens[i];
        let lower = token.to_lowercase();

        // Key:value format
        if let Some((k, v)) = token.split_once(':') {
            let k_lower = k.to_lowercase();
            match k_lower.as_str() {
                "provider" | "type" => {
                    provider = normalize_provider_name(v);
                }
                "base_url" | "url" | "endpoint" | "host" => {
                    base_url = Some(v.to_string());
                }
                "key" | "api_key" | "apikey" | "token" => {
                    api_key = Some(v.to_string());
                }
                // Could be a provider:key or url with port
                _ => {
                    if v.starts_with("//") || v.starts_with("http") {
                        // It's a URL like "https://..."
                        base_url = Some(token.to_string());
                    } else if normalize_provider_name(&lower).is_some() {
                        // e.g. "openai:sk-xxx" — treat as provider + key
                        provider = normalize_provider_name(k);
                        api_key = Some(v.to_string());
                    }
                }
            }
        } else if token.starts_with("http://") || token.starts_with("https://") {
            // Positional: bare URL
            base_url = Some(token.to_string());
        } else if normalize_provider_name(&lower).is_some() && provider.is_none() {
            // Positional: provider name
            provider = normalize_provider_name(&lower);
        } else if token.len() >= 8 && api_key.is_none() {
            // Positional: assume it's the API key (long enough token)
            api_key = Some(token.to_string());
        }

        i += 1;
    }

    // Provider defaults to "openai" for proxies (most common use case)
    let provider = provider.unwrap_or("openai");
    let api_key = api_key?;

    Some(DetectedCredential {
        provider,
        api_key,
        base_url,
    })
}

/// Normalize provider name string to a static str.
fn normalize_provider_name(name: &str) -> Option<&'static str> {
    match name.to_lowercase().as_str() {
        "anthropic" | "claude" => Some("anthropic"),
        "openai" | "gpt" => Some("openai"),
        "gemini" | "google" => Some("gemini"),
        "grok" | "xai" => Some("grok"),
        "openrouter" => Some("openrouter"),
        "minimax" => Some("minimax"),
        _ => None,
    }
}

/// Default model for each provider.
fn default_model(provider_name: &str) -> &'static str {
    match provider_name {
        "anthropic" => "claude-sonnet-4-6",
        "openai" => "gpt-5.2",
        "gemini" => "gemini-2.5-flash",
        "grok" | "xai" => "grok-4-1-fast-non-reasoning",
        "openrouter" => "anthropic/claude-sonnet-4-6",
        "minimax" => "MiniMax-M2.5",
        _ => "claude-sonnet-4-6",
    }
}

/// Credentials file layout (multi-provider, multi-key).
///
/// ```toml
/// active = "anthropic"
///
/// [[providers]]
/// name = "anthropic"
/// keys = ["sk-ant-key1", "sk-ant-key2"]
/// model = "claude-sonnet-4-6"
/// ```
#[derive(serde::Serialize, serde::Deserialize, Default, Clone)]
struct CredentialsFile {
    /// Name of the currently active provider.
    #[serde(default)]
    active: String,
    /// All configured providers.
    #[serde(default)]
    providers: Vec<CredentialsProvider>,
}

#[derive(serde::Serialize, serde::Deserialize, Clone)]
struct CredentialsProvider {
    name: String,
    #[serde(default)]
    keys: Vec<String>,
    model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    base_url: Option<String>,
}

fn credentials_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".skyclaw")
        .join("credentials.toml")
}

/// Load the full credentials file. Falls back to legacy single-provider format.
fn load_credentials_file() -> Option<CredentialsFile> {
    let path = credentials_path();
    let content = std::fs::read_to_string(&path).ok()?;

    // Try new format first
    if let Ok(creds) = toml::from_str::<CredentialsFile>(&content) {
        if !creds.providers.is_empty() {
            return Some(creds);
        }
    }

    // Fallback: legacy single-provider format
    let table: toml::Table = content.parse().ok()?;
    let provider = table.get("provider")?.as_table()?;
    let name = provider.get("name")?.as_str()?.to_string();
    let key = provider.get("api_key")?.as_str()?.to_string();
    let model = provider.get("model")?.as_str()?.to_string();
    if name.is_empty() || key.is_empty() {
        return None;
    }
    Some(CredentialsFile {
        active: name.clone(),
        providers: vec![CredentialsProvider {
            name,
            keys: vec![key],
            model,
            base_url: None,
        }],
    })
}

/// Save credentials — appends key to existing provider or creates new entry.
/// If `custom_base_url` is provided, it creates a separate proxy entry.
async fn save_credentials(
    provider_name: &str,
    api_key: &str,
    model: &str,
    custom_base_url: Option<&str>,
) -> Result<()> {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".skyclaw");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("credentials.toml");

    let mut creds = load_credentials_file().unwrap_or_default();

    // For proxy providers with custom base_url, match on name + base_url
    // to keep them separate from the default endpoint entry.
    let match_fn = |p: &CredentialsProvider| -> bool {
        p.name == provider_name && p.base_url == custom_base_url.map(|s| s.to_string())
    };

    if let Some(existing) = creds.providers.iter_mut().find(|p| match_fn(p)) {
        if !existing.keys.contains(&api_key.to_string()) {
            existing.keys.push(api_key.to_string());
            tracing::info!(
                provider = %provider_name,
                total_keys = existing.keys.len(),
                "Added new key to existing provider"
            );
        }
        existing.model = model.to_string();
    } else {
        creds.providers.push(CredentialsProvider {
            name: provider_name.to_string(),
            keys: vec![api_key.to_string()],
            model: model.to_string(),
            base_url: custom_base_url.map(|s| s.to_string()),
        });
    }

    // Set this provider as active
    creds.active = provider_name.to_string();

    let content = toml::to_string_pretty(&creds)?;
    tokio::fs::write(&path, content).await?;
    tracing::info!(path = %path.display(), provider = %provider_name, "Credentials saved");
    Ok(())
}

/// Load the active provider's credentials (backwards-compatible return type).
/// Filters out placeholder/dummy keys — returns None if no valid key exists.
fn load_saved_credentials() -> Option<(String, String, String)> {
    let creds = load_credentials_file()?;
    // Find active provider
    let provider = creds
        .providers
        .iter()
        .find(|p| p.name == creds.active)
        .or_else(|| creds.providers.first())?;
    // Find the first non-placeholder key
    let first_valid_key = provider.keys.iter().find(|k| !is_placeholder_key(k))?.clone();
    if provider.name.is_empty() || first_valid_key.is_empty() {
        return None;
    }
    Some((provider.name.clone(), first_valid_key, provider.model.clone()))
}

/// Load all keys for the active provider.
/// Filters out placeholder/dummy keys — returns None if no valid keys remain.
fn load_active_provider_keys() -> Option<(String, Vec<String>, String, Option<String>)> {
    let creds = load_credentials_file()?;
    let provider = creds
        .providers
        .iter()
        .find(|p| p.name == creds.active)
        .or_else(|| creds.providers.first())?;
    // Filter out placeholders
    let valid_keys: Vec<String> = provider.keys.iter()
        .filter(|k| !is_placeholder_key(k))
        .cloned()
        .collect();
    if provider.name.is_empty() || valid_keys.is_empty() {
        return None;
    }
    Some((
        provider.name.clone(),
        valid_keys,
        provider.model.clone(),
        provider.base_url.clone(),
    ))
}

const ONBOARDING_MESSAGE: &str = "\
Welcome to SkyClaw!\n\n\
To get started, paste your API key below. I'll auto-detect the provider and get you online.\n\n\
You can add more keys later, switch providers mid-conversation, or use a custom proxy endpoint.\n\n\
Paste your key to begin.";

const ONBOARDING_REFERENCE: &str = "\
Supported formats:\n\n\
1\u{fe0f}\u{20e3} Auto-detect (just paste the key):\n\
sk-ant-...     \u{2192} Anthropic\n\
sk-...         \u{2192} OpenAI\n\
AIzaSy...      \u{2192} Gemini\n\
xai-...        \u{2192} Grok\n\
sk-or-...      \u{2192} OpenRouter\n\n\
2\u{fe0f}\u{20e3} Explicit (for keys without unique prefix):\n\
minimax:YOUR_KEY\n\
openrouter:YOUR_KEY\n\n\
3\u{fe0f}\u{20e3} Proxy / custom endpoint:\n\
proxy <provider> <base_url> <api_key>\n\n\
Example:\n\
proxy openai https://my-proxy.com/v1 sk-xxx\n\
proxy anthropic https://gateway.ai/v1/anthropic sk-ant-xxx";

const SYSTEM_PROMPT_BASE: &str = "\
You are SkyClaw, a cloud-native AI agent running on a remote server. \
You have full access to these tools:\n\
- shell: run any command\n\
- file_read / file_write / file_list: filesystem operations\n\
- web_fetch: HTTP GET requests\n\
- browser: control a real Chrome browser (navigate, click, type, screenshot, \
  get_text, evaluate JS, get_html) — use this for any website interaction\n\
- send_message: send real-time messages to the user during tasks\n\
- send_file: send files to the user\n\n\
KEY RULES:\n\
- Shell output (stdout/stderr) is NOT visible to the user. Only YOUR \
  final text reply and send_message calls reach the user.\n\
- To send real-time updates during multi-step work, call send_message \
  each round. You can do this repeatedly — each call delivers instantly.\n\
- For timed/periodic tasks (e.g. send something every N seconds), use \
  shell(\"sleep N\") then send_message in a loop across tool rounds.\n\
- When asked to visit a website, open a page, or interact with a web app, \
  USE THE BROWSER TOOL. Do not refuse or explain why you can't — just do it.\n\
- After finishing browser work, call browser with action 'close' to shut it down.\n\
- Reply in the same language the user writes in.\n\
- Be concise. No emoji unless the user uses them.\n\
- NEVER give up on a task by explaining limitations. You have a multi-round \
  tool loop — keep calling tools until the task is done or you hit a real \
  error. Do not stop early to explain what you 'cannot' do.";

/// Build the full system prompt with dynamic provider/model context.
/// This ensures the bot always knows what's actually configured.
fn build_system_prompt() -> String {
    let mut prompt = SYSTEM_PROMPT_BASE.to_string();

    // ── Provider/model context ────────────────────────────────
    prompt.push_str("\n\nSUPPORTED PROVIDERS & DEFAULT MODELS:\n");
    prompt.push_str("- anthropic: claude-sonnet-4-6, claude-opus-4-6, claude-haiku-4-6\n");
    prompt.push_str("- openai: gpt-5.2, gpt-4.1, gpt-4.1-mini, o4-mini\n");
    prompt.push_str("- gemini: gemini-2.5-flash, gemini-2.5-pro\n");
    prompt.push_str("- grok (xai): grok-4-1-fast-non-reasoning, grok-3\n");
    prompt.push_str("- openrouter: any model via anthropic/claude-sonnet-4-6, openai/gpt-5.2, etc.\n");
    prompt.push_str("- minimax: MiniMax-M2.5\n");

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
    prompt.push_str("\n\
SELF-CONFIGURATION:\n\
Your config lives at ~/.skyclaw/credentials.toml.\n\
To change the active provider or model, edit ONLY the 'active' field or 'model' \
field in credentials.toml. NEVER modify or add API keys directly — keys are \
managed by the onboarding system. If the user wants to add a key, tell them to \
paste it in chat.\n\
Changes take effect immediately — SkyClaw validates the key and auto-reloads \
after each response. If a key is invalid, the switch is rejected and the \
current provider stays active.\n\
Users can add keys anytime by pasting them in chat. SkyClaw auto-detects the \
provider and validates before saving.");

    prompt
}

// ── Stop-command detection ─────────────────────────────────
fn is_stop_command(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    const STOP_WORDS: &[&str] = &[
        // English
        "stop",
        "cancel",
        "abort",
        "quit",
        "halt",
        "enough",
        // Vietnamese
        "dừng",
        "dung",
        "thôi",
        "thoi",
        "ngừng",
        "ngung",
        "hủy",
        "huy",
        "dẹp",
        "dep",
        // Spanish
        "para",
        "detente",
        "basta",
        "cancela",
        "alto",
        // French
        "arrête",
        "arrete",
        "arrêter",
        "arreter",
        "annuler",
        "suffit",
        // German
        "stopp",
        "aufhören",
        "aufhoren",
        "abbrechen",
        "genug",
        // Portuguese
        "pare",
        "parar",
        "cancele",
        "cancelar",
        "chega",
        // Italian
        "ferma",
        "fermati",
        "basta",
        "annulla",
        "smettila",
        // Russian
        "стоп",
        "стой",
        "хватит",
        "отмена",
        "довольно",
        // Japanese
        "止めて",
        "やめて",
        "やめろ",
        "ストップ",
        "止め",
        "やめ",
        // Korean
        "멈춰",
        "그만",
        "중지",
        "취소",
        "됐어",
        // Chinese
        "停",
        "停止",
        "取消",
        "别说了",
        "够了",
        "算了",
        // Arabic
        "توقف",
        "الغاء",
        "كفى",
        "قف",
        // Thai
        "หยุด",
        "ยกเลิก",
        "พอ",
        "เลิก",
        // Indonesian / Malay
        "berhenti",
        "hentikan",
        "batalkan",
        "cukup",
        "sudah",
        // Hindi
        "रुको",
        "बंद",
        "रद्द",
        "बस",
        "ruko",
        "bas",
        // Turkish
        "dur",
        "durdur",
        "iptal",
        "yeter",
    ];

    if STOP_WORDS.contains(&t.as_str()) {
        return true;
    }

    if t.len() <= 60 {
        const STOP_PHRASES: &[&str] = &[
            "stop it",
            "stop that",
            "please stop",
            "stop now",
            "cancel that",
            "shut up",
            "dừng lại",
            "dung lai",
            "thôi đi",
            "thoi di",
            "dừng đi",
            "dung di",
            "ngừng lại",
            "ngung lai",
            "dung viet",
            "dừng viết",
            "thoi dung",
            "thôi dừng",
            "đừng nói nữa",
            "dung noi nua",
            "im đi",
            "im di",
            "para ya",
            "deja de",
            "arrête ça",
            "arrete ca",
            "hör auf",
            "hor auf",
            "止めてください",
            "やめてください",
            "停下来",
            "不要说了",
            "别说了",
            "그만해",
            "멈춰줘",
        ];

        for phrase in STOP_PHRASES {
            if t.contains(phrase) {
                return true;
            }
        }
    }

    false
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    // Initialize logging
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .json()
        .init();

    // Load configuration
    let config_path = cli.config.as_ref().map(std::path::Path::new);
    let config = skyclaw_core::config::load_config(config_path)?;

    tracing::info!(mode = %cli.mode, "SkyClaw starting");

    match cli.command {
        Commands::Start => {
            tracing::info!("Starting SkyClaw gateway");

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
                    .join(".skyclaw");
                std::fs::create_dir_all(&data_dir).ok();
                format!("sqlite:{}/memory.db?mode=rwc", data_dir.display())
            });
            let memory: Arc<dyn skyclaw_core::Memory> = Arc::from(
                skyclaw_memory::create_memory_backend(&config.memory.backend, &memory_url).await?,
            );
            tracing::info!(backend = %config.memory.backend, "Memory initialized");

            // ── Telegram channel ───────────────────────────────
            let mut channels: Vec<Arc<dyn skyclaw_core::Channel>> = Vec::new();
            let mut primary_channel: Option<Arc<dyn skyclaw_core::Channel>> = None;
            let mut tg_rx: Option<
                tokio::sync::mpsc::Receiver<skyclaw_core::types::message::InboundMessage>,
            > = None;

            if let Some(tg_config) = config.channel.get("telegram") {
                if tg_config.enabled {
                    let mut tg = skyclaw_channels::TelegramChannel::new(tg_config)?;
                    tg.start().await?;
                    tg_rx = tg.take_receiver();
                    let tg_arc: Arc<dyn skyclaw_core::Channel> = Arc::new(tg);
                    channels.push(tg_arc.clone());
                    primary_channel = Some(tg_arc.clone());
                    tracing::info!("Telegram channel started");
                }
            }

            // ── Pending messages ───────────────────────────────
            let pending_messages: skyclaw_tools::PendingMessages =
                Arc::new(std::sync::Mutex::new(std::collections::HashMap::new()));

            // ── Tools ──────────────────────────────────────────
            let tools = skyclaw_tools::create_tools(
                &config.tools,
                primary_channel.clone(),
                Some(pending_messages.clone()),
            );
            tracing::info!(count = tools.len(), "Tools initialized");

            let system_prompt = Some(build_system_prompt());

            // ── Agent state (None during onboarding) ───────────
            let agent_state: Arc<tokio::sync::RwLock<Option<Arc<skyclaw_agent::AgentRuntime>>>> =
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
                        let valid: Vec<String> = keys.into_iter().filter(|k| !is_placeholder_key(k)).collect();
                        (valid, burl)
                    })
                    .unwrap_or_else(|| (vec![key.clone()], None));
                let effective_base_url = saved_base_url.or_else(|| config.provider.base_url.clone());
                let provider_config = skyclaw_core::types::config::ProviderConfig {
                    name: Some(pname.clone()),
                    api_key: Some(key.clone()),
                    keys: all_keys,
                    model: Some(model.clone()),
                    base_url: effective_base_url,
                    extra_headers: config.provider.extra_headers.clone(),
                };
                let provider: Arc<dyn skyclaw_core::Provider> =
                    Arc::from(skyclaw_providers::create_provider(&provider_config)?);
                let agent = Arc::new(skyclaw_agent::AgentRuntime::with_limits(
                    provider.clone(),
                    memory.clone(),
                    tools.clone(),
                    model.clone(),
                    system_prompt.clone(),
                    config.agent.max_turns,
                    config.agent.max_context_tokens,
                    config.agent.max_tool_rounds,
                    config.agent.max_task_duration_secs,
                ));
                *agent_state.write().await = Some(agent);
                tracing::info!(provider = %pname, model = %model, "Agent initialized");
                }
            } else {
                tracing::info!("No API key — starting in onboarding mode");
            }

            // ── Unified message channel ────────────────────────
            let (msg_tx, mut msg_rx) =
                tokio::sync::mpsc::channel::<skyclaw_core::types::message::InboundMessage>(32);

            // Wire Telegram messages into the unified channel
            if let Some(mut tg_rx) = tg_rx {
                let tx = msg_tx.clone();
                tokio::spawn(async move {
                    while let Some(msg) = tg_rx.recv().await {
                        if tx.send(msg).await.is_err() {
                            break;
                        }
                    }
                });
            }

            // ── Workspace ──────────────────────────────────────
            let workspace_path = dirs::home_dir()
                .unwrap_or_else(|| std::path::PathBuf::from("."))
                .join(".skyclaw")
                .join("workspace");
            std::fs::create_dir_all(&workspace_path).ok();

            // ── Heartbeat ──────────────────────────────────────
            if config.heartbeat.enabled {
                let heartbeat_chat_id = config
                    .heartbeat
                    .report_to
                    .clone()
                    .unwrap_or_else(|| "heartbeat".to_string());
                let runner = skyclaw_automation::HeartbeatRunner::new(
                    config.heartbeat.clone(),
                    workspace_path.clone(),
                    heartbeat_chat_id,
                );
                let hb_tx = msg_tx.clone();
                tokio::spawn(async move {
                    runner.run(hb_tx).await;
                });
                tracing::info!(
                    interval = %config.heartbeat.interval,
                    checklist = %config.heartbeat.checklist,
                    "Heartbeat runner started"
                );
            }

            // ── Per-chat serial executor ───────────────────────

            /// Tracks the active task state for a single chat.
            struct ChatSlot {
                tx: tokio::sync::mpsc::Sender<skyclaw_core::types::message::InboundMessage>,
                interrupt: Arc<AtomicBool>,
                is_heartbeat: Arc<AtomicBool>,
            }

            if let Some(sender) = primary_channel.clone() {
                let agent_state_clone = agent_state.clone();
                let memory_clone = memory.clone();
                let tools_clone = tools.clone();
                let agent_max_turns = config.agent.max_turns;
                let agent_max_context_tokens = config.agent.max_context_tokens;
                let agent_max_tool_rounds = config.agent.max_tool_rounds;
                let agent_max_task_duration = config.agent.max_task_duration_secs;
                let provider_base_url = config.provider.base_url.clone();
                let ws_path = workspace_path.clone();
                let pending_clone = pending_messages.clone();

                let chat_slots: Arc<Mutex<HashMap<String, ChatSlot>>> =
                    Arc::new(Mutex::new(HashMap::new()));

                tokio::spawn(async move {
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
                                }

                                let is_stop = inbound
                                    .text
                                    .as_deref()
                                    .map(is_stop_command)
                                    .unwrap_or(false);

                                if is_stop {
                                    tracing::info!(
                                        chat_id = %chat_id,
                                        "Stop command detected — interrupting active task"
                                    );
                                    slot.interrupt.store(true, Ordering::Relaxed);
                                    continue;
                                }

                                if let Some(text) = inbound.text.as_deref() {
                                    if let Ok(mut pq) = pending_clone.lock() {
                                        pq.entry(chat_id.clone())
                                            .or_default()
                                            .push(text.to_string());
                                    }
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
                        let slot = slots.entry(chat_id.clone()).or_insert_with(|| {
                            let (chat_tx, mut chat_rx) =
                                tokio::sync::mpsc::channel::<skyclaw_core::types::message::InboundMessage>(4);

                            let interrupt = Arc::new(AtomicBool::new(false));
                            let is_heartbeat = Arc::new(AtomicBool::new(false));

                            let agent_state = agent_state_clone.clone();
                            let memory = memory_clone.clone();
                            let tools_template = tools_clone.clone();
                            let max_turns = agent_max_turns;
                            let max_ctx = agent_max_context_tokens;
                            let max_rounds = agent_max_tool_rounds;
                            let max_task_duration = agent_max_task_duration;
                            let base_url = provider_base_url.clone();
                            let sender = sender.clone();
                            let workspace_path = ws_path.clone();
                            let interrupt_clone = interrupt.clone();
                            let is_heartbeat_clone = is_heartbeat.clone();
                            let pending_for_worker = pending_clone.clone();
                            let worker_chat_id = chat_id.clone();

                            tokio::spawn(async move {
                                while let Some(mut msg) = chat_rx.recv().await {
                                    let is_hb = msg.channel == "heartbeat";
                                    is_heartbeat_clone.store(is_hb, Ordering::Relaxed);
                                    interrupt_clone.store(false, Ordering::Relaxed);

                                    let interrupt_flag = Some(interrupt_clone.clone());

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
                                            let test_config = skyclaw_core::types::config::ProviderConfig {
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
                                                        let reload_config = skyclaw_core::types::config::ProviderConfig {
                                                            name: Some(name.clone()),
                                                            api_key: keys.first().cloned(),
                                                            keys: keys.clone(),
                                                            model: Some(mdl.clone()),
                                                            base_url: reload_base_url,
                                                            extra_headers: std::collections::HashMap::new(),
                                                        };
                                                        if let Ok(new_provider) = skyclaw_providers::create_provider(&reload_config) {
                                                            let new_agent = Arc::new(skyclaw_agent::AgentRuntime::with_limits(
                                                                Arc::from(new_provider),
                                                                memory.clone(),
                                                                tools_template.clone(),
                                                                mdl.clone(),
                                                                Some(build_system_prompt()),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                            ));
                                                            *agent_state.write().await = Some(new_agent);
                                                            let key_count = keys.len();
                                                            let reply = skyclaw_core::types::message::OutboundMessage {
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
                                                            let _ = sender.send_message(reply).await;
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
                                                    let reply = skyclaw_core::types::message::OutboundMessage {
                                                        chat_id: msg.chat_id.clone(),
                                                        text: format!(
                                                            "Invalid API key — {} returned an error:\n{}\n\nThe current provider is still active. Check the key and try again.",
                                                            cred.provider, err
                                                        ),
                                                        reply_to: Some(msg.id.clone()),
                                                        parse_mode: None,
                                                    };
                                                    let _ = sender.send_message(reply).await;
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
                                            continue;
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

                                        let mut session = skyclaw_core::types::session::SessionContext {
                                            session_id: format!("{}-{}", msg.channel, msg.chat_id),
                                            user_id: msg.user_id.clone(),
                                            channel: msg.channel.clone(),
                                            chat_id: msg.chat_id.clone(),
                                            history: Vec::new(),
                                            workspace_path: workspace_path.clone(),
                                        };

                                        match agent.process_message(&msg, &mut session, interrupt_flag, Some(pending_for_worker.clone())).await {
                                            Ok(reply) => {
                                                if let Err(e) = sender.send_message(reply).await {
                                                    tracing::error!(error = %e, "Failed to send reply");
                                                }
                                            }
                                            Err(e) => {
                                                tracing::error!(error = %e, "Agent processing error");
                                                let error_reply = skyclaw_core::types::message::OutboundMessage {
                                                    chat_id: msg.chat_id.clone(),
                                                    text: format!("Error: {}", e),
                                                    reply_to: Some(msg.id.clone()),
                                                    parse_mode: None,
                                                };
                                                let _ = sender.send_message(error_reply).await;
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
                                                    let reload_config = skyclaw_core::types::config::ProviderConfig {
                                                        name: Some(new_name.clone()),
                                                        api_key: valid_keys.first().cloned(),
                                                        keys: valid_keys,
                                                        model: Some(new_model.clone()),
                                                        base_url: effective_base_url,
                                                        extra_headers: std::collections::HashMap::new(),
                                                    };
                                                    match validate_provider_key(&reload_config).await {
                                                        Ok(validated_provider) => {
                                                            let new_agent = Arc::new(skyclaw_agent::AgentRuntime::with_limits(
                                                                validated_provider,
                                                                memory.clone(),
                                                                tools_template.clone(),
                                                                new_model.clone(),
                                                                Some(build_system_prompt()),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                            ));
                                                            *agent_state.write().await = Some(new_agent);
                                                            tracing::info!(provider = %new_name, model = %new_model, "Agent hot-reloaded (key validated)");
                                                        }
                                                        Err(err) => {
                                                            tracing::warn!(
                                                                provider = %new_name,
                                                                error = %err,
                                                                "Hot-reload aborted — new key failed validation, keeping current agent"
                                                            );
                                                        }
                                                    }
                                                }
                                            }
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
                                            let provider_config = skyclaw_core::types::config::ProviderConfig {
                                                name: Some(provider_name.to_string()),
                                                api_key: Some(api_key.clone()),
                                                keys: all_keys,
                                                model: Some(model.clone()),
                                                base_url: effective_base_url,
                                                extra_headers: std::collections::HashMap::new(),
                                            };

                                            match skyclaw_providers::create_provider(&provider_config) {
                                                Ok(_provider) => {
                                                    // Use shared validation (handles auth vs non-auth errors)
                                                    match validate_provider_key(&provider_config).await {
                                                        Ok(validated_provider) => {
                                                            // Key is valid — create agent and go online
                                                            let new_agent = Arc::new(skyclaw_agent::AgentRuntime::with_limits(
                                                                validated_provider,
                                                                memory.clone(),
                                                                tools_template.clone(),
                                                                model.clone(),
                                                                Some(build_system_prompt()),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                                max_task_duration,
                                                            ));
                                                            *agent_state.write().await = Some(new_agent);

                                                            if let Err(e) = save_credentials(provider_name, &api_key, &model, custom_base_url.as_deref()).await {
                                                                tracing::error!(error = %e, "Failed to save credentials");
                                                            }

                                                            let proxy_note = if custom_base_url.is_some() {
                                                                " (via proxy)"
                                                            } else {
                                                                ""
                                                            };
                                                            let reply = skyclaw_core::types::message::OutboundMessage {
                                                                chat_id: msg.chat_id.clone(),
                                                                text: format!(
                                                                    "API key verified! Configured {}{} with model {}.\n\nSkyClaw is online! You can:\n- Add more keys anytime (just paste them)\n- Use a proxy: \"proxy openai https://your-proxy/v1 your-key\"\n- Change settings in natural language\n\nHow can I help?",
                                                                    provider_name, proxy_note, model
                                                                ),
                                                                reply_to: Some(msg.id.clone()),
                                                                parse_mode: None,
                                                            };
                                                            let _ = sender.send_message(reply).await;
                                                            tracing::info!(provider = %provider_name, model = %model, "API key validated — agent online");
                                                        }
                                                        Err(e) => {
                                                            // Key failed auth validation
                                                            let reply = skyclaw_core::types::message::OutboundMessage {
                                                                chat_id: msg.chat_id.clone(),
                                                                text: format!(
                                                                    "Invalid API key — the {} API returned an error:\n{}\n\nPlease check your key and paste it again.",
                                                                    provider_name, e
                                                                ),
                                                                reply_to: Some(msg.id.clone()),
                                                                parse_mode: None,
                                                            };
                                                            let _ = sender.send_message(reply).await;
                                                            tracing::warn!(provider = %provider_name, error = %e, "API key validation failed");
                                                        }
                                                    }
                                                }
                                                Err(e) => {
                                                    let reply = skyclaw_core::types::message::OutboundMessage {
                                                        chat_id: msg.chat_id.clone(),
                                                        text: format!("Failed to configure provider: {}", e),
                                                        reply_to: Some(msg.id.clone()),
                                                        parse_mode: None,
                                                    };
                                                    let _ = sender.send_message(reply).await;
                                                }
                                            }
                                        } else {
                                            // Send onboarding welcome
                                            let reply = skyclaw_core::types::message::OutboundMessage {
                                                chat_id: msg.chat_id.clone(),
                                                text: ONBOARDING_MESSAGE.to_string(),
                                                reply_to: Some(msg.id.clone()),
                                                parse_mode: None,
                                            };
                                            let _ = sender.send_message(reply).await;

                                            // Send format reference as separate message for easy copy-paste
                                            let ref_msg = skyclaw_core::types::message::OutboundMessage {
                                                chat_id: msg.chat_id.clone(),
                                                text: ONBOARDING_REFERENCE.to_string(),
                                                reply_to: None,
                                                parse_mode: None,
                                            };
                                            let _ = sender.send_message(ref_msg).await;
                                        }
                                    }

                                    // Clear active state and pending queue
                                    is_heartbeat_clone.store(false, Ordering::Relaxed);
                                    interrupt_clone.store(false, Ordering::Relaxed);
                                    if let Ok(mut pq) = pending_for_worker.lock() {
                                        pq.remove(&worker_chat_id);
                                    }
                                }
                            });

                            ChatSlot { tx: chat_tx, interrupt, is_heartbeat }
                        });

                        // Send message into the chat's dedicated queue
                        if !is_heartbeat_msg {
                            if let Err(e) = slot.tx.send(inbound).await {
                                tracing::error!(error = %e, "Chat worker closed unexpectedly");
                            }
                        }
                    }
                });
            }

            // ── Start gateway + block ──────────────────────────
            let is_online = agent_state.read().await.is_some();

            println!("SkyClaw gateway starting...");
            println!("  Mode: {}", cli.mode);

            if is_online {
                let agent = agent_state.read().await.as_ref().unwrap().clone();
                let gate = skyclaw_gateway::SkyGate::new(channels, agent, config.gateway.clone());
                tokio::spawn(async move {
                    if let Err(e) = gate.start().await {
                        tracing::error!(error = %e, "Gateway error");
                    }
                });
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

            // Block until Ctrl+C
            tokio::signal::ctrl_c().await?;
            println!("\nSkyClaw shutting down...");
        }
        Commands::Chat => {
            println!("SkyClaw interactive chat");
            println!("Type 'exit' to quit.");
        }
        Commands::Status => {
            println!("SkyClaw Status");
            println!("  Mode: {}", config.skyclaw.mode);
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
        Commands::Version => {
            println!("skyclaw {}", env!("CARGO_PKG_VERSION"));
            println!("Cloud-native Rust AI agent runtime — Telegram-native");
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
        let result = detect_api_key("proxy provider:openai base_url:https://my-proxy.com/v1 key:sk-test-key-12345678").unwrap();
        assert_eq!(result.provider, "openai");
        assert_eq!(result.api_key, "sk-test-key-12345678");
        assert_eq!(result.base_url.unwrap(), "https://my-proxy.com/v1");
    }

    #[test]
    fn proxy_with_positional_format() {
        let result = detect_api_key("proxy openai https://my-proxy.com/v1 sk-test-key-12345678").unwrap();
        assert_eq!(result.provider, "openai");
        assert_eq!(result.api_key, "sk-test-key-12345678");
        assert_eq!(result.base_url.unwrap(), "https://my-proxy.com/v1");
    }

    #[test]
    fn proxy_with_url_alias() {
        let result = detect_api_key("proxy provider:anthropic url:https://claude-proxy.com/v1 key:sk-ant-test1234").unwrap();
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
        assert_eq!(default_model("gemini"), "gemini-2.5-flash");
        assert_eq!(default_model("grok"), "grok-4-1-fast-non-reasoning");
        assert_eq!(default_model("xai"), "grok-4-1-fast-non-reasoning");
        assert_eq!(default_model("openrouter"), "anthropic/claude-sonnet-4-6");
        assert_eq!(default_model("minimax"), "MiniMax-M2.5");
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
        assert!(!is_placeholder_key("sk-ant-api03-abc123def456ghi789jkl012mno345pqr678stu"));
        assert!(!is_placeholder_key("sk-proj-abcdefghijklmnopqrstuv"));
        assert!(!is_placeholder_key("sk-or-v1-abcdefghijklmnopqrstuv"));
        assert!(!is_placeholder_key("xai-abcdefghijklmnopqrstuvwxyz"));
        assert!(!is_placeholder_key("AIzaSyA-abcdefghijklmnopqrstu"));
        assert!(!is_placeholder_key("sk-test-key-12345678"));  // valid test fixture format
    }

    #[test]
    fn detect_rejects_placeholder_auto_format() {
        // These match sk- prefix but are obvious placeholders
        assert!(detect_api_key("sk-your_key_here_12345").is_none());
        assert!(detect_api_key("sk-ant-paste_your_key_here").is_none());
    }
}
