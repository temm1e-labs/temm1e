//! λ-Memory — continuous decay with hash-based recall.
//!
//! Memories fade over time through exponential decay but never disappear.
//! Tem sees faded memories as hashes and can recall them on demand.
//!
//! **Key invariant:** `bone + active + output_reserve + guard + λ_tokens ≤ skull`
//!
//! See `tems_lab/LAMBDA_MEMORY.md` for full design.

use std::collections::HashMap;

use temm1e_core::types::config::LambdaMemoryConfig;
use temm1e_core::{LambdaMemoryEntry, Memory};
use tracing::{debug, warn};

use crate::context::estimate_tokens;

/// Minimum tokens to fit a single faded entry (hash + timestamp + essence).
const MIN_ENTRY_TOKENS: usize = 15;

/// Score below which a memory is completely invisible.
const GONE_THRESHOLD: f32 = 0.01;

// ── Decay Function ─────────────────────────────────────────────

/// Compute the decay score for a memory at time `now`.
///
/// `score = importance × exp(−λ × hours_since_last_access)`
///
/// This is NEVER stored — computed at read time from immutable fields.
pub fn decay_score(entry: &LambdaMemoryEntry, now: u64, lambda: f32) -> f32 {
    let age_hours = (now.saturating_sub(entry.last_accessed)) as f32 / 3600.0;
    entry.importance * (-age_hours * lambda).exp()
}

// ── Adaptive Thresholds ────────────────────────────────────────

/// Fidelity tier thresholds, adjusted by memory pressure.
pub struct Thresholds {
    pub hot: f32,
    pub warm: f32,
    pub cool: f32,
}

/// Compute effective thresholds based on memory pressure.
///
/// As `budget` shrinks relative to `max_budget`, thresholds rise,
/// causing more memories to display at lower fidelity tiers.
pub fn effective_thresholds(
    budget: usize,
    max_budget: usize,
    config: &LambdaMemoryConfig,
) -> Thresholds {
    let pressure = 1.0 - (budget as f32 / max_budget.max(1) as f32).min(1.0);
    Thresholds {
        hot: config.hot_threshold + (pressure * 2.0),
        warm: config.warm_threshold + (pressure * 1.0),
        cool: config.cool_threshold + (pressure * 0.5),
    }
}

// ── Skull Budget ───────────────────────────────────────────────

/// Calculate the token budget available for λ-Memory.
///
/// Memory is elastic — it gets what's left after everything with higher
/// priority (bone, active conversation, output reserve, guard).
pub fn lambda_budget(
    skull: usize,
    max_output: usize,
    bone_tokens: usize,
    active_tokens: usize,
) -> usize {
    let output_reserve = max_output.min(skull / 10);
    let guard = skull / 50; // 2% safety margin
    let occupied = bone_tokens + active_tokens + output_reserve + guard;
    skull.saturating_sub(occupied)
}

// ── Formatting ─────────────────────────────────────────────────

fn format_hot(entry: &LambdaMemoryEntry) -> String {
    let accessed = if entry.access_count > 0 {
        format!(" | accessed: {}x", entry.access_count)
    } else {
        String::new()
    };
    let explicit = if entry.explicit_save {
        " | explicit save"
    } else {
        ""
    };
    format!(
        "[hot] {}\n      (#{} | {} | importance: {:.1}{}{})\n\n",
        entry.full_text,
        &entry.hash[..7.min(entry.hash.len())],
        format_timestamp(entry.created_at),
        entry.importance,
        accessed,
        explicit,
    )
}

fn format_warm(entry: &LambdaMemoryEntry) -> String {
    format!(
        "[warm] {}\n       (#{} | {})\n\n",
        entry.summary_text,
        &entry.hash[..7.min(entry.hash.len())],
        format_timestamp(entry.created_at),
    )
}

fn format_cool(entry: &LambdaMemoryEntry) -> String {
    format!(
        "[cool] {} (#{} | {})\n",
        entry.essence_text,
        &entry.hash[..7.min(entry.hash.len())],
        format_timestamp(entry.created_at),
    )
}

fn format_faded(entry: &LambdaMemoryEntry) -> String {
    format!(
        "[faded] #{} | {} | {}\n",
        &entry.hash[..7.min(entry.hash.len())],
        format_timestamp(entry.created_at),
        entry.essence_text,
    )
}

fn format_timestamp(epoch: u64) -> String {
    chrono::DateTime::from_timestamp(epoch as i64, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

// ── Best Representation ────────────────────────────────────────

/// Choose the best representation that fits within `remaining` tokens.
/// Falls through from highest fidelity to lowest.
fn best_representation(
    entry: &LambdaMemoryEntry,
    remaining: usize,
    score: f32,
    thresholds: &Thresholds,
) -> (String, usize) {
    if score > thresholds.hot {
        let text = format_hot(entry);
        let cost = estimate_tokens(&text);
        if cost <= remaining {
            return (text, cost);
        }
    }
    if score > thresholds.warm {
        let text = format_warm(entry);
        let cost = estimate_tokens(&text);
        if cost <= remaining {
            return (text, cost);
        }
    }
    if score > thresholds.cool {
        let text = format_cool(entry);
        let cost = estimate_tokens(&text);
        if cost <= remaining {
            return (text, cost);
        }
    }
    if score > GONE_THRESHOLD {
        let text = format_faded(entry);
        let cost = estimate_tokens(&text);
        if cost <= remaining {
            return (text, cost);
        }
    }
    (String::new(), 0)
}

// ── Context Assembly ───────────────────────────────────────────

/// Assemble the λ-Memory section for injection into the context window.
///
/// Returns the formatted string and its estimated token count.
pub async fn assemble_lambda_context(
    memory: &dyn Memory,
    budget: usize,
    max_budget: usize,
    config: &LambdaMemoryConfig,
    current_query: &str,
) -> (String, usize) {
    if budget < MIN_ENTRY_TOKENS || !config.enabled {
        return (String::new(), 0);
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let thresholds = effective_thresholds(budget, max_budget, config);

    // Step 1: Query candidates
    let candidates = match memory.lambda_query_candidates(config.candidate_limit).await {
        Ok(c) => c,
        Err(e) => {
            warn!(error = %e, "λ-Memory candidate query failed");
            return (String::new(), 0);
        }
    };

    if candidates.is_empty() {
        return (String::new(), 0);
    }

    // Step 2: Compute decay scores
    let mut scored: Vec<(f32, &LambdaMemoryEntry)> = candidates
        .iter()
        .map(|m| (decay_score(m, now, config.decay_lambda), m))
        .collect();

    // Step 3: Boost scores for FTS-relevant memories
    if !current_query.is_empty() {
        if let Ok(fts_results) = memory.lambda_fts_search(current_query, 20).await {
            let fts_map: HashMap<&str, f64> = fts_results
                .iter()
                .map(|(hash, rank)| (hash.as_str(), *rank))
                .collect();

            for (score, entry) in &mut scored {
                if let Some(&rank) = fts_map.get(entry.hash.as_str()) {
                    // BM25 rank is negative (lower = better match in FTS5).
                    // Convert to a positive boost: max 2.0 for best matches.
                    let relevance_boost = (1.0 + (-rank as f32).ln().max(0.0)).min(2.0);
                    *score += relevance_boost * 0.4; // 40% weight on relevance
                }
            }
        }
    }

    // Step 4: Sort by final score descending
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Step 5: Pack into budget
    let header = "═══ λ-Memory ═══\n\n";
    let footer = "\n═══════════════\n";
    let header_cost = estimate_tokens(header);
    let footer_cost = estimate_tokens(footer);
    let mut remaining = budget.saturating_sub(header_cost + footer_cost);

    let mut output = String::from(header);
    let mut packed_count = 0usize;

    // 5a: Explicit saves first (always included at minimum fidelity)
    for (score, entry) in scored.iter().filter(|(_, e)| e.explicit_save) {
        if remaining < MIN_ENTRY_TOKENS {
            break;
        }
        let (text, cost) = best_representation(entry, remaining, *score, &thresholds);
        if cost == 0 {
            continue;
        }
        output.push_str(&text);
        remaining -= cost;
        packed_count += 1;
    }

    // 5b: Remaining by score
    for (score, entry) in &scored {
        if entry.explicit_save {
            continue;
        }
        if remaining < MIN_ENTRY_TOKENS {
            break;
        }
        let (text, cost) = best_representation(entry, remaining, *score, &thresholds);
        if cost == 0 {
            continue;
        }
        output.push_str(&text);
        remaining -= cost;
        packed_count += 1;
    }

    if packed_count == 0 {
        return (String::new(), 0);
    }

    output.push_str(footer);
    let total_cost = estimate_tokens(&output);

    debug!(
        budget = budget,
        packed = packed_count,
        tokens = total_cost,
        candidates = candidates.len(),
        "λ-Memory context assembled"
    );

    (output, total_cost)
}

// ── Memory Creation Helpers ────────────────────────────────────

/// Gate: is this turn worth remembering?
///
/// Returns true if the user's message contains decision language,
/// explicit "remember" requests, emotional markers, or substantive tool work.
pub fn worth_remembering(user_text: &str, has_tool_calls: bool) -> bool {
    let text_lower = user_text.to_lowercase();

    // Explicit request
    if (text_lower.contains("remember") && text_lower.contains("this"))
        || text_lower.contains("remember:")
        || text_lower.contains("don't forget")
    {
        return true;
    }

    // Decision language
    let decision_words = [
        "decide", "chose", "choose", "switch", "change", "use", "prefer", "always", "never",
        "refactor", "rewrite", "deploy", "ship", "merge", "approve", "reject",
    ];
    if decision_words.iter().any(|w| text_lower.contains(w)) {
        return true;
    }

    // Has tool calls and substantive text
    if has_tool_calls && user_text.len() > 80 {
        return true;
    }

    // Emotional markers
    let emotional = [
        "frustrated",
        "love",
        "hate",
        "amazing",
        "terrible",
        "important",
        "critical",
        "urgent",
        "excited",
        "worried",
    ];
    if emotional.iter().any(|w| text_lower.contains(w)) {
        return true;
    }

    false
}

/// Parsed result from a `<memory>` block in the LLM response.
pub struct ParsedMemoryBlock {
    pub summary: String,
    pub essence: String,
    pub importance: f32,
    pub tags: Vec<String>,
}

/// Parse a `<memory>` block from the LLM response text.
///
/// Returns None if no block found or parsing fails.
pub fn parse_memory_block(response_text: &str) -> Option<ParsedMemoryBlock> {
    let start = response_text.find("<memory>")?;
    let end = response_text.find("</memory>")?;
    if end <= start {
        return None;
    }

    let block = &response_text[start + 8..end];
    let mut summary = String::new();
    let mut essence = String::new();
    let mut importance: f32 = 2.0;
    let mut tags: Vec<String> = Vec::new();

    for line in block.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("summary:") {
            summary = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("essence:") {
            essence = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("importance:") {
            importance = val.trim().parse::<f32>().unwrap_or(2.0).clamp(1.0, 5.0);
        } else if let Some(val) = line.strip_prefix("tags:") {
            tags = val
                .split(',')
                .map(|t| t.trim().to_string())
                .filter(|t| !t.is_empty())
                .collect();
        }
    }

    if summary.is_empty() && essence.is_empty() {
        return None;
    }

    Some(ParsedMemoryBlock {
        summary,
        essence,
        importance,
        tags,
    })
}

/// Strip `<memory>...</memory>` blocks from response text before sending to user.
pub fn strip_memory_blocks(text: &str) -> String {
    let mut result = text.to_string();
    while let Some(start) = result.find("<memory>") {
        if let Some(end) = result[start..].find("</memory>") {
            result.replace_range(start..start + end + 9, "");
        } else {
            break;
        }
    }
    result.trim().to_string()
}

/// Generate a SHA-256 based hash for a λ-memory entry (first 12 hex chars).
pub fn make_hash(session_id: &str, round: usize, now: u64) -> String {
    use sha2::{Digest, Sha256};
    let input = format!("{session_id}:{round}:{now}");
    let hash = Sha256::digest(input.as_bytes());
    hex::encode(&hash[..6]) // 6 bytes = 12 hex chars
}

// ── Tests ──────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use temm1e_core::LambdaMemoryType;

    fn test_entry(importance: f32, created_at: u64, last_accessed: u64) -> LambdaMemoryEntry {
        LambdaMemoryEntry {
            hash: "test1234abcd".to_string(),
            created_at,
            last_accessed,
            access_count: 0,
            importance,
            explicit_save: false,
            full_text: "test full content".to_string(),
            summary_text: "test summary".to_string(),
            essence_text: "test".to_string(),
            tags: vec!["test".to_string()],
            memory_type: LambdaMemoryType::Conversation,
            session_id: "test-session".to_string(),
        }
    }

    #[test]
    fn decay_score_at_creation() {
        let entry = test_entry(3.0, 1000, 1000);
        let score = decay_score(&entry, 1000, 0.01);
        assert!((score - 3.0).abs() < 0.001);
    }

    #[test]
    fn decay_score_after_24h() {
        let entry = test_entry(3.0, 0, 0);
        let now = 86400; // 24 hours
        let score = decay_score(&entry, now, 0.01);
        // 3.0 * exp(-24 * 0.01) = 3.0 * 0.7866 = 2.36
        assert!((score - 2.36).abs() < 0.02);
    }

    #[test]
    fn decay_score_after_7_days() {
        let entry = test_entry(3.0, 0, 0);
        let now = 7 * 86400;
        let score = decay_score(&entry, now, 0.01);
        // 3.0 * exp(-168 * 0.01) = 3.0 * 0.1864 = 0.559
        assert!((score - 0.559).abs() < 0.02);
    }

    #[test]
    fn high_importance_decays_slower() {
        let low = test_entry(1.0, 0, 0);
        let high = test_entry(5.0, 0, 0);
        let now = 3 * 86400;
        assert!(decay_score(&high, now, 0.01) > decay_score(&low, now, 0.01));
    }

    #[test]
    fn recall_reheats() {
        let old = test_entry(3.0, 0, 0);
        let mut recalled = old.clone();
        let now = 7 * 86400;
        recalled.last_accessed = now; // just recalled
        assert!(decay_score(&recalled, now, 0.01) > decay_score(&old, now, 0.01) * 5.0);
    }

    #[test]
    fn parse_memory_block_valid() {
        let text = "Some response\n<memory>\nsummary: did a thing\nessence: thing done\nimportance: 3\ntags: foo, bar\n</memory>";
        let parsed = parse_memory_block(text).unwrap();
        assert_eq!(parsed.summary, "did a thing");
        assert_eq!(parsed.essence, "thing done");
        assert!((parsed.importance - 3.0).abs() < 0.01);
        assert_eq!(parsed.tags, vec!["foo", "bar"]);
    }

    #[test]
    fn parse_memory_block_missing() {
        assert!(parse_memory_block("no block here").is_none());
    }

    #[test]
    fn parse_memory_block_empty_content() {
        let text = "<memory>\nsummary:\nessence:\n</memory>";
        assert!(parse_memory_block(text).is_none());
    }

    #[test]
    fn parse_memory_block_clamps_importance() {
        let text = "<memory>\nsummary: test\nimportance: 99\n</memory>";
        let parsed = parse_memory_block(text).unwrap();
        assert!((parsed.importance - 5.0).abs() < 0.01);
    }

    #[test]
    fn strip_memory_blocks_clean() {
        let text = "Hello world\n<memory>\nsummary: test\n</memory>\nGoodbye";
        let result = strip_memory_blocks(text);
        assert!(result.contains("Hello world"));
        assert!(result.contains("Goodbye"));
        assert!(!result.contains("<memory>"));
    }

    #[test]
    fn strip_memory_blocks_no_block() {
        assert_eq!(strip_memory_blocks("just text"), "just text");
    }

    #[test]
    fn worth_remembering_explicit() {
        assert!(worth_remembering(
            "remember this: use tabs not spaces",
            false
        ));
    }

    #[test]
    fn worth_remembering_decision() {
        assert!(worth_remembering("let's deploy to staging", false));
    }

    #[test]
    fn worth_remembering_trivial() {
        assert!(!worth_remembering("thanks", false));
        assert!(!worth_remembering("ok", false));
        assert!(!worth_remembering("hi", false));
    }

    #[test]
    fn effective_thresholds_no_pressure() {
        let config = LambdaMemoryConfig::default();
        let t = effective_thresholds(10000, 10000, &config);
        assert!((t.hot - 2.0).abs() < 0.01);
        assert!((t.warm - 1.0).abs() < 0.01);
    }

    #[test]
    fn effective_thresholds_full_pressure() {
        let config = LambdaMemoryConfig::default();
        let t = effective_thresholds(0, 10000, &config);
        assert!((t.hot - 4.0).abs() < 0.01);
        assert!((t.warm - 2.0).abs() < 0.01);
    }

    #[test]
    fn make_hash_deterministic() {
        let h1 = make_hash("sess1", 1, 1000);
        let h2 = make_hash("sess1", 1, 1000);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 12);
    }

    #[test]
    fn make_hash_unique() {
        let h1 = make_hash("sess1", 1, 1000);
        let h2 = make_hash("sess1", 2, 1000);
        assert_ne!(h1, h2);
    }
}
