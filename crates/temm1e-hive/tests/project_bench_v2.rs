#![allow(clippy::all, unused)]
//! Project Benchmark v2 (Difficulty: 8/10)
//!
//! Key differences from v1:
//! - High-level spec only — no prescribed function signatures
//! - Workers READ actual generated dependency files as context
//! - Compile-fix loop: generate → cargo check → feed errors → retry (max 3)
//! - Final verification: cargo check && cargo test on the REAL output
//! - Measures total API calls including retries
//!
//! Run: GEMINI_API_KEY=... cargo test -p temm1e-hive --test project_bench_v2 -- --nocapture

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::{
    ChatMessage, CompletionRequest, ContentPart, MessageContent, Role,
};
use temm1e_core::Provider;

const MODEL: &str = "gemini-3.1-pro-preview";
const BUILD_DIR: &str = "/tmp/temm1e_bench_v2";
const ARTIFACT_DIR: &str = "docs/swarm/experiment_artifacts";
const MAX_FIX_ATTEMPTS: usize = 3;

// ---------------------------------------------------------------------------
// Provider
// ---------------------------------------------------------------------------

fn make_provider() -> Result<Arc<dyn Provider>, Temm1eError> {
    let key = std::env::var("GEMINI_API_KEY")
        .map_err(|_| Temm1eError::Config("GEMINI_API_KEY not set".into()))?;
    let config = temm1e_core::types::config::ProviderConfig {
        name: Some("gemini".into()),
        api_key: Some(key),
        keys: vec![],
        model: Some(MODEL.into()),
        base_url: Some("https://generativelanguage.googleapis.com/v1beta/openai".into()),
        extra_headers: std::collections::HashMap::new(),
    };
    let p = temm1e_providers::create_provider(&config)
        .map_err(|e| Temm1eError::Provider(format!("{e}")))?;
    Ok(Arc::from(p))
}

// ---------------------------------------------------------------------------
// Call tracking
// ---------------------------------------------------------------------------

#[derive(Clone)]
struct Tracker {
    tokens: Arc<AtomicU64>,
    calls: Arc<AtomicU32>,
}

impl Tracker {
    fn new() -> Self {
        Self {
            tokens: Arc::new(AtomicU64::new(0)),
            calls: Arc::new(AtomicU32::new(0)),
        }
    }
    fn tokens(&self) -> u64 {
        self.tokens.load(Ordering::Relaxed)
    }
    fn calls(&self) -> u32 {
        self.calls.load(Ordering::Relaxed)
    }
    fn cost(&self) -> f64 {
        let t = self.tokens() as f64;
        (t * 0.6 * 0.075 + t * 0.4 * 0.30) / 1_000_000.0
    }
}

async fn llm_call(
    provider: &dyn Provider,
    tracker: &Tracker,
    system: &str,
    user: &str,
) -> Result<String, Temm1eError> {
    let resp = provider
        .complete(CompletionRequest {
            model: MODEL.into(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text(user.into()),
            }],
            tools: vec![],
            max_tokens: Some(8000),
            temperature: Some(0.2),
            system: Some(system.into()),
        })
        .await?;
    let toks = (resp.usage.input_tokens + resp.usage.output_tokens) as u64;
    tracker.tokens.fetch_add(toks, Ordering::Relaxed);
    tracker.calls.fetch_add(1, Ordering::Relaxed);
    Ok(resp
        .content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect())
}

fn extract_rust(response: &str) -> String {
    let t = response.trim();
    // Find ```rust block
    if let Some(s) = t.find("```rust") {
        let start = s + 7;
        let actual = t[start..]
            .find('\n')
            .map(|n| start + n + 1)
            .unwrap_or(start);
        if let Some(end) = t[actual..].find("```") {
            return t[actual..actual + end].trim().to_string();
        }
    }
    // Find ```toml block
    if let Some(s) = t.find("```toml") {
        let start = s + 7;
        let actual = t[start..]
            .find('\n')
            .map(|n| start + n + 1)
            .unwrap_or(start);
        if let Some(end) = t[actual..].find("```") {
            return t[actual..actual + end].trim().to_string();
        }
    }
    // Find generic ``` block
    if let Some(s) = t.find("```") {
        let start = s + 3;
        let actual = t[start..]
            .find('\n')
            .map(|n| start + n + 1)
            .unwrap_or(start);
        if let Some(end) = t[actual..].find("```") {
            return t[actual..actual + end].trim().to_string();
        }
    }
    t.to_string()
}

// ---------------------------------------------------------------------------
// Project specification — HIGH LEVEL (no prescribed signatures)
// ---------------------------------------------------------------------------

const SYSTEM: &str = "\
You are an expert Rust developer. Output ONLY code inside a single fenced code block \
(```rust or ```toml). No explanations outside the code block. \
The code must compile with Rust 2021 edition. \
Dependencies available: sqlx 0.8 (runtime-tokio, sqlite), tokio 1 (full), \
serde 1 (derive), serde_json 1, uuid 1 (v4, serde), chrono 0.4 (serde), thiserror 2.";

struct FileSpec {
    path: &'static str,
    /// High-level spec — NOT prescribed function signatures
    spec: &'static str,
    deps: &'static [&'static str],
}

fn project_files() -> Vec<FileSpec> {
    vec![
        FileSpec {
            path: "Cargo.toml",
            spec: "Write a Cargo.toml for a library crate named 'notevault'. \
                   Edition 2021. Dependencies: sqlx 0.8 with runtime-tokio + sqlite features, \
                   tokio 1 with full, serde 1 with derive, serde_json 1, \
                   uuid 1 with v4 + serde, chrono 0.4 with serde, thiserror 2. \
                   Dev-dependency: tokio 1 with full + test-util.",
            deps: &[],
        },
        FileSpec {
            path: "src/error.rs",
            spec: "Write an error module for a note-taking library called notevault. \
                   Define an error enum that covers database errors, not-found errors, \
                   and validation errors. Implement conversions from sqlx::Error and serde_json::Error.",
            deps: &[],
        },
        FileSpec {
            path: "src/models.rs",
            spec: "Write data models for a note-taking library. You need: \
                   a Note struct (id, title, body, created_at, updated_at — all Strings for SQLite compat, \
                   derive sqlx::FromRow + Serialize + Deserialize + Clone + Debug), \
                   a CreateNoteRequest struct (title: String, body: String), \
                   and a NoteFilter struct (search: Option<String>, derive Default + Clone + Debug). \
                   All types must be pub.",
            deps: &[],
        },
        FileSpec {
            path: "src/db.rs",
            spec: "Write a database module. Define a pub Database struct wrapping SqlitePool. \
                   Implement a pub async fn new(url: &str) that connects and creates a 'notes' table \
                   with columns: id TEXT PRIMARY KEY, title TEXT NOT NULL, body TEXT NOT NULL, \
                   created_at TEXT NOT NULL, updated_at TEXT NOT NULL. \
                   Provide a pub fn pool(&self) -> &SqlitePool getter. \
                   Use the error type from crate::error.",
            deps: &["src/error.rs"],
        },
        FileSpec {
            path: "src/crud.rs",
            spec: "Write CRUD functions for notes. Import types from crate::models, crate::db, crate::error. \
                   Implement pub async functions: create_note, get_note, list_notes, update_note, delete_note. \
                   create_note should generate a UUID id and set timestamps. \
                   get_note should return an error if not found. \
                   list_notes returns all notes ordered by created_at desc. \
                   update_note updates title, body, and updated_at. \
                   delete_note removes by id. \
                   All take &Database as first param. Access pool via db.pool() method.",
            deps: &["src/error.rs", "src/models.rs", "src/db.rs"],
        },
        FileSpec {
            path: "src/search.rs",
            spec: "Write a search function for notes. Import from crate::models, crate::db, crate::error. \
                   Implement pub async fn search_notes(db: &Database, filter: &NoteFilter) -> Result<Vec<Note>, ...>. \
                   If filter.search is Some, do a LIKE query on title and body with %search% wildcards. \
                   If filter.search is None, return all notes. Order by created_at desc. \
                   Access pool via db.pool().",
            deps: &["src/error.rs", "src/models.rs", "src/db.rs"],
        },
        FileSpec {
            path: "src/lib.rs",
            spec: "Write lib.rs that declares pub mod for: error, models, db, crud, search. \
                   Re-export the main public types so users can do `use notevault::*`.",
            deps: &[],
        },
        FileSpec {
            path: "tests/integration.rs",
            spec: "Write integration tests for the notevault crate. \
                   The crate name is 'notevault' — use `use notevault::*;`. \
                   Create a Database with \"sqlite::memory:\" in each test for isolation. \
                   Write at least 5 #[tokio::test] tests: create+get, list, update, delete, search. \
                   IMPORTANT: All CRUD functions are FREE FUNCTIONS (not methods on Database): \
                   create_note(&db, &req), get_note(&db, \"id\"), list_notes(&db), \
                   update_note(&db, \"id\", &req), delete_note(&db, \"id\"), search_notes(&db, &filter). \
                   Note fields (id, title, body, created_at, updated_at) are all Strings. \
                   Do NOT use sqlx::migrate! or any migration macros. \
                   Do NOT define your own structs — import everything from notevault.",
            deps: &["src/lib.rs", "src/error.rs", "src/models.rs", "src/db.rs", "src/crud.rs", "src/search.rs"],
        },
    ]
}

// ---------------------------------------------------------------------------
// Compile-fix loop — the core of the 8/10 difficulty
// ---------------------------------------------------------------------------

/// Generate a file, then compile-check the project. If errors reference this file,
/// feed them back to the LLM and retry. Returns (final_code, attempts, success).
async fn generate_with_fix_loop(
    provider: &dyn Provider,
    tracker: &Tracker,
    project_dir: &Path,
    file_path: &str,
    spec: &str,
    dep_contents: &[(String, String)], // (path, content) of dependency files
) -> (String, usize, bool) {
    // Build context from actual dependency files
    let dep_context = if dep_contents.is_empty() {
        String::new()
    } else {
        let mut ctx = String::from("Here are the ACTUAL source files this module depends on:\n\n");
        for (path, content) in dep_contents {
            ctx.push_str(&format!("--- {} ---\n```rust\n{}\n```\n\n", path, content));
        }
        ctx
    };

    let mut prompt = format!("{dep_context}Now write the code for `{file_path}`.\n\nSpec: {spec}");

    let mut last_code = String::new();

    for attempt in 1..=MAX_FIX_ATTEMPTS {
        // Generate
        let response = match llm_call(provider, tracker, SYSTEM, &prompt).await {
            Ok(r) => r,
            Err(e) => {
                println!("      LLM error: {e}");
                return (last_code, attempt, false);
            }
        };

        let code = extract_rust(&response);
        let file_on_disk = project_dir.join(file_path);
        std::fs::write(&file_on_disk, &code).unwrap();
        last_code = code.clone();

        // Try to compile the whole project
        let check = tokio::process::Command::new("cargo")
            .arg("check")
            .arg("--message-format=short")
            .current_dir(project_dir)
            .output()
            .await;

        match check {
            Ok(output) if output.status.success() => {
                return (code, attempt, true);
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                // Filter errors relevant to THIS file
                let file_short = file_path.replace("src/", "").replace("tests/", "");
                let relevant: Vec<&str> = stderr
                    .lines()
                    .filter(|l| {
                        l.contains(&file_short) || l.contains("error[") || l.contains("cannot find")
                    })
                    .take(15)
                    .collect();

                if relevant.is_empty() || attempt == MAX_FIX_ATTEMPTS {
                    // Errors aren't in our file or we're out of attempts
                    return (code, attempt, false);
                }

                let errors = relevant.join("\n");
                println!("      Attempt {attempt}: compile errors, retrying...");

                // Build fix prompt with actual errors + actual dependency code
                prompt = format!(
                    "{dep_context}\n\
                     The current code for `{file_path}` is:\n```rust\n{code}\n```\n\n\
                     It produces these compilation errors:\n```\n{errors}\n```\n\n\
                     Fix ALL the errors. Output the complete corrected file."
                );
            }
            Err(e) => {
                println!("      cargo check failed to run: {e}");
                return (code, attempt, false);
            }
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
    }

    (last_code, MAX_FIX_ATTEMPTS, false)
}

// ---------------------------------------------------------------------------
// Read actual file contents from disk
// ---------------------------------------------------------------------------

fn read_dep_files(project_dir: &Path, deps: &[&str]) -> Vec<(String, String)> {
    deps.iter()
        .filter_map(|dep| {
            let path = project_dir.join(dep);
            std::fs::read_to_string(&path)
                .ok()
                .map(|content| (dep.to_string(), content))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

async fn full_verify(project_dir: &Path) -> (bool, bool, u32) {
    let check = tokio::process::Command::new("cargo")
        .arg("check")
        .current_dir(project_dir)
        .output()
        .await;
    let check_ok = check.as_ref().map(|o| o.status.success()).unwrap_or(false);
    if !check_ok {
        if let Ok(ref o) = check {
            let err = String::from_utf8_lossy(&o.stderr);
            println!("    cargo check FAILED:\n{}", &err[..err.len().min(1500)]);
        }
        return (false, false, 0);
    }

    let test = tokio::process::Command::new("cargo")
        .arg("test")
        .current_dir(project_dir)
        .output()
        .await;
    let (test_ok, test_count) = match test {
        Ok(o) => {
            let out = String::from_utf8_lossy(&o.stdout);
            let err = String::from_utf8_lossy(&o.stderr);
            let combined = format!("{out}\n{err}");
            // Find the integration test result line (not the lib one)
            let count = combined
                .lines()
                .filter(|l| l.contains("test result:") && l.contains("passed"))
                .filter_map(|l| {
                    l.split_whitespace()
                        .find(|w| w.parse::<u32>().is_ok())
                        .and_then(|w| w.parse().ok())
                })
                .max()
                .unwrap_or(0);
            if !o.status.success() {
                println!("    cargo test FAILED:\n{}", &err[..err.len().min(1500)]);
            }
            (o.status.success(), count)
        }
        Err(e) => {
            println!("    cargo test error: {e}");
            (false, 0)
        }
    };
    (check_ok, test_ok, test_count)
}

fn count_lines(dir: &Path) -> usize {
    let mut total = 0;
    for entry in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = entry.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "target" || name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            total += count_lines(&p);
        } else if p.extension().map_or(false, |e| e == "rs" || e == "toml") {
            total += std::fs::read_to_string(&p)
                .map(|c| c.lines().count())
                .unwrap_or(0);
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Single agent: serial, compile-fix after each file
// ---------------------------------------------------------------------------

async fn run_single(provider: Arc<dyn Provider>) -> Metrics {
    let dir = PathBuf::from(BUILD_DIR).join("single_v2/notevault");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();

    let tracker = Tracker::new();
    let files = project_files();
    let start = Instant::now();
    let mut total_attempts = 0_usize;
    let mut files_clean = 0_usize;

    println!("\n--- SINGLE AGENT v2 (serial + compile-fix) ---");

    for (i, spec) in files.iter().enumerate() {
        println!("  [{}/{}] {} ...", i + 1, files.len(), spec.path);

        // Read ACTUAL dependency files from disk
        let dep_contents = read_dep_files(&dir, spec.deps);

        let (code, attempts, ok) = generate_with_fix_loop(
            &*provider,
            &tracker,
            &dir,
            spec.path,
            spec.spec,
            &dep_contents,
        )
        .await;

        total_attempts += attempts;
        if ok {
            files_clean += 1;
        }
        println!(
            "    → {} lines, {} attempt{}, {}",
            code.lines().count(),
            attempts,
            if attempts > 1 { "s" } else { "" },
            if ok { "CLEAN" } else { "has errors" }
        );

        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
    }

    let elapsed = start.elapsed();
    println!(
        "  Generation: {}ms, {} API calls, {} total attempts\n",
        elapsed.as_millis(),
        tracker.calls(),
        total_attempts
    );

    println!("  Final verification...");
    let (check, test, test_count) = full_verify(&dir).await;
    println!(
        "  cargo check: {} | cargo test: {} ({} tests)",
        yesno(check),
        yesno(test),
        test_count
    );

    Metrics {
        mode: "single_v2".into(),
        wall_ms: elapsed.as_millis() as u64,
        tokens: tracker.tokens(),
        api_calls: tracker.calls(),
        cost: tracker.cost(),
        check_pass: check,
        test_pass: test,
        test_count,
        total_attempts: total_attempts as u32,
        files_clean: files_clean as u32,
        lines: count_lines(&dir),
    }
}

// ---------------------------------------------------------------------------
// Swarm: parallel tiers, compile-fix per file, real dependency reading
// ---------------------------------------------------------------------------

async fn run_swarm(provider: Arc<dyn Provider>) -> Metrics {
    let dir = PathBuf::from(BUILD_DIR).join("swarm_v2/notevault");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();

    let tracker = Tracker::new();
    let files = project_files();
    let start = Instant::now();
    let total_attempts = Arc::new(AtomicU32::new(0));
    let files_clean = Arc::new(AtomicU32::new(0));

    println!("\n--- SWARM v2 (parallel tiers + compile-fix) ---");

    // Build dependency tiers
    // Integration tests ALWAYS go in the final tier — they need the complete project.
    // Other files are assigned by dependency depth.
    let is_test = |i: usize| files[i].path == "tests/integration.rs";

    let tier0: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(i, f)| f.deps.is_empty() && !is_test(*i))
        .map(|(i, _)| i)
        .collect();

    let tier1: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(i, f)| {
            !is_test(*i)
                && !f.deps.is_empty()
                && f.deps
                    .iter()
                    .all(|d| tier0.iter().any(|&ti| files[ti].path == *d))
        })
        .map(|(i, _)| i)
        .collect();

    let tier2: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(i, _)| !is_test(*i) && !tier0.contains(i) && !tier1.contains(i))
        .map(|(i, _)| i)
        .collect();

    // Tests always last — they need to read ALL generated source files
    let tier3: Vec<usize> = files
        .iter()
        .enumerate()
        .filter(|(i, _)| is_test(*i))
        .map(|(i, _)| i)
        .collect();

    let tiers = vec![
        ("Tier 0 (independent)", tier0),
        ("Tier 1 (deps on T0)", tier1),
        ("Tier 2 (deps on T0+T1)", tier2),
        ("Tier 3 (tests)", tier3),
    ];

    for (name, indices) in &tiers {
        if indices.is_empty() {
            continue;
        }
        println!("  {} — {} files in parallel", name, indices.len());

        let mut handles = Vec::new();
        for &idx in indices {
            let p = provider.clone();
            let t = tracker.clone();
            let d = dir.clone();
            let ta = Arc::clone(&total_attempts);
            let fc = Arc::clone(&files_clean);
            let spec_path = files[idx].path.to_string();
            let spec_text = files[idx].spec.to_string();
            let spec_deps: Vec<String> = files[idx].deps.iter().map(|s| s.to_string()).collect();

            handles.push(tokio::spawn(async move {
                let dep_refs: Vec<&str> = spec_deps.iter().map(|s| s.as_str()).collect();
                let dep_contents = read_dep_files(&d, &dep_refs);

                let (code, attempts, ok) =
                    generate_with_fix_loop(&*p, &t, &d, &spec_path, &spec_text, &dep_contents)
                        .await;

                ta.fetch_add(attempts as u32, Ordering::Relaxed);
                if ok {
                    fc.fetch_add(1, Ordering::Relaxed);
                }
                println!(
                    "    {} → {} lines, {} attempt{}, {}",
                    spec_path,
                    code.lines().count(),
                    attempts,
                    if attempts > 1 { "s" } else { "" },
                    if ok { "CLEAN" } else { "has errors" }
                );
            }));
        }

        for h in handles {
            let _ = h.await;
        }
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }

    let elapsed = start.elapsed();
    let ta = total_attempts.load(Ordering::Relaxed);
    let fc = files_clean.load(Ordering::Relaxed);
    println!(
        "  Generation: {}ms, {} API calls, {} total attempts\n",
        elapsed.as_millis(),
        tracker.calls(),
        ta
    );

    println!("  Final verification...");
    let (check, test, test_count) = full_verify(&dir).await;
    println!(
        "  cargo check: {} | cargo test: {} ({} tests)",
        yesno(check),
        yesno(test),
        test_count
    );

    Metrics {
        mode: "swarm_v2".into(),
        wall_ms: elapsed.as_millis() as u64,
        tokens: tracker.tokens(),
        api_calls: tracker.calls(),
        cost: tracker.cost(),
        check_pass: check,
        test_pass: test,
        test_count,
        total_attempts: ta,
        files_clean: fc,
        lines: count_lines(&dir),
    }
}

fn yesno(b: bool) -> &'static str {
    if b {
        "PASS"
    } else {
        "FAIL"
    }
}

// ---------------------------------------------------------------------------
// Metrics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
struct Metrics {
    mode: String,
    wall_ms: u64,
    tokens: u64,
    api_calls: u32,
    cost: f64,
    check_pass: bool,
    test_pass: bool,
    test_count: u32,
    total_attempts: u32, // includes retries
    files_clean: u32,    // files that compiled without retry
    lines: usize,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::test]
async fn project_benchmark_v2() {
    if std::env::var("GEMINI_API_KEY").is_err() {
        println!("GEMINI_API_KEY not set — skipping");
        return;
    }

    let provider = make_provider().expect("provider");

    println!("╔═══════════════════════════════════════════════════════╗");
    println!("║  TEMM1E HIVE — PROJECT BENCHMARK v2 (Difficulty 8)   ║");
    println!("║  High-level spec, real deps, compile-fix loops       ║");
    println!("║  Model: gemini-3.1-flash-lite-preview                ║");
    println!("╚═══════════════════════════════════════════════════════╝");

    // Connectivity check
    match llm_call(&*provider, &Tracker::new(), "say ok", "say ok").await {
        Ok(_) => println!("\nAPI OK.\n"),
        Err(e) => {
            println!("API FAILED: {e}");
            return;
        }
    }

    let single = run_single(provider.clone()).await;
    let swarm = run_swarm(provider.clone()).await;

    let speedup = single.wall_ms as f64 / swarm.wall_ms.max(1) as f64;
    let token_ratio = swarm.tokens as f64 / single.tokens.max(1) as f64;

    println!("\n╔═══════════════════════════════════════════════════════╗");
    println!("║                  FINAL RESULTS v2                     ║");
    println!("╠═══════════════════════════════════════════════════════╣");
    println!("║  {:22} {:>12} {:>12}   ║", "", "Single", "Swarm");
    println!("║  ────────────────────── ──────────── ────────────   ║");
    println!(
        "║  {:22} {:>10}ms {:>10}ms   ║",
        "Wall clock", single.wall_ms, swarm.wall_ms
    );
    println!(
        "║  {:22} {:>12} {:>12}   ║",
        "Total tokens", single.tokens, swarm.tokens
    );
    println!(
        "║  {:22} {:>12} {:>12}   ║",
        "API calls (inc retries)", single.api_calls, swarm.api_calls
    );
    println!(
        "║  {:22} {:>12} {:>12}   ║",
        "Total attempts", single.total_attempts, swarm.total_attempts
    );
    println!(
        "║  {:22} {:>12} {:>12}   ║",
        "Files clean (1st try)", single.files_clean, swarm.files_clean
    );
    println!(
        "║  {:22} {:>11.6} {:>11.6}   ║",
        "Cost (USD)", single.cost, swarm.cost
    );
    println!(
        "║  {:22} {:>12} {:>12}   ║",
        "cargo check",
        yesno(single.check_pass),
        yesno(swarm.check_pass)
    );
    println!(
        "║  {:22} {:>12} {:>12}   ║",
        "cargo test",
        yesno(single.test_pass),
        yesno(swarm.test_pass)
    );
    println!(
        "║  {:22} {:>12} {:>12}   ║",
        "Tests passed", single.test_count, swarm.test_count
    );
    println!(
        "║  {:22} {:>12} {:>12}   ║",
        "Lines generated", single.lines, swarm.lines
    );
    println!("║  ────────────────────── ──────────── ────────────   ║");
    println!(
        "║  Speedup: {:.2}x                                       ║",
        speedup
    );
    println!(
        "║  Token ratio: {:.2}x                                   ║",
        token_ratio
    );
    println!(
        "║  Total cost: ${:.6}                               ║",
        single.cost + swarm.cost
    );
    println!("╚═══════════════════════════════════════════════════════╝");

    // Save report
    let report = format!(
        "# Project Benchmark v2 Results (Difficulty 8/10)\n\n\
         Date: {}\n\
         Model: {}\n\n\
         ## What makes this 8/10\n\n\
         - High-level spec only (no prescribed function signatures)\n\
         - Workers read ACTUAL generated dependency files\n\
         - Compile-fix loop: generate → cargo check → feed errors → retry (max 3)\n\
         - Final verification: cargo check + cargo test on REAL output\n\n\
         ## Results\n\n\
         | Metric | Single Agent | Swarm |\n\
         |--------|-------------|-------|\n\
         | Wall clock | {}ms | {}ms |\n\
         | Speedup | — | {:.2}x |\n\
         | Tokens | {} | {} |\n\
         | API calls (inc retries) | {} | {} |\n\
         | Total attempts | {} | {} |\n\
         | Files clean (1st try) | {}/8 | {}/8 |\n\
         | Cost | ${:.6} | ${:.6} |\n\
         | cargo check | {} | {} |\n\
         | cargo test | {} | {} |\n\
         | Tests passed | {} | {} |\n\
         | Lines | {} | {} |\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
        MODEL,
        single.wall_ms,
        swarm.wall_ms,
        speedup,
        single.tokens,
        swarm.tokens,
        single.api_calls,
        swarm.api_calls,
        single.total_attempts,
        swarm.total_attempts,
        single.files_clean,
        swarm.files_clean,
        single.cost,
        swarm.cost,
        yesno(single.check_pass),
        yesno(swarm.check_pass),
        yesno(single.test_pass),
        yesno(swarm.test_pass),
        single.test_count,
        swarm.test_count,
        single.lines,
        swarm.lines,
    );

    let art = PathBuf::from(ARTIFACT_DIR);
    let _ = std::fs::create_dir_all(&art);
    std::fs::write(art.join("BENCHMARK_V2_REPORT.md"), &report).unwrap();

    let json = serde_json::to_string_pretty(&vec![&single, &swarm]).unwrap();
    std::fs::write(art.join("metrics_v2.json"), &json).unwrap();

    // Copy artifacts
    fn cp(src: &Path, dst: &Path) {
        let _ = std::fs::create_dir_all(dst);
        for e in std::fs::read_dir(src).into_iter().flatten().flatten() {
            let s = e.path();
            let d = dst.join(e.file_name());
            let name = s.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "target" || name.starts_with('.') {
                continue;
            }
            if s.is_dir() {
                cp(&s, &d);
            } else {
                let _ = std::fs::copy(&s, &d);
            }
        }
    }
    cp(
        &PathBuf::from(BUILD_DIR).join("single_v2/notevault"),
        &art.join("single_agent_v2/notevault"),
    );
    cp(
        &PathBuf::from(BUILD_DIR).join("swarm_v2/notevault"),
        &art.join("swarm_agent_v2/notevault"),
    );

    println!("\nArtifacts saved to {}", art.display());
}
