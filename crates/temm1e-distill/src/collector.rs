//! Eigen-Tune Collector — captures (request, response) pairs from provider calls.
//!
//! The collector is a fire-and-forget hook inserted after Provider.complete() returns.
//! It has zero latency impact on the user's response — all work is spawned as a
//! background tokio task.

use crate::store::EigenTuneStore;
use crate::types::{EigenTier, QualitySignal, TrainingPair};
use chrono::Utc;
use std::sync::Arc;
use tracing;
use uuid::Uuid;

/// Data captured from a single provider call.
#[derive(Debug, Clone)]
pub struct EigenTunePairData {
    pub messages_json: String,
    pub system_prompt: Option<String>,
    pub tools_json: Option<String>,
    pub response_json: String,
    pub model: String,
    pub provider: String,
    pub complexity: String,
    pub conversation_id: String,
    pub turn: i32,
    pub tokens_in: Option<u32>,
    pub tokens_out: Option<u32>,
    pub cost_usd: Option<f64>,
}

pub struct EigenTuneCollector {
    store: Arc<EigenTuneStore>,
    enabled: bool,
}

impl EigenTuneCollector {
    pub fn new(store: Arc<EigenTuneStore>, enabled: bool) -> Self {
        Self { store, enabled }
    }

    /// Called after every Provider.complete() — fire-and-forget.
    /// Returns the pair ID on success.
    pub async fn collect(
        &self,
        data: EigenTunePairData,
    ) -> Result<String, temm1e_core::types::error::Temm1eError> {
        if !self.enabled {
            return Ok(String::new());
        }

        let id = Uuid::new_v4().to_string();
        let tier = EigenTier::from_str(&data.complexity);
        let domain = Self::classify_domain(&data.messages_json, &data.tools_json);

        let pair = TrainingPair {
            id: id.clone(),
            conversation_id: data.conversation_id,
            turn: data.turn,
            created_at: Utc::now(),
            messages_json: data.messages_json,
            system_prompt: data.system_prompt,
            tools_json: data.tools_json,
            response_json: data.response_json,
            source_model: data.model,
            source_provider: data.provider,
            complexity: tier,
            domain_category: Some(domain),
            quality_alpha: 2.0,
            quality_beta: 2.0,
            quality_score: Some(0.5), // Beta(2,2) mean = 0.5
            user_continued: None,
            user_retried: None,
            tool_success: None,
            response_error: None,
            tokens_in: data.tokens_in,
            tokens_out: data.tokens_out,
            cost_usd: data.cost_usd,
            dataset_version: None,
            is_eval_holdout: false,
        };

        self.store.save_pair(&pair).await?;

        tracing::debug!(
            pair_id = %id,
            tier = %tier.as_str(),
            domain = %pair.domain_category.as_deref().unwrap_or("unknown"),
            "Eigen-Tune: collected training pair"
        );

        Ok(id)
    }

    /// Called when a quality signal is observed (user behavior).
    pub async fn observe_signal(
        &self,
        conversation_id: &str,
        signal: QualitySignal,
    ) -> Result<(), temm1e_core::types::error::Temm1eError> {
        if !self.enabled {
            return Ok(());
        }

        let pair = self.store.get_recent_pair(conversation_id).await?;
        let pair = match pair {
            Some(p) => p,
            None => return Ok(()), // No pair to update
        };

        let weight = signal.weight();
        let (new_alpha, new_beta) = if signal.is_positive() {
            (pair.quality_alpha + weight, pair.quality_beta)
        } else {
            (pair.quality_alpha, pair.quality_beta + weight)
        };
        let new_score = new_alpha / (new_alpha + new_beta);

        self.store
            .update_quality(&pair.id, new_alpha, new_beta, new_score)
            .await?;

        // Update the specific signal field
        let field = match signal {
            QualitySignal::UserContinued | QualitySignal::ConversationExtended => "user_continued",
            QualitySignal::ToolCallSucceeded => "tool_success",
            QualitySignal::UserRetried => "user_retried",
            QualitySignal::UserRejected => "user_retried", // same column
            QualitySignal::ResponseError => "response_error",
            QualitySignal::ConversationAbandoned => "user_continued", // false = abandoned
        };
        let value = signal.is_positive();
        self.store.update_signal(&pair.id, field, value).await?;

        tracing::debug!(
            pair_id = %pair.id,
            signal = ?signal,
            new_score = new_score,
            "Eigen-Tune: quality signal observed"
        );

        Ok(())
    }

    /// Classify domain category from message content (heuristic, no LLM).
    pub fn classify_domain(messages_json: &str, tools_json: &Option<String>) -> String {
        let text = messages_json.to_lowercase();

        // Tool-use detection (highest priority — check tools_json)
        if let Some(ref tools) = tools_json {
            if !tools.is_empty()
                && tools != "[]"
                && tools != "null"
                && (text.contains("tool_use") || text.contains("tool_result"))
            {
                return "tool-use".into();
            }
        }

        // Code detection
        if text.contains("```")
            || text.contains("fn ")
            || text.contains("function ")
            || text.contains("class ")
            || text.contains("def ")
            || text.contains("import ")
            || text.contains("async fn")
            || text.contains("pub struct")
        {
            return "coding".into();
        }

        // Reasoning detection
        if text.contains("explain")
            || text.contains("why ")
            || text.contains("how does")
            || text.contains("compare")
            || text.contains("analyze")
            || text.contains("difference between")
        {
            return "reasoning".into();
        }

        // Creative detection
        if text.contains("write a ")
            || text.contains("poem")
            || text.contains("story")
            || text.contains("haiku")
            || text.contains("imagine")
            || text.contains("creative")
        {
            return "creative".into();
        }

        // Factual detection
        if text.contains("what is")
            || text.contains("when did")
            || text.contains("who ")
            || text.contains("where ")
            || text.contains("define ")
            || text.contains("convert ")
        {
            return "factual".into();
        }

        // Analysis detection
        if text.contains("data")
            || text.contains("trend")
            || text.contains("graph")
            || text.contains("statistics")
            || text.contains("report")
            || text.contains("summarize")
        {
            return "analysis".into();
        }

        // Meta detection (about the agent itself)
        if text.contains("/eigen")
            || text.contains("/memory")
            || text.contains("/keys")
            || text.contains("settings")
            || text.contains("your model")
        {
            return "meta".into();
        }

        // Default
        "conversation".into()
    }
}

/// Detect if a user message is a retry/rephrase of the previous message.
pub fn is_likely_retry(current: &str, previous: &str, elapsed_secs: u64) -> bool {
    // Only check within 60-second window
    if elapsed_secs > 60 {
        return false;
    }

    let current = current.trim();
    let previous = previous.trim();

    if current.is_empty() || previous.is_empty() {
        return false;
    }

    // Heuristic 1: Edit distance ratio (simplified — character-level)
    let max_len = current.len().max(previous.len());
    if max_len > 0 {
        let distance = simple_edit_distance(current, previous);
        let ratio = distance as f64 / max_len as f64;
        if ratio < 0.3 {
            return true;
        }
    }

    // Heuristic 2: Shared prefix (5+ words)
    let current_words: Vec<&str> = current.split_whitespace().collect();
    let previous_words: Vec<&str> = previous.split_whitespace().collect();
    let shared = current_words
        .iter()
        .zip(&previous_words)
        .take_while(|(a, b)| a.to_lowercase() == b.to_lowercase())
        .count();
    if shared >= 5 {
        return true;
    }

    false
}

/// Simple Levenshtein-like edit distance (character level).
/// Not optimized — fine for short user messages.
fn simple_edit_distance(a: &str, b: &str) -> usize {
    let a_chars: Vec<char> = a.chars().collect();
    let b_chars: Vec<char> = b.chars().collect();
    let m = a_chars.len();
    let n = b_chars.len();

    if m == 0 {
        return n;
    }
    if n == 0 {
        return m;
    }

    let mut prev = (0..=n).collect::<Vec<_>>();
    let mut curr = vec![0; n + 1];

    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a_chars[i - 1] == b_chars[j - 1] {
                0
            } else {
                1
            };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }

    prev[n]
}

/// Detect explicit rejection in user message.
pub fn is_rejection(message: &str) -> bool {
    const REJECTION_KEYWORDS: &[&str] = &[
        "wrong",
        "no that's",
        "not right",
        "incorrect",
        "try again",
        "that's wrong",
        "not what i asked",
        "not correct",
    ];

    let lower = message.to_lowercase();
    REJECTION_KEYWORDS.iter().any(|kw| lower.contains(kw))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_classify_domain_coding() {
        let msg = r#"[{"role":"user","content":"Write a function in Rust"}]"#;
        assert_eq!(EigenTuneCollector::classify_domain(msg, &None), "coding");
    }

    #[test]
    fn test_classify_domain_reasoning() {
        let msg = r#"[{"role":"user","content":"Explain why the sky is blue"}]"#;
        assert_eq!(EigenTuneCollector::classify_domain(msg, &None), "reasoning");
    }

    #[test]
    fn test_classify_domain_creative() {
        let msg = r#"[{"role":"user","content":"Write a haiku about clouds"}]"#;
        assert_eq!(EigenTuneCollector::classify_domain(msg, &None), "creative");
    }

    #[test]
    fn test_classify_domain_conversation_default() {
        let msg = r#"[{"role":"user","content":"Hello there"}]"#;
        assert_eq!(
            EigenTuneCollector::classify_domain(msg, &None),
            "conversation"
        );
    }

    #[test]
    fn test_classify_domain_tool_use() {
        let msg = r#"[{"role":"user","content":"tool_use something"}]"#;
        let tools = Some(r#"[{"name":"shell"}]"#.to_string());
        assert_eq!(EigenTuneCollector::classify_domain(msg, &tools), "tool-use");
    }

    #[test]
    fn test_is_likely_retry_similar() {
        assert!(is_likely_retry(
            "What is the weather",
            "What is the weather today",
            30
        ));
    }

    #[test]
    fn test_is_likely_retry_different() {
        assert!(!is_likely_retry(
            "What is the weather",
            "Tell me a joke",
            30
        ));
    }

    #[test]
    fn test_is_likely_retry_timeout() {
        assert!(!is_likely_retry("same text", "same text", 120));
    }

    #[test]
    fn test_is_rejection_detected() {
        assert!(is_rejection("That's wrong, try again"));
        assert!(is_rejection("No that's not what I asked"));
        assert!(is_rejection("incorrect answer"));
    }

    #[test]
    fn test_is_rejection_not_detected() {
        assert!(!is_rejection("Thanks, that's helpful"));
        assert!(!is_rejection("Can you tell me more?"));
    }

    #[test]
    fn test_edit_distance() {
        assert_eq!(simple_edit_distance("kitten", "sitting"), 3);
        assert_eq!(simple_edit_distance("", "abc"), 3);
        assert_eq!(simple_edit_distance("same", "same"), 0);
    }
}
