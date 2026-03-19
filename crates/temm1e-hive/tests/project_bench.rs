#![allow(clippy::all, unused)]
//! Project Benchmark: Single Agent vs Swarm building a real Rust project
//!
//! Both modes build the same project: "taskforge" — a Rust library with
//! SQLite persistence, CRUD operations, search, and tests.
//!
//! Verification: `cargo check && cargo test` must pass on the output.
//! Metrics: wall-clock time, total tokens, total cost, API calls, verified done.
//!
//! Run: GEMINI_API_KEY=... cargo test -p temm1e-hive --test project_bench -- --nocapture

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::{
    ChatMessage, CompletionRequest, ContentPart, MessageContent, Role,
};
use temm1e_core::Provider;

// ---------------------------------------------------------------------------
// Constants
// ---------------------------------------------------------------------------

const MODEL: &str = "gemini-3.1-flash-lite-preview";
/// Artifacts go to /tmp during build, then copied to docs/ after verification.
/// This avoids Cargo workspace auto-detection on nested Cargo.toml files.
const ARTIFACT_BASE: &str = "/tmp/temm1e_hive_bench";
const ARTIFACT_FINAL: &str = "docs/swarm/experiment_artifacts";

// ---------------------------------------------------------------------------
// Provider setup
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
// Metrics
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
struct ProjectMetrics {
    mode: String,
    wall_clock_ms: u64,
    total_tokens: u64,
    api_calls: u32,
    cost_usd: f64,
    cargo_check_pass: bool,
    cargo_test_pass: bool,
    test_count: u32,
    files_generated: usize,
    total_lines: usize,
}

// ---------------------------------------------------------------------------
// LLM call with metrics tracking
// ---------------------------------------------------------------------------

struct CallTracker {
    tokens: AtomicU64,
    calls: AtomicU32,
}

impl CallTracker {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            tokens: AtomicU64::new(0),
            calls: AtomicU32::new(0),
        })
    }

    fn total_tokens(&self) -> u64 {
        self.tokens.load(Ordering::Relaxed)
    }

    fn total_calls(&self) -> u32 {
        self.calls.load(Ordering::Relaxed)
    }

    fn cost_usd(&self) -> f64 {
        let t = self.total_tokens() as f64;
        (t * 0.6 * 0.075 / 1_000_000.0) + (t * 0.4 * 0.30 / 1_000_000.0)
    }
}

async fn tracked_call(
    provider: &dyn Provider,
    tracker: &CallTracker,
    system: &str,
    prompt: &str,
) -> Result<String, Temm1eError> {
    let request = CompletionRequest {
        model: MODEL.into(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text(prompt.into()),
        }],
        tools: vec![],
        max_tokens: Some(8000),
        temperature: Some(0.2),
        system: Some(system.into()),
    };

    let response = provider.complete(request).await?;
    let tokens = (response.usage.input_tokens + response.usage.output_tokens) as u64;
    tracker.tokens.fetch_add(tokens, Ordering::Relaxed);
    tracker.calls.fetch_add(1, Ordering::Relaxed);

    let text = response
        .content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect::<String>();

    Ok(text)
}

/// Extract code from LLM response (strip markdown fences).
/// For .toml files, extracts TOML block. For .rs files, extracts Rust block.
fn extract_code(response: &str, file_ext: &str) -> String {
    let trimmed = response.trim();

    // Determine which language block to look for
    let lang_tag = match file_ext {
        "toml" => "toml",
        _ => "rust",
    };

    // Try ```lang ... ``` first
    let tag = format!("```{lang_tag}");
    if let Some(start) = trimmed.find(&tag) {
        let code_start = start + tag.len();
        // Skip to next newline
        let actual_start = trimmed[code_start..]
            .find('\n')
            .map(|n| code_start + n + 1)
            .unwrap_or(code_start);
        if let Some(end) = trimmed[actual_start..].find("```") {
            return trimmed[actual_start..actual_start + end].trim().to_string();
        }
    }

    // Try generic ``` ... ``` — take the FIRST block
    if let Some(start) = trimmed.find("```") {
        let code_start = start + 3;
        let actual_start = trimmed[code_start..]
            .find('\n')
            .map(|n| code_start + n + 1)
            .unwrap_or(code_start);
        if let Some(end) = trimmed[actual_start..].find("```") {
            let candidate = trimmed[actual_start..actual_start + end].trim();
            // For .rs files, skip if it looks like TOML (starts with [package])
            if file_ext == "rs" && candidate.starts_with("[package]") {
                // Look for the NEXT code block
                let after_first = actual_start + end + 3;
                if let Some(start2) = trimmed[after_first..].find("```") {
                    let code_start2 = after_first + start2 + 3;
                    let actual_start2 = trimmed[code_start2..]
                        .find('\n')
                        .map(|n| code_start2 + n + 1)
                        .unwrap_or(code_start2);
                    if let Some(end2) = trimmed[actual_start2..].find("```") {
                        return trimmed[actual_start2..actual_start2 + end2]
                            .trim()
                            .to_string();
                    }
                }
            }
            return candidate.to_string();
        }
    }

    // Raw text — for .rs files, skip anything that looks like TOML at the start
    if file_ext == "rs" {
        if let Some(use_pos) = trimmed.find("use ") {
            return trimmed[use_pos..].to_string();
        }
        if let Some(pub_pos) = trimmed.find("pub ") {
            return trimmed[pub_pos..].to_string();
        }
    }

    trimmed.to_string()
}

// ---------------------------------------------------------------------------
// Project spec — what we're building
// ---------------------------------------------------------------------------

const SYSTEM_PROMPT: &str = "\
You are an expert Rust developer. Output ONLY the requested code inside a single fenced code block. \
For Cargo.toml: output ```toml ... ```. For Rust files: output ```rust ... ```. \
Output EXACTLY ONE code block. No other text, no explanations, no extra code blocks. \
The code must compile with Rust 2021 edition. Use these exact crate versions: \
sqlx 0.8 (features: runtime-tokio, sqlite), tokio 1 (features: full), \
serde 1 (features: derive), serde_json 1, uuid 1 (features: v4, serde), \
chrono 0.4 (features: serde), thiserror 2. \
IMPORTANT: For sqlx queries, always access the pool via db.pool() method, not db.pool field.";

struct FileSpec {
    path: &'static str,
    prompt: &'static str,
    depends_on: &'static [&'static str], // files this depends on
}

fn project_files() -> Vec<FileSpec> {
    vec![
        FileSpec {
            path: "Cargo.toml",
            prompt: "\
Write a Cargo.toml for a library crate named 'taskforge'. Edition 2021. \
Dependencies: sqlx = { version = \"0.8\", features = [\"runtime-tokio\", \"sqlite\"] }, \
tokio = { version = \"1\", features = [\"full\"] }, \
serde = { version = \"1\", features = [\"derive\"] }, \
serde_json = \"1\", uuid = { version = \"1\", features = [\"v4\", \"serde\"] }, \
chrono = { version = \"0.4\", features = [\"serde\"] }, thiserror = \"2\". \
Dev-dependencies: tokio with test-util feature.",
            depends_on: &[],
        },
        FileSpec {
            path: "src/error.rs",
            prompt: "\
Write a Rust error module for a task management library. \
Define a `TaskForgeError` enum using thiserror with variants: \
Database(String), NotFound(String), Validation(String), Serialization(String). \
Implement From<sqlx::Error> and From<serde_json::Error>.",
            depends_on: &[],
        },
        FileSpec {
            path: "src/models.rs",
            prompt: "\
Write Rust data models for a task management library. Define: \
1. `Priority` enum: Low, Medium, High, Critical. Derive Serialize, Deserialize, Clone, Debug, PartialEq. \
2. `TaskStatus` enum: Todo, InProgress, Done, Cancelled. Derive Serialize, Deserialize, Clone, Debug, PartialEq. \
3. `Task` struct: id (String), title (String), description (Option<String>), \
   priority (String), status (String), created_at (String), updated_at (String). \
   Derive Serialize, Deserialize, Clone, Debug, sqlx::FromRow. \
   Note: priority and status are stored as String in SQLite, not as enums. \
4. `CreateTaskRequest` struct: title (String), description (Option<String>), priority (Priority). \
   Derive Serialize, Deserialize, Clone, Debug. \
5. `TaskFilter` struct: status (Option<TaskStatus>), priority (Option<Priority>), \
   search (Option<String>). Derive Default, Clone, Debug. \
All types must be pub. Use `use serde::{Serialize, Deserialize};` only.",
            depends_on: &[],
        },
        FileSpec {
            path: "src/db.rs",
            prompt: "\
Write a Rust database module. Use `use sqlx::sqlite::SqlitePoolOptions;` and `use sqlx::SqlitePool;` \
and `use crate::error::TaskForgeError;`. \
Define `pub struct Database { pool: SqlitePool }`. Implement: \
1. `pub async fn new(url: &str) -> Result<Self, TaskForgeError>` — use \
   SqlitePoolOptions::new().max_connections(5).connect(url).await \
   .map_err(|e| TaskForgeError::Database(e.to_string()))?, \
   then execute CREATE TABLE IF NOT EXISTS tasks (id TEXT PRIMARY KEY, title TEXT NOT NULL, \
   description TEXT, priority TEXT NOT NULL DEFAULT 'medium', status TEXT NOT NULL DEFAULT 'todo', \
   created_at TEXT NOT NULL, updated_at TEXT NOT NULL) via sqlx::query(...).execute(&pool).await. \
2. `pub fn pool(&self) -> &SqlitePool { &self.pool }` — getter.",
            depends_on: &["src/error.rs"],
        },
        FileSpec {
            path: "src/crud.rs",
            prompt: "\
Write Rust CRUD operations for tasks. Use `use crate::error::TaskForgeError;` and \
`use crate::models::{Task, CreateTaskRequest, TaskStatus};` and `use crate::db::Database;`. \
Access the pool via `db.pool()` method (NOT `db.pool` — the field is private). \
Use `.map_err(|e: sqlx::Error| TaskForgeError::Database(e.to_string()))` for error conversion. \
Implement these pub async functions: \
1. `create_task(db: &Database, req: &CreateTaskRequest) -> Result<Task, TaskForgeError>` \
   — generate UUID id via uuid::Uuid::new_v4().to_string(), set created_at/updated_at to chrono::Utc::now().to_rfc3339(), \
     bind priority as format!(\"{:?}\", req.priority).to_lowercase(), bind status as \"todo\", INSERT, return via get_task. \
2. `get_task(db: &Database, id: &str) -> Result<Task, TaskForgeError>` \
   — sqlx::query_as::<_, Task>(\"SELECT * FROM tasks WHERE id = ?\").bind(id).fetch_optional(db.pool()), return NotFound if None. \
3. `list_tasks(db: &Database) -> Result<Vec<Task>, TaskForgeError>` \
   — SELECT all ORDER BY created_at DESC. \
4. `update_status(db: &Database, id: &str, status: TaskStatus) -> Result<Task, TaskForgeError>` \
   — bind status as format!(\"{:?}\", status).to_lowercase(), UPDATE status + updated_at, return via get_task. \
5. `delete_task(db: &Database, id: &str) -> Result<(), TaskForgeError>` \
   — DELETE by id.",
            depends_on: &["src/error.rs", "src/models.rs", "src/db.rs"],
        },
        FileSpec {
            path: "src/search.rs",
            prompt: "\
Write a Rust search/filter module. Use `use crate::error::TaskForgeError;` and \
`use crate::models::{Task, TaskFilter};` and `use crate::db::Database;`. \
Access the pool via `db.pool()` method. \
Implement: `pub async fn search_tasks(db: &Database, filter: &TaskFilter) -> Result<Vec<Task>, TaskForgeError>`. \
Build a dynamic SQL query: start with `let mut sql = String::from(\"SELECT * FROM tasks WHERE 1=1\");` \
and `let mut binds: Vec<String> = Vec::new();`. \
If filter.status is Some, push ' AND status = ?' to sql, push format!(\"{:?}\", s).to_lowercase() to binds. \
If filter.priority is Some, push ' AND priority = ?' to sql, push format!(\"{:?}\", p).to_lowercase() to binds. \
If filter.search is Some, push ' AND (title LIKE ? OR description LIKE ?)' to sql, push format!(\"%{}%\", search) twice to binds. \
Append ' ORDER BY created_at DESC'. \
Then build the query: `let mut query = sqlx::query_as::<_, Task>(&sql);` \
Bind each string: `for b in &binds { query = query.bind(b); }` \
Execute: `query.fetch_all(db.pool()).await.map_err(|e| TaskForgeError::Database(e.to_string()))`. \
Use `.map_err(|e: sqlx::Error| TaskForgeError::Database(e.to_string()))` for errors.",
            depends_on: &["src/error.rs", "src/models.rs", "src/db.rs"],
        },
        FileSpec {
            path: "src/lib.rs",
            prompt: "\
Write the lib.rs for the taskforge crate. Declare these public modules: \
pub mod error; pub mod models; pub mod db; pub mod crud; pub mod search; \
Then re-export the main types: \
pub use error::TaskForgeError; \
pub use models::{Task, CreateTaskRequest, TaskFilter, Priority, TaskStatus}; \
pub use db::Database; \
pub use crud::{create_task, get_task, list_tasks, update_status, delete_task}; \
pub use search::search_tasks;",
            depends_on: &[],
        },
        FileSpec {
            path: "tests/integration.rs",
            prompt: "\
Write integration tests for the taskforge crate. Use `use taskforge::*;`. \
IMPORTANT: All CRUD functions are FREE FUNCTIONS (not methods on Database): \
  create_task(&db, &req), get_task(&db, &id), list_tasks(&db), \
  update_status(&db, &id, status), delete_task(&db, &id), \
  search_tasks(&db, &filter). \
The status enum is TaskStatus (not Status). Values: TaskStatus::Todo, TaskStatus::InProgress, TaskStatus::Done. \
Write these #[tokio::test] async tests: \
1. `test_create_and_get` — let db = Database::new(\"sqlite::memory:\").await.unwrap(); \
   let req = CreateTaskRequest { title: \"Test\".into(), description: None, priority: Priority::Medium }; \
   let task = create_task(&db, &req).await.unwrap(); \
   let fetched = get_task(&db, &task.id).await.unwrap(); assert_eq!(fetched.title, \"Test\"); \
2. `test_list_tasks` — create 3 tasks with create_task(&db, &req), call list_tasks(&db), assert len >= 3. \
3. `test_update_status` — create task, call update_status(&db, &task.id, TaskStatus::InProgress), \
   verify with assert_eq!(updated.status, \"inprogress\") since status is stored as lowercase String. \
4. `test_delete_task` — create task, call delete_task(&db, &task.id), verify get_task returns Err. \
5. `test_search_by_status` — create 2 tasks, update one to Done via update_status, \
   call search_tasks(&db, &TaskFilter { status: Some(TaskStatus::Done), ..Default::default() }), assert len 1. \
IMPORTANT: Task.status and Task.priority are String fields, not enum fields. \
Compare with string literals like \"todo\", \"inprogress\", \"done\", not with enum variants.",
            depends_on: &["src/lib.rs"],
        },
    ]
}

// ---------------------------------------------------------------------------
// Verification
// ---------------------------------------------------------------------------

async fn verify_project(project_dir: &Path) -> (bool, bool, u32) {
    // cargo check
    let check = tokio::process::Command::new("cargo")
        .arg("check")
        .current_dir(project_dir)
        .output()
        .await;

    let check_pass = match &check {
        Ok(o) => {
            if !o.status.success() {
                let stderr = String::from_utf8_lossy(&o.stderr);
                println!(
                    "    cargo check FAILED:\n{}",
                    &stderr[..stderr.len().min(2000)]
                );
            }
            o.status.success()
        }
        Err(e) => {
            println!("    cargo check error: {e}");
            false
        }
    };

    if !check_pass {
        return (false, false, 0);
    }

    // cargo test
    let test = tokio::process::Command::new("cargo")
        .arg("test")
        .arg("--")
        .arg("--nocapture")
        .current_dir(project_dir)
        .output()
        .await;

    let (test_pass, test_count) = match &test {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            let stderr = String::from_utf8_lossy(&o.stderr);
            // Parse test count from "test result: ok. N passed"
            let count = stdout
                .lines()
                .chain(stderr.lines())
                .find(|l| l.contains("test result:"))
                .and_then(|l| {
                    l.split_whitespace()
                        .find(|w| w.parse::<u32>().is_ok())
                        .and_then(|w| w.parse().ok())
                })
                .unwrap_or(0);
            if !o.status.success() {
                println!(
                    "    cargo test FAILED:\n{}",
                    &stderr[..stderr.len().min(2000)]
                );
            }
            (o.status.success(), count)
        }
        Err(e) => {
            println!("    cargo test error: {e}");
            (false, 0)
        }
    };

    (check_pass, test_pass, test_count)
}

fn count_lines(dir: &Path) -> usize {
    let mut total = 0;
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            // Skip target/ and hidden directories
            if path.is_dir() {
                let name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");
                if name == "target" || name.starts_with('.') {
                    continue;
                }
                total += count_lines(&path);
            } else if path.is_file() && path.extension().map_or(false, |e| e == "rs" || e == "toml")
            {
                if let Ok(content) = std::fs::read_to_string(&path) {
                    total += content.lines().count();
                }
            }
        }
    }
    total
}

// ---------------------------------------------------------------------------
// Single agent: serial execution
// ---------------------------------------------------------------------------

async fn run_single_agent(provider: Arc<dyn Provider>) -> ProjectMetrics {
    let dir = PathBuf::from(ARTIFACT_BASE).join("single_agent/taskforge");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();

    let tracker = CallTracker::new();
    let files = project_files();
    let start = Instant::now();

    println!("\n--- SINGLE AGENT (serial) ---");

    for (i, spec) in files.iter().enumerate() {
        println!("  [{}/{}] Generating {}...", i + 1, files.len(), spec.path);
        let call_start = Instant::now();

        let ext = if spec.path.ends_with(".toml") {
            "toml"
        } else {
            "rs"
        };
        match tracked_call(&*provider, &tracker, SYSTEM_PROMPT, spec.prompt).await {
            Ok(response) => {
                let code = extract_code(&response, ext);
                let file_path = dir.join(spec.path);
                std::fs::write(&file_path, &code).unwrap();
                let lines = code.lines().count();
                println!(
                    "    → {} lines, {}ms, call #{}",
                    lines,
                    call_start.elapsed().as_millis(),
                    tracker.total_calls()
                );
            }
            Err(e) => {
                println!("    ERROR: {e}");
            }
        }

        // Small pause between calls
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
    }

    let elapsed = start.elapsed();
    println!("  Generation complete: {}ms\n", elapsed.as_millis());

    // Verify
    println!("  Verifying...");
    let (check_pass, test_pass, test_count) = verify_project(&dir).await;
    println!(
        "  cargo check: {} | cargo test: {} ({} tests)",
        if check_pass { "PASS" } else { "FAIL" },
        if test_pass { "PASS" } else { "FAIL" },
        test_count
    );

    let total_lines = count_lines(&dir);
    let files_count = files.len();

    ProjectMetrics {
        mode: "single_agent".into(),
        wall_clock_ms: elapsed.as_millis() as u64,
        total_tokens: tracker.total_tokens(),
        api_calls: tracker.total_calls(),
        cost_usd: tracker.cost_usd(),
        cargo_check_pass: check_pass,
        cargo_test_pass: test_pass,
        test_count,
        files_generated: files_count,
        total_lines,
    }
}

// ---------------------------------------------------------------------------
// Swarm: parallel execution respecting dependencies
// ---------------------------------------------------------------------------

async fn run_swarm(provider: Arc<dyn Provider>) -> ProjectMetrics {
    let dir = PathBuf::from(ARTIFACT_BASE).join("swarm_agent/taskforge");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("src")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();

    let tracker = CallTracker::new();
    let files = project_files();
    let start = Instant::now();

    println!("\n--- SWARM (parallel where possible) ---");

    // Group files by dependency tier:
    // Tier 0: no dependencies (Cargo.toml, error.rs, models.rs, lib.rs)
    // Tier 1: depends on tier 0 (db.rs)
    // Tier 2: depends on tier 0+1 (crud.rs, search.rs)
    // Tier 3: depends on everything (tests/integration.rs)

    let tier0: Vec<&FileSpec> = files.iter().filter(|f| f.depends_on.is_empty()).collect();
    let tier1: Vec<&FileSpec> = files
        .iter()
        .filter(|f| {
            !f.depends_on.is_empty()
                && f.depends_on
                    .iter()
                    .all(|d| tier0.iter().any(|t| t.path == *d))
        })
        .collect();
    let tier2: Vec<&FileSpec> = files
        .iter()
        .filter(|f| {
            !f.depends_on.is_empty()
                && !tier0.iter().any(|t| t.path == f.path)
                && !tier1.iter().any(|t| t.path == f.path)
                && f.path != "tests/integration.rs"
        })
        .collect();
    let tier3: Vec<&FileSpec> = files
        .iter()
        .filter(|f| f.path == "tests/integration.rs")
        .collect();

    let tiers: Vec<(&str, Vec<&FileSpec>)> = vec![
        ("Tier 0 (independent)", tier0),
        ("Tier 1 (depends on tier 0)", tier1),
        ("Tier 2 (depends on tier 0+1)", tier2),
        ("Tier 3 (depends on all)", tier3),
    ];

    for (tier_name, tier_files) in &tiers {
        if tier_files.is_empty() {
            continue;
        }
        println!("  {} — {} files in parallel", tier_name, tier_files.len());

        let mut handles = Vec::new();
        for spec in tier_files {
            let p = provider.clone();
            let t = tracker.clone();
            let prompt = spec.prompt.to_string();
            let path = spec.path.to_string();
            let dir = dir.clone();
            let ext = if spec.path.ends_with(".toml") {
                "toml"
            } else {
                "rs"
            };
            let ext = ext.to_string();

            handles.push(tokio::spawn(async move {
                let call_start = Instant::now();
                match tracked_call(&*p, &t, SYSTEM_PROMPT, &prompt).await {
                    Ok(response) => {
                        let code = extract_code(&response, &ext);
                        let file_path = dir.join(&path);
                        std::fs::write(&file_path, &code).unwrap();
                        let lines = code.lines().count();
                        println!(
                            "    {} → {} lines, {}ms",
                            path,
                            lines,
                            call_start.elapsed().as_millis()
                        );
                    }
                    Err(e) => {
                        println!("    {} ERROR: {e}", path);
                    }
                }
            }));
        }

        for h in handles {
            let _ = h.await;
        }

        // Small pause between tiers
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }

    let elapsed = start.elapsed();
    println!("  Generation complete: {}ms\n", elapsed.as_millis());

    // Verify
    println!("  Verifying...");
    let (check_pass, test_pass, test_count) = verify_project(&dir).await;
    println!(
        "  cargo check: {} | cargo test: {} ({} tests)",
        if check_pass { "PASS" } else { "FAIL" },
        if test_pass { "PASS" } else { "FAIL" },
        test_count
    );

    let total_lines = count_lines(&dir);
    let files_count = files.len();

    ProjectMetrics {
        mode: "swarm_agent".into(),
        wall_clock_ms: elapsed.as_millis() as u64,
        total_tokens: tracker.total_tokens(),
        api_calls: tracker.total_calls(),
        cost_usd: tracker.cost_usd(),
        cargo_check_pass: check_pass,
        cargo_test_pass: test_pass,
        test_count,
        files_generated: files_count,
        total_lines,
    }
}

// ---------------------------------------------------------------------------
// Main benchmark
// ---------------------------------------------------------------------------

#[tokio::test]
async fn project_benchmark() {
    let key = std::env::var("GEMINI_API_KEY");
    if key.is_err() {
        println!("GEMINI_API_KEY not set — skipping project benchmark");
        return;
    }

    let provider = make_provider().expect("Failed to create Gemini provider");

    println!("╔══════════════════════════════════════════════════╗");
    println!("║  TEMM1E HIVE — PROJECT BUILD BENCHMARK          ║");
    println!("║  Single Agent vs Swarm: build 'taskforge' lib   ║");
    println!("║  Model: gemini-3.1-flash-lite-preview            ║");
    println!("║  Verification: cargo check && cargo test         ║");
    println!("╚══════════════════════════════════════════════════╝");

    // Verify API
    let request = CompletionRequest {
        model: MODEL.into(),
        messages: vec![ChatMessage {
            role: Role::User,
            content: MessageContent::Text("Say 'ready'".into()),
        }],
        tools: vec![],
        max_tokens: Some(10),
        temperature: Some(0.0),
        system: None,
    };
    match provider.complete(request).await {
        Ok(_) => println!("\nAPI connected.\n"),
        Err(e) => {
            println!("API FAILED: {e} — aborting");
            return;
        }
    }

    // Run both modes
    let single_metrics = run_single_agent(provider.clone()).await;
    let swarm_metrics = run_swarm(provider.clone()).await;

    // ── Final report ──
    let speedup = if swarm_metrics.wall_clock_ms > 0 {
        single_metrics.wall_clock_ms as f64 / swarm_metrics.wall_clock_ms as f64
    } else {
        1.0
    };
    let token_ratio = if single_metrics.total_tokens > 0 {
        swarm_metrics.total_tokens as f64 / single_metrics.total_tokens as f64
    } else {
        1.0
    };

    println!("\n╔══════════════════════════════════════════════════╗");
    println!("║              FINAL RESULTS                       ║");
    println!("╠══════════════════════════════════════════════════╣");
    println!("║  {:20} {:>12} {:>12}  ║", "", "Single", "Swarm");
    println!("║  ─────────────────── ──────────── ────────────  ║");
    println!(
        "║  {:20} {:>10}ms {:>10}ms  ║",
        "Wall clock", single_metrics.wall_clock_ms, swarm_metrics.wall_clock_ms
    );
    println!(
        "║  {:20} {:>12} {:>12}  ║",
        "Total tokens", single_metrics.total_tokens, swarm_metrics.total_tokens
    );
    println!(
        "║  {:20} {:>12} {:>12}  ║",
        "API calls", single_metrics.api_calls, swarm_metrics.api_calls
    );
    println!(
        "║  {:20} {:>11.6} {:>11.6}  ║",
        "Cost (USD)", single_metrics.cost_usd, swarm_metrics.cost_usd
    );
    println!(
        "║  {:20} {:>12} {:>12}  ║",
        "cargo check",
        if single_metrics.cargo_check_pass {
            "PASS"
        } else {
            "FAIL"
        },
        if swarm_metrics.cargo_check_pass {
            "PASS"
        } else {
            "FAIL"
        }
    );
    println!(
        "║  {:20} {:>12} {:>12}  ║",
        "cargo test",
        if single_metrics.cargo_test_pass {
            "PASS"
        } else {
            "FAIL"
        },
        if swarm_metrics.cargo_test_pass {
            "PASS"
        } else {
            "FAIL"
        }
    );
    println!(
        "║  {:20} {:>12} {:>12}  ║",
        "Tests passed", single_metrics.test_count, swarm_metrics.test_count
    );
    println!(
        "║  {:20} {:>12} {:>12}  ║",
        "Lines generated", single_metrics.total_lines, swarm_metrics.total_lines
    );
    println!("║  ─────────────────── ──────────── ────────────  ║");
    println!(
        "║  Speedup: {:.2}x                                  ║",
        speedup
    );
    println!(
        "║  Token ratio: {:.2}x                              ║",
        token_ratio
    );
    println!(
        "║  Total cost: ${:.6}                          ║",
        single_metrics.cost_usd + swarm_metrics.cost_usd
    );
    println!("╚══════════════════════════════════════════════════╝");

    // Save report
    let report = format!(
        "# Project Build Benchmark Results\n\n\
         Date: {}\n\
         Model: {}\n\n\
         ## Metrics\n\n\
         | Metric | Single Agent | Swarm |\n\
         |--------|-------------|-------|\n\
         | Wall clock | {}ms | {}ms |\n\
         | Speedup | — | {:.2}x |\n\
         | Total tokens | {} | {} |\n\
         | Token ratio | — | {:.2}x |\n\
         | API calls | {} | {} |\n\
         | Cost (USD) | ${:.6} | ${:.6} |\n\
         | cargo check | {} | {} |\n\
         | cargo test | {} | {} |\n\
         | Tests passed | {} | {} |\n\
         | Lines generated | {} | {} |\n\n\
         ## Artifacts\n\n\
         - Single agent output: `{}/single_agent/taskforge/`\n\
         - Swarm output: `{}/swarm_agent/taskforge/`\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
        MODEL,
        single_metrics.wall_clock_ms,
        swarm_metrics.wall_clock_ms,
        speedup,
        single_metrics.total_tokens,
        swarm_metrics.total_tokens,
        token_ratio,
        single_metrics.api_calls,
        swarm_metrics.api_calls,
        single_metrics.cost_usd,
        swarm_metrics.cost_usd,
        if single_metrics.cargo_check_pass {
            "PASS"
        } else {
            "FAIL"
        },
        if swarm_metrics.cargo_check_pass {
            "PASS"
        } else {
            "FAIL"
        },
        if single_metrics.cargo_test_pass {
            "PASS"
        } else {
            "FAIL"
        },
        if swarm_metrics.cargo_test_pass {
            "PASS"
        } else {
            "FAIL"
        },
        single_metrics.test_count,
        swarm_metrics.test_count,
        single_metrics.total_lines,
        swarm_metrics.total_lines,
        ARTIFACT_BASE,
        ARTIFACT_BASE,
    );

    // Copy artifacts to final location in the repo
    let final_dir = PathBuf::from(ARTIFACT_FINAL);
    let _ = std::fs::create_dir_all(&final_dir);

    let report_path = final_dir.join("BENCHMARK_REPORT.md");
    std::fs::write(&report_path, &report).unwrap();
    println!("\nReport saved to: {}", report_path.display());

    let json = serde_json::to_string_pretty(&vec![&single_metrics, &swarm_metrics]).unwrap();
    let json_path = final_dir.join("metrics.json");
    std::fs::write(&json_path, &json).unwrap();

    // Copy generated project source files
    fn copy_dir_recursive(src: &Path, dst: &Path) {
        let _ = std::fs::create_dir_all(dst);
        if let Ok(entries) = std::fs::read_dir(src) {
            for entry in entries.flatten() {
                let src_path = entry.path();
                let dst_path = dst.join(entry.file_name());
                if src_path.is_dir() {
                    copy_dir_recursive(&src_path, &dst_path);
                } else {
                    let _ = std::fs::copy(&src_path, &dst_path);
                }
            }
        }
    }

    let single_src = PathBuf::from(ARTIFACT_BASE).join("single_agent/taskforge");
    let single_dst = final_dir.join("single_agent/taskforge");
    copy_dir_recursive(&single_src, &single_dst);

    let swarm_src = PathBuf::from(ARTIFACT_BASE).join("swarm_agent/taskforge");
    let swarm_dst = final_dir.join("swarm_agent/taskforge");
    copy_dir_recursive(&swarm_src, &swarm_dst);

    println!("Artifacts copied to: {}", final_dir.display());
}
