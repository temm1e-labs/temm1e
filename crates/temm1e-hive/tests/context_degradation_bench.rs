#![allow(clippy::all, unused)]
//! Context Degradation Benchmark — The Honest Test
//!
//! Tests the core hypothesis: context accumulation degrades LLM output quality.
//!
//! Single agent: generates function N with functions 1..N-1 in context
//!               (simulates real multi-turn conversation where history grows)
//! Swarm: each worker gets fresh context with only its spec
//!
//! 12 independent utility functions, each with a unit test.
//! Metric: how many pass their individual test.
//!
//! GEMINI_API_KEY=... cargo test -p temm1e-hive --test context_degradation_bench -- --nocapture

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::*;
use temm1e_core::Provider;

const MODEL: &str = "gemini-3.1-pro-preview";
const BUILD_DIR: &str = "/tmp/temm1e_context_bench";
const ARTIFACT_DIR: &str = "docs/swarm/experiment_artifacts";

// ---------------------------------------------------------------------------
// Provider + tracking
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
        (t * 0.6 * 0.15 + t * 0.4 * 0.60) / 1_000_000.0
    }
}

async fn llm(
    provider: &dyn Provider,
    tracker: &Tracker,
    system: &str,
    user: &str,
) -> Result<(String, u64), Temm1eError> {
    let resp = provider
        .complete(CompletionRequest {
            model: MODEL.into(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text(user.into()),
            }],
            tools: vec![],
            max_tokens: Some(4000),
            temperature: Some(0.2),
            system: Some(system.into()),
        })
        .await?;
    let toks = (resp.usage.input_tokens + resp.usage.output_tokens) as u64;
    tracker.tokens.fetch_add(toks, Ordering::Relaxed);
    tracker.calls.fetch_add(1, Ordering::Relaxed);
    let text = resp
        .content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.clone()),
            _ => None,
        })
        .collect();
    Ok((text, toks))
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
// The 12 functions + their tests
// ---------------------------------------------------------------------------

struct FuncSpec {
    name: &'static str,
    spec: &'static str,
    test_code: &'static str,
}

fn functions() -> Vec<FuncSpec> {
    vec![
        FuncSpec {
            name: "reverse_words",
            spec: "Write a function reverse_words(s: str) -> str that reverses the order of words in a string. \
                   Example: reverse_words('hello world foo') returns 'foo world hello'.",
            test_code: "assert reverse_words('hello world foo') == 'foo world hello'\n\
                        assert reverse_words('a') == 'a'\n\
                        assert reverse_words('') == ''",
        },
        FuncSpec {
            name: "flatten_list",
            spec: "Write a function flatten_list(lst) -> list that takes a nested list of arbitrary depth \
                   and returns a flat list. Example: flatten_list([1, [2, [3, 4], 5]]) returns [1, 2, 3, 4, 5].",
            test_code: "assert flatten_list([1, [2, [3, 4], 5]]) == [1, 2, 3, 4, 5]\n\
                        assert flatten_list([]) == []\n\
                        assert flatten_list([[1], [2], [3]]) == [1, 2, 3]",
        },
        FuncSpec {
            name: "is_palindrome",
            spec: "Write a function is_palindrome(s: str) -> bool that checks if a string is a palindrome, \
                   ignoring case and non-alphanumeric characters. Example: is_palindrome('A man, a plan, a canal: Panama') returns True.",
            test_code: "assert is_palindrome('A man, a plan, a canal: Panama') == True\n\
                        assert is_palindrome('racecar') == True\n\
                        assert is_palindrome('hello') == False",
        },
        FuncSpec {
            name: "chunk_list",
            spec: "Write a function chunk_list(lst: list, n: int) -> list that splits a list into chunks of size n. \
                   The last chunk may be smaller. Example: chunk_list([1,2,3,4,5], 2) returns [[1,2],[3,4],[5]].",
            test_code: "assert chunk_list([1,2,3,4,5], 2) == [[1,2],[3,4],[5]]\n\
                        assert chunk_list([1,2,3], 3) == [[1,2,3]]\n\
                        assert chunk_list([], 5) == []",
        },
        FuncSpec {
            name: "caesar_cipher",
            spec: "Write a function caesar_cipher(text: str, shift: int) -> str that applies a Caesar cipher. \
                   Only shift letters (a-z, A-Z), leave other characters unchanged. \
                   Example: caesar_cipher('Hello, World!', 3) returns 'Khoor, Zruog!'.",
            test_code: "assert caesar_cipher('Hello, World!', 3) == 'Khoor, Zruog!'\n\
                        assert caesar_cipher('abc', 1) == 'bcd'\n\
                        assert caesar_cipher('xyz', 3) == 'abc'",
        },
        FuncSpec {
            name: "merge_sorted",
            spec: "Write a function merge_sorted(a: list, b: list) -> list that merges two sorted lists \
                   into one sorted list without using sort(). Example: merge_sorted([1,3,5], [2,4,6]) returns [1,2,3,4,5,6].",
            test_code: "assert merge_sorted([1,3,5], [2,4,6]) == [1,2,3,4,5,6]\n\
                        assert merge_sorted([], [1,2]) == [1,2]\n\
                        assert merge_sorted([1], []) == [1]",
        },
        FuncSpec {
            name: "most_frequent",
            spec: "Write a function most_frequent(lst: list) -> any that returns the most frequently occurring \
                   element. If tie, return any of the tied elements. Example: most_frequent([1,2,2,3,3,3]) returns 3.",
            test_code: "assert most_frequent([1,2,2,3,3,3]) == 3\n\
                        assert most_frequent(['a','b','a']) == 'a'\n\
                        assert most_frequent([42]) == 42",
        },
        FuncSpec {
            name: "matrix_transpose",
            spec: "Write a function matrix_transpose(matrix: list) -> list that transposes a 2D matrix (list of lists). \
                   Example: matrix_transpose([[1,2,3],[4,5,6]]) returns [[1,4],[2,5],[3,6]].",
            test_code: "assert matrix_transpose([[1,2,3],[4,5,6]]) == [[1,4],[2,5],[3,6]]\n\
                        assert matrix_transpose([[1]]) == [[1]]\n\
                        assert matrix_transpose([[1,2],[3,4],[5,6]]) == [[1,3,5],[2,4,6]]",
        },
        FuncSpec {
            name: "run_length_encode",
            spec: "Write a function run_length_encode(s: str) -> str that performs run-length encoding. \
                   Example: run_length_encode('aaabbbcc') returns 'a3b3c2'. Single chars get count 1: 'abc' -> 'a1b1c1'.",
            test_code: "assert run_length_encode('aaabbbcc') == 'a3b3c2'\n\
                        assert run_length_encode('abc') == 'a1b1c1'\n\
                        assert run_length_encode('') == ''",
        },
        FuncSpec {
            name: "deep_get",
            spec: "Write a function deep_get(d: dict, path: str, default=None) that gets a nested dict value by dot path. \
                   Example: deep_get({'a': {'b': {'c': 42}}}, 'a.b.c') returns 42. Returns default if path doesn't exist.",
            test_code: "assert deep_get({'a': {'b': {'c': 42}}}, 'a.b.c') == 42\n\
                        assert deep_get({'a': 1}, 'a.b', 'nope') == 'nope'\n\
                        assert deep_get({}, 'x') is None",
        },
        FuncSpec {
            name: "validate_brackets",
            spec: "Write a function validate_brackets(s: str) -> bool that checks if brackets ()[]\\{\\} are balanced. \
                   Example: validate_brackets('([{}])') returns True, validate_brackets('([)]') returns False.",
            test_code: "assert validate_brackets('([{}])') == True\n\
                        assert validate_brackets('([)]') == False\n\
                        assert validate_brackets('') == True\n\
                        assert validate_brackets('((())') == False",
        },
        FuncSpec {
            name: "int_to_roman",
            spec: "Write a function int_to_roman(num: int) -> str that converts an integer (1-3999) to a Roman numeral. \
                   Example: int_to_roman(1994) returns 'MCMXCIV'.",
            test_code: "assert int_to_roman(1994) == 'MCMXCIV'\n\
                        assert int_to_roman(3) == 'III'\n\
                        assert int_to_roman(58) == 'LVIII'\n\
                        assert int_to_roman(9) == 'IX'",
        },
    ]
}

const SYSTEM: &str = "\
You are a Python developer. Output ONLY the function inside a ```python block. \
No imports needed (just use builtins). No explanation. Just the function.";

// ---------------------------------------------------------------------------
// Run test on a single function
// ---------------------------------------------------------------------------

async fn test_function(dir: &Path, func_name: &str, code: &str, test_code: &str) -> bool {
    let test_script = format!("{code}\n\n# Test\n{test_code}\nprint('PASS: {func_name}')");
    let test_file = dir.join(format!("test_{func_name}.py"));
    std::fs::write(&test_file, &test_script).unwrap();

    let out = tokio::process::Command::new("python3")
        .arg(&test_file)
        .output()
        .await;

    match out {
        Ok(o) => {
            let stdout = String::from_utf8_lossy(&o.stdout);
            o.status.success() && stdout.contains("PASS")
        }
        Err(_) => false,
    }
}

// ---------------------------------------------------------------------------
// Single agent: accumulating context (simulates multi-turn conversation)
// ---------------------------------------------------------------------------

async fn run_single(provider: Arc<dyn Provider>) -> RunResult {
    let dir = PathBuf::from(BUILD_DIR).join("single");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let tracker = Tracker::new();
    let funcs = functions();
    let start = Instant::now();

    // This accumulates ALL previous outputs — simulating conversation history
    let mut history = String::new();
    let mut results: Vec<FuncResult> = Vec::new();

    println!("\n--- SINGLE AGENT (accumulating context) ---");
    println!("  Each function sees ALL previous outputs in context.\n");

    for (i, func) in funcs.iter().enumerate() {
        let prompt = if history.is_empty() {
            format!("Write the Python function: {}", func.spec)
        } else {
            // THIS IS THE KEY: stuff all previous code into the prompt
            format!(
                "Here is the conversation history of all functions we've written so far:\n\n\
                 {history}\n\n\
                 ---\n\n\
                 Now write the NEXT function: {}",
                func.spec
            )
        };

        let call_start = Instant::now();
        let (response, toks) = match llm(&*provider, &tracker, SYSTEM, &prompt).await {
            Ok(r) => r,
            Err(e) => {
                println!("  [{:>2}/12] {} — ERROR: {e}", i + 1, func.name);
                results.push(FuncResult {
                    name: func.name.into(),
                    passed: false,
                    tokens: 0,
                    latency_ms: call_start.elapsed().as_millis() as u64,
                    context_size: history.len(),
                });
                continue;
            }
        };
        let code = extract_python(&response);
        let latency = call_start.elapsed().as_millis() as u64;

        // Test it
        let passed = test_function(&dir, func.name, &code, func.test_code).await;

        // Accumulate into history (this is what makes later functions harder)
        history.push_str(&format!(
            "## Function {}: {}\n```python\n{}\n```\n\n",
            i + 1,
            func.name,
            code
        ));

        let ctx_size = history.len();
        println!(
            "  [{:>2}/12] {:<22} {}  {}ms  {}tok  ctx={}",
            i + 1,
            func.name,
            if passed { "PASS" } else { "FAIL" },
            latency,
            toks,
            ctx_size,
        );

        results.push(FuncResult {
            name: func.name.into(),
            passed,
            tokens: toks,
            latency_ms: latency,
            context_size: ctx_size,
        });

        tokio::time::sleep(tokio::time::Duration::from_millis(200)).await;
    }

    let elapsed = start.elapsed();
    let passed = results.iter().filter(|r| r.passed).count();
    println!(
        "\n  Result: {passed}/12 passed, {}ms, {} tokens\n",
        elapsed.as_millis(),
        tracker.tokens()
    );

    RunResult {
        mode: "single".into(),
        wall_ms: elapsed.as_millis() as u64,
        tokens: tracker.tokens(),
        calls: tracker.calls(),
        cost: tracker.cost(),
        passed: passed as u32,
        total: 12,
        results,
    }
}

// ---------------------------------------------------------------------------
// Swarm: fresh context per function (parallel)
// ---------------------------------------------------------------------------

async fn run_swarm(provider: Arc<dyn Provider>) -> RunResult {
    let dir = PathBuf::from(BUILD_DIR).join("swarm");
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();

    let tracker = Tracker::new();
    let funcs = functions();
    let start = Instant::now();

    println!("--- SWARM (fresh context, 12 parallel workers) ---");
    println!("  Each function gets ONLY its spec. No history.\n");

    let mut handles = Vec::new();

    for (i, func) in funcs.iter().enumerate() {
        let p = provider.clone();
        let t = tracker.clone();
        let d = dir.clone();
        let name = func.name.to_string();
        let spec = func.spec.to_string();
        let test = func.test_code.to_string();

        handles.push(tokio::spawn(async move {
            // FRESH context — just the spec, nothing else
            let prompt = format!("Write the Python function: {spec}");

            let call_start = Instant::now();
            let (response, toks) = match llm(&*p, &t, SYSTEM, &prompt).await {
                Ok(r) => r,
                Err(e) => {
                    println!("  [{:>2}/12] {:<22} ERROR: {e}", i + 1, name);
                    return FuncResult {
                        name: name.clone(),
                        passed: false,
                        tokens: 0,
                        latency_ms: call_start.elapsed().as_millis() as u64,
                        context_size: 0,
                    };
                }
            };
            let code = extract_python(&response);
            let latency = call_start.elapsed().as_millis() as u64;

            let passed = test_function(&d, &name, &code, &test).await;

            println!(
                "  [{:>2}/12] {:<22} {}  {}ms  {}tok  ctx={}",
                i + 1,
                name,
                if passed { "PASS" } else { "FAIL" },
                latency,
                toks,
                spec.len(), // context is just the spec
            );

            FuncResult {
                name,
                passed,
                tokens: toks,
                latency_ms: latency,
                context_size: spec.len(),
            }
        }));
    }

    let mut results = Vec::new();
    for h in handles {
        match h.await {
            Ok(r) => results.push(r),
            Err(_) => {}
        }
    }

    let elapsed = start.elapsed();
    let passed = results.iter().filter(|r| r.passed).count();
    println!(
        "\n  Result: {passed}/12 passed, {}ms, {} tokens\n",
        elapsed.as_millis(),
        tracker.tokens()
    );

    RunResult {
        mode: "swarm".into(),
        wall_ms: elapsed.as_millis() as u64,
        tokens: tracker.tokens(),
        calls: tracker.calls(),
        cost: tracker.cost(),
        passed: passed as u32,
        total: 12,
        results,
    }
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, serde::Serialize)]
struct FuncResult {
    name: String,
    passed: bool,
    tokens: u64,
    latency_ms: u64,
    context_size: usize,
}

#[derive(Debug, Clone, serde::Serialize)]
struct RunResult {
    mode: String,
    wall_ms: u64,
    tokens: u64,
    calls: u32,
    cost: f64,
    passed: u32,
    total: u32,
    results: Vec<FuncResult>,
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::test]
async fn context_degradation_benchmark() {
    if std::env::var("GEMINI_API_KEY").is_err() {
        println!("GEMINI_API_KEY not set — skipping");
        return;
    }
    let provider = make_provider().expect("provider");

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║  CONTEXT DEGRADATION BENCHMARK                          ║");
    println!("║  Single agent: accumulates ALL previous outputs         ║");
    println!("║  Swarm: fresh context per function (parallel)           ║");
    println!("║  12 independent functions, each individually tested     ║");
    println!("║  Model: {:<46} ║", MODEL);
    println!("╚══════════════════════════════════════════════════════════╝");

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

    println!("╔══════════════════════════════════════════════════════════╗");
    println!("║                    FINAL RESULTS                         ║");
    println!("╠══════════════════════════════════════════════════════════╣");
    println!("║  {:26} {:>10} {:>10}   ║", "", "Single", "Swarm");
    println!("║  ──────────────────────────── ────────── ──────────   ║");
    println!(
        "║  {:26} {:>8}/12 {:>8}/12   ║",
        "Functions passing tests", single.passed, swarm.passed
    );
    println!(
        "║  {:26} {:>8}ms {:>8}ms   ║",
        "Wall clock", single.wall_ms, swarm.wall_ms
    );
    println!(
        "║  {:26} {:>10} {:>10}   ║",
        "Total tokens", single.tokens, swarm.tokens
    );
    println!(
        "║  {:26} {:>10} {:>10}   ║",
        "API calls", single.calls, swarm.calls
    );
    println!(
        "║  {:26} {:>9.6} {:>9.6}   ║",
        "Cost (USD)", single.cost, swarm.cost
    );
    println!("║  ──────────────────────────── ────────── ──────────   ║");
    println!(
        "║  Speedup: {:.2}x                                          ║",
        speedup
    );
    println!(
        "║  Quality: {} vs {}                                     ║",
        format!("{}/12", single.passed),
        format!("{}/12", swarm.passed)
    );
    println!("╚══════════════════════════════════════════════════════════╝");

    // Per-function comparison
    println!("\n  Per-function breakdown:");
    println!(
        "  {:<24} {:>8} {:>8}  {:>8} {:>8}",
        "Function", "Single", "ctx(b)", "Swarm", "ctx(b)"
    );
    println!("  {}", "-".repeat(64));

    // Sort swarm results by name to align
    let mut swarm_map: std::collections::HashMap<String, &FuncResult> =
        swarm.results.iter().map(|r| (r.name.clone(), r)).collect();

    for sr in &single.results {
        let sw = swarm_map.get(&sr.name);
        println!(
            "  {:<24} {:>8} {:>8}  {:>8} {:>8}",
            sr.name,
            if sr.passed { "PASS" } else { "FAIL" },
            sr.context_size,
            sw.map(|r| if r.passed { "PASS" } else { "FAIL" })
                .unwrap_or("?"),
            sw.map(|r| r.context_size).unwrap_or(0),
        );
    }

    // Save
    let art = PathBuf::from(ARTIFACT_DIR);
    let _ = std::fs::create_dir_all(&art);

    let report = format!(
        "# Context Degradation Benchmark\n\n\
         Date: {}\nModel: {}\n\n\
         ## Hypothesis\n\n\
         Single agent accumulating conversation history degrades on later functions.\n\
         Swarm with fresh context maintains consistent quality.\n\n\
         ## Results\n\n\
         | Metric | Single Agent | Swarm |\n\
         |--------|-------------|-------|\n\
         | **Functions passing** | **{}/12** | **{}/12** |\n\
         | Wall clock | {}ms | {}ms |\n\
         | Speedup | — | {:.2}x |\n\
         | Tokens | {} | {} |\n\
         | Cost | ${:.6} | ${:.6} |\n",
        chrono::Utc::now().format("%Y-%m-%d %H:%M:%S UTC"),
        MODEL,
        single.passed,
        swarm.passed,
        single.wall_ms,
        swarm.wall_ms,
        speedup,
        single.tokens,
        swarm.tokens,
        single.cost,
        swarm.cost,
    );
    std::fs::write(art.join("CONTEXT_DEGRADATION_REPORT.md"), &report).unwrap();

    let json = serde_json::to_string_pretty(&vec![&single, &swarm]).unwrap();
    std::fs::write(art.join("metrics_context.json"), &json).unwrap();

    println!("\nArtifacts saved.");
}
