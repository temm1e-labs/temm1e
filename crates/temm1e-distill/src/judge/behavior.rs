//! Behavior Judge — user behavior signals for SPRT/CUSUM.
//! Default shadow/monitor method. Zero LLM cost.
//!
//! Two-tier detection:
//! - Tier 1 (instant): Levenshtein edit distance + keyword matching
//! - Tier 2 (embedding): Semantic similarity via local Ollama embeddings
//!
//! Tier 2 catches what Tier 1 misses:
//! - Semantic retries: "What's the weather?" → "Tell me the temperature"
//! - Paraphrased rejections: "that doesn't help", "completely useless"
//! - Non-English rejections
//! - Context-dependent meanings ("wrong" in "What went wrong with the economy?" is NOT a rejection)

use crate::collector::{is_likely_retry, is_rejection};
use crate::judge::embedding::cosine_similarity;
use crate::types::QualitySignal;

/// Pre-defined rejection prototype sentences.
/// At runtime, these are embedded once and cached.
/// User messages are compared against these prototypes via cosine similarity.
///
/// Multilingual: embedding models encode MEANING, not words.
/// A Vietnamese "sai rồi" or Japanese "違います" will have high cosine
/// similarity to "That's wrong" because the embedding captures the
/// semantic intent (disagreement/rejection), not the surface language.
///
/// We include multilingual prototypes as anchors to improve coverage
/// across languages — but even English-only prototypes catch non-English
/// rejections because modern embedding models (nomic-embed-text, etc.)
/// are trained on multilingual corpora.
pub const REJECTION_PROTOTYPES: &[&str] = &[
    // English
    "That's wrong, try again",
    "That's not what I asked for",
    "No, that's incorrect",
    "That answer is completely wrong",
    "You misunderstood my question",
    "That doesn't help at all",
    "Can you redo that? It's not right",
    "That's not correct, please fix it",
    "Useless response, do it again",
    "That's the opposite of what I wanted",
    // Vietnamese
    "Sai rồi, làm lại đi",
    "Không đúng, thử lại",
    "Câu trả lời sai",
    // Japanese
    "違います、やり直してください",
    "間違っています",
    // Chinese
    "不对，重新来",
    "回答错了",
    // Korean
    "틀렸어요, 다시 해주세요",
    // Spanish
    "Eso está mal, inténtalo de nuevo",
    "No es lo que pedí",
    // French
    "C'est faux, réessayez",
    // German
    "Das ist falsch, versuchen Sie es nochmal",
    // Portuguese
    "Está errado, tente novamente",
    // Arabic
    "هذا خطأ، حاول مرة أخرى",
    // Thai
    "ผิดแล้ว ลองใหม่",
    // Indonesian
    "Salah, coba lagi",
];

/// Threshold for embedding-based retry detection.
/// Two consecutive user messages with similarity > 0.80 within 60s = retry.
pub const RETRY_EMBEDDING_THRESHOLD: f64 = 0.80;

/// Threshold for embedding-based rejection detection.
/// User message similarity to any rejection prototype > 0.75 = rejection.
pub const REJECTION_EMBEDDING_THRESHOLD: f64 = 0.75;

/// Determine the SPRT observation from user behavior (Tier 1: instant heuristics).
/// Returns (observation, signal_type) where observation is true (agree) or false (disagree).
pub fn behavior_observation(
    current_message: &str,
    previous_message: Option<&str>,
    elapsed_secs: u64,
    tool_failed: bool,
) -> (bool, &'static str) {
    // Priority 1: Tool failure (objective signal, no ambiguity)
    if tool_failed {
        return (false, "tool_failure");
    }

    // Priority 2: Explicit rejection (keyword match — fast path)
    if is_rejection(current_message) {
        return (false, "explicit_rejection");
    }

    // Priority 3: Retry/rephrase (edit distance — fast path)
    if let Some(prev) = previous_message {
        if is_likely_retry(current_message, prev, elapsed_secs) {
            return (false, "retry_rephrase");
        }
    }

    // Default: user continued normally (implicit agreement)
    (true, "continued_normally")
}

/// Determine the SPRT observation using embeddings (Tier 2: semantic).
/// Called when Tier 1 returns "continued_normally" — checks if the semantic
/// signal tells a different story.
///
/// `current_embedding` — embedding of the current user message
/// `previous_embedding` — embedding of the previous user message (if available)
/// `rejection_prototypes` — pre-computed embeddings of rejection prototype sentences
/// `elapsed_secs` — seconds since the previous message
///
/// Returns None if Tier 2 agrees with Tier 1 (no override).
/// Returns Some((false, reason)) if Tier 2 detects a retry or rejection.
pub fn behavior_observation_embedding(
    current_embedding: &[f64],
    previous_embedding: Option<&[f64]>,
    rejection_prototypes: &[Vec<f64>],
    elapsed_secs: u64,
) -> Option<(bool, &'static str)> {
    // Check semantic retry: high similarity to previous message within time window
    if elapsed_secs <= 60 {
        if let Some(prev) = previous_embedding {
            let sim = cosine_similarity(current_embedding, prev);
            if sim > RETRY_EMBEDDING_THRESHOLD {
                return Some((false, "semantic_retry"));
            }
        }
    }

    // Check semantic rejection: high similarity to any rejection prototype
    let max_rejection_sim = rejection_prototypes
        .iter()
        .map(|proto| cosine_similarity(current_embedding, proto))
        .fold(f64::NEG_INFINITY, f64::max);

    if max_rejection_sim > REJECTION_EMBEDDING_THRESHOLD {
        return Some((false, "semantic_rejection"));
    }

    // Tier 2 agrees with Tier 1 — no override
    None
}

/// Combined two-tier behavior observation.
///
/// Tier 1 runs first (instant). If Tier 1 already detected a problem, return immediately.
/// If Tier 1 says "continued_normally", Tier 2 checks semantic signals.
///
/// This is the primary entry point for the behavior judge.
pub fn behavior_observation_tiered(
    current_message: &str,
    previous_message: Option<&str>,
    elapsed_secs: u64,
    tool_failed: bool,
    current_embedding: Option<&[f64]>,
    previous_embedding: Option<&[f64]>,
    rejection_prototypes: &[Vec<f64>],
) -> (bool, &'static str) {
    // Tier 1: instant heuristics
    let (agree, signal) =
        behavior_observation(current_message, previous_message, elapsed_secs, tool_failed);

    // If Tier 1 detected a problem, trust it
    if !agree {
        return (agree, signal);
    }

    // Tier 2: semantic analysis (only if embeddings available)
    if let Some(curr_emb) = current_embedding {
        if let Some((agree_t2, signal_t2)) = behavior_observation_embedding(
            curr_emb,
            previous_embedding,
            rejection_prototypes,
            elapsed_secs,
        ) {
            return (agree_t2, signal_t2);
        }
    }

    // Both tiers agree: user continued normally
    (true, "continued_normally")
}

/// Map QualitySignal to SPRT observation.
pub fn signal_to_observation(signal: QualitySignal) -> bool {
    signal.is_positive()
}

#[cfg(test)]
mod tests {
    use super::*;

    // ─── Tier 1 tests ───

    #[test]
    fn test_tool_failure_disagrees() {
        let (agree, signal) = behavior_observation("ok", None, 0, true);
        assert!(!agree);
        assert_eq!(signal, "tool_failure");
    }

    #[test]
    fn test_rejection_disagrees() {
        let (agree, signal) = behavior_observation("That's wrong", None, 0, false);
        assert!(!agree);
        assert_eq!(signal, "explicit_rejection");
    }

    #[test]
    fn test_retry_disagrees() {
        let (agree, signal) = behavior_observation(
            "What is the weather today",
            Some("What is the weather"),
            30,
            false,
        );
        assert!(!agree);
        assert_eq!(signal, "retry_rephrase");
    }

    #[test]
    fn test_normal_continuation_agrees() {
        let (agree, signal) = behavior_observation(
            "Thanks, now tell me about Rust",
            Some("What is Python?"),
            45,
            false,
        );
        assert!(agree);
        assert_eq!(signal, "continued_normally");
    }

    // ─── Tier 2 tests (embedding-based) ───

    #[test]
    fn test_semantic_retry_detected() {
        // Two similar embeddings → semantic retry
        let current = vec![0.9, 0.1, 0.0, 0.0];
        let previous = vec![0.85, 0.15, 0.0, 0.0]; // very similar direction
        let sim = cosine_similarity(&current, &previous);
        assert!(sim > RETRY_EMBEDDING_THRESHOLD);

        let result = behavior_observation_embedding(&current, Some(&previous), &[], 30);
        assert_eq!(result, Some((false, "semantic_retry")));
    }

    #[test]
    fn test_semantic_retry_not_triggered_after_timeout() {
        let current = vec![0.9, 0.1, 0.0, 0.0];
        let previous = vec![0.85, 0.15, 0.0, 0.0];

        // 120 seconds elapsed — beyond the 60s window
        let result = behavior_observation_embedding(&current, Some(&previous), &[], 120);
        assert_eq!(result, None); // No retry detected — outside time window
    }

    #[test]
    fn test_semantic_rejection_detected() {
        // User message embedding very similar to a rejection prototype
        let current = vec![0.8, 0.6, 0.0, 0.0];
        let rejection_proto = vec![0.81, 0.59, 0.0, 0.0]; // nearly identical direction
        let sim = cosine_similarity(&current, &rejection_proto);
        assert!(sim > REJECTION_EMBEDDING_THRESHOLD);

        let result = behavior_observation_embedding(&current, None, &[rejection_proto], 0);
        assert_eq!(result, Some((false, "semantic_rejection")));
    }

    #[test]
    fn test_semantic_no_rejection_for_dissimilar() {
        // User message NOT similar to rejection prototypes
        let current = vec![1.0, 0.0, 0.0, 0.0];
        let rejection_proto = vec![0.0, 1.0, 0.0, 0.0]; // orthogonal
        let result = behavior_observation_embedding(&current, None, &[rejection_proto], 0);
        assert_eq!(result, None); // No rejection
    }

    // ─── Tiered tests ───

    #[test]
    fn test_tiered_tier1_takes_priority() {
        // Tool failure is detected at Tier 1, no need for Tier 2
        let (agree, signal) = behavior_observation_tiered(
            "ok",
            None,
            0,
            true, // tool failed
            None,
            None,
            &[],
        );
        assert!(!agree);
        assert_eq!(signal, "tool_failure");
    }

    #[test]
    fn test_tiered_tier2_catches_semantic_retry() {
        // Tier 1 says "continued_normally" (different words),
        // but Tier 2 detects semantic similarity
        let current_emb = vec![0.9, 0.1, 0.0, 0.0];
        let previous_emb = vec![0.85, 0.15, 0.0, 0.0];

        let (agree, signal) = behavior_observation_tiered(
            "Tell me the temperature outside",      // different words
            Some("What's the weather like today?"), // different words
            30,
            false,
            Some(&current_emb),
            Some(&previous_emb),
            &[],
        );
        assert!(!agree);
        assert_eq!(signal, "semantic_retry");
    }

    #[test]
    fn test_tiered_tier2_catches_semantic_rejection() {
        let current_emb = vec![0.8, 0.6, 0.0, 0.0];
        let rejection_proto = vec![0.81, 0.59, 0.0, 0.0];

        let (agree, signal) = behavior_observation_tiered(
            "That response was completely useless", // no keyword match
            None,
            0,
            false,
            Some(&current_emb),
            None,
            &[rejection_proto],
        );
        assert!(!agree);
        assert_eq!(signal, "semantic_rejection");
    }

    #[test]
    fn test_tiered_both_agree_normal() {
        let current_emb = vec![1.0, 0.0, 0.0, 0.0];
        let previous_emb = vec![0.0, 1.0, 0.0, 0.0]; // orthogonal = different topic

        let (agree, signal) = behavior_observation_tiered(
            "Now let's talk about something else",
            Some("What is the weather?"),
            45,
            false,
            Some(&current_emb),
            Some(&previous_emb),
            &[],
        );
        assert!(agree);
        assert_eq!(signal, "continued_normally");
    }

    // ─── Signal mapping ───

    #[test]
    fn test_signal_to_observation_positive() {
        assert!(signal_to_observation(QualitySignal::UserContinued));
        assert!(signal_to_observation(QualitySignal::ToolCallSucceeded));
    }

    #[test]
    fn test_signal_to_observation_negative() {
        assert!(!signal_to_observation(QualitySignal::UserRetried));
        assert!(!signal_to_observation(QualitySignal::ResponseError));
    }

    // ─── Rejection prototypes ───

    #[test]
    fn test_rejection_prototypes_not_empty() {
        assert!(!REJECTION_PROTOTYPES.is_empty());
        assert!(REJECTION_PROTOTYPES.len() >= 8);
    }
}
