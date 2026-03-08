use std::collections::HashMap;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use clap::{Parser, Subcommand};
use anyhow::Result;
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

/// Detect API provider from key pattern.
fn detect_api_key(text: &str) -> Option<(&'static str, String)> {
    let trimmed = text.trim();
    if trimmed.starts_with("sk-ant-") {
        Some(("anthropic", trimmed.to_string()))
    } else if trimmed.starts_with("sk-") {
        Some(("openai", trimmed.to_string()))
    } else if trimmed.starts_with("AIzaSy") {
        Some(("gemini", trimmed.to_string()))
    } else {
        None
    }
}

/// Default model for each provider.
fn default_model(provider_name: &str) -> &'static str {
    match provider_name {
        "anthropic" => "claude-sonnet-4-6",
        "openai" => "gpt-5.2",
        "gemini" => "gemini-3-flash-preview",
        _ => "claude-sonnet-4-6",
    }
}

/// Save credentials to ~/.skyclaw/credentials.toml
async fn save_credentials(provider_name: &str, api_key: &str, model: &str) -> Result<()> {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join(".skyclaw");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("credentials.toml");
    let content = format!(
        "[provider]\nname = \"{}\"\napi_key = \"{}\"\nmodel = \"{}\"\n",
        provider_name, api_key, model
    );
    tokio::fs::write(&path, content).await?;
    tracing::info!(path = %path.display(), "Credentials saved");
    Ok(())
}

/// Load saved credentials from ~/.skyclaw/credentials.toml
fn load_saved_credentials() -> Option<(String, String, String)> {
    let path = dirs::home_dir()?.join(".skyclaw").join("credentials.toml");
    let content = std::fs::read_to_string(&path).ok()?;
    let table: toml::Table = content.parse().ok()?;
    let provider = table.get("provider")?.as_table()?;
    let name = provider.get("name")?.as_str()?.to_string();
    let key = provider.get("api_key")?.as_str()?.to_string();
    let model = provider.get("model")?.as_str()?.to_string();
    Some((name, key, model))
}

const ONBOARDING_MESSAGE: &str = "\
Welcome to SkyClaw!\n\n\
To get started, paste your API key from any of these providers:\n\n\
- Anthropic (starts with sk-ant-)\n\
- OpenAI (starts with sk-)\n\
- Google Gemini (starts with AIzaSy)\n\n\
Just paste the key here and I'll handle the rest.";

const SYSTEM_PROMPT: &str = "\
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
  error. Do not stop early to explain what you 'cannot' do.\n\n\
SELF-CONFIGURATION:\n\
Your config lives at ~/.skyclaw/credentials.toml (provider, api_key, model). \
You can read and edit this file to change your own settings. For example, \
if the user says 'change model to claude-opus-4-6', edit credentials.toml \
and confirm. Changes take effect on next restart. Tell the user they can \
configure you through natural language — just ask.";

// ── Stop-command detection ─────────────────────────────────
fn is_stop_command(text: &str) -> bool {
    let t = text.trim().to_lowercase();
    const STOP_WORDS: &[&str] = &[
        // English
        "stop", "cancel", "abort", "quit", "halt", "enough",
        // Vietnamese
        "dừng", "dung", "thôi", "thoi", "ngừng", "ngung",
        "hủy", "huy", "dẹp", "dep",
        // Spanish
        "para", "detente", "basta", "cancela", "alto",
        // French
        "arrête", "arrete", "arrêter", "arreter", "annuler", "suffit",
        // German
        "stopp", "aufhören", "aufhoren", "abbrechen", "genug",
        // Portuguese
        "pare", "parar", "cancele", "cancelar", "chega",
        // Italian
        "ferma", "fermati", "basta", "annulla", "smettila",
        // Russian
        "стоп", "стой", "хватит", "отмена", "довольно",
        // Japanese
        "止めて", "やめて", "やめろ", "ストップ", "止め", "やめ",
        // Korean
        "멈춰", "그만", "중지", "취소", "됐어",
        // Chinese
        "停", "停止", "取消", "别说了", "够了", "算了",
        // Arabic
        "توقف", "الغاء", "كفى", "قف",
        // Thai
        "หยุด", "ยกเลิก", "พอ", "เลิก",
        // Indonesian / Malay
        "berhenti", "hentikan", "batalkan", "cukup", "sudah",
        // Hindi
        "रुको", "बंद", "रद्द", "बस", "ruko", "bas",
        // Turkish
        "dur", "durdur", "iptal", "yeter",
    ];

    if STOP_WORDS.contains(&t.as_str()) {
        return true;
    }

    if t.len() <= 60 {
        const STOP_PHRASES: &[&str] = &[
            "stop it", "stop that", "please stop", "stop now",
            "cancel that", "shut up",
            "dừng lại", "dung lai", "thôi đi", "thoi di",
            "dừng đi", "dung di", "ngừng lại", "ngung lai",
            "dung viet", "dừng viết", "thoi dung", "thôi dừng",
            "đừng nói nữa", "dung noi nua", "im đi", "im di",
            "para ya", "deja de",
            "arrête ça", "arrete ca",
            "hör auf", "hor auf",
            "止めてください", "やめてください",
            "停下来", "不要说了", "别说了",
            "그만해", "멈춰줘",
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
                        let name = config.provider.name.clone()
                            .unwrap_or_else(|| "anthropic".to_string());
                        let model = config.provider.model.clone()
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
                skyclaw_memory::create_memory_backend(&config.memory.backend, &memory_url).await?
            );
            tracing::info!(backend = %config.memory.backend, "Memory initialized");

            // ── Telegram channel ───────────────────────────────
            let mut channels: Vec<Arc<dyn skyclaw_core::Channel>> = Vec::new();
            let mut primary_channel: Option<Arc<dyn skyclaw_core::Channel>> = None;
            let mut tg_rx: Option<tokio::sync::mpsc::Receiver<skyclaw_core::types::message::InboundMessage>> = None;

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

            let system_prompt = Some(SYSTEM_PROMPT.to_string());

            // ── Agent state (None during onboarding) ───────────
            let agent_state: Arc<tokio::sync::RwLock<Option<Arc<skyclaw_agent::AgentRuntime>>>> =
                Arc::new(tokio::sync::RwLock::new(None));

            if let Some((ref pname, ref key, ref model)) = credentials {
                let provider_config = skyclaw_core::types::config::ProviderConfig {
                    name: Some(pname.clone()),
                    api_key: Some(key.clone()),
                    model: Some(model.clone()),
                    base_url: config.provider.base_url.clone(),
                };
                let provider: Arc<dyn skyclaw_core::Provider> = Arc::from(
                    skyclaw_providers::create_provider(&provider_config)?
                );
                let agent = Arc::new(skyclaw_agent::AgentRuntime::with_limits(
                    provider.clone(),
                    memory.clone(),
                    tools.clone(),
                    model.clone(),
                    system_prompt.clone(),
                    config.agent.max_turns,
                    config.agent.max_context_tokens,
                    config.agent.max_tool_rounds,
                ));
                *agent_state.write().await = Some(agent);
                tracing::info!(provider = %pname, model = %model, "Agent initialized");
            } else {
                tracing::info!("No API key — starting in onboarding mode");
            }

            // ── Unified message channel ────────────────────────
            let (msg_tx, mut msg_rx) = tokio::sync::mpsc::channel::<skyclaw_core::types::message::InboundMessage>(32);

            // Wire Telegram messages into the unified channel
            if let Some(mut tg_rx) = tg_rx {
                let tx = msg_tx.clone();
                tokio::spawn(async move {
                    while let Some(msg) = tg_rx.recv().await {
                        if tx.send(msg).await.is_err() { break; }
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
                let heartbeat_chat_id = config.heartbeat.report_to.clone()
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
                let system_prompt_clone = system_prompt.clone();
                let agent_max_turns = config.agent.max_turns;
                let agent_max_context_tokens = config.agent.max_context_tokens;
                let agent_max_tool_rounds = config.agent.max_tool_rounds;
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

                                let is_stop = inbound.text.as_deref()
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
                            let sys_prompt = system_prompt_clone.clone();
                            let max_turns = agent_max_turns;
                            let max_ctx = agent_max_context_tokens;
                            let max_rounds = agent_max_tool_rounds;
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
                                    } else {
                                        // ── Onboarding mode: detect API key ────
                                        let msg_text = msg.text.as_deref().unwrap_or("");

                                        if let Some((provider_name, api_key)) = detect_api_key(msg_text) {
                                            let model = default_model(provider_name).to_string();
                                            let provider_config = skyclaw_core::types::config::ProviderConfig {
                                                name: Some(provider_name.to_string()),
                                                api_key: Some(api_key.clone()),
                                                model: Some(model.clone()),
                                                base_url: base_url.clone(),
                                            };

                                            match skyclaw_providers::create_provider(&provider_config) {
                                                Ok(provider) => {
                                                    // Validate the key by making a small test request
                                                    let test_req = skyclaw_core::types::message::CompletionRequest {
                                                        model: model.clone(),
                                                        messages: vec![skyclaw_core::types::message::ChatMessage {
                                                            role: skyclaw_core::types::message::Role::User,
                                                            content: skyclaw_core::types::message::MessageContent::Text("Hi".to_string()),
                                                        }],
                                                        tools: Vec::new(),
                                                        max_tokens: Some(1),
                                                        temperature: Some(0.0),
                                                        system: None,
                                                    };

                                                    let provider_arc: Arc<dyn skyclaw_core::Provider> = Arc::from(provider);

                                                    match provider_arc.complete(test_req).await {
                                                        Ok(_) => {
                                                            // Key is valid — create agent and go online
                                                            let new_agent = Arc::new(skyclaw_agent::AgentRuntime::with_limits(
                                                                provider_arc,
                                                                memory.clone(),
                                                                tools_template.clone(),
                                                                model.clone(),
                                                                sys_prompt.clone(),
                                                                max_turns,
                                                                max_ctx,
                                                                max_rounds,
                                                            ));
                                                            *agent_state.write().await = Some(new_agent);

                                                            if let Err(e) = save_credentials(provider_name, &api_key, &model).await {
                                                                tracing::error!(error = %e, "Failed to save credentials");
                                                            }

                                                            let reply = skyclaw_core::types::message::OutboundMessage {
                                                                chat_id: msg.chat_id.clone(),
                                                                text: format!(
                                                                    "API key verified! Configured {} with model {}.\n\nSkyClaw is online! You can change settings anytime — just tell me in plain language (e.g. \"change model to claude-opus-4-6\").\n\nHow can I help?",
                                                                    provider_name, model
                                                                ),
                                                                reply_to: Some(msg.id.clone()),
                                                                parse_mode: None,
                                                            };
                                                            let _ = sender.send_message(reply).await;
                                                            tracing::info!(provider = %provider_name, model = %model, "API key validated — agent online");
                                                        }
                                                        Err(e) => {
                                                            // Key failed validation
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
                let gate = skyclaw_gateway::SkyGate::new(
                    channels,
                    agent,
                    config.gateway.clone(),
                );
                tokio::spawn(async move {
                    if let Err(e) = gate.start().await {
                        tracing::error!(error = %e, "Gateway error");
                    }
                });
                println!("  Status: Online");
                println!("  Gateway: http://{}:{}", config.gateway.host, config.gateway.port);
                println!("  Health: http://{}:{}/health", config.gateway.host, config.gateway.port);
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
            println!("  Provider: {}", config.provider.name.as_deref().unwrap_or("not configured"));
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
                println!("  Provider: {}", config.provider.name.as_deref().unwrap_or("none"));
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
