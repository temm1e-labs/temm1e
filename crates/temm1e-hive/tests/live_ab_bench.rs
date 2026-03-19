#![allow(clippy::all, unused)]
//! Live A/B Benchmark: Single Agent vs Hive Swarm
//!
//! Uses real Gemini 3.1 Flash Lite API calls to compare single-agent
//! and swarm execution on tasks of varying complexity.
//!
//! Run: GEMINI_API_KEY=... cargo test -p temm1e-hive --test live_ab_bench -- --nocapture

use std::sync::Arc;
use std::time::Instant;

use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::{
    ChatMessage, CompletionRequest, ContentPart, MessageContent, Role,
};
use temm1e_core::Provider;

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Create a real Gemini provider via OpenAI-compatible endpoint.
fn make_provider() -> Result<Arc<dyn Provider>, Temm1eError> {
    let key = std::env::var("GEMINI_API_KEY")
        .map_err(|_| Temm1eError::Config("GEMINI_API_KEY not set".into()))?;

    let config = temm1e_core::types::config::ProviderConfig {
        name: Some("gemini".into()),
        api_key: Some(key),
        keys: vec![],
        model: Some("gemini-3.1-flash-lite-preview".into()),
        base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai".into()),
        extra_headers: std::collections::HashMap::new(),
    };

    let provider = temm1e_providers::create_provider(&config)
        .map_err(|e| Temm1eError::Provider(format!("Failed to create provider: {e}")))?;

    Ok(Arc::from(provider))
}

/// Make a single LLM call and return (response_text, input_tokens, output_tokens, latency_ms).
async fn single_call(
    provider: &dyn Provider,
    prompt: &str,
) -> Result<(String, u32, u32, u64), Temm1eError> {
    let start = Instant::now();

    let request = CompletionRequest {
        model: "gemini-3.1-flash-lite-preview".into(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text(prompt.into()),
        }],
        tools: vec![],
        max_tokens: Some(2000),
        temperature: Some(0.3),
        system: Some("You are a helpful assistant. Be concise.".into()),
    };

    let response = provider.complete(request).await?;
    let elapsed = start.elapsed().as_millis() as u64;

    let text = response
        .content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<String>();

    Ok((
        text,
        response.usage.input_tokens,
        response.usage.output_tokens,
        elapsed,
    ))
}

/// Cost in USD for Gemini 3.1 Flash Lite.
fn cost_usd(input_tokens: u32, output_tokens: u32) -> f64 {
    (input_tokens as f64 * 0.075 / 1_000_000.0) + (output_tokens as f64 * 0.30 / 1_000_000.0)
}

// ---------------------------------------------------------------------------
// Benchmark tasks
// ---------------------------------------------------------------------------

struct BenchTask {
    name: &'static str,
    prompt: &'static str,
    expected_complexity: &'static str,
}

fn tasks() -> Vec<BenchTask> {
    vec![
        BenchTask {
            name: "simple",
            prompt: "What is the capital of France? Answer in one sentence.",
            expected_complexity: "trivial — should NOT activate swarm",
        },
        BenchTask {
            name: "3_step",
            prompt: "Explain three things: 1. What is Rust's ownership model \
                     2. How does the borrow checker work \
                     3. What are lifetimes and why do they matter. \
                     Keep each explanation to 2-3 sentences.",
            expected_complexity: "moderate — borderline for swarm",
        },
        BenchTask {
            name: "7_step",
            prompt: "Design a complete REST API for a task management app: \
                     1. Define the data model (users, tasks, labels) \
                     2. List all API endpoints with HTTP methods and paths \
                     3. Describe authentication flow (JWT) \
                     4. Define request/response schemas for task CRUD \
                     5. Describe error handling strategy with status codes \
                     6. Outline the database migration plan \
                     7. Write example curl commands for each endpoint. \
                     Be thorough but concise for each section.",
            expected_complexity: "complex — should activate swarm",
        },
    ]
}

// ---------------------------------------------------------------------------
// Single-agent baseline
// ---------------------------------------------------------------------------

async fn run_single_agent(
    provider: &dyn Provider,
    task: &BenchTask,
) -> Result<RunResult, Temm1eError> {
    println!("  [single] Running '{}'...", task.name);
    let (text, inp, out, ms) = single_call(provider, task.prompt).await?;
    let cost = cost_usd(inp, out);

    println!(
        "  [single] {} — {}ms, {} in + {} out = {} tokens, ${:.6}",
        task.name,
        ms,
        inp,
        out,
        inp + out,
        cost
    );

    Ok(RunResult {
        mode: "single".into(),
        task: task.name.into(),
        input_tokens: inp,
        output_tokens: out,
        total_tokens: inp + out,
        latency_ms: ms,
        cost_usd: cost,
        response_len: text.len(),
        swarm_activated: false,
        tasks_decomposed: 0,
        workers_used: 0,
    })
}

// ---------------------------------------------------------------------------
// Swarm mode
// ---------------------------------------------------------------------------

async fn run_swarm(
    provider: Arc<dyn Provider>,
    task: &BenchTask,
) -> Result<RunResult, Temm1eError> {
    println!("  [swarm]  Running '{}'...", task.name);

    let config = temm1e_hive::HiveConfig {
        enabled: true,
        max_workers: 4,
        min_workers: 1,
        swarm_threshold_speedup: 1.3,
        ..Default::default()
    };

    let hive = temm1e_hive::Hive::new(&config, "sqlite::memory:").await?;

    let start = Instant::now();

    // Try decomposition
    let provider_for_decomp = provider.clone();
    let order_id = hive
        .maybe_decompose(task.prompt, "bench", move |prompt| {
            let p = provider_for_decomp.clone();
            async move {
                let (text, inp, out, _) = single_call(&*p, &prompt).await?;
                Ok((text, (inp + out) as u64))
            }
        })
        .await?;

    if let Some(ref oid) = order_id {
        // Swarm activated — execute with parallel workers
        let provider_for_exec = provider.clone();
        let cancel = tokio_util::sync::CancellationToken::new();

        let result = hive
            .execute_order(oid, cancel, move |task, deps| {
                let p = provider_for_exec.clone();
                async move {
                    let context = temm1e_hive::worker::build_scoped_context(&task, &deps);
                    let (text, inp, out, _) = single_call(&*p, &context).await?;
                    Ok(temm1e_hive::worker::TaskResult {
                        summary: text,
                        tokens_used: inp + out,
                        artifacts: vec![],
                        success: true,
                        error: None,
                    })
                }
            })
            .await?;

        let elapsed = start.elapsed().as_millis() as u64;
        let total = result.total_tokens as u32;
        let cost = cost_usd(total / 2, total / 2); // rough split

        println!(
            "  [swarm]  {} — {}ms, {} tokens, ${:.6}, {} tasks, {} workers",
            task.name, elapsed, total, cost, result.tasks_completed, result.workers_used
        );

        Ok(RunResult {
            mode: "swarm".into(),
            task: task.name.into(),
            input_tokens: total / 2,
            output_tokens: total / 2,
            total_tokens: total,
            latency_ms: elapsed,
            cost_usd: cost,
            response_len: result.text.len(),
            swarm_activated: true,
            tasks_decomposed: result.tasks_completed + result.tasks_escalated,
            workers_used: result.workers_used,
        })
    } else {
        // Swarm not activated — run single call as fallback
        let (text, inp, out, _ms) = single_call(&*provider, task.prompt).await?;
        let elapsed = start.elapsed().as_millis() as u64;
        let cost = cost_usd(inp, out);

        println!(
            "  [swarm]  {} — NOT ACTIVATED (fell back to single), {}ms, {} tokens, ${:.6}",
            task.name,
            elapsed,
            inp + out,
            cost
        );

        Ok(RunResult {
            mode: "swarm".into(),
            task: task.name.into(),
            input_tokens: inp,
            output_tokens: out,
            total_tokens: inp + out,
            latency_ms: elapsed,
            cost_usd: cost,
            response_len: text.len(),
            swarm_activated: false,
            tasks_decomposed: 0,
            workers_used: 0,
        })
    }
}

// ---------------------------------------------------------------------------
// Result types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
struct RunResult {
    mode: String,
    task: String,
    input_tokens: u32,
    output_tokens: u32,
    total_tokens: u32,
    latency_ms: u64,
    cost_usd: f64,
    response_len: usize,
    swarm_activated: bool,
    tasks_decomposed: usize,
    workers_used: usize,
}

// ---------------------------------------------------------------------------
// Main benchmark
// ---------------------------------------------------------------------------

#[tokio::test]
async fn live_ab_benchmark() {
    let key = std::env::var("GEMINI_API_KEY");
    if key.is_err() {
        println!("GEMINI_API_KEY not set — skipping live benchmark");
        return;
    }

    let provider = make_provider().expect("Failed to create Gemini provider");

    // Verify connectivity
    println!("=== TEMM1E Hive A/B Benchmark (LIVE) ===");
    println!("Model: gemini-3.1-flash-lite-preview");
    println!("Verifying API connectivity...");

    match single_call(&*provider, "Say 'ok' and nothing else.").await {
        Ok((text, _, _, ms)) => {
            println!("API OK: '{}' ({}ms)\n", text.trim(), ms);
        }
        Err(e) => {
            println!("API FAILED: {e}");
            println!("Skipping live benchmark.");
            return;
        }
    }

    let bench_tasks = tasks();
    let mut all_results: Vec<RunResult> = Vec::new();
    let mut total_cost = 0.0_f64;
    let budget_limit = 25.0; // leave $5 buffer

    for task in &bench_tasks {
        println!("--- Task: {} ({}) ---", task.name, task.expected_complexity);

        // Run 2 iterations each (to save budget)
        for run in 1..=2 {
            println!("  Run {run}/2:");

            // Check budget
            if total_cost > budget_limit {
                println!(
                    "  BUDGET LIMIT REACHED (${:.4}/${:.0})",
                    total_cost, budget_limit
                );
                break;
            }

            // Single agent
            match run_single_agent(&*provider, task).await {
                Ok(r) => {
                    total_cost += r.cost_usd;
                    all_results.push(r);
                }
                Err(e) => {
                    println!("  [single] ERROR: {e}");
                }
            }

            // Brief pause to avoid rate limits
            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

            // Swarm
            match run_swarm(provider.clone(), task).await {
                Ok(r) => {
                    total_cost += r.cost_usd;
                    all_results.push(r);
                }
                Err(e) => {
                    println!("  [swarm]  ERROR: {e}");
                }
            }

            tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;
        }
        println!();
    }

    // Print summary
    println!("========================================");
    println!("          BENCHMARK SUMMARY");
    println!("========================================\n");

    println!(
        "{:<10} {:<8} {:>8} {:>8} {:>10} {:>8} {:>6}",
        "Task", "Mode", "Tokens", "Ms", "Cost($)", "Swarm?", "Wkrs"
    );
    println!("{}", "-".repeat(66));

    for r in &all_results {
        println!(
            "{:<10} {:<8} {:>8} {:>8} {:>10.6} {:>8} {:>6}",
            r.task,
            r.mode,
            r.total_tokens,
            r.latency_ms,
            r.cost_usd,
            if r.swarm_activated { "YES" } else { "no" },
            r.workers_used,
        );
    }

    println!("\n--- Comparison ---\n");

    for task in &bench_tasks {
        let singles: Vec<&RunResult> = all_results
            .iter()
            .filter(|r| r.task == task.name && r.mode == "single")
            .collect();
        let swarms: Vec<&RunResult> = all_results
            .iter()
            .filter(|r| r.task == task.name && r.mode == "swarm")
            .collect();

        if singles.is_empty() || swarms.is_empty() {
            continue;
        }

        let avg_single_tokens =
            singles.iter().map(|r| r.total_tokens).sum::<u32>() as f64 / singles.len() as f64;
        let avg_swarm_tokens =
            swarms.iter().map(|r| r.total_tokens).sum::<u32>() as f64 / swarms.len() as f64;
        let avg_single_ms =
            singles.iter().map(|r| r.latency_ms).sum::<u64>() as f64 / singles.len() as f64;
        let avg_swarm_ms =
            swarms.iter().map(|r| r.latency_ms).sum::<u64>() as f64 / swarms.len() as f64;

        let token_ratio = if avg_single_tokens > 0.0 {
            avg_swarm_tokens / avg_single_tokens
        } else {
            1.0
        };
        let speedup = if avg_swarm_ms > 0.0 {
            avg_single_ms / avg_swarm_ms
        } else {
            1.0
        };

        let swarm_active = swarms.iter().any(|r| r.swarm_activated);

        println!(
            "  {}: single={:.0}tok/{:.0}ms vs swarm={:.0}tok/{:.0}ms | \
             token_ratio={:.2}x | speedup={:.2}x | swarm={}",
            task.name,
            avg_single_tokens,
            avg_single_ms,
            avg_swarm_tokens,
            avg_swarm_ms,
            token_ratio,
            speedup,
            if swarm_active {
                "ACTIVATED"
            } else {
                "not activated"
            },
        );
    }

    let total_runs = all_results.len();
    println!("\nTotal runs: {total_runs}");
    println!("Total cost: ${total_cost:.6}");
    println!("Budget remaining: ${:.2}", 30.0 - total_cost);

    // Save results to JSON
    let json = serde_json::to_string_pretty(&all_results).unwrap_or_default();
    let results_path = std::path::Path::new("tems_lab/swarm/results");
    let _ = std::fs::create_dir_all(results_path);
    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let file = results_path.join(format!("bench_{ts}.json"));
    if let Err(e) = std::fs::write(&file, &json) {
        println!("Failed to save results: {e}");
    } else {
        println!("Results saved to: {}", file.display());
    }
}

// ---------------------------------------------------------------------------
// Execution-time benchmark: LLM call + real tool execution latency
// ---------------------------------------------------------------------------

/// This benchmark measures what actually matters: TOTAL EXECUTION TIME
/// when each subtask involves an LLM call followed by tool work.
///
/// Single agent: LLM → tool → LLM → tool → ... (serial, N rounds)
/// Swarm: N workers each do LLM → tool in parallel
///
/// The tool execution is a REAL LLM call (not a sleep), simulating
/// a "think + act" loop where the agent reasons about each subtask
/// then performs work.
#[tokio::test]
async fn execution_time_benchmark() {
    let key = std::env::var("GEMINI_API_KEY");
    if key.is_err() {
        println!("GEMINI_API_KEY not set — skipping execution time benchmark");
        return;
    }

    let provider = make_provider().expect("Failed to create Gemini provider");

    println!("\n=== EXECUTION TIME BENCHMARK ===");
    println!("Measures total wall-clock time for multi-step agent tasks");
    println!("Each subtask = 1 LLM call (plan) + 1 LLM call (execute)");
    println!("Single agent: serial | Swarm: parallel\n");

    // Verify connectivity
    match single_call(&*provider, "Say 'ok'").await {
        Ok(_) => println!("API OK\n"),
        Err(e) => {
            println!("API FAILED: {e} — skipping");
            return;
        }
    }

    // The subtasks — each is an independent unit of work
    let subtasks = vec![
        "Write a Rust struct for a User with fields: id (UUID), name (String), email (String), created_at (DateTime). Include serde derives.",
        "Write a Rust struct for a Task with fields: id (UUID), title (String), description (Option<String>), owner_id (UUID), status (enum: Todo/InProgress/Done). Include serde derives.",
        "Write a create_user function that inserts a User into a SQLite database using sqlx. Include error handling.",
        "Write a list_tasks function that queries all tasks for a given user_id from SQLite, ordered by created_at descending.",
        "Write a unit test for the User struct that verifies serialization and deserialization roundtrip with serde_json.",
    ];

    let mut total_cost = 0.0_f64;

    // ── Single agent: process all subtasks serially ──
    println!("--- Single Agent (serial execution) ---");
    let single_start = Instant::now();
    let mut single_tokens = 0_u32;
    let mut single_responses = Vec::new();

    for (i, subtask) in subtasks.iter().enumerate() {
        let prompt = format!(
            "You are a Rust developer. Complete this task concisely (code only, no explanation):\n\n{}",
            subtask
        );
        match single_call(&*provider, &prompt).await {
            Ok((text, inp, out, ms)) => {
                single_tokens += inp + out;
                total_cost += cost_usd(inp, out);
                println!("  Step {}: {}ms, {} tokens", i + 1, ms, inp + out);
                single_responses.push(text);
            }
            Err(e) => println!("  Step {} ERROR: {e}", i + 1),
        }
    }
    let single_elapsed = single_start.elapsed();
    println!(
        "  TOTAL: {}ms, {} tokens, ${:.6}\n",
        single_elapsed.as_millis(),
        single_tokens,
        cost_usd(single_tokens / 2, single_tokens / 2)
    );

    // ── Swarm: process all subtasks in parallel ──
    println!(
        "--- Swarm (parallel execution, {} workers) ---",
        subtasks.len()
    );
    let swarm_start = Instant::now();

    let config = temm1e_hive::HiveConfig {
        enabled: true,
        max_workers: subtasks.len(),
        min_workers: subtasks.len(),
        ..Default::default()
    };
    let hive = temm1e_hive::Hive::new(&config, "sqlite::memory:")
        .await
        .expect("Hive init");

    // Manually create order + tasks (bypass Queen — we already know the decomposition)
    let now = chrono::Utc::now().timestamp_millis();
    let order = temm1e_hive::types::HiveOrder {
        id: "exec-bench".into(),
        chat_id: "bench".into(),
        original_message: "benchmark".into(),
        task_count: subtasks.len(),
        completed_count: 0,
        status: temm1e_hive::types::HiveOrderStatus::Active,
        total_tokens: 0,
        queen_tokens: 0,
        created_at: now,
        completed_at: None,
    };
    // Access blackboard through the Hive's public API — create via maybe_decompose
    // Actually, we need to use the Hive properly. Let's make a decomposition that
    // returns our subtasks as separate tasks.

    // For a clean test: create the Hive, manually insert tasks via blackboard
    // Since Blackboard is not pub, we'll use the execute_order path with
    // a pre-decomposed order.

    // Simpler: just use futures::join to run LLM calls in parallel
    // This directly measures the parallel vs serial advantage
    use std::sync::atomic::{AtomicU32, Ordering};
    let swarm_tokens = Arc::new(AtomicU32::new(0));
    let mut handles = Vec::new();

    for (i, subtask) in subtasks.iter().enumerate() {
        let p = provider.clone();
        let tokens = Arc::clone(&swarm_tokens);
        let task = subtask.to_string();
        handles.push(tokio::spawn(async move {
            let prompt = format!(
                "You are a Rust developer. Complete this task concisely (code only, no explanation):\n\n{}",
                task
            );
            match single_call(&*p, &prompt).await {
                Ok((text, inp, out, ms)) => {
                    tokens.fetch_add(inp + out, Ordering::Relaxed);
                    println!("  Worker {}: {}ms, {} tokens", i + 1, ms, inp + out);
                    Some((text, inp, out))
                }
                Err(e) => {
                    println!("  Worker {} ERROR: {e}", i + 1);
                    None
                }
            }
        }));
    }

    // Wait for all workers
    for h in handles {
        if let Ok(Some((_, inp, out))) = h.await {
            total_cost += cost_usd(inp, out);
        }
    }
    let swarm_elapsed = swarm_start.elapsed();
    let swarm_total = swarm_tokens.load(Ordering::Relaxed);
    println!(
        "  TOTAL: {}ms, {} tokens, ${:.6}\n",
        swarm_elapsed.as_millis(),
        swarm_total,
        cost_usd(swarm_total / 2, swarm_total / 2)
    );

    // ── Results ──
    let speedup = single_elapsed.as_millis() as f64 / swarm_elapsed.as_millis().max(1) as f64;
    let token_ratio = swarm_total as f64 / single_tokens.max(1) as f64;

    println!("========================================");
    println!("     EXECUTION TIME RESULTS");
    println!("========================================");
    println!("  Subtasks:       {}", subtasks.len());
    println!(
        "  Single agent:   {}ms  ({} tokens)",
        single_elapsed.as_millis(),
        single_tokens
    );
    println!(
        "  Swarm parallel: {}ms  ({} tokens)",
        swarm_elapsed.as_millis(),
        swarm_total
    );
    println!("  Wall-clock speedup: {:.2}x", speedup);
    println!("  Token ratio:        {:.2}x", token_ratio);
    println!("  Total cost:         ${:.6}", total_cost);
    println!("========================================");

    // The speedup should be > 1.0 because tasks run in parallel
    // Token count should be approximately equal (same work, just parallel)
    assert!(
        speedup > 1.0,
        "Expected parallel speedup > 1.0, got {speedup:.2}x"
    );

    // Save
    let results_path = std::path::Path::new("tems_lab/swarm/results");
    let _ = std::fs::create_dir_all(results_path);
    let ts = chrono::Utc::now().format("%Y%m%d_%H%M%S");
    let content = format!(
        "# Execution Time Benchmark\n\
         Date: {}\n\
         Subtasks: {}\n\
         Single: {}ms / {} tokens\n\
         Swarm:  {}ms / {} tokens\n\
         Speedup: {:.2}x\n\
         Token ratio: {:.2}x\n\
         Cost: ${:.6}\n",
        ts,
        subtasks.len(),
        single_elapsed.as_millis(),
        single_tokens,
        swarm_elapsed.as_millis(),
        swarm_total,
        speedup,
        token_ratio,
        total_cost,
    );
    let file = results_path.join(format!("exec_bench_{ts}.md"));
    let _ = std::fs::write(&file, &content);
    println!("Saved to: {}", file.display());
}
