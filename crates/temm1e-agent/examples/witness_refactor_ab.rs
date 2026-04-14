//! Witness Codebase Refactor A/B — drives a real LLM through a substantive
//! multi-file refactor of *real* TEM source files, in two paired arms (with
//! and without Witness), and validates the result via Witness predicates.

#![allow(clippy::too_many_arguments)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::unnecessary_to_owned)]

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use temm1e_agent::AgentRuntime;
use temm1e_core::config::credentials::load_credentials_file;
use temm1e_core::traits::Memory;
use temm1e_core::types::message::{InboundMessage, TurnUsage};
use temm1e_core::types::session::SessionContext;
use temm1e_test_utils::MockMemory;
use temm1e_tools::{FileListTool, FileReadTool, FileWriteTool};
use temm1e_witness::{
    config::WitnessStrictness,
    ledger::Ledger,
    oath::seal_oath,
    types::{Oath, Predicate},
    witness::Witness,
};

struct RefactorTask {
    name: &'static str,
    description: &'static str,
    source_files: &'static [&'static str],
    prompt: &'static str,
    predicate_specs: &'static [PredicateSpec],
}

#[derive(Debug, Clone, Copy)]
enum PredicateSpec {
    FileExists(&'static str),
    FileContains(&'static str, &'static str),
    GrepCountAtLeast(&'static str, &'static str, u32),
    GrepAbsent(&'static str, &'static str),
    GrepPresent(&'static str, &'static str),
}

const REFACTOR_TASKS: &[RefactorTask] = &[
    RefactorTask {
        name: "rename_helper_in_predicates",
        description: "Rename the `resolve` helper to `resolve_workspace_path` in predicates.rs",
        source_files: &["src/predicates.rs"],
        prompt: r#"Refactor the file `src/predicates.rs` in the current workspace. Rename the private helper function `resolve` to `resolve_workspace_path`. You must:

1. Rename the function definition (`fn resolve(...)`) to `fn resolve_workspace_path(...)`.
2. Update EVERY call site in the file from `resolve(` to `resolve_workspace_path(`. There are many call sites — do not miss any.
3. The function's signature, body, and behavior must NOT change. Only the name.
4. Use the file_read tool to read the file first, then file_write to overwrite it with the refactored version.

Reply 'done' when finished."#,
        predicate_specs: &[
            PredicateSpec::FileExists("src/predicates.rs"),
            PredicateSpec::GrepAbsent(r"\bresolve\(", "src/predicates.rs"),
            PredicateSpec::GrepAbsent(r"fn\s+resolve\b", "src/predicates.rs"),
            PredicateSpec::FileContains("src/predicates.rs", r"fn\s+resolve_workspace_path"),
            PredicateSpec::GrepCountAtLeast("resolve_workspace_path", "src/predicates.rs", 5),
            PredicateSpec::GrepAbsent(r"todo!\(|unimplemented!\(", "src/predicates.rs"),
        ],
    },
    RefactorTask {
        name: "add_doc_to_predicate_checkers",
        description: "Add /// doc comments to every async fn check_* in predicates.rs",
        source_files: &["src/predicates.rs"],
        prompt: r#"Refactor `src/predicates.rs` in the current workspace. For EVERY private async function whose name starts with `check_` (e.g., `check_file_exists`, `check_command_exits`, `check_grep_present`, etc.), add a `///` documentation comment immediately above the function definition that briefly explains what it checks.

Requirements:
1. Use the file_read tool to read the file first.
2. For each `async fn check_<something>` function in the file, add at least one `///` line describing what the function does. The doc comment must be on the line(s) immediately before `async fn check_...`.
3. Use file_write to overwrite the file with the documented version.
4. Do NOT change the function signatures or bodies. Only add doc comments.
5. The file must still compile cleanly.

There are roughly 25 such functions. Document all of them. Reply 'done' when finished."#,
        predicate_specs: &[
            PredicateSpec::FileExists("src/predicates.rs"),
            PredicateSpec::GrepCountAtLeast(r"^\s*///", "src/predicates.rs", 20),
            PredicateSpec::GrepCountAtLeast(r"async fn check_", "src/predicates.rs", 25),
            PredicateSpec::GrepAbsent(r"todo!\(|unimplemented!\(", "src/predicates.rs"),
        ],
    },
    RefactorTask {
        name: "add_check_predicate_dispatch_wrapper",
        description: "Add a new public function check_predicate_dispatch as a thin wrapper around check_tier0",
        source_files: &["src/predicates.rs"],
        prompt: r#"Refactor `src/predicates.rs` in the current workspace. Add a NEW public async function:

```rust
pub async fn check_predicate_dispatch(
    predicate: &Predicate,
    ctx: &CheckContext,
) -> Result<PredicateCheckResult, WitnessError> {
    check_tier0(predicate, ctx).await
}
```

Requirements:
1. Use file_read to read the file first.
2. Add the new function in the file. The exact location doesn't matter as long as it's at module level (not nested inside another fn).
3. Add a `///` doc comment above it briefly explaining that it's a public dispatch wrapper.
4. The new function must use the EXACT signature shown above (including parameter names and return type).
5. Use file_write to write the modified file.
6. Do NOT remove or modify any existing functions.

Reply 'done' when finished."#,
        predicate_specs: &[
            PredicateSpec::FileExists("src/predicates.rs"),
            PredicateSpec::FileContains(
                "src/predicates.rs",
                r"pub async fn check_predicate_dispatch",
            ),
            PredicateSpec::FileContains("src/predicates.rs", r"check_tier0\(predicate, ctx\)"),
            PredicateSpec::GrepPresent(r"async fn check_file_exists", "src/predicates.rs"),
            PredicateSpec::GrepPresent(r"async fn check_command_exits", "src/predicates.rs"),
            PredicateSpec::GrepPresent(r"async fn check_grep_present", "src/predicates.rs"),
            PredicateSpec::GrepAbsent(r"todo!\(|unimplemented!\(", "src/predicates.rs"),
        ],
    },
];

fn host_source_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .unwrap()
        .join("temm1e-witness")
}

async fn setup_sandbox(sandbox: &Path, source_files: &[&str]) -> std::io::Result<()> {
    let host = host_source_root();
    for rel in source_files {
        let src = host.join(rel);
        let dst = sandbox.join(rel);
        if let Some(parent) = dst.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        let bytes = tokio::fs::read(&src).await?;
        tokio::fs::write(&dst, bytes).await?;
    }
    Ok(())
}

fn build_oath(task: &RefactorTask, sandbox: &Path, session_id: &str) -> Oath {
    let mut oath = Oath::draft(
        format!("st-{}", task.name),
        format!("root-{}", task.name),
        session_id,
        format!("refactor: {}", task.description),
    );
    for spec in task.predicate_specs {
        let p = match spec {
            PredicateSpec::FileExists(rel) => Predicate::FileExists {
                path: sandbox.join(rel),
            },
            PredicateSpec::FileContains(rel, regex) => Predicate::FileContains {
                path: sandbox.join(rel),
                regex: regex.to_string(),
            },
            PredicateSpec::GrepCountAtLeast(pattern, rel, n) => Predicate::GrepCountAtLeast {
                pattern: pattern.to_string(),
                path_glob: sandbox.join(rel).to_string_lossy().into_owned(),
                n: *n,
            },
            PredicateSpec::GrepAbsent(pattern, rel) => Predicate::GrepAbsent {
                pattern: pattern.to_string(),
                path_glob: sandbox.join(rel).to_string_lossy().into_owned(),
            },
            PredicateSpec::GrepPresent(pattern, rel) => Predicate::GrepPresent {
                pattern: pattern.to_string(),
                path_glob: sandbox.join(rel).to_string_lossy().into_owned(),
            },
        };
        oath = oath.with_postcondition(p);
    }
    oath
}

#[derive(Debug, Clone, Serialize)]
struct ArmResult {
    arm: &'static str,
    final_reply: String,
    files_present: Vec<String>,
    file_size_after: i64,
    file_size_delta: i64,
    witness_outcome: String,
    witness_pass: u32,
    witness_fail: u32,
    witness_inconclusive: u32,
    api_calls: u32,
    input_tokens: u32,
    output_tokens: u32,
    cost_usd: f64,
    latency_ms: u64,
    witness_rewrote_reply: bool,
    error: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
struct RefactorResult {
    task: &'static str,
    arm_a: ArmResult,
    arm_b: ArmResult,
    cost_overhead_usd: f64,
    latency_overhead_ms: i64,
}

#[derive(Debug, Serialize)]
struct AbReport {
    model: String,
    budget_ceiling_usd: f64,
    tasks_attempted: usize,
    tasks_completed: usize,
    aborted_due_to_budget: bool,
    cumulative_cost_usd: f64,
    per_task: Vec<RefactorResult>,
}

fn list_files_recursive(dir: &Path) -> Vec<String> {
    let mut out = Vec::new();
    fn walk(root: &Path, prefix: &Path, out: &mut Vec<String>) {
        if let Ok(entries) = std::fs::read_dir(root) {
            for e in entries.flatten() {
                let path = e.path();
                let rel = path.strip_prefix(prefix).unwrap_or(&path);
                if path.is_file() {
                    if let Some(s) = rel.to_str() {
                        out.push(s.to_string());
                    }
                } else if path.is_dir() {
                    walk(&path, prefix, out);
                }
            }
        }
    }
    walk(dir, dir, &mut out);
    out.sort();
    out
}

fn file_size(path: &Path) -> i64 {
    std::fs::metadata(path)
        .map(|m| m.len() as i64)
        .unwrap_or(-1)
}

async fn build_runtime(
    provider: Arc<dyn temm1e_core::traits::Provider>,
    workspace: &PathBuf,
    model: &str,
    witness: Option<Arc<Witness>>,
) -> AgentRuntime {
    let memory: Arc<dyn Memory> = Arc::new(MockMemory::new());
    let tools: Vec<Arc<dyn temm1e_core::traits::Tool>> = vec![
        Arc::new(FileReadTool::new()),
        Arc::new(FileWriteTool::new()),
        Arc::new(FileListTool::new()),
    ];
    let system = format!(
        "You are a precise Rust refactoring agent. Your workspace is {}. \
         Use file_read to inspect files first, then file_write to overwrite \
         them with the refactored version. Be exhaustive — do not miss any \
         call sites. When complete, reply with the single word 'done'.",
        workspace.display()
    );
    let mut runtime = AgentRuntime::new(provider, memory, tools, model.to_string(), Some(system));
    if let Some(w) = witness {
        runtime = runtime.with_witness(w, WitnessStrictness::Block, true);
    }
    runtime
}

fn make_session_for(workspace: &PathBuf, session_id: &str) -> SessionContext {
    SessionContext {
        session_id: session_id.to_string(),
        channel: "refactor-ab".to_string(),
        chat_id: "refactor-chat".to_string(),
        user_id: "refactor-user".to_string(),
        role: temm1e_core::types::rbac::Role::Admin,
        history: Vec::new(),
        workspace_path: workspace.clone(),
        read_tracker: std::sync::Arc::new(tokio::sync::RwLock::new(HashSet::new())),
    }
}

fn make_inbound(text: &str) -> InboundMessage {
    InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        channel: "refactor-ab".to_string(),
        chat_id: "refactor-chat".to_string(),
        user_id: "refactor-user".to_string(),
        username: Some("refactor-tester".to_string()),
        text: Some(text.to_string()),
        attachments: Vec::new(),
        reply_to: None,
        timestamp: chrono::Utc::now(),
    }
}

async fn run_one_arm(
    arm_name: &'static str,
    task: &RefactorTask,
    provider: Arc<dyn temm1e_core::traits::Provider>,
    model: &str,
    workspace: PathBuf,
    witness: Option<Arc<Witness>>,
    interrupt: Arc<AtomicBool>,
    initial_size: i64,
) -> ArmResult {
    const MAX_RETRIES: u32 = 1;
    const PER_ATTEMPT_TIMEOUT_SECS: u64 = 180;

    let session_id = format!("refactor-{}-{}", task.name, arm_name);
    let inbound = make_inbound(task.prompt);

    let mut last_error: Option<String> = None;
    let mut last_result: Option<(temm1e_core::types::message::OutboundMessage, TurnUsage)> = None;
    let mut total_latency_ms: u64 = 0;

    for attempt in 0..=MAX_RETRIES {
        let runtime = build_runtime(provider.clone(), &workspace, model, witness.clone()).await;
        let mut session = make_session_for(&workspace, &session_id);
        let started = Instant::now();
        let result = tokio::time::timeout(
            Duration::from_secs(PER_ATTEMPT_TIMEOUT_SECS),
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
        total_latency_ms += started.elapsed().as_millis() as u64;

        match result {
            Ok(Ok(ok)) => {
                last_result = Some(ok);
                last_error = None;
                break;
            }
            Ok(Err(e)) => {
                let msg = format!("agent error: {e}");
                let is_5xx = msg.contains("500")
                    || msg.contains("502")
                    || msg.contains("503")
                    || msg.contains("504")
                    || msg.contains("Internal Server Error");
                last_error = Some(msg.clone());
                if attempt < MAX_RETRIES && is_5xx {
                    let backoff = 2u64.pow(attempt + 1);
                    eprintln!(
                        "    [retry] {} arm {} attempt {} hit 5xx; backing off {}s",
                        task.name,
                        arm_name,
                        attempt + 1,
                        backoff
                    );
                    tokio::time::sleep(Duration::from_secs(backoff)).await;
                    continue;
                }
                break;
            }
            Err(_) => {
                last_error = Some("timeout".to_string());
                if attempt < MAX_RETRIES {
                    eprintln!(
                        "    [retry] {} arm {} attempt {} timed out; retrying",
                        task.name,
                        arm_name,
                        attempt + 1
                    );
                    continue;
                }
                break;
            }
        }
    }

    let latency_ms = total_latency_ms;
    let result_pair = last_result.map(|(o, u)| (o.text, u));

    let (out, usage, error): (Option<String>, Option<TurnUsage>, Option<String>) = match result_pair
    {
        Some((text, usage)) => (Some(text), Some(usage), last_error),
        None => (
            None,
            None,
            last_error.or_else(|| Some("unknown".to_string())),
        ),
    };

    let final_reply = out.unwrap_or_default();
    let witness_rewrote_reply =
        final_reply.contains("Partial completion") || final_reply.contains("Could not verify");

    let files_present = list_files_recursive(&workspace);
    let predicates_path = workspace.join("src/predicates.rs");
    let final_size = file_size(&predicates_path);
    let file_size_delta = if initial_size >= 0 && final_size >= 0 {
        final_size - initial_size
    } else {
        0
    };

    let (witness_outcome, w_pass, w_fail, w_inc) = match witness.as_ref() {
        Some(w) => match w.active_oath(&session_id).await {
            Ok(Some(oath)) => match w.verify_oath(&oath).await {
                Ok(v) => (
                    format!("{:?}", v.outcome),
                    v.pass_count(),
                    v.fail_count(),
                    v.inconclusive_count(),
                ),
                Err(e) => (format!("verify_error:{e}"), 0, 0, 0),
            },
            Ok(None) => ("no_active_oath".to_string(), 0, 0, 0),
            Err(e) => (format!("lookup_error:{e}"), 0, 0, 0),
        },
        None => {
            let ledger = match Ledger::open("sqlite::memory:").await {
                Ok(l) => l,
                Err(e) => {
                    return ArmResult {
                        arm: arm_name,
                        final_reply,
                        files_present,
                        file_size_after: final_size,
                        file_size_delta,
                        witness_outcome: format!("post_ledger_open_error:{e}"),
                        witness_pass: 0,
                        witness_fail: 0,
                        witness_inconclusive: 0,
                        api_calls: usage.as_ref().map(|u| u.api_calls).unwrap_or(0),
                        input_tokens: usage.as_ref().map(|u| u.input_tokens).unwrap_or(0),
                        output_tokens: usage.as_ref().map(|u| u.output_tokens).unwrap_or(0),
                        cost_usd: usage.as_ref().map(|u| u.total_cost_usd).unwrap_or(0.0),
                        latency_ms,
                        witness_rewrote_reply,
                        error,
                    };
                }
            };
            let ad_hoc = Witness::new(ledger, workspace.clone());
            let oath = build_oath(task, &workspace, &session_id);
            match seal_oath(ad_hoc.ledger(), oath).await {
                Ok((sealed, _)) => match ad_hoc.verify_oath(&sealed).await {
                    Ok(v) => (
                        format!("{:?}", v.outcome),
                        v.pass_count(),
                        v.fail_count(),
                        v.inconclusive_count(),
                    ),
                    Err(e) => (format!("verify_error:{e}"), 0, 0, 0),
                },
                Err(e) => (format!("seal_error:{e}"), 0, 0, 0),
            }
        }
    };

    ArmResult {
        arm: arm_name,
        final_reply,
        files_present,
        file_size_after: final_size,
        file_size_delta,
        witness_outcome,
        witness_pass: w_pass,
        witness_fail: w_fail,
        witness_inconclusive: w_inc,
        api_calls: usage.as_ref().map(|u| u.api_calls).unwrap_or(0),
        input_tokens: usage.as_ref().map(|u| u.input_tokens).unwrap_or(0),
        output_tokens: usage.as_ref().map(|u| u.output_tokens).unwrap_or(0),
        cost_usd: usage.as_ref().map(|u| u.total_cost_usd).unwrap_or(0.0),
        latency_ms,
        witness_rewrote_reply,
        error,
    }
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let model =
        std::env::var("WITNESS_AB_MODEL").unwrap_or_else(|_| "gemini-3-flash-preview".to_string());
    let budget_ceiling: f64 = std::env::var("WITNESS_AB_BUDGET_USD")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(8.0);

    println!("════════════════════════════════════════════════════════════════");
    println!("  Witness Codebase Refactor A/B Harness");
    println!("  Model:           {}", model);
    println!("  Budget ceiling:  ${:.2}", budget_ceiling);
    println!("  Refactor tasks:  {}", REFACTOR_TASKS.len());
    println!("════════════════════════════════════════════════════════════════\n");

    // Auto-detect provider from model name prefix.
    let provider_name = if model.starts_with("gemini-") {
        "gemini"
    } else if model.starts_with("gpt-") || model.starts_with("o1") || model.starts_with("o3") {
        "openai"
    } else if model.starts_with("claude-") {
        "anthropic"
    } else if model.starts_with("grok-") {
        "grok"
    } else {
        "openai" // default fallback
    };

    let creds = load_credentials_file().ok_or("no credentials.toml found")?;
    let cred_provider = creds
        .providers
        .iter()
        .find(|p| p.name == provider_name)
        .ok_or_else(|| {
            format!(
                "no {} provider in credentials.toml — available: {:?}",
                provider_name,
                creds.providers.iter().map(|p| &p.name).collect::<Vec<_>>()
            )
        })?;
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
        "grok" => Arc::new(
            temm1e_providers::OpenAICompatProvider::new(api_key)
                .with_base_url("https://api.x.ai/v1".to_string()),
        ),
        _ => Arc::new(temm1e_providers::OpenAICompatProvider::new(api_key)),
    };

    println!("  Provider:        {} ({})", provider_name, provider.name());

    let interrupt = Arc::new(AtomicBool::new(false));

    let mut per_task: Vec<RefactorResult> = Vec::new();
    let mut cumulative_cost: f64 = 0.0;
    let mut tasks_attempted: usize = 0;
    let mut tasks_completed: usize = 0;
    let mut aborted = false;

    for task in REFACTOR_TASKS {
        if cumulative_cost >= budget_ceiling {
            eprintln!(
                "⚠ Budget ceiling ${:.2} reached, aborting after {} tasks",
                budget_ceiling, tasks_completed
            );
            aborted = true;
            break;
        }
        tasks_attempted += 1;
        println!(
            "→ Running task {}/{}: {}",
            tasks_attempted,
            REFACTOR_TASKS.len(),
            task.name
        );

        let workspace_a = tempfile::tempdir()?;
        setup_sandbox(workspace_a.path(), task.source_files).await?;
        let initial_size_a = file_size(&workspace_a.path().join("src/predicates.rs"));
        let arm_a = run_one_arm(
            "A",
            task,
            provider.clone(),
            &model,
            workspace_a.path().to_path_buf(),
            None,
            interrupt.clone(),
            initial_size_a,
        )
        .await;

        let workspace_b = tempfile::tempdir()?;
        setup_sandbox(workspace_b.path(), task.source_files).await?;
        let initial_size_b = file_size(&workspace_b.path().join("src/predicates.rs"));
        let ledger = Ledger::open("sqlite::memory:").await?;
        let witness = Arc::new(Witness::new(ledger, workspace_b.path().to_path_buf()));
        let session_b = format!("refactor-{}-B", task.name);
        let oath = build_oath(task, &workspace_b.path().to_path_buf(), &session_b);
        if let Err(e) = seal_oath(witness.ledger(), oath).await {
            eprintln!("Arm B seal_oath failed for {}: {}", task.name, e);
        }
        let arm_b = run_one_arm(
            "B",
            task,
            provider.clone(),
            &model,
            workspace_b.path().to_path_buf(),
            Some(witness),
            interrupt.clone(),
            initial_size_b,
        )
        .await;

        cumulative_cost += arm_a.cost_usd + arm_b.cost_usd;
        let cost_overhead_usd = arm_b.cost_usd - arm_a.cost_usd;
        let latency_overhead_ms = arm_b.latency_ms as i64 - arm_a.latency_ms as i64;

        println!(
            "  {:<32} A={:?}/{}  B={:?}/{}  cost A=${:.4} B=${:.4} (cum ${:.4}) latΔ={}ms  Asize={}b Bsize={}b",
            task.name,
            arm_a.witness_outcome,
            arm_a.witness_fail,
            arm_b.witness_outcome,
            arm_b.witness_fail,
            arm_a.cost_usd,
            arm_b.cost_usd,
            cumulative_cost,
            latency_overhead_ms,
            arm_a.file_size_after,
            arm_b.file_size_after,
        );

        per_task.push(RefactorResult {
            task: task.name,
            arm_a,
            arm_b,
            cost_overhead_usd,
            latency_overhead_ms,
        });
        tasks_completed += 1;
    }

    let arm_a_pass: u32 = per_task
        .iter()
        .filter(|r| r.arm_a.witness_outcome == "Pass")
        .count() as u32;
    let arm_a_fail: u32 = per_task
        .iter()
        .filter(|r| r.arm_a.witness_outcome == "Fail")
        .count() as u32;
    let arm_b_pass: u32 = per_task
        .iter()
        .filter(|r| r.arm_b.witness_outcome == "Pass")
        .count() as u32;
    let arm_b_fail: u32 = per_task
        .iter()
        .filter(|r| r.arm_b.witness_outcome == "Fail")
        .count() as u32;
    let rewritten: u32 = per_task
        .iter()
        .filter(|r| r.arm_b.witness_rewrote_reply)
        .count() as u32;

    let arm_a_total_cost: f64 = per_task.iter().map(|r| r.arm_a.cost_usd).sum();
    let arm_b_total_cost: f64 = per_task.iter().map(|r| r.arm_b.cost_usd).sum();

    println!("\n════════════════════════════════════════════════════════════════");
    println!("  REFACTOR A/B AGGREGATE");
    println!("════════════════════════════════════════════════════════════════");
    println!("  Tasks attempted:                {}", tasks_attempted);
    println!("  Tasks completed:                {}", tasks_completed);
    println!("  Aborted by budget ceiling:      {}", aborted);
    println!("  Cumulative cost:                ${:.4}", cumulative_cost);
    println!();
    println!(
        "  Arm A Witness PASS:             {}/{}",
        arm_a_pass,
        per_task.len()
    );
    println!(
        "  Arm A Witness FAIL:             {}/{}",
        arm_a_fail,
        per_task.len()
    );
    println!(
        "  Arm B Witness PASS:             {}/{}",
        arm_b_pass,
        per_task.len()
    );
    println!(
        "  Arm B Witness FAIL:             {}/{}",
        arm_b_fail,
        per_task.len()
    );
    println!("  Arm B replies rewritten:        {}", rewritten);
    println!();
    println!("  Arm A total cost:               ${:.4}", arm_a_total_cost);
    println!("  Arm B total cost:               ${:.4}", arm_b_total_cost);
    let cost_overhead_pct = if arm_a_total_cost > 0.0 {
        (arm_b_total_cost - arm_a_total_cost) / arm_a_total_cost * 100.0
    } else {
        0.0
    };
    println!(
        "  Witness cost overhead:          {:.1}%",
        cost_overhead_pct
    );
    println!();

    let report = AbReport {
        model: model.clone(),
        budget_ceiling_usd: budget_ceiling,
        tasks_attempted,
        tasks_completed,
        aborted_due_to_budget: aborted,
        cumulative_cost_usd: cumulative_cost,
        per_task,
    };
    let out_dir = PathBuf::from("tems_lab/witness");
    std::fs::create_dir_all(&out_dir)?;
    let json_path = out_dir.join("refactor_ab_results.json");
    std::fs::write(&json_path, serde_json::to_string_pretty(&report)?)?;
    println!("Wrote {}", json_path.display());

    Ok(())
}
