//! Probe the existing rule-based complexity classifier against the 15 A/B
//! prompts to confirm the gate will actually fire on code tasks and skip
//! chat/channel. Runs in microseconds, no LLM calls.

use temm1e_agent::{ModelRouter, ModelRouterConfig, TaskComplexity};

const PROMPTS: &[(&str, &str, &str)] = &[
    // (class, name, prompt)
    (
        "refactor",
        "rename_helper_in_oath",
        "In the workspace, write a Rust file `oath_demo.rs` containing a public function \
         `pub fn oath_hash_prefix(hash: &str, n: usize) -> String` that returns the first \
         `n` chars of `hash` (or the full string if shorter). Use file_write. Reply 'done'.",
    ),
    (
        "refactor",
        "two_function_module",
        "Write `math_demo.rs` in the workspace with two pub functions: `pub fn add(a: i32, \
         b: i32) -> i32` and `pub fn mul(a: i32, b: i32) -> i32`. Both must be implemented \
         with real bodies (no todo!() / unimplemented!()). Use file_write. Reply 'done'.",
    ),
    (
        "chat_qa",
        "haiku_about_rust",
        "Write a single haiku about the Rust borrow checker. Reply with only the haiku.",
    ),
    (
        "chat_qa",
        "math_question",
        "What is 73 * 84? Show the multiplication step-by-step in two short lines.",
    ),
    (
        "chat_qa",
        "concept_explanation",
        "Explain Rust's `?` operator in one sentence, in plain English, no code samples needed.",
    ),
    (
        "chat_qa",
        "summarize_paragraph",
        "Summarize this paragraph in one sentence: \"The Witness verification system is a \
         pre-committed verification framework where an Oath is sealed before work begins, \
         and a verifier checks postconditions after, producing a tamper-evident verdict.\"",
    ),
    (
        "chat_qa",
        "creative_short",
        "Suggest 3 catchy product names for an AI agent runtime. Just the names, comma-separated.",
    ),
    (
        "tool_sequence",
        "write_then_read_back",
        "In the workspace: 1) use file_write to create `note.txt` with the single line \
         `hello from witness`. 2) use file_read to read `note.txt` back. 3) Reply with the \
         contents you read.",
    ),
    (
        "tool_sequence",
        "write_two_files",
        "Create two files in the workspace via file_write: `a.txt` containing `apple` and \
         `b.txt` containing `banana`. Then use file_list to confirm both exist. Reply with \
         a short summary listing both filenames.",
    ),
    (
        "tool_sequence",
        "manifest_file",
        "Create `manifest.json` in the workspace via file_write. Content must be exactly: \
         {\"name\": \"witness-test\", \"version\": \"1.0.0\"}. Reply 'done' when written.",
    ),
    ("channel_style", "greet_back", "hey"),
    ("channel_style", "ack_short", "ok thanks"),
    (
        "channel_style",
        "yes_no",
        "is rust faster than python at numeric loops? one word answer.",
    ),
    ("channel_style", "what_is_x", "what is a SQLite WAL?"),
    ("channel_style", "tiny_followup", "and how big can it get?"),
];

fn main() {
    println!("Layer-2 observer fire table for 15 A/B prompts\n");
    println!(
        "{:<14} {:<22} {:<12} {:<8} {:<10}",
        "class", "task", "complexity", "planner?", "conscious?"
    );
    println!("{}", "─".repeat(72));

    // Both observers now share `turn_is_code_shaped` at runtime.rs — probe
    // mirrors the helper exactly by inlining the same rule here (keeps this
    // example a pure self-contained verification artifact).
    let router = ModelRouter::new(ModelRouterConfig::default());
    let mut planner_fires = 0usize;
    let mut conscious_fires = 0usize;
    for (class, name, prompt) in PROMPTS {
        let complexity = router.classify_complexity(&[], &[], prompt);
        let t = prompt.to_ascii_lowercase();
        let has_code_signal = t.contains("file_")
            || t.contains("workspace")
            || t.contains(".rs")
            || t.contains(".py")
            || t.contains(".ts")
            || t.contains(".js")
            || t.contains(".json")
            || t.contains(".toml")
            || t.contains(".md")
            || t.contains("pub fn")
            || t.contains("fn ")
            || t.contains("class ")
            || t.contains("struct ")
            || t.contains("```");
        let trivial_or_simple =
            matches!(complexity, TaskComplexity::Trivial | TaskComplexity::Simple);
        let is_code_shaped = !trivial_or_simple && has_code_signal;
        if is_code_shaped {
            planner_fires += 1;
            conscious_fires += 1;
        }
        println!(
            "{:<14} {:<22} {:<12} {:<8} {:<10}",
            class,
            name,
            format!("{:?}", complexity),
            if is_code_shaped { "YES" } else { "no" },
            if is_code_shaped { "YES (×2)" } else { "no" }
        );
    }
    println!();
    println!(
        "Planner fires on       {:>2}/{} tasks ({:.0}%)",
        planner_fires,
        PROMPTS.len(),
        (planner_fires as f64 / PROMPTS.len() as f64) * 100.0
    );
    println!(
        "Consciousness fires on {:>2}/{} tasks ({:.0}%) — pre + post = 2× calls per firing",
        conscious_fires,
        PROMPTS.len(),
        (conscious_fires as f64 / PROMPTS.len() as f64) * 100.0
    );
}
