//! Gemini max_tokens hang-reproducer.
//!
//! Probes the native `GeminiProvider` directly (no agent loop, no streaming
//! state, no blueprint authoring) with combinations of `max_tokens` and
//! prompt shape that match what triggered the 75-min hang in the release
//! integration test. Wraps every call in `tokio::time::timeout(60s)` so
//! hangs surface as `TIMEOUT` rather than blocking the probe forever.
//!
//! Hypothesis: `max_tokens: None` on Gemini 3 Flash Preview causes a
//! streaming stall on longer outputs. If the hypothesis is correct, the
//! None + long-prompt variants should timeout while Some(N) ones complete.
//!
//! Run:
//!   cargo run --release -p temm1e-providers --example gemini_max_tokens_probe

use std::sync::Arc;
use std::time::{Duration, Instant};

use temm1e_core::config::credentials::load_credentials_file;
use temm1e_core::traits::Provider;
use temm1e_core::types::message::{ChatMessage, CompletionRequest, MessageContent, Role};

const SHORT_PROMPT: &str = "Reply with exactly one word: pong";
const MEDIUM_PROMPT: &str = "Write a 3-line haiku about the Rust borrow checker. Reply only with the haiku.";
const LONG_PROMPT: &str = "Explain Rust's ownership model in detail with 5 code examples. Include the borrow checker, lifetimes, and move semantics. Write at least 400 words.";
const JSON_PROMPT: &str = "Emit a JSON object with this exact shape (no preamble, no explanation): \
    {\"goal\": \"refactor hello.py\", \"postconditions\": [\
    {\"type\": \"FileExists\", \"path\": \"hello.py\"},\
    {\"type\": \"GrepPresent\", \"pattern\": \"def greet\", \"path\": \"hello.py\"},\
    {\"type\": \"GrepPresent\", \"pattern\": \"type: str\", \"path\": \"hello.py\"},\
    {\"type\": \"GrepAbsent\", \"pattern\": \"todo!\", \"path\": \"hello.py\"},\
    {\"type\": \"FileExists\", \"path\": \"test_hello.py\"},\
    {\"type\": \"GrepPresent\", \"pattern\": \"greet\", \"path\": \"test_hello.py\"}]}";

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let creds = load_credentials_file().ok_or("no credentials.toml")?;
    let gemini_cfg = creds
        .providers
        .iter()
        .find(|p| p.name == "gemini")
        .ok_or("no gemini provider in credentials")?;
    let api_key = gemini_cfg.keys.first().cloned().ok_or("gemini has no key")?;

    let provider: Arc<dyn Provider> = Arc::new(temm1e_providers::GeminiProvider::new(api_key));
    let model = gemini_cfg.model.clone();

    println!("════════════════════════════════════════════════════════════════");
    println!("  Gemini max_tokens probe — model: {}", model);
    println!("════════════════════════════════════════════════════════════════\n");

    let cases: &[(&str, &str, Option<u32>)] = &[
        ("A1", SHORT_PROMPT, None),
        ("A2", SHORT_PROMPT, Some(2048)),
        ("B1", MEDIUM_PROMPT, None),
        ("B2", MEDIUM_PROMPT, Some(2048)),
        ("C1", LONG_PROMPT, None),
        ("C2", LONG_PROMPT, Some(2048)),
        ("D1", JSON_PROMPT, None),
        ("D2", JSON_PROMPT, Some(2048)),
        ("D3", JSON_PROMPT, Some(8192)),
    ];

    println!(
        "{:<4} {:<8} {:>12}  {:>8}  {:>12}  verdict",
        "id", "max_tok", "elapsed_ms", "in_tok", "out_tok"
    );
    println!("{:─<80}", "");

    for (id, prompt, max_tokens) in cases {
        let req = CompletionRequest {
            model: model.clone(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text(prompt.to_string()),
            }],
            tools: vec![],
            max_tokens: *max_tokens,
            temperature: Some(0.0),
            system: None,
            system_volatile: None,
        };
        let cap_str = match max_tokens {
            Some(n) => format!("Some({n})"),
            None => "None".to_string(),
        };
        let started = Instant::now();
        let result = tokio::time::timeout(Duration::from_secs(60), provider.complete(req)).await;
        let elapsed = started.elapsed().as_millis();
        match result {
            Ok(Ok(resp)) => {
                let out_tok = resp.usage.output_tokens;
                let in_tok = resp.usage.input_tokens;
                println!(
                    "{:<4} {:<8} {:>12}  {:>8}  {:>12}  OK",
                    id, cap_str, elapsed, in_tok, out_tok
                );
            }
            Ok(Err(e)) => {
                println!(
                    "{:<4} {:<8} {:>12}  {:>8}  {:>12}  ERROR: {}",
                    id, cap_str, elapsed, "-", "-", e
                );
            }
            Err(_) => {
                println!(
                    "{:<4} {:<8} {:>12}  {:>8}  {:>12}  TIMEOUT@60s",
                    id, cap_str, elapsed, "-", "-"
                );
            }
        }
    }

    Ok(())
}
