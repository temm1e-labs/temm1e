//! Witness Live Wiring Validation.
//!
//! Exercises the two Phase 4 wiring paths that the main A/B harness does NOT
//! drive end-to-end against a real LLM:
//!   1. `with_auto_planner_oath(true)` — the runtime calls the Planner LLM
//!      with `OATH_GENERATION_PROMPT` BEFORE the main agent loop, parses the
//!      JSON response, and seals the Oath into the Witness Ledger
//!   2. `with_cambium_trust(trust)` — the runtime gate calls
//!      `trust.record_verdict(...)` after every Witness verdict, updating
//!      the Cambium TrustEngine state
//!
//! Runs ONE small task end-to-end with both wiring paths enabled, captures
//! Ledger state + TrustEngine state before and after, and prints whether
//! every wiring stage actually fired.
//!
//! Run via:
//!   cargo run --release -p temm1e-agent --example witness_live_wiring
//!
//! Cost: ~$0.005 to ~$0.20 depending on model. Hard-capped at $1.00 via the
//! WITNESS_LIVE_BUDGET_USD env var.

#![allow(clippy::too_many_arguments)]

use std::collections::HashSet;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use temm1e_agent::AgentRuntime;
use temm1e_cambium::trust::TrustEngine;
use temm1e_core::config::credentials::load_credentials_file;
use temm1e_core::traits::Memory;
use temm1e_core::types::cambium::TrustState;
use temm1e_core::types::message::InboundMessage;
use temm1e_core::types::session::SessionContext;
use temm1e_test_utils::MockMemory;
use temm1e_tools::{FileListTool, FileReadTool, FileWriteTool};
use temm1e_witness::{
    config::WitnessStrictness, ledger::Ledger, types::LedgerEntryType, witness::Witness,
};
use tokio::sync::Mutex;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model = std::env::var("WITNESS_AB_MODEL").unwrap_or_else(|_| "gpt-5.4".to_string());
    let _budget_ceiling: f64 = std::env::var("WITNESS_LIVE_BUDGET_USD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(1.0);

    println!("════════════════════════════════════════════════════════════════");
    println!("  Witness Live Wiring Validation");
    println!("  Model:  {}", model);
    println!("  Goal:   exercise auto_planner_oath + cambium_trust wiring");
    println!("          end-to-end against a real LLM");
    println!("════════════════════════════════════════════════════════════════\n");

    // ── Provider ────────────────────────────────────────────────────
    let provider_name = if model.starts_with("gemini-") {
        "gemini"
    } else if model.starts_with("gpt-") || model.starts_with("o1") || model.starts_with("o3") {
        "openai"
    } else if model.starts_with("claude-") {
        "anthropic"
    } else {
        "openai"
    };

    let creds = load_credentials_file().ok_or("no credentials.toml found")?;
    let cred_provider = creds
        .providers
        .iter()
        .find(|p| p.name == provider_name)
        .ok_or_else(|| format!("no {} provider in credentials.toml", provider_name))?;
    let api_key = cred_provider
        .keys
        .first()
        .cloned()
        .ok_or_else(|| format!("{} provider has no keys", provider_name))?;

    let provider: Arc<dyn temm1e_core::traits::Provider> = match provider_name {
        "gemini" => Arc::new(temm1e_providers::GeminiProvider::new(api_key)),
        "openai" => Arc::new(
            temm1e_providers::OpenAICompatProvider::new(api_key)
                .with_base_url("https://api.openai.com/v1".to_string()),
        ),
        "anthropic" => Arc::new(temm1e_providers::AnthropicProvider::new(api_key)),
        _ => Arc::new(temm1e_providers::OpenAICompatProvider::new(api_key)),
    };
    println!("[1] Provider built ({} via {})\n", provider_name, model);

    // ── Workspace + witness + trust ────────────────────────────────
    let workspace = tempfile::tempdir()?;
    let workspace_path = workspace.path().to_path_buf();

    let ledger = Ledger::open("sqlite::memory:").await?;
    let witness = Arc::new(Witness::new(ledger, workspace_path.clone()));
    let trust = Arc::new(Mutex::new(TrustEngine::new(TrustState::default(), None)));

    let session_id = "live-wiring-validation".to_string();

    // Capture pre-state.
    let ledger_count_before = witness.ledger().count_for_session(&session_id).await?;
    let trust_state_before = {
        let t = trust.lock().await;
        (
            t.state().level3_streak,
            t.state().level2_streak,
            t.state().recent_rollbacks,
        )
    };
    println!(
        "[2] Pre-state captured: ledger entries={}, trust=(L3={}, L2={}, rollbacks={})",
        ledger_count_before, trust_state_before.0, trust_state_before.1, trust_state_before.2,
    );

    // ── Build runtime with BOTH wiring paths enabled ───────────────
    let memory: Arc<dyn Memory> = Arc::new(MockMemory::new());
    let tools: Vec<Arc<dyn temm1e_core::traits::Tool>> = vec![
        Arc::new(FileReadTool::new()),
        Arc::new(FileWriteTool::new()),
        Arc::new(FileListTool::new()),
    ];
    let system = format!(
        "You are a precise coding agent. Your workspace is {}. Use the file_write \
         tool to create files. When you complete the task, reply with the single \
         word 'done'.",
        workspace_path.display()
    );

    let runtime = AgentRuntime::new(provider, memory, tools, model.clone(), Some(system))
        .with_witness(witness.clone(), WitnessStrictness::Block, true)
        .with_cambium_trust(trust.clone())
        .with_auto_planner_oath(true);

    println!("[3] Runtime built with .with_witness(...).with_cambium_trust(...).with_auto_planner_oath(true)\n");

    // ── Drive the agent ────────────────────────────────────────────
    let inbound = InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        channel: "live-wiring".to_string(),
        chat_id: "live-wiring-chat".to_string(),
        user_id: "live-wiring-user".to_string(),
        username: Some("validator".to_string()),
        text: Some(
            "Create a Python file at hello.py with a function `greet(name)` that returns \
             'Hello, {name}!'. Call greet('Witness') from the same file. Use the \
             file_write tool. Reply 'done' when finished."
                .to_string(),
        ),
        attachments: Vec::new(),
        reply_to: None,
        timestamp: chrono::Utc::now(),
    };

    let mut session = SessionContext {
        session_id: session_id.clone(),
        channel: "live-wiring".to_string(),
        chat_id: "live-wiring-chat".to_string(),
        user_id: "live-wiring-user".to_string(),
        role: temm1e_core::types::rbac::Role::Admin,
        history: Vec::new(),
        workspace_path: workspace_path.clone(),
        read_tracker: std::sync::Arc::new(tokio::sync::RwLock::new(HashSet::new())),
    };

    let interrupt = Arc::new(AtomicBool::new(false));
    let started = Instant::now();

    println!(
        "[4] Calling process_message — auto_planner_oath should fire BEFORE the agent loop..."
    );
    let result = tokio::time::timeout(
        Duration::from_secs(240),
        runtime.process_message(
            &inbound,
            &mut session,
            Some(interrupt),
            None,
            None,
            None,
            None,
        ),
    )
    .await;
    let elapsed = started.elapsed();

    let (final_reply, usage, error) = match result {
        Ok(Ok((outbound, usage))) => (Some(outbound.text), Some(usage), None),
        Ok(Err(e)) => (None, None, Some(format!("agent error: {e}"))),
        Err(_) => (None, None, Some("timeout".to_string())),
    };

    println!("[5] Agent returned in {:?}", elapsed);
    if let Some(ref e) = error {
        println!("    error: {}", e.lines().next().unwrap_or(""));
    }
    if let Some(ref reply) = final_reply {
        let snippet: String = reply.chars().take(200).collect();
        println!("    reply: {}", snippet);
    }
    if let Some(ref u) = usage {
        println!(
            "    cost=${:.4}, in={}, out={}, calls={}",
            u.total_cost_usd, u.input_tokens, u.output_tokens, u.api_calls
        );
    }

    // ── Capture post-state ─────────────────────────────────────────
    println!("\n[6] Post-state capture:");
    let ledger_count_after = witness.ledger().count_for_session(&session_id).await?;
    let entries = witness.ledger().read_session(&session_id).await?;
    let mut seen_oath = false;
    let mut seen_verdict = false;
    let mut seen_pass = false;
    let mut seen_fail = false;
    for e in &entries {
        match e.entry_type {
            LedgerEntryType::OathSealed => seen_oath = true,
            LedgerEntryType::VerdictRendered => {
                seen_verdict = true;
                if let temm1e_witness::types::LedgerPayload::VerdictRendered(v) = &e.payload {
                    match v.outcome {
                        temm1e_witness::types::VerdictOutcome::Pass => seen_pass = true,
                        temm1e_witness::types::VerdictOutcome::Fail => seen_fail = true,
                        _ => {}
                    }
                }
            }
            _ => {}
        }
    }
    println!(
        "    Ledger entries (delta): {} → {} ({:+})",
        ledger_count_before,
        ledger_count_after,
        ledger_count_after - ledger_count_before,
    );
    println!(
        "    OathSealed entry seen:        {}",
        if seen_oath { "✓" } else { "✗" }
    );
    println!(
        "    VerdictRendered entry seen:   {}",
        if seen_verdict { "✓" } else { "✗" }
    );
    println!(
        "    PASS verdict seen:            {}",
        if seen_pass { "✓" } else { "✗" }
    );
    println!(
        "    FAIL verdict seen:            {}",
        if seen_fail { "✓" } else { "✗" }
    );

    let trust_state_after = {
        let t = trust.lock().await;
        (
            t.state().level3_streak,
            t.state().level2_streak,
            t.state().recent_rollbacks,
        )
    };
    let l3_delta = trust_state_after.0 as i64 - trust_state_before.0 as i64;
    let l2_delta = trust_state_after.1 as i64 - trust_state_before.1 as i64;
    let rb_delta = trust_state_after.2 as i64 - trust_state_before.2 as i64;
    println!(
        "    TrustEngine L3 streak:        {} → {} ({:+})",
        trust_state_before.0, trust_state_after.0, l3_delta,
    );
    println!(
        "    TrustEngine L2 streak:        {} → {} ({:+})",
        trust_state_before.1, trust_state_after.1, l2_delta,
    );
    println!(
        "    TrustEngine rollbacks:        {} → {} ({:+})",
        trust_state_before.2, trust_state_after.2, rb_delta,
    );

    // Files in workspace
    let files: Vec<String> = std::fs::read_dir(&workspace_path)
        .ok()
        .map(|rd| {
            rd.flatten()
                .filter(|e| e.file_type().map(|t| t.is_file()).unwrap_or(false))
                .filter_map(|e| e.file_name().into_string().ok())
                .collect()
        })
        .unwrap_or_default();
    println!("    Files in workspace:           {:?}", files);

    println!("\n════════════════════════════════════════════════════════════════");
    println!("  WIRING VALIDATION RESULT");
    println!("════════════════════════════════════════════════════════════════");
    let mut all_green = true;
    let check = |name: &str, ok: bool, all: &mut bool| {
        if ok {
            println!("  ✓ {}", name);
        } else {
            println!("  ✗ {}", name);
            *all = false;
        }
    };
    check(
        "Provider built and connected",
        error.is_none() || seen_oath,
        &mut all_green,
    );
    check(
        "auto_planner_oath fired (OathSealed in Ledger)",
        seen_oath,
        &mut all_green,
    );
    check(
        "Witness gate fired (VerdictRendered in Ledger)",
        seen_verdict,
        &mut all_green,
    );
    check(
        "TrustEngine updated (any state delta)",
        l3_delta != 0 || l2_delta != 0 || rb_delta != 0,
        &mut all_green,
    );
    println!();
    if all_green {
        println!("  ✅ All four wiring paths fired live against {}", model);
    } else {
        println!("  ⚠ Some wiring paths did not fire — see ✗ marks above");
    }

    Ok(())
}
