//! Agent bridge — sets up AgentRuntime and runs the message processing loop.
//!
//! This module handles all the complexity of creating the provider, memory backend,
//! tools, and agent runtime, then runs a background task that processes inbound
//! messages and sends responses + status updates back to the TUI event loop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
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

/// Read the `[hive] enabled` flag directly from the config TOML file
/// (mirrors the pattern in main.rs since HiveConfig is not part of
/// the main Temm1eConfig struct — circular-dep constraint).
///
/// v5.5.0: default-ON to match `HiveConfig::default()` at
/// `temm1e_hive/src/config.rs:64`. Explicit opt-out via
/// `[hive] enabled = false` still works.
fn read_hive_enabled() -> bool {
    #[derive(serde::Deserialize, Default)]
    struct HC {
        #[serde(default)]
        hive: HE,
    }
    #[derive(serde::Deserialize)]
    struct HE {
        #[serde(default = "hive_default_enabled_tui")]
        enabled: bool,
    }
    impl Default for HE {
        fn default() -> Self {
            Self {
                enabled: hive_default_enabled_tui(),
            }
        }
    }
    fn hive_default_enabled_tui() -> bool {
        true
    }
    dirs::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".temm1e/config.toml")).ok())
        .or_else(|| std::fs::read_to_string("temm1e.toml").ok())
        .and_then(|c| toml::from_str::<HC>(&c).ok())
        .map(|c| c.hive.enabled)
        .unwrap_or(true)
}

fn read_hive_config() -> temm1e_hive::HiveConfig {
    #[derive(serde::Deserialize, Default)]
    struct HW {
        #[serde(default)]
        hive: temm1e_hive::HiveConfig,
    }
    dirs::home_dir()
        .and_then(|h| std::fs::read_to_string(h.join(".temm1e/config.toml")).ok())
        .or_else(|| std::fs::read_to_string("temm1e.toml").ok())
        .and_then(|c| toml::from_str::<HW>(&c).ok())
        .map(|w| w.hive)
        .unwrap_or_default()
}

/// Everything needed to communicate with the running agent.
pub struct AgentHandle {
    /// Send user messages to the agent processing loop.
    pub inbound_tx: mpsc::Sender<InboundMessage>,
    /// Watch channel for real-time status updates.
    pub status_rx: watch::Receiver<AgentTaskStatus>,
    /// Cancel signal — set to true from the TUI on Escape/Ctrl+C while
    /// the agent is working. The agent loop polls this between rounds
    /// (`runtime.rs:927`) and emits `AgentTaskPhase::Interrupted`.
    /// Reset to false at the start of each new message.
    pub interrupt_flag: Arc<AtomicBool>,
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
            // Proxy providers use lenient placeholder check so short LM Studio /
            // Ollama keys survive TUI agent spawn.
            let has_custom = burl.is_some();
            let valid: Vec<String> = keys
                .into_iter()
                .filter(|k| {
                    if has_custom {
                        !credentials::is_placeholder_key_lenient(k)
                    } else {
                        !credentials::is_placeholder_key(k)
                    }
                })
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

    // ── Vault (encrypted credential store) for TUI ──
    let tui_vault: Option<Arc<dyn temm1e_core::Vault>> = match temm1e_vault::LocalVault::new().await
    {
        Ok(v) => {
            tracing::info!("Vault initialized (TUI)");
            Some(Arc::new(v) as Arc<dyn temm1e_core::Vault>)
        }
        Err(e) => {
            tracing::warn!(error = %e, "TUI: vault init failed — browser authenticate disabled");
            None
        }
    };

    // ── Skill registry for TUI ──
    let tui_skill_registry: Arc<tokio::sync::RwLock<temm1e_skills::SkillRegistry>> = {
        let workspace_for_skills = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
        let mut reg = temm1e_skills::SkillRegistry::new(workspace_for_skills);
        if let Err(e) = reg.load_skills().await {
            tracing::warn!(error = %e, "TUI: failed to load skills");
        }
        let count = reg.list_skills().len();
        if count > 0 {
            tracing::info!(count, "Skills loaded (TUI)");
        }
        Arc::new(tokio::sync::RwLock::new(reg))
    };

    let mut tools = temm1e_tools::create_tools(
        &setup.config.tools,
        None, // No channel for tool output — TUI handles display
        None, // No pending messages
        Some(memory.clone()),
        None, // No setup link generator
        None, // No usage store for tools
        Some(shared_mode.clone()),
        tui_vault.clone(),
        Some(tui_skill_registry.clone()),
    );

    // ── Custom script tools (user/agent-authored) ──
    let tui_custom_tool_registry = Arc::new(temm1e_tools::CustomToolRegistry::new());
    {
        let custom_tools = tui_custom_tool_registry.load_tools();
        if !custom_tools.is_empty() {
            tracing::info!(
                count = custom_tools.len(),
                "Custom script tools loaded (TUI)"
            );
            tools.extend(custom_tools);
        }
        tools.push(Arc::new(temm1e_tools::SelfCreateTool::new(
            tui_custom_tool_registry.clone(),
        )));
    }

    // ── MCP servers (external tool sources) ──
    #[cfg(feature = "mcp")]
    {
        let mgr = Arc::new(temm1e_mcp::McpManager::new());
        mgr.connect_all().await;
        let tool_names: Vec<String> = tools.iter().map(|t| t.name().to_string()).collect();
        let mcp_tools = mgr.bridge_tools(&tool_names).await;
        if !mcp_tools.is_empty() {
            tracing::info!(count = mcp_tools.len(), "MCP bridge tools loaded (TUI)");
            tools.extend(mcp_tools);
        }
        tools.push(Arc::new(temm1e_mcp::McpManageTool::new(mgr.clone())));
        tools.push(Arc::new(temm1e_mcp::SelfExtendTool::new()));
        tools.push(Arc::new(temm1e_mcp::SelfAddMcpTool::new(mgr.clone())));
        tracing::info!("Loaded MCP config (TUI)");
    }

    // ── TemDOS core registry (specialist sub-agents) ──
    let tui_core_registry = {
        let mut registry = temm1e_cores::CoreRegistry::new();
        let ws_path = dirs::home_dir()
            .map(|h| h.join(".temm1e"))
            .unwrap_or_default();
        registry
            .load(Some(ws_path.as_path()))
            .await
            .unwrap_or_else(|e| {
                tracing::warn!(error = %e, "TUI: failed to load TemDOS cores");
            });
        if !registry.is_empty() {
            tracing::info!(count = registry.len(), "TemDOS cores loaded (TUI)");
        }
        Arc::new(tokio::sync::RwLock::new(registry))
    };

    // ── TemDOS invoke_core tool (provider is already known at this point) ──
    if !tui_core_registry.read().await.is_empty() {
        let model_pricing =
            temm1e_agent::budget::get_pricing_with_custom(&setup.provider_name, &setup.model);
        let invoke_core = temm1e_cores::InvokeCoreTool::new(
            tui_core_registry.clone(),
            provider.clone(),
            tools.clone(),
            Arc::new(temm1e_agent::budget::BudgetTracker::new(
                setup.config.agent.max_spend_usd,
            )),
            model_pricing,
            setup.model.clone(),
            setup.config.agent.max_context_tokens,
            memory.clone(),
        );
        tools.push(Arc::new(invoke_core));
        tracing::info!("TemDOS invoke_core tool registered (TUI)");
    }

    // ── Perpetuum (P1 critical — default-on per config) ──
    let tui_perp_temporal: Arc<RwLock<String>> = Arc::new(RwLock::new(String::new()));
    let tui_perpetuum: Option<Arc<temm1e_perpetuum::Perpetuum>> = if setup.config.perpetuum.enabled
    {
        let perp_db = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".temm1e/perpetuum.db");
        let db_url = format!("sqlite:{}?mode=rwc", perp_db.display());
        let perp_config = temm1e_perpetuum::PerpetualConfig {
            enabled: true,
            timezone: setup.config.perpetuum.timezone.clone(),
            max_concerns: setup.config.perpetuum.max_concerns,
            conscience: temm1e_perpetuum::ConscienceConfig {
                idle_threshold_secs: setup
                    .config
                    .perpetuum
                    .conscience_idle_threshold_secs
                    .unwrap_or(900),
                dream_threshold_secs: setup
                    .config
                    .perpetuum
                    .conscience_dream_threshold_secs
                    .unwrap_or(3600),
            },
            cognitive: temm1e_perpetuum::CognitiveConfig {
                review_every_n_checks: setup.config.perpetuum.review_every_n_checks,
                interpret_changes: true,
            },
            volition: temm1e_perpetuum::VolitionConfig {
                enabled: setup.config.perpetuum.volition_enabled,
                interval_secs: setup.config.perpetuum.volition_interval_secs,
                max_actions_per_cycle: setup.config.perpetuum.volition_max_actions,
                event_triggered: true,
            },
        };
        // Channel map — TUI doesn't route Perpetuum notifications to a
        // specific channel; we use an empty map (Perpetuum logs internally).
        let channel_map: Arc<HashMap<String, Arc<dyn temm1e_core::Channel>>> =
            Arc::new(HashMap::new());
        match temm1e_perpetuum::Perpetuum::new(
            perp_config,
            provider.clone(),
            setup.model.clone(),
            channel_map,
            &db_url,
        )
        .await
        {
            Ok(p) => {
                let p = Arc::new(p);
                let perp_tools = p.tools();
                tracing::info!(count = perp_tools.len(), "Perpetuum tools loaded (TUI)");
                tools.extend(perp_tools);
                p.start();
                tracing::info!("Perpetuum runtime started (TUI)");
                // Populate initial temporal string so the first turn gets it.
                let initial_temporal = p.temporal_injection("default").await;
                *tui_perp_temporal.write().await = initial_temporal;
                Some(p)
            }
            Err(e) => {
                tracing::warn!(error = %e, "TUI: Perpetuum init failed");
                None
            }
        }
    } else {
        None
    };

    // ── Eigen-Tune engine (opt-in) ──
    let tui_eigentune_cfg: temm1e_distill::config::EigenTuneConfig = {
        #[derive(serde::Deserialize, Default)]
        struct ETWrapper {
            #[serde(default)]
            eigentune: temm1e_distill::config::EigenTuneConfig,
        }
        dirs::home_dir()
            .and_then(|h| std::fs::read_to_string(h.join(".temm1e/config.toml")).ok())
            .or_else(|| std::fs::read_to_string("temm1e.toml").ok())
            .and_then(|c| toml::from_str::<ETWrapper>(&c).ok())
            .map(|w| w.eigentune)
            .unwrap_or_default()
    };
    let tui_eigen_tune_engine: Option<Arc<temm1e_distill::EigenTuneEngine>> =
        if tui_eigentune_cfg.enabled {
            let et_db = dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".temm1e/eigentune.db");
            let et_url = format!("sqlite:{}?mode=rwc", et_db.display());
            match temm1e_distill::EigenTuneEngine::new(&tui_eigentune_cfg, &et_url).await {
                Ok(engine) => {
                    tracing::info!("Eigen-Tune: engine initialized (TUI)");
                    Some(Arc::new(engine))
                }
                Err(e) => {
                    tracing::warn!(error = %e, "TUI: Eigen-Tune init failed");
                    None
                }
            }
        } else {
            None
        };

    // ── Load personality for TUI (matches server/CLI pattern) ──
    let tui_personality = Arc::new(temm1e_anima::personality::PersonalityConfig::load(
        &dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".temm1e"),
    ));

    // ── Social intelligence: user profile storage ──
    let tui_social_config = setup.config.social.clone();
    let tui_social_storage: Option<Arc<temm1e_anima::SocialStorage>> = if tui_social_config.enabled
    {
        let social_db_url = format!(
            "sqlite:{}/social.db?mode=rwc",
            dirs::home_dir()
                .unwrap_or_else(|| PathBuf::from("."))
                .join(".temm1e")
                .display()
        );
        match temm1e_anima::SocialStorage::new(&social_db_url).await {
            Ok(s) => {
                tracing::info!("Social intelligence initialized (TUI)");
                Some(Arc::new(s))
            }
            Err(e) => {
                tracing::warn!(error = %e, "TUI: failed to init social storage");
                None
            }
        }
    } else {
        None
    };

    // ── Shared memory strategy handle (for /memory lambda command) ──
    // Default to Lambda to match server/CLI.
    let shared_memory_strategy: Arc<RwLock<temm1e_core::types::config::MemoryStrategy>> = Arc::new(
        RwLock::new(temm1e_core::types::config::MemoryStrategy::Lambda),
    );

    // ── JIT spawn_swarm tool registration (TUI path) ──
    // Per feedback_interactive_interface_parity: TUI must have feature
    // parity with server/CLI for every interactive interface. The tool
    // snapshot captured here (before spawn_swarm is pushed) is what
    // workers see — plus the per-runtime tool_filter blocks recursion.
    let tui_hive_enabled = read_hive_enabled();
    let tui_swarm_snapshot = tools.clone();
    let tui_swarm_handle: Option<temm1e_agent::spawn_swarm::SwarmHandle> = if tui_hive_enabled {
        let h = temm1e_agent::spawn_swarm::SpawnSwarmTool::fresh_handle();
        tools.push(Arc::new(temm1e_agent::spawn_swarm::SpawnSwarmTool::new(
            h.clone(),
        )));
        tracing::info!("JIT spawn_swarm tool registered (TUI, context deferred)");
        Some(h)
    } else {
        None
    };

    // ── Hive pack initialization for TUI ──
    let tui_hive_instance: Option<Arc<temm1e_hive::Hive>> = if tui_hive_enabled {
        let hive_config = read_hive_config();
        let hive_db = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".temm1e/hive.db");
        let hive_url = format!("sqlite:{}?mode=rwc", hive_db.display());
        match temm1e_hive::Hive::new(&hive_config, &hive_url).await {
            Ok(h) => {
                tracing::info!(
                    max_workers = hive_config.max_workers,
                    threshold = hive_config.swarm_threshold_speedup,
                    "Many Tems initialized (TUI Swarm Intelligence)"
                );
                Some(Arc::new(h))
            }
            Err(e) => {
                tracing::warn!(error = %e, "TUI Hive init failed — JIT swarm disabled");
                None
            }
        }
    } else {
        None
    };

    // 5. Build system prompt
    let system_prompt = Some(build_tui_system_prompt());

    // 6. Create agent runtime
    let mut agent = AgentRuntime::with_limits(
        provider.clone(),
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
    .with_parallel_phases(setup.config.agent.parallel_phases)
    .with_hive_enabled(tui_hive_enabled)
    .with_shared_mode(shared_mode.clone())
    .with_shared_memory_strategy(shared_memory_strategy.clone())
    .with_personality(tui_personality)
    .with_social(
        tui_social_storage.clone(),
        if tui_social_config.enabled {
            Some(tui_social_config.clone())
        } else {
            None
        },
    );

    // ── Consciousness: enable LLM-powered observer if configured ──
    if setup.config.consciousness.enabled {
        let consciousness_cfg = temm1e_agent::consciousness::ConsciousnessConfig {
            enabled: true,
            confidence_threshold: setup.config.consciousness.confidence_threshold,
            max_interventions_per_session: setup.config.consciousness.max_interventions_per_session,
            observation_mode: setup.config.consciousness.observation_mode.clone(),
        };
        agent = agent.with_consciousness(
            temm1e_agent::consciousness_engine::ConsciousnessEngine::new(
                consciousness_cfg,
                provider.clone(),
                setup.model.clone(),
            ),
        );
        tracing::info!("Tem Conscious initialized (TUI)");
    }

    // ── Perpetuum temporal context (if Perpetuum initialized) ──
    if tui_perpetuum.is_some() {
        agent = agent.with_perpetuum_temporal(tui_perp_temporal.clone());
    }

    // ── Eigen-Tune engine (if configured) ──
    if let Some(et) = tui_eigen_tune_engine.clone() {
        agent = agent.with_eigen_tune(et, tui_eigentune_cfg.enable_local_routing);
    }

    // ── Witness attachments (if enabled; no-op otherwise) ──
    let tui_witness_attachments =
        match temm1e_agent::witness_init::build_witness_attachments(&setup.config.witness).await {
            Ok(a) => {
                if a.is_some() {
                    tracing::info!(
                        strictness = %setup.config.witness.strictness,
                        auto_planner_oath = setup.config.witness.auto_planner_oath,
                        "Witness enabled (TUI)"
                    );
                }
                a
            }
            Err(e) => {
                tracing::warn!(error = %e, "Witness init failed — continuing without Witness");
                None
            }
        };
    agent = agent.with_witness_attachments(tui_witness_attachments.as_ref());

    // ── Fill JIT spawn_swarm handle (TUI path) ──
    // Context uses the tool snapshot captured before spawn_swarm was
    // pushed — workers physically cannot see it (recursion block).
    if let (Some(hive), Some(handle)) = (tui_hive_instance.as_ref(), tui_swarm_handle.as_ref()) {
        let ctx = temm1e_agent::spawn_swarm::SpawnSwarmContext {
            hive: Arc::clone(hive),
            provider: agent.provider_arc(),
            memory: memory.clone(),
            tools_template: tui_swarm_snapshot.clone(),
            model: agent.model().to_string(),
            parent_budget: Arc::new(temm1e_agent::budget::BudgetTracker::new(
                setup.config.agent.max_spend_usd,
            )),
            cancel: tokio_util::sync::CancellationToken::new(),
        };
        *handle.write().await = Some(ctx);
        tracing::info!("JIT spawn_swarm context wired (TUI)");
    }

    let agent = agent; // freeze mutability

    // 7. Set up channels
    let (inbound_tx, mut inbound_rx) = mpsc::channel::<InboundMessage>(64);
    let (status_tx, status_rx) = watch::channel(AgentTaskStatus::default());
    let interrupt_flag = Arc::new(AtomicBool::new(false));
    let interrupt_for_task = interrupt_flag.clone();

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
            // CRITICAL: reset the interrupt flag before each turn.
            // If a previous turn was cancelled and we didn't reset,
            // the new turn would cancel immediately. (Tier C.)
            interrupt_for_task.store(false, Ordering::Relaxed);

            let current_history = history_clone.lock().await.clone();
            let mut session = SessionContext {
                session_id: "tui-tui".to_string(),
                user_id: msg.user_id.clone(),
                channel: msg.channel.clone(),
                chat_id: msg.chat_id.clone(),
                role: temm1e_core::types::rbac::Role::Admin,
                history: current_history.clone(),
                workspace_path: workspace.clone(),
                read_tracker: std::sync::Arc::new(tokio::sync::RwLock::new(
                    std::collections::HashSet::new(),
                )),
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
                    Some(interrupt_for_task.clone()), // Tier C: real interrupt flag
                    None,                             // pending
                    Some(early_tx),                   // reply_tx (early replies)
                    Some(status_tx.clone()),          // status_tx (real-time phase updates)
                    None,                             // cancel (reserved for v4.9.0)
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
        interrupt_flag,
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

    // Custom endpoint — skip test completion.
    // See src/main.rs::validate_provider_key for full rationale.
    if base_url.is_some() {
        tracing::debug!(
            base_url = ?base_url,
            model = %model,
            "Skipping TUI validate_provider_key test call — custom base_url set"
        );
        return Ok(());
    }

    let test_req = temm1e_core::types::message::CompletionRequest {
        model: model.to_string(),
        messages: vec![temm1e_core::types::message::ChatMessage {
            role: temm1e_core::types::message::Role::User,
            content: temm1e_core::types::message::MessageContent::Text("Hi".to_string()),
        }],
        tools: Vec::new(),
        // Per project rule (feedback_no_max_tokens): no hardcoded output caps.
        // This is a key-validation probe — model will respond with a short
        // ack anyway. The cost is minimal compared to correctness.
        max_tokens: None,
        temperature: Some(0.0),
        system: None,
        system_volatile: None,
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
