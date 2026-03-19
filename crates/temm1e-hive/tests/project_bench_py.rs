#![allow(clippy::all, unused)]
//! Project Benchmark — Python (Fair Test)
//!
//! Why Python: each file is independently syntax-checkable via `python -m py_compile`.
//! No all-or-nothing compilation. Each swarm worker can verify its own file in isolation.
//! Final verification: `python -m pytest` on the complete project.
//!
//! Difficulty: 8/10 — high-level spec, real dependency reading, compile-fix loops.
//!
//! Run: GEMINI_API_KEY=... cargo test -p temm1e-hive --test project_bench_py -- --nocapture

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::*;
use temm1e_core::Provider;

const MODEL: &str = "gemini-3.1-pro-preview";
const BUILD_DIR: &str = "/tmp/temm1e_bench_py";
const ARTIFACT_DIR: &str = "docs/swarm/experiment_artifacts";

// ---------------------------------------------------------------------------
// Provider + tracking (same as v2)
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
    Ok(Arc::from(
        temm1e_providers::create_provider(&config)
            .map_err(|e| Temm1eError::Provider(format!("{e}")))?,
    ))
}

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
        // Gemini 3.1 Pro pricing estimate
        (t * 0.6 * 0.15 + t * 0.4 * 0.60) / 1_000_000.0
    }
}

async fn llm(
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

fn extract_python(response: &str) -> String {
    let t = response.trim();
    for tag in &["```python", "```py", "```"] {
        if let Some(s) = t.find(tag) {
            let start = s + tag.len();
            let actual = t[start..]
                .find('\n')
                .map(|n| start + n + 1)
                .unwrap_or(start);
            if let Some(end) = t[actual..].find("```") {
                return t[actual..actual + end].trim().to_string();
            }
        }
    }
    t.to_string()
}

// ---------------------------------------------------------------------------
// Project spec — high level, no prescribed signatures
// ---------------------------------------------------------------------------

const SYSTEM: &str = "\
You are an expert Python developer. Output ONLY Python code inside a single ```python block. \
No explanations outside the code block. Use type hints. Use dataclasses for models. \
Use json for persistence. Use pathlib for file paths. Python 3.9+ compatible. \
No external dependencies — stdlib only (json, dataclasses, pathlib, uuid, datetime, typing, os).";

struct FileSpec {
    path: &'static str,
    spec: &'static str,
    deps: &'static [&'static str],
}

fn project_files() -> Vec<FileSpec> {
    vec![
        FileSpec {
            path: "taskboard/__init__.py",
            spec: "Write an __init__.py that imports and re-exports the main public classes and functions \
                   from the submodules: models, storage, board, search, export. \
                   Use lazy imports or direct imports. The package name is 'taskboard'.",
            deps: &[],
        },
        FileSpec {
            path: "taskboard/models.py",
            spec: "Write data models for a task board (like Trello). Use @dataclass from dataclasses. \
                   Define: TaskStatus (enum with TODO, IN_PROGRESS, DONE), \
                   Priority (enum with LOW, MEDIUM, HIGH, CRITICAL), \
                   Task (id: str, title: str, description: str, status: TaskStatus, priority: Priority, \
                   created_at: str, column_id: str), \
                   Column (id: str, name: str, task_ids: list of str), \
                   Board (id: str, name: str, columns: list of Column). \
                   Add a to_dict() and from_dict(cls, data) classmethod on Task, Column, Board for JSON serialization. \
                   Use uuid.uuid4() for default IDs. Use datetime.utcnow().isoformat() for timestamps.",
            deps: &[],
        },
        FileSpec {
            path: "taskboard/storage.py",
            spec: "Write a JSON file-based storage engine. Define class Storage with: \
                   __init__(self, path: str) — stores the file path, \
                   save(self, board: Board) — serialize board to JSON and write to file, \
                   load(self) -> Board — read file and deserialize, return a new empty board if file doesn't exist, \
                   exists(self) -> bool — check if file exists. \
                   Use the to_dict/from_dict methods on models for serialization. \
                   Import models from taskboard.models.",
            deps: &["taskboard/models.py"],
        },
        FileSpec {
            path: "taskboard/board.py",
            spec: "Write board operations. Import from taskboard.models. Define functions (NOT methods): \
                   create_board(name: str) -> Board, \
                   add_column(board: Board, name: str) -> Column — creates column and appends to board.columns, \
                   add_task(board: Board, column_id: str, title: str, description: str, priority: Priority) -> Task \
                   — creates task, appends its id to the matching column's task_ids, returns task, \
                   move_task(board: Board, task_id: str, to_column_id: str) -> None \
                   — removes task_id from current column, adds to target column, \
                   get_all_tasks(board: Board) -> list of Task — collect all tasks from all columns, \
                   find_task(board: Board, task_id: str) -> Task or None — search across columns. \
                   Store tasks in a dict on the board object or as a flat list. \
                   IMPORTANT: You'll need somewhere to store the actual Task objects. Add a 'tasks' dict \
                   attribute to Board (id -> Task) if the model doesn't have it, or store tasks inside columns.",
            deps: &["taskboard/models.py"],
        },
        FileSpec {
            path: "taskboard/search.py",
            spec: "Write search/filter functions. Import from taskboard.models and taskboard.board. \
                   Define: search_tasks(board: Board, query: str) -> list of Task \
                   — case-insensitive search in title and description, \
                   filter_by_status(board: Board, status: TaskStatus) -> list of Task, \
                   filter_by_priority(board: Board, priority: Priority) -> list of Task. \
                   Use get_all_tasks from board module to get the task list.",
            deps: &["taskboard/models.py", "taskboard/board.py"],
        },
        FileSpec {
            path: "taskboard/export.py",
            spec: "Write export functions. Import from taskboard.models and taskboard.board. \
                   Define: export_to_markdown(board: Board) -> str \
                   — format the board as Markdown with columns as ## headers and tasks as bullet points \
                   (include title, status, priority), \
                   export_to_json(board: Board) -> str — use board.to_dict() and json.dumps with indent=2.",
            deps: &["taskboard/models.py", "taskboard/board.py"],
        },
        FileSpec {
            path: "tests/test_board.py",
            spec: "Write pytest tests for the taskboard package. \
                   Import from taskboard.models, taskboard.board, taskboard.search, \
                   taskboard.export, taskboard.storage. \
                   Write at least 8 tests: \
                   1. test_create_board — create a board, assert name matches \
                   2. test_add_column — add 2 columns, assert board has 2 columns \
                   3. test_add_task — add a task to a column, verify it exists \
                   4. test_move_task — add task to col1, move to col2, verify col2 has it \
                   5. test_search — add tasks with different titles, search for keyword, verify results \
                   6. test_filter_status — add tasks with different statuses, filter, verify \
                   7. test_export_markdown — export board to markdown, verify it contains column names and task titles \
                   8. test_storage_roundtrip — save board to temp file, load it back, verify data matches \
                   Use tmp_path fixture for storage tests. \
                   All functions are FREE FUNCTIONS, not methods: create_board(), add_column(), etc.",
            deps: &[
                "taskboard/models.py", "taskboard/board.py",
                "taskboard/search.py", "taskboard/export.py", "taskboard/storage.py",
            ],
        },
    ]
}

// ---------------------------------------------------------------------------
// Per-file verification: python -m py_compile (independent!)
// ---------------------------------------------------------------------------

async fn check_syntax(file_path: &Path) -> (bool, String) {
    let out = tokio::process::Command::new("python3")
        .args(["-m", "py_compile"])
        .arg(file_path)
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => (true, String::new()),
        Ok(o) => (false, String::from_utf8_lossy(&o.stderr).to_string()),
        Err(e) => (false, format!("Failed to run py_compile: {e}")),
    }
}

/// Import check — verifies the module can actually be imported
async fn check_import(project_dir: &Path, module: &str) -> (bool, String) {
    let out = tokio::process::Command::new("python3")
        .args(["-c", &format!("import {module}")])
        .env("PYTHONPATH", project_dir.parent().unwrap_or(project_dir))
        .current_dir(project_dir.parent().unwrap_or(project_dir))
        .output()
        .await;
    match out {
        Ok(o) if o.status.success() => (true, String::new()),
        Ok(o) => (false, String::from_utf8_lossy(&o.stderr).to_string()),
        Err(e) => (false, format!("{e}")),
    }
}

// ---------------------------------------------------------------------------
// Compile-fix loop — per-file, independent
// ---------------------------------------------------------------------------

async fn generate_with_fix(
    provider: &dyn Provider,
    tracker: &Tracker,
    project_dir: &Path,
    file_path: &str,
    spec: &str,
    dep_contents: &[(String, String)],
) -> (String, usize, bool) {
    let dep_ctx = if dep_contents.is_empty() {
        String::new()
    } else {
        let mut ctx = String::from("Here are the ACTUAL source files this module depends on:\n\n");
        for (p, c) in dep_contents {
            ctx.push_str(&format!("--- {p} ---\n```python\n{c}\n```\n\n"));
        }
        ctx
    };

    let mut prompt = format!("{dep_ctx}Now write the code for `{file_path}`.\n\nSpec: {spec}");
    let mut last_code = String::new();
    let max_attempts = 3;

    for attempt in 1..=max_attempts {
        let response = match llm(provider, tracker, SYSTEM, &prompt).await {
            Ok(r) => r,
            Err(e) => {
                println!("      LLM error: {e}");
                return (last_code, attempt, false);
            }
        };

        let code = extract_python(&response);
        let file_on_disk = project_dir.join(file_path);
        if let Some(parent) = file_on_disk.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::write(&file_on_disk, &code).unwrap();
        last_code = code.clone();

        // Per-file syntax check (independent — doesn't need other files)
        let (syntax_ok, syntax_err) = check_syntax(&file_on_disk).await;

        if syntax_ok {
            // Also try import check if it's a module (not test)
            if file_path.starts_with("taskboard/") && file_path != "taskboard/__init__.py" {
                let module = file_path.replace('/', ".").replace(".py", "");
                let (import_ok, import_err) = check_import(project_dir, &module).await;
                if import_ok {
                    return (code, attempt, true);
                }
                if attempt < max_attempts {
                    println!("      Attempt {attempt}: import error, retrying...");
                    prompt = format!(
                        "{dep_ctx}\nCurrent code for `{file_path}`:\n```python\n{code}\n```\n\n\
                         Import error:\n```\n{}\n```\n\n\
                         Fix the errors. Output the complete corrected file.",
                        &import_err[..import_err.len().min(1000)]
                    );
                    continue;
                }
            }
            return (code, attempt, true);
        }

        if attempt < max_attempts {
            println!("      Attempt {attempt}: syntax error, retrying...");
            prompt = format!(
                "{dep_ctx}\nCurrent code for `{file_path}`:\n```python\n{code}\n```\n\n\
                 Syntax errors:\n```\n{}\n```\n\n\
                 Fix ALL errors. Output the complete corrected file.",
                &syntax_err[..syntax_err.len().min(1000)]
            );
        }

        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;
    }

    (last_code, max_attempts, false)
}

fn read_deps(project_dir: &Path, deps: &[&str]) -> Vec<(String, String)> {
    deps.iter()
        .filter_map(|d| {
            std::fs::read_to_string(project_dir.join(d))
                .ok()
                .map(|c| (d.to_string(), c))
        })
        .collect()
}

// ---------------------------------------------------------------------------
// Final verification: pytest
// ---------------------------------------------------------------------------

async fn run_pytest(project_dir: &Path) -> (bool, u32, String) {
    let out = tokio::process::Command::new("python3")
        .args(["-m", "pytest", "tests/", "-v", "--tb=short"])
        .env("PYTHONPATH", project_dir)
        .current_dir(project_dir)
        .output()
        .await;
    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            let stderr = String::from_utf8_lossy(&o.stderr).to_string();
            let combined = format!("{stdout}\n{stderr}");

            // Count passed tests
            let passed = combined.lines().filter(|l| l.contains("PASSED")).count() as u32;

            let failed = combined.lines().filter(|l| l.contains("FAILED")).count() as u32;

            (
                o.status.success(),
                passed,
                format!(
                    "{passed} passed, {failed} failed\n{}",
                    combined
                        .lines()
                        .rev()
                        .take(5)
                        .collect::<Vec<_>>()
                        .into_iter()
                        .rev()
                        .collect::<Vec<_>>()
                        .join("\n")
                ),
            )
        }
        Err(e) => (false, 0, format!("pytest error: {e}")),
    }
}

// ---------------------------------------------------------------------------
// Single agent: serial
// ---------------------------------------------------------------------------

async fn run_single(provider: Arc<dyn Provider>) -> Metrics {
    let dir = PathBuf::from(BUILD_DIR).join("single");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("taskboard")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    // Empty __init__ for tests
    std::fs::write(dir.join("tests/__init__.py"), "").unwrap();

    let tracker = Tracker::new();
    let files = project_files();
    let start = Instant::now();
    let mut total_attempts = 0_usize;
    let mut files_clean = 0_usize;

    println!("\n--- SINGLE AGENT (serial + per-file compile-fix) ---");

    for (i, spec) in files.iter().enumerate() {
        println!("  [{}/{}] {} ...", i + 1, files.len(), spec.path);
        let deps = read_deps(&dir, spec.deps);
        let (code, attempts, ok) =
            generate_with_fix(&*provider, &tracker, &dir, spec.path, spec.spec, &deps).await;
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
        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }

    let elapsed = start.elapsed();
    println!(
        "  Generation: {}ms, {} calls, {} attempts\n",
        elapsed.as_millis(),
        tracker.calls(),
        total_attempts
    );

    println!("  Running pytest...");
    let (test_pass, test_count, test_output) = run_pytest(&dir).await;
    println!("  {test_output}");

    Metrics {
        mode: "single".into(),
        wall_ms: elapsed.as_millis() as u64,
        tokens: tracker.tokens(),
        api_calls: tracker.calls(),
        cost: tracker.cost(),
        test_pass,
        test_count,
        total_attempts: total_attempts as u32,
        files_clean: files_clean as u32,
        lines: count_py_lines(&dir),
    }
}

// ---------------------------------------------------------------------------
// Swarm: parallel tiers
// ---------------------------------------------------------------------------

async fn run_swarm(provider: Arc<dyn Provider>) -> Metrics {
    let dir = PathBuf::from(BUILD_DIR).join("swarm");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("taskboard")).unwrap();
    std::fs::create_dir_all(dir.join("tests")).unwrap();
    std::fs::write(dir.join("tests/__init__.py"), "").unwrap();

    let tracker = Tracker::new();
    let files = project_files();
    let start = Instant::now();
    let total_attempts = Arc::new(AtomicU32::new(0));
    let files_clean = Arc::new(AtomicU32::new(0));

    println!("\n--- SWARM (parallel tiers + per-file compile-fix) ---");

    // Tier assignment
    let is_test = |i: usize| files[i].path.starts_with("tests/");
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
        ("Tier 3 (tests — reads all)", tier3),
    ];

    for (name, indices) in &tiers {
        if indices.is_empty() {
            continue;
        }
        println!(
            "  {} — {} file{}",
            name,
            indices.len(),
            if indices.len() > 1 { "s parallel" } else { "" }
        );

        let mut handles = Vec::new();
        for &idx in indices {
            let p = provider.clone();
            let t = tracker.clone();
            let d = dir.clone();
            let ta = Arc::clone(&total_attempts);
            let fc = Arc::clone(&files_clean);
            let path = files[idx].path.to_string();
            let spec = files[idx].spec.to_string();
            let dep_paths: Vec<String> = files[idx].deps.iter().map(|s| s.to_string()).collect();

            handles.push(tokio::spawn(async move {
                let dep_refs: Vec<&str> = dep_paths.iter().map(|s| s.as_str()).collect();
                let deps = read_deps(&d, &dep_refs);
                let (code, attempts, ok) =
                    generate_with_fix(&*p, &t, &d, &path, &spec, &deps).await;
                ta.fetch_add(attempts as u32, Ordering::Relaxed);
                if ok {
                    fc.fetch_add(1, Ordering::Relaxed);
                }
                println!(
                    "    {} → {} lines, {} attempt{}, {}",
                    path,
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
        "  Generation: {}ms, {} calls, {} attempts\n",
        elapsed.as_millis(),
        tracker.calls(),
        ta
    );

    println!("  Running pytest...");
    let (test_pass, test_count, test_output) = run_pytest(&dir).await;
    println!("  {test_output}");

    Metrics {
        mode: "swarm".into(),
        wall_ms: elapsed.as_millis() as u64,
        tokens: tracker.tokens(),
        api_calls: tracker.calls(),
        cost: tracker.cost(),
        test_pass,
        test_count,
        total_attempts: ta,
        files_clean: fc,
        lines: count_py_lines(&dir),
    }
}

fn count_py_lines(dir: &Path) -> usize {
    let mut total = 0;
    for e in std::fs::read_dir(dir).into_iter().flatten().flatten() {
        let p = e.path();
        let name = p.file_name().and_then(|n| n.to_str()).unwrap_or("");
        if name == "__pycache__" || name.starts_with('.') {
            continue;
        }
        if p.is_dir() {
            total += count_py_lines(&p);
        } else if p.extension().map_or(false, |e| e == "py") {
            total += std::fs::read_to_string(&p)
                .map(|c| c.lines().count())
                .unwrap_or(0);
        }
    }
    total
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
    test_pass: bool,
    test_count: u32,
    total_attempts: u32,
    files_clean: u32,
    lines: usize,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::test]
async fn python_project_benchmark() {
    if std::env::var("GEMINI_API_KEY").is_err() {
        println!("GEMINI_API_KEY not set — skipping");
        return;
    }
    let provider = make_provider().expect("provider");

    println!("╔═══════════════════════════════════════════════════════╗");
    println!("║  TEMM1E HIVE — PYTHON PROJECT BENCHMARK (Diff 8)    ║");
    println!("║  Per-file syntax check (no all-or-nothing)          ║");
    println!("║  Model: {}                ║", MODEL);
    println!("╚═══════════════════════════════════════════════════════╝");

    match llm(&*provider, &Tracker::new(), "say ok", "say ok").await {
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
    println!("║              FINAL RESULTS (Python)                   ║");
    println!("╠═══════════════════════════════════════════════════════╣");
    println!("║  {:24} {:>10} {:>10}   ║", "", "Single", "Swarm");
    println!("║  ──────────────────────── ────────── ──────────   ║");
    println!(
        "║  {:24} {:>8}ms {:>8}ms   ║",
        "Wall clock", single.wall_ms, swarm.wall_ms
    );
    println!(
        "║  {:24} {:>10} {:>10}   ║",
        "Total tokens", single.tokens, swarm.tokens
    );
    println!(
        "║  {:24} {:>10} {:>10}   ║",
        "API calls (inc retries)", single.api_calls, swarm.api_calls
    );
    println!(
        "║  {:24} {:>10} {:>10}   ║",
        "Total attempts", single.total_attempts, swarm.total_attempts
    );
    println!(
        "║  {:24} {:>10} {:>10}   ║",
        "Files clean (1st try)",
        format!("{}/7", single.files_clean),
        format!("{}/7", swarm.files_clean)
    );
    println!(
        "║  {:24} {:>9.6} {:>9.6}   ║",
        "Cost (USD)", single.cost, swarm.cost
    );
    println!(
        "║  {:24} {:>10} {:>10}   ║",
        "pytest",
        if single.test_pass { "PASS" } else { "FAIL" },
        if swarm.test_pass { "PASS" } else { "FAIL" }
    );
    println!(
        "║  {:24} {:>10} {:>10}   ║",
        "Tests passed", single.test_count, swarm.test_count
    );
    println!(
        "║  {:24} {:>10} {:>10}   ║",
        "Lines generated", single.lines, swarm.lines
    );
    println!("║  ──────────────────────── ────────── ──────────   ║");
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

    // Save
    let art = PathBuf::from(ARTIFACT_DIR);
    let _ = std::fs::create_dir_all(&art);
    let json = serde_json::to_string_pretty(&vec![&single, &swarm]).unwrap();
    std::fs::write(art.join("metrics_python.json"), &json).unwrap();

    // Copy artifacts
    fn cp(src: &Path, dst: &Path) {
        let _ = std::fs::create_dir_all(dst);
        for e in std::fs::read_dir(src).into_iter().flatten().flatten() {
            let s = e.path();
            let d = dst.join(e.file_name());
            let name = s.file_name().and_then(|n| n.to_str()).unwrap_or("");
            if name == "__pycache__" || name.starts_with('.') {
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
        &PathBuf::from(BUILD_DIR).join("single"),
        &art.join("single_agent_py"),
    );
    cp(
        &PathBuf::from(BUILD_DIR).join("swarm"),
        &art.join("swarm_agent_py"),
    );

    let report = format!(
        "# Python Project Benchmark (Difficulty 8/10)\n\n\
         Date: {}\nModel: {}\n\n\
         ## Why Python (fair test)\n\n\
         - Per-file syntax check: `python -m py_compile` — each worker verifies independently\n\
         - No all-or-nothing compilation (unlike Rust's cargo check)\n\
         - Final test: `pytest` on complete project\n\n\
         ## Results\n\n\
         | Metric | Single | Swarm |\n\
         |--------|--------|-------|\n\
         | Wall clock | {}ms | {}ms |\n\
         | Speedup | — | {:.2}x |\n\
         | Tokens | {} | {} |\n\
         | API calls | {} | {} |\n\
         | Total attempts | {} | {} |\n\
         | Files clean 1st try | {}/7 | {}/7 |\n\
         | Cost | ${:.6} | ${:.6} |\n\
         | pytest | {} | {} |\n\
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
        if single.test_pass { "PASS" } else { "FAIL" },
        if swarm.test_pass { "PASS" } else { "FAIL" },
        single.test_count,
        swarm.test_count,
        single.lines,
        swarm.lines,
    );
    std::fs::write(art.join("BENCHMARK_PYTHON_REPORT.md"), &report).unwrap();
    println!("\nArtifacts saved.");
}
