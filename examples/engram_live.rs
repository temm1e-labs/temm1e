//! LIVE Engram end-to-end test against a real LLM.
//!
//! Verifies the in-loop write path + permanent-block injection with a real
//! provider (no mocks):
//!   Turn 1: "remember my birthday is March 2 1994" → agent calls the `engram`
//!           tool → fact persisted.
//!   Turn 2: "when is my birthday?" → the permanent block is injected → the
//!           agent answers from memory (no re-asking).
//!
//! Run:  OPENAI_API_KEY=$(grep '^OPENAI_API_KEY=' .env | cut -d= -f2-) \
//!         cargo run --example engram_live
//!
//! Exits non-zero if the behavior is wrong, so it doubles as a smoke test.

use std::collections::HashSet;
use std::sync::Arc;

use temm1e_agent::AgentRuntime;
use temm1e_core::types::config::ProviderConfig;
use temm1e_core::types::message::InboundMessage;
use temm1e_core::types::rbac::Role;
use temm1e_core::types::session::SessionContext;
use temm1e_core::Memory;

fn msg(text: &str) -> InboundMessage {
    InboundMessage {
        id: format!("m{}", text.len()),
        channel: "cli".to_string(),
        chat_id: "live-chat".to_string(),
        user_id: "live-user".to_string(),
        username: Some("tester".to_string()),
        text: Some(text.to_string()),
        attachments: Vec::new(),
        reply_to: None,
        timestamp: chrono::Utc::now(),
    }
}

#[tokio::main]
async fn main() {
    let _ = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::WARN)
        .try_init();

    let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if key.trim().is_empty() {
        eprintln!("OPENAI_API_KEY not set — cannot run the live test.");
        std::process::exit(2);
    }
    let model = std::env::var("ENGRAM_LIVE_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let provider = temm1e_providers::create_provider(&ProviderConfig {
        name: Some("openai".to_string()),
        api_key: Some(key),
        keys: Vec::new(),
        model: Some(model.clone()),
        base_url: None,
        extra_headers: Default::default(),
    })
    .expect("create provider");
    let provider: Arc<dyn temm1e_core::Provider> = Arc::from(provider);

    let memory: Arc<dyn Memory> = Arc::new(
        temm1e_memory::SqliteMemory::new("sqlite::memory:")
            .await
            .expect("sqlite mem"),
    );

    let engram_tool: Arc<dyn temm1e_core::Tool> =
        Arc::new(temm1e_tools::EngramTool::new(Arc::clone(&memory)));

    let runtime = AgentRuntime::new(
        Arc::clone(&provider),
        Arc::clone(&memory),
        vec![engram_tool],
        model,
        Some("You are Tem, a helpful assistant. Use your tools when appropriate.".to_string()),
    )
    .with_v2_optimizations(false);

    let mut session = SessionContext {
        session_id: "cli-live-chat".to_string(),
        user_id: "live-user".to_string(),
        channel: "cli".to_string(),
        chat_id: "live-chat".to_string(),
        role: Role::Admin,
        history: Vec::new(),
        workspace_path: std::path::PathBuf::from("."),
        read_tracker: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
    };

    // ── Turn 1: ask Tem to remember a durable fact ──────────────────
    println!("\n=== TURN 1: 'remember my birthday is March 2, 1994' ===");
    let (reply1, _u1) = runtime
        .process_message(
            &msg("Please remember that my birthday is March 2, 1994."),
            &mut session,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("turn 1");
    println!("Tem: {}", reply1.text);

    // ── Inspect what was actually persisted ─────────────────────────
    let stored = memory
        .engram_recall("birthday", "", "live-chat", 10)
        .await
        .expect("recall");
    println!("\n=== STORED ENGRAM FACTS ({}) ===", stored.len());
    for f in &stored {
        println!(
            "  - id={} scope={:?} pin={:?} imp={} :: {}",
            f.id, f.scope, f.pinned_by, f.importance, f.content
        );
    }
    let wrote_fact = stored.iter().any(|f| f.content.contains("1994"));

    // ── Turn 2: a fresh question that needs the permanent block ─────
    println!("\n=== TURN 2: 'when is my birthday?' (must use injected memory) ===");
    let (reply2, _u2) = runtime
        .process_message(
            &msg("When is my birthday?"),
            &mut session,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("turn 2");
    println!("Tem: {}", reply2.text);

    let recalled = reply2.text.contains("1994")
        && (reply2.text.contains("March") || reply2.text.contains("3/2") || reply2.text.contains("2"));

    println!("\n=== VERDICT ===");
    println!("  wrote permanent fact via tool : {}", if wrote_fact { "PASS" } else { "FAIL" });
    println!("  recalled it on turn 2          : {}", if recalled { "PASS" } else { "FAIL" });

    if wrote_fact && recalled {
        println!("\nLIVE ENGRAM TEST: PASS ✅");
    } else {
        eprintln!("\nLIVE ENGRAM TEST: FAIL ❌");
        std::process::exit(1);
    }
}
