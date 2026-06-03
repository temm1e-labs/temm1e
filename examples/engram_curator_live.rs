//! LIVE test of the Engram **curator** (auto-capture) against a real LLM.
//!
//! Turn 1 states a durable preference WITHOUT saying "remember". The gated
//! background curator should extract + persist it (Agent-pinned). After a short
//! wait, turn 2 asks about it and should answer from the injected block.
//!
//! Run: OPENAI_API_KEY=$(grep '^OPENAI_API_KEY=' .env | cut -d= -f2-) \
//!        cargo run --example engram_curator_live

use std::collections::HashSet;
use std::sync::Arc;

use temm1e_agent::AgentRuntime;
use temm1e_core::types::config::ProviderConfig;
use temm1e_core::types::message::InboundMessage;
use temm1e_core::types::rbac::Role;
use temm1e_core::types::session::SessionContext;
use temm1e_core::{Memory, PinnedBy};

fn msg(id: &str, text: &str) -> InboundMessage {
    InboundMessage {
        id: id.to_string(),
        channel: "cli".to_string(),
        chat_id: "cur-chat".to_string(),
        user_id: "cur-user".to_string(),
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
        .with_max_level(tracing::Level::INFO)
        .try_init();

    let key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
    if key.trim().is_empty() {
        eprintln!("OPENAI_API_KEY not set — cannot run.");
        std::process::exit(2);
    }
    let model = std::env::var("ENGRAM_LIVE_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());

    let provider: Arc<dyn temm1e_core::Provider> = Arc::from(
        temm1e_providers::create_provider(&ProviderConfig {
            name: Some("openai".to_string()),
            api_key: Some(key),
            keys: Vec::new(),
            model: Some(model.clone()),
            base_url: None,
            extra_headers: Default::default(),
        })
        .expect("provider"),
    );
    let memory: Arc<dyn Memory> = Arc::new(
        temm1e_memory::SqliteMemory::new("sqlite::memory:")
            .await
            .expect("mem"),
    );
    let tool: Arc<dyn temm1e_core::Tool> =
        Arc::new(temm1e_tools::EngramTool::new(Arc::clone(&memory)));
    let runtime = AgentRuntime::new(
        Arc::clone(&provider),
        Arc::clone(&memory),
        vec![tool],
        model,
        Some("You are Tem, a helpful assistant.".to_string()),
    )
    .with_v2_optimizations(false);

    let mut session = SessionContext {
        session_id: "cli-cur-chat".to_string(),
        user_id: "cur-user".to_string(),
        channel: "cli".to_string(),
        chat_id: "cur-chat".to_string(),
        role: Role::Admin,
        history: Vec::new(),
        workspace_path: std::path::PathBuf::from("."),
        read_tracker: Arc::new(tokio::sync::RwLock::new(HashSet::new())),
    };

    // Turn 1: durable preference, NO "remember" keyword.
    println!("\n=== TURN 1 (no 'remember'): states a durable preference ===");
    let (r1, _) = runtime
        .process_message(
            &msg(
                "m1",
                "By the way, for all my side projects I always deploy on Lambda Labs GPUs — I never use RunPod.",
            ),
            &mut session,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("turn1");
    println!("Tem: {}", r1.text);

    // Wait for the detached background curator to finish its LLM call + store.
    println!("\n... waiting 7s for the background curator ...");
    tokio::time::sleep(std::time::Duration::from_secs(7)).await;

    let facts = memory
        .engram_recall("deploy", "", "cur-chat", 10)
        .await
        .unwrap_or_default();
    let facts2 = memory.engram_recall("Lambda", "", "cur-chat", 10).await.unwrap_or_default();
    let mut all = facts;
    all.extend(facts2);
    all.dedup_by(|a, b| a.id == b.id);
    println!("\n=== CAPTURED FACTS ({}) ===", all.len());
    for f in &all {
        println!("  - pin={:?} type={:?} imp={} :: {}", f.pinned_by, f.fact_type, f.importance, f.content);
    }
    let captured = all.iter().any(|f| f.content.to_lowercase().contains("lambda"));
    let by_curator = all.iter().any(|f| f.pinned_by == PinnedBy::Agent);

    // Turn 2: ask — must answer from the injected permanent block.
    println!("\n=== TURN 2: 'which GPU provider do I use for my projects?' ===");
    let (r2, _) = runtime
        .process_message(
            &msg("m2", "Which GPU provider do I use for my projects?"),
            &mut session,
            None,
            None,
            None,
            None,
            None,
        )
        .await
        .expect("turn2");
    println!("Tem: {}", r2.text);
    let recalled = r2.text.to_lowercase().contains("lambda");

    println!("\n=== VERDICT ===");
    println!("  durable fact captured (no 'remember'): {}", if captured { "PASS" } else { "FAIL" });
    println!("  captured by curator (Agent pin)?       : {}", if by_curator { "yes" } else { "no (tool captured it)" });
    println!("  recalled on turn 2                     : {}", if recalled { "PASS" } else { "FAIL" });
    if captured && recalled {
        println!("\nLIVE CURATOR TEST: PASS ✅");
    } else {
        eprintln!("\nLIVE CURATOR TEST: FAIL ❌");
        std::process::exit(1);
    }
}
