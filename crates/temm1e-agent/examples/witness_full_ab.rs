//! Witness Full A/B Sweep — validates the production wiring path
//! (`witness_init` + `with_witness_attachments`) across 4 task classes:
//!
//!   1. Refactor — code tasks Witness was historically validated on
//!   2. Chat/QA — freeform conversational turns (no Oath grounding)
//!   3. Tool sequences — multi-tool flows (file_read + file_write)
//!   4. Channel-style — short Telegram-like prompts
//!
//! Each task runs PAIRED:
//!   - Arm A: WitnessConfig::default() with enabled=false (no wiring)
//!   - Arm B: WitnessConfig::default() (Warn strictness, auto_planner=true,
//!     tier1+tier2 enabled — exact production default for v5.5.0)
//!
//! Captured per arm: reply text, cost, latency, witness verdict outcome,
//! whether Warn appended a footer (false-positive proxy on PASS expected
//! tasks).
//!
//! Hard budget cap via `WITNESS_AB_BUDGET_USD` env var (default $10).
//! Provider/model from credentials.toml; defaults to `gemini-3-flash-preview`.
//!
//! Run:
//!   cargo run --release -p temm1e-agent --example witness_full_ab
//!
//! Output: JSON results to `tems_lab/witness/full_ab_results.json` plus
//! a Markdown harmonized report to stdout.

#![allow(clippy::too_many_arguments)]
#![allow(clippy::field_reassign_with_default)]

use std::path::PathBuf;
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;

use temm1e_agent::witness_init::{build_witness_attachments, WitnessAttachments};
use temm1e_agent::AgentRuntime;
use temm1e_core::config::credentials::load_credentials_file;
use temm1e_core::traits::{Memory, Tool};
use temm1e_core::types::config::WitnessConfig;
use temm1e_core::types::message::{InboundMessage, TurnUsage};
use temm1e_core::types::session::SessionContext;
use temm1e_test_utils::MockMemory;
use temm1e_tools::{FileListTool, FileReadTool, FileWriteTool};

#[derive(Debug, Clone, Copy)]
enum TaskClass {
    Refactor,
    ChatQa,
    ToolSequence,
    ChannelStyle,
}

impl TaskClass {
    fn name(&self) -> &'static str {
        match self {
            TaskClass::Refactor => "refactor",
            TaskClass::ChatQa => "chat_qa",
            TaskClass::ToolSequence => "tool_sequence",
            TaskClass::ChannelStyle => "channel_style",
        }
    }
}

struct AbTask {
    class: TaskClass,
    name: &'static str,
    prompt: &'static str,
}

const TASKS: &[AbTask] = &[
    // ── Refactor (lite — proves Witness still works on the validated class) ──
    AbTask {
        class: TaskClass::Refactor,
        name: "rename_helper_in_oath",
        prompt:
            "In the workspace, write a Rust file `oath_demo.rs` containing a public function \
             `pub fn oath_hash_prefix(hash: &str, n: usize) -> String` that returns the first \
             `n` chars of `hash` (or the full string if shorter). Use file_write. Reply 'done'.",
    },
    AbTask {
        class: TaskClass::Refactor,
        name: "two_function_module",
        prompt:
            "Write `math_demo.rs` in the workspace with two pub functions: `pub fn add(a: i32, \
             b: i32) -> i32` and `pub fn mul(a: i32, b: i32) -> i32`. Both must be implemented \
             with real bodies (no todo!() / unimplemented!()). Use file_write. Reply 'done'.",
    },
    // ── Chat/QA (the gap the rollout report flagged) ──
    AbTask {
        class: TaskClass::ChatQa,
        name: "haiku_about_rust",
        prompt: "Write a single haiku about the Rust borrow checker. Reply with only the haiku.",
    },
    AbTask {
        class: TaskClass::ChatQa,
        name: "math_question",
        prompt: "What is 73 * 84? Show the multiplication step-by-step in two short lines.",
    },
    AbTask {
        class: TaskClass::ChatQa,
        name: "concept_explanation",
        prompt:
            "Explain Rust's `?` operator in one sentence, in plain English, no code samples needed.",
    },
    AbTask {
        class: TaskClass::ChatQa,
        name: "summarize_paragraph",
        prompt:
            "Summarize this paragraph in one sentence: \"The Witness verification system is a \
             pre-committed verification framework where an Oath is sealed before work begins, \
             and a verifier checks postconditions after, producing a tamper-evident verdict.\"",
    },
    AbTask {
        class: TaskClass::ChatQa,
        name: "creative_short",
        prompt: "Suggest 3 catchy product names for an AI agent runtime. Just the names, comma-separated.",
    },
    // ── Tool sequences (multi-tool flows) ──
    AbTask {
        class: TaskClass::ToolSequence,
        name: "write_then_read_back",
        prompt:
            "In the workspace: 1) use file_write to create `note.txt` with the single line \
             `hello from witness`. 2) use file_read to read `note.txt` back. 3) Reply with the \
             contents you read.",
    },
    AbTask {
        class: TaskClass::ToolSequence,
        name: "write_two_files",
        prompt:
            "Create two files in the workspace via file_write: `a.txt` containing `apple` and \
             `b.txt` containing `banana`. Then use file_list to confirm both exist. Reply with \
             a short summary listing both filenames.",
    },
    AbTask {
        class: TaskClass::ToolSequence,
        name: "manifest_file",
        prompt:
            "Create `manifest.json` in the workspace via file_write. Content must be exactly: \
             {\"name\": \"witness-test\", \"version\": \"1.0.0\"}. Reply 'done' when written.",
    },
    // ── Channel-style (short Telegram-like prompts) ──
    AbTask {
        class: TaskClass::ChannelStyle,
        name: "greet_back",
        prompt: "hey",
    },
    AbTask {
        class: TaskClass::ChannelStyle,
        name: "ack_short",
        prompt: "ok thanks",
    },
    AbTask {
        class: TaskClass::ChannelStyle,
        name: "yes_no",
        prompt: "is rust faster than python at numeric loops? one word answer.",
    },
    AbTask {
        class: TaskClass::ChannelStyle,
        name: "what_is_x",
        prompt: "what is a SQLite WAL?",
    },
    AbTask {
        class: TaskClass::ChannelStyle,
        name: "tiny_followup",
        prompt: "and how big can it get?",
    },
];

#[derive(Debug, Clone, Serialize)]
struct ArmResult {
    arm: &'static str,
    final_reply: String,
    reply_chars: usize,
    api_calls: u32,
    input_tokens: u32,
    output_tokens: u32,
    cost_usd: f64,
    latency_ms: u128,
    error: Option<String>,
    /// Did Warn append a footer? Detected via "Witness:" substring after a "---".
    witness_footer_appended: bool,
    /// Did the final reply text appear to come through unchanged (a substring of ArmA reply)?
    reply_preserved_baseline_substring: Option<bool>,
}

#[derive(Debug, Clone, Serialize)]
struct PairedResult {
    class: String,
    task: String,
    arm_a: ArmResult,
    arm_b: ArmResult,
    cost_overhead_usd: f64,
    cost_overhead_pct: f64,
    latency_overhead_ms: i128,
    latency_overhead_pct: f64,
}

#[derive(Debug, Serialize)]
struct AbReport {
    model: String,
    provider: String,
    budget_ceiling_usd: f64,
    cumulative_cost_usd: f64,
    aborted_due_to_budget: bool,
    tasks_attempted: usize,
    tasks_completed: usize,
    per_task: Vec<PairedResult>,
    per_class_summary: Vec<ClassSummary>,
}

#[derive(Debug, Serialize)]
struct ClassSummary {
    class: String,
    n_tasks: usize,
    n_completed: usize,
    avg_cost_overhead_pct: f64,
    avg_latency_overhead_pct: f64,
    n_warn_footer_appended: usize,
    n_reply_preserved: usize,
}

fn make_inbound(text: &str, channel: &str) -> InboundMessage {
    InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        channel: channel.to_string(),
        chat_id: format!("{}-chat", channel),
        user_id: format!("{}-user", channel),
        username: Some("ab-tester".into()),
        text: Some(text.to_string()),
        attachments: vec![],
        reply_to: None,
        timestamp: chrono::Utc::now(),
    }
}

fn make_session(workspace: PathBuf, session_id: &str) -> SessionContext {
    SessionContext {
        session_id: session_id.into(),
        channel: "ab".into(),
        chat_id: "ab-chat".into(),
        user_id: "ab-user".into(),
        role: temm1e_core::types::rbac::Role::Admin,
        history: vec![],
        workspace_path: workspace,
        read_tracker: Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new())),
    }
}

async fn build_runtime(
    provider: Arc<dyn temm1e_core::traits::Provider>,
    model: &str,
    workspace: &std::path::Path,
    witness_att: Option<&WitnessAttachments>,
) -> AgentRuntime {
    let memory: Arc<dyn Memory> = Arc::new(MockMemory::new());
    let tools: Vec<Arc<dyn Tool>> = vec![
        Arc::new(FileReadTool::new()),
        Arc::new(FileWriteTool::new()),
        Arc::new(FileListTool::new()),
    ];
    let system = format!(
        "You are an agent in a controlled A/B test. Your workspace is {}. \
         Be concise. For code tasks use file_write; do not stub with todo!(). \
         For chat/QA, answer directly and briefly.",
        workspace.display()
    );
    AgentRuntime::new(provider, memory, tools, model.to_string(), Some(system))
        .with_witness_attachments(witness_att)
}

async fn run_one_arm(
    arm_name: &'static str,
    task: &AbTask,
    provider: Arc<dyn temm1e_core::traits::Provider>,
    model: &str,
    workspace: PathBuf,
    witness_att: Option<&WitnessAttachments>,
    interrupt: Arc<AtomicBool>,
    baseline_reply: Option<&str>,
) -> ArmResult {
    let per_attempt_timeout_secs: u64 = std::env::var("WITNESS_AB_ATTEMPT_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(180);

    let session_id = format!("{}-{}-{}", task.class.name(), task.name, arm_name);
    let inbound = make_inbound(task.prompt, "ab");
    let runtime = build_runtime(provider.clone(), model, &workspace, witness_att).await;
    let mut session = make_session(workspace, &session_id);

    let started = Instant::now();
    let result = tokio::time::timeout(
        Duration::from_secs(per_attempt_timeout_secs),
        runtime.process_message(
            &inbound,
            &mut session,
            Some(interrupt.clone()),
            None,
            None,
            None,
            None,
        ),
    )
    .await;
    let latency_ms = started.elapsed().as_millis();

    let (final_reply, usage, error): (String, Option<TurnUsage>, Option<String>) = match result {
        Ok(Ok((out, usage))) => (out.text, Some(usage), None),
        Ok(Err(e)) => (String::new(), None, Some(format!("agent error: {e}"))),
        Err(_) => (
            String::new(),
            None,
            Some(format!("timeout after {}s", per_attempt_timeout_secs)),
        ),
    };

    let witness_footer_appended = final_reply.contains("---")
        && (final_reply.contains("Witness:") || final_reply.contains("⚠"));
    let reply_preserved_baseline_substring = baseline_reply.map(|baseline| {
        let baseline = baseline.trim();
        // Warn preserves the agent's reply as a substring; Block discards it.
        // We look for the first 40 chars of the baseline somewhere in arm B reply.
        let probe: String = baseline.chars().take(40).collect();
        !probe.is_empty() && final_reply.contains(probe.as_str())
    });

    let api_calls = usage.as_ref().map(|u| u.api_calls).unwrap_or(0);
    let input_tokens = usage.as_ref().map(|u| u.input_tokens).unwrap_or(0);
    let output_tokens = usage.as_ref().map(|u| u.output_tokens).unwrap_or(0);
    let cost_usd = usage.as_ref().map(|u| u.total_cost_usd).unwrap_or(0.0);

    ArmResult {
        arm: arm_name,
        reply_chars: final_reply.chars().count(),
        final_reply: final_reply.chars().take(800).collect(),
        api_calls,
        input_tokens,
        output_tokens,
        cost_usd,
        latency_ms,
        error,
        witness_footer_appended,
        reply_preserved_baseline_substring,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let budget_ceiling: f64 = std::env::var("WITNESS_AB_BUDGET_USD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(10.0);

    let model =
        std::env::var("WITNESS_AB_MODEL").unwrap_or_else(|_| "gemini-3-flash-preview".into());
    let provider_name = if model.starts_with("gemini-") {
        "gemini"
    } else if model.starts_with("gpt-") || model.starts_with("o1") || model.starts_with("o3") {
        "openai"
    } else if model.starts_with("claude-") {
        "anthropic"
    } else {
        "openai"
    };

    println!("════════════════════════════════════════════════════════════════");
    println!("  Witness Full A/B Sweep");
    println!("  Model:      {}", model);
    println!("  Provider:   {}", provider_name);
    println!("  Budget cap: ${:.2}", budget_ceiling);
    println!("  Tasks:      {} ({} classes)", TASKS.len(), 4);
    println!("════════════════════════════════════════════════════════════════\n");

    let creds = load_credentials_file().ok_or("no credentials.toml found at ~/.temm1e/")?;
    let cred_provider = creds
        .providers
        .iter()
        .find(|p| p.name == provider_name)
        .ok_or_else(|| format!("no '{}' provider in credentials.toml", provider_name))?;
    let api_key = cred_provider
        .keys
        .first()
        .cloned()
        .ok_or_else(|| format!("'{}' provider has no keys", provider_name))?;

    let provider: Arc<dyn temm1e_core::traits::Provider> = match provider_name {
        "gemini" => Arc::new(temm1e_providers::GeminiProvider::new(api_key)),
        "openai" => Arc::new(
            temm1e_providers::OpenAICompatProvider::new(api_key)
                .with_base_url("https://api.openai.com/v1".into()),
        ),
        "anthropic" => Arc::new(temm1e_providers::AnthropicProvider::new(api_key)),
        _ => Arc::new(temm1e_providers::OpenAICompatProvider::new(api_key)),
    };

    // Build the production-default WitnessAttachments once; reused per Arm B.
    let tmp = tempfile::tempdir()?;
    let witness_cfg = {
        let mut c = WitnessConfig::default();
        c.ledger_path = Some(tmp.path().join("ab-witness.db").to_string_lossy().into());
        c
    };
    let witness_att = build_witness_attachments(&witness_cfg).await?;
    if witness_att.is_none() {
        return Err("WitnessConfig::default() returned no attachments — config drift".into());
    }
    println!(
        "[setup] Witness attachments built (strictness={}, auto_planner={}, tier1={}, tier2={})\n",
        witness_cfg.strictness,
        witness_cfg.auto_planner_oath,
        witness_cfg.tier1_enabled,
        witness_cfg.tier2_enabled
    );

    let interrupt = Arc::new(AtomicBool::new(false));
    let mut per_task = Vec::new();
    let mut cumulative_cost = 0.0_f64;
    let mut aborted = false;
    let mut completed = 0;

    for task in TASKS {
        if cumulative_cost >= budget_ceiling {
            println!(
                "[budget] Cap reached (${:.4}/${:.2}) — stopping",
                cumulative_cost, budget_ceiling
            );
            aborted = true;
            break;
        }

        println!(
            "── [{}] {} (cost so far: ${:.4})",
            task.class.name(),
            task.name,
            cumulative_cost
        );

        // Fresh sandbox per task pair
        let workspace_a = tempfile::tempdir()?;
        let workspace_b = tempfile::tempdir()?;
        let _ws_a = workspace_a.path().to_path_buf();
        let _ws_b = workspace_b.path().to_path_buf();

        // Arm A: no witness wiring (baseline)
        let arm_a = run_one_arm(
            "A",
            task,
            provider.clone(),
            &model,
            workspace_a.path().to_path_buf(),
            None,
            interrupt.clone(),
            None,
        )
        .await;
        cumulative_cost += arm_a.cost_usd;
        println!(
            "  Arm A: {:.4}s  ${:.5}  {} chars  {}",
            arm_a.latency_ms as f64 / 1000.0,
            arm_a.cost_usd,
            arm_a.reply_chars,
            if arm_a.error.is_some() { "ERR" } else { "ok" }
        );

        // Arm B: production-default witness wiring
        let arm_b = run_one_arm(
            "B",
            task,
            provider.clone(),
            &model,
            workspace_b.path().to_path_buf(),
            witness_att.as_ref(),
            interrupt.clone(),
            Some(&arm_a.final_reply),
        )
        .await;
        cumulative_cost += arm_b.cost_usd;
        println!(
            "  Arm B: {:.4}s  ${:.5}  {} chars  {}  footer={}",
            arm_b.latency_ms as f64 / 1000.0,
            arm_b.cost_usd,
            arm_b.reply_chars,
            if arm_b.error.is_some() { "ERR" } else { "ok" },
            arm_b.witness_footer_appended,
        );

        let cost_overhead_usd = arm_b.cost_usd - arm_a.cost_usd;
        let cost_overhead_pct = if arm_a.cost_usd > 0.0 {
            (cost_overhead_usd / arm_a.cost_usd) * 100.0
        } else {
            0.0
        };
        let latency_overhead_ms = arm_b.latency_ms as i128 - arm_a.latency_ms as i128;
        let latency_overhead_pct = if arm_a.latency_ms > 0 {
            (latency_overhead_ms as f64 / arm_a.latency_ms as f64) * 100.0
        } else {
            0.0
        };

        per_task.push(PairedResult {
            class: task.class.name().into(),
            task: task.name.into(),
            arm_a,
            arm_b,
            cost_overhead_usd,
            cost_overhead_pct,
            latency_overhead_ms,
            latency_overhead_pct,
        });
        completed += 1;
    }

    // Per-class summary aggregation
    let mut per_class_summary = Vec::new();
    for class in [
        TaskClass::Refactor,
        TaskClass::ChatQa,
        TaskClass::ToolSequence,
        TaskClass::ChannelStyle,
    ] {
        let in_class: Vec<&PairedResult> = per_task
            .iter()
            .filter(|p| p.class == class.name())
            .collect();
        let n_tasks = in_class.len();
        let n_completed = in_class
            .iter()
            .filter(|p| p.arm_a.error.is_none() && p.arm_b.error.is_none())
            .count();
        let avg_cost_overhead_pct = if !in_class.is_empty() {
            in_class.iter().map(|p| p.cost_overhead_pct).sum::<f64>() / in_class.len() as f64
        } else {
            0.0
        };
        let avg_latency_overhead_pct = if !in_class.is_empty() {
            in_class.iter().map(|p| p.latency_overhead_pct).sum::<f64>() / in_class.len() as f64
        } else {
            0.0
        };
        let n_warn_footer_appended = in_class
            .iter()
            .filter(|p| p.arm_b.witness_footer_appended)
            .count();
        let n_reply_preserved = in_class
            .iter()
            .filter(|p| p.arm_b.reply_preserved_baseline_substring == Some(true))
            .count();
        per_class_summary.push(ClassSummary {
            class: class.name().into(),
            n_tasks,
            n_completed,
            avg_cost_overhead_pct,
            avg_latency_overhead_pct,
            n_warn_footer_appended,
            n_reply_preserved,
        });
    }

    let report = AbReport {
        model: model.clone(),
        provider: provider_name.into(),
        budget_ceiling_usd: budget_ceiling,
        cumulative_cost_usd: cumulative_cost,
        aborted_due_to_budget: aborted,
        tasks_attempted: per_task.len(),
        tasks_completed: completed,
        per_task,
        per_class_summary,
    };

    let out_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../tems_lab/witness/full_ab_results.json");
    std::fs::create_dir_all(out_path.parent().unwrap()).ok();
    let json = serde_json::to_string_pretty(&report)?;
    std::fs::write(&out_path, &json)?;

    println!("\n════════════════════════════════════════════════════════════════");
    println!("  Per-class summary");
    println!("════════════════════════════════════════════════════════════════");
    println!(
        "{:<16} {:>6} {:>6} {:>14} {:>14} {:>10} {:>10}",
        "class", "tasks", "ok", "cost_ovh%", "latency_ovh%", "footers", "preserved"
    );
    for c in &report.per_class_summary {
        println!(
            "{:<16} {:>6} {:>6} {:>14.1} {:>14.1} {:>10} {:>10}",
            c.class,
            c.n_tasks,
            c.n_completed,
            c.avg_cost_overhead_pct,
            c.avg_latency_overhead_pct,
            c.n_warn_footer_appended,
            c.n_reply_preserved
        );
    }
    println!(
        "\n  Cumulative cost: ${:.4} / ${:.2} budget",
        report.cumulative_cost_usd, report.budget_ceiling_usd
    );
    println!(
        "  Tasks completed: {}/{}",
        report.tasks_completed,
        TASKS.len()
    );
    println!("  Aborted: {}", report.aborted_due_to_budget);
    println!("\n  Results JSON: {}", out_path.display());

    Ok(())
}
