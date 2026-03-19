//! Agent bridge — sets up AgentRuntime and runs the message processing loop.
//!
//! This module handles all the complexity of creating the provider, memory backend,
//! tools, and agent runtime, then runs a background task that processes inbound
//! messages and sends responses + status updates back to the TUI event loop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::{mpsc, watch, Mutex, RwLock};

use temm1e_agent::agent_task_status::AgentTaskStatus;
use temm1e_agent::AgentRuntime;
use temm1e_core::config::credentials;
use temm1e_core::types::config::{Temm1eConfig, Temm1eMode};
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::{InboundMessage, OutboundMessage};
use temm1e_core::types::session::SessionContext;

use crate::event::{AgentResponseEvent, Event};

/// Everything needed to communicate with the running agent.
pub struct AgentHandle {
    /// Send user messages to the agent processing loop.
    pub inbound_tx: mpsc::Sender<InboundMessage>,
    /// Watch channel for real-time status updates.
    pub status_rx: watch::Receiver<AgentTaskStatus>,
}

/// Configuration for agent setup.
pub struct AgentSetup {
    pub provider_name: String,
    pub api_key: String,
    pub model: String,
    pub base_url: Option<String>,
    pub config: Temm1eConfig,
    /// Selected personality mode (auto/play/work/pro).
    pub mode: Option<String>,
}

/// Create the agent runtime from credentials and spawn the processing loop.
///
/// Returns an `AgentHandle` for communication, or an error if setup fails.
pub async fn spawn_agent(
    setup: AgentSetup,
    event_tx: mpsc::UnboundedSender<Event>,
) -> Result<AgentHandle, Temm1eError> {
    // 1. Create provider
    let (all_keys, saved_base_url) = credentials::load_active_provider_keys()
        .map(|(_, keys, _, burl)| {
            let valid: Vec<String> = keys
                .into_iter()
                .filter(|k| !credentials::is_placeholder_key(k))
                .collect();
            (valid, burl)
        })
        .unwrap_or_else(|| (vec![setup.api_key.clone()], None));

    let effective_base_url = saved_base_url.or(setup.config.provider.base_url.clone());

    let provider_config = temm1e_core::types::config::ProviderConfig {
        name: Some(setup.provider_name.clone()),
        api_key: Some(setup.api_key.clone()),
        keys: all_keys,
        model: Some(setup.model.clone()),
        base_url: effective_base_url,
        extra_headers: setup.config.provider.extra_headers.clone(),
    };

    let provider: Arc<dyn temm1e_core::Provider> = {
        #[cfg(feature = "codex-oauth")]
        if setup.provider_name == "openai-codex" {
            match temm1e_codex_oauth::TokenStore::load() {
                Ok(store) => Arc::new(temm1e_codex_oauth::CodexResponsesProvider::new(
                    setup.model.clone(),
                    std::sync::Arc::new(store),
                )),
                Err(e) => {
                    return Err(Temm1eError::Auth(format!(
                        "Codex OAuth not configured: {}",
                        e
                    )));
                }
            }
        } else {
            Arc::from(
                temm1e_providers::create_provider(&provider_config)
                    .map_err(|e| Temm1eError::Provider(e.to_string()))?,
            )
        }

        #[cfg(not(feature = "codex-oauth"))]
        Arc::from(
            temm1e_providers::create_provider(&provider_config)
                .map_err(|e| Temm1eError::Provider(e.to_string()))?,
        )
    };

    // 2. Create memory backend
    let memory_url = setup.config.memory.path.clone().unwrap_or_else(|| {
        let data_dir = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".temm1e");
        std::fs::create_dir_all(&data_dir).ok();
        format!("sqlite:{}/memory.db?mode=rwc", data_dir.display())
    });
    let memory: Arc<dyn temm1e_core::Memory> = Arc::from(
        temm1e_memory::create_memory_backend(&setup.config.memory.backend, &memory_url).await?,
    );

    // 3. Create workspace
    let workspace = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".temm1e")
        .join("workspace");
    std::fs::create_dir_all(&workspace).ok();

    // 4. Determine personality mode
    let initial_mode = match setup.mode.as_deref() {
        Some("work") => Temm1eMode::Work,
        Some("pro") => Temm1eMode::Pro,
        Some("none") => Temm1eMode::None,
        _ => Temm1eMode::Play, // "auto" and "play" both start as Play
    };
    let shared_mode: Arc<RwLock<Temm1eMode>> = Arc::new(RwLock::new(initial_mode));

    let tools = temm1e_tools::create_tools(
        &setup.config.tools,
        None, // No channel for tool output — TUI handles display
        None, // No pending messages
        Some(memory.clone()),
        None, // No setup link generator
        None, // No usage store for tools
        Some(shared_mode.clone()),
    );

    // 5. Build system prompt
    let system_prompt = Some(build_tui_system_prompt());

    // 6. Create agent runtime
    let agent = AgentRuntime::with_limits(
        provider,
        memory.clone(),
        tools,
        setup.model.clone(),
        system_prompt,
        setup.config.agent.max_turns,
        setup.config.agent.max_context_tokens,
        setup.config.agent.max_tool_rounds,
        setup.config.agent.max_task_duration_secs,
        setup.config.agent.max_spend_usd,
    )
    .with_v2_optimizations(setup.config.agent.v2_optimizations)
    .with_shared_mode(shared_mode);

    // 7. Set up channels
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<InboundMessage>(64);
    let (status_tx, status_rx) = watch::channel(AgentTaskStatus::default());

    // 8. Load conversation history
    let cli_history_key = "chat_history:tui".to_string();
    let history: Vec<temm1e_core::types::message::ChatMessage> =
        match memory.get(&cli_history_key).await {
            Ok(Some(entry)) => serde_json::from_str(&entry.content).unwrap_or_default(),
            _ => Vec::new(),
        };
    let history = Arc::new(Mutex::new(history));

    // 9. Spawn processing loop
    let history_clone = history.clone();
    let memory_clone = memory.clone();
    tokio::spawn(async move {
        while let Some(msg) = inbound_rx.recv().await {
            let current_history = history_clone.lock().await.clone();
            let mut session = SessionContext {
                session_id: "tui-tui".to_string(),
                user_id: msg.user_id.clone(),
                channel: msg.channel.clone(),
                chat_id: msg.chat_id.clone(),
                history: current_history.clone(),
                workspace_path: workspace.clone(),
            };

            // Create early reply channel for classifier acknowledgments
            let (early_tx, mut early_rx) = mpsc::unbounded_channel::<OutboundMessage>();
            let event_tx_early = event_tx.clone();
            tokio::spawn(async move {
                while let Some(early_msg) = early_rx.recv().await {
                    let _ = event_tx_early.send(Event::AgentResponse(AgentResponseEvent {
                        message: early_msg,
                        input_tokens: 0,
                        output_tokens: 0,
                        cost_usd: 0.0,
                    }));
                }
            });

            let result = agent
                .process_message(
                    &msg,
                    &mut session,
                    None,                    // interrupt
                    None,                    // pending
                    Some(early_tx),          // reply_tx (early replies)
                    Some(status_tx.clone()), // status_tx (real-time phase updates)
                    None,                    // cancel
                )
                .await;

            match result {
                Ok((reply, usage)) => {
                    // Send response to TUI
                    let _ = event_tx.send(Event::AgentResponse(AgentResponseEvent {
                        message: reply,
                        input_tokens: usage.input_tokens,
                        output_tokens: usage.output_tokens,
                        cost_usd: usage.total_cost_usd,
                    }));

                    // Update history
                    let mut hist = history_clone.lock().await;
                    *hist = session.history;

                    // Persist conversation history
                    if let Ok(json) = serde_json::to_string(&*hist) {
                        let entry = temm1e_core::MemoryEntry {
                            id: cli_history_key.clone(),
                            content: json,
                            metadata: serde_json::json!({"chat_id": "tui"}),
                            timestamp: chrono::Utc::now(),
                            session_id: Some("tui".to_string()),
                            entry_type: temm1e_core::MemoryEntryType::Conversation,
                        };
                        let _ = memory_clone.store(entry).await;
                    }
                }
                Err(e) => {
                    // Send error to TUI as a system message
                    let _ = event_tx.send(Event::AgentResponse(AgentResponseEvent {
                        message: OutboundMessage {
                            chat_id: "tui".to_string(),
                            text: format!("[Error: {}]", e),
                            reply_to: None,
                            parse_mode: None,
                        },
                        input_tokens: 0,
                        output_tokens: 0,
                        cost_usd: 0.0,
                    }));
                }
            }
        }
    });

    Ok(AgentHandle {
        inbound_tx,
        status_rx,
    })
}

/// Validate a provider key by making a minimal API call.
pub async fn validate_provider_key(
    provider_name: &str,
    api_key: &str,
    model: &str,
    base_url: Option<&str>,
) -> Result<(), String> {
    let config = temm1e_core::types::config::ProviderConfig {
        name: Some(provider_name.to_string()),
        api_key: Some(api_key.to_string()),
        keys: vec![api_key.to_string()],
        model: Some(model.to_string()),
        base_url: base_url.map(|s| s.to_string()),
        extra_headers: HashMap::new(),
    };

    let provider = temm1e_providers::create_provider(&config)
        .map_err(|e| format!("Failed to create provider: {}", e))?;

    let test_req = temm1e_core::types::message::CompletionRequest {
        model: model.to_string(),
        messages: vec![temm1e_core::types::message::ChatMessage {
            role: temm1e_core::types::message::Role::User,
            content: temm1e_core::types::message::MessageContent::Text("Hi".to_string()),
        }],
        tools: Vec::new(),
        max_tokens: Some(1),
        temperature: Some(0.0),
        system: None,
    };

    match provider.complete(test_req).await {
        Ok(_) => Ok(()),
        Err(e) => {
            let err_str = format!("{}", e);
            let err_lower = err_str.to_lowercase();
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
                // Non-auth errors mean the key IS valid
                Ok(())
            }
        }
    }
}

/// Build system prompt for the TUI mode.
fn build_tui_system_prompt() -> String {
    "You are TEMM1E, an AI agent running LOCALLY on the user's computer via an interactive terminal (TUI). \
     Your personal nickname is Tem. Your official name is TEMM1E. \
     Always refer to yourself as Tem.\n\n\
     CRITICAL CONTEXT: You are running directly on the user's local machine — NOT on a remote server. \
     You have full, direct access to the user's filesystem, shell, and local applications. \
     When the user asks you to open a file, run a command, or interact with their system, \
     JUST DO IT — use the shell tool directly. Do not ask for confirmation or explain what you will do. \
     Act like a local assistant that has already been given permission.\n\n\
     You have full access to these tools:\n\
     - shell: run any command on the user's machine (e.g. `open file.html`, `ls`, `git status`)\n\
     - file_read / file_write / file_list: filesystem operations\n\
     - web_fetch: HTTP GET requests\n\
     - browser: control a real Chrome browser (navigate, click, type, screenshot, get_text, evaluate JS, get_html)\n\
     - send_message: send real-time messages to the user during long tasks\n\
     - memory_manage: your persistent knowledge store (remember/recall/forget/update/list)\n\n\
     KEY RULES:\n\
     - Shell output (stdout/stderr) is NOT visible to the user. Only YOUR \
       final text reply and send_message calls reach the user.\n\
     - To open files/URLs on macOS: use `open <path>` or `open <url>` via the shell tool.\n\
     - To open files/URLs on Linux: use `xdg-open <path>` via the shell tool.\n\
     - Be concise and helpful. Format responses with markdown.\n\
     - When executing multi-step tasks, call send_message to provide real-time progress updates.\n\
     - After finishing browser work, call browser with action 'close' to shut it down."
        .to_string()
}
