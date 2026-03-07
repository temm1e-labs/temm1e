//! Hybrid search helper for memory entries.
//!
//! For v0.1 this implements keyword-only scoring with a simple TF-IDF-like
//! relevance model. Vector similarity can be layered in later.

use skyclaw_core::MemoryEntry;
use std::collections::HashMap;

/// Compute keyword relevance scores and return entries sorted by descending score.
///
/// `vector_weight` is accepted for API compatibility but currently unused (v0.1
/// is keyword-only). `keyword_weight` scales the keyword score.
pub fn hybrid_search(
    query: &str,
    entries: &[MemoryEntry],
    _vector_weight: f32,
    keyword_weight: f32,
) -> Vec<MemoryEntry> {
    let query_terms = tokenize(query);
    if query_terms.is_empty() {
        return entries.to_vec();
    }

    // Build document-frequency map across the whole corpus.
    let num_docs = entries.len() as f32;
    let mut doc_freq: HashMap<String, usize> = HashMap::new();
    let entry_tokens: Vec<Vec<String>> = entries.iter().map(|e| tokenize(&e.content)).collect();

    for tokens in &entry_tokens {
        let unique: std::collections::HashSet<&String> = tokens.iter().collect();
        for t in unique {
            *doc_freq.entry(t.clone()).or_insert(0) += 1;
        }
    }

    // Score each entry using a TF-IDF-like metric.
    let mut scored: Vec<(f32, &MemoryEntry)> = entries
        .iter()
        .zip(entry_tokens.iter())
        .map(|(entry, tokens)| {
            let tf_map = term_frequencies(tokens);
            let score: f32 = query_terms
                .iter()
                .map(|qt| {
                    let tf = tf_map.get(qt.as_str()).copied().unwrap_or(0.0);
                    let df = doc_freq.get(qt).copied().unwrap_or(0) as f32;
                    let idf = if df > 0.0 {
                        ((num_docs + 1.0) / (df + 1.0)).ln() + 1.0
                    } else {
                        1.0
                    };
                    tf * idf
                })
                .sum();
            (score * keyword_weight, entry)
        })
        .collect();

    // Sort descending by score; only return entries with positive score.
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    scored
        .into_iter()
        .filter(|(score, _)| *score > 0.0)
        .map(|(_, entry)| entry.clone())
        .collect()
}

/// Tokenize a string into lower-case alphanumeric words.
fn tokenize(text: &str) -> Vec<String> {
    text.to_lowercase()
        .split(|c: char| !c.is_alphanumeric())
        .filter(|s| !s.is_empty() && s.len() > 1)
        .map(String::from)
        .collect()
}

/// Build a term-frequency map (normalised by document length).
fn term_frequencies(tokens: &[String]) -> HashMap<&str, f32> {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for t in tokens {
        *counts.entry(t.as_str()).or_insert(0) += 1;
    }
    let len = tokens.len().max(1) as f32;
    counts
        .into_iter()
        .map(|(k, v)| (k, v as f32 / len))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use skyclaw_core::MemoryEntryType;

    fn make_entry(id: &str, content: &str) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            content: content.to_string(),
            metadata: serde_json::json!({}),
            timestamp: Utc::now(),
            session_id: None,
            entry_type: MemoryEntryType::Conversation,
        }
    }

    #[test]
    fn test_search_ranks_relevant_first() {
        let entries = vec![
            make_entry("1", "The quick brown fox jumps over the lazy dog"),
            make_entry("2", "Rust programming language is fast"),
            make_entry("3", "The fox is quick and clever"),
        ];
        let results = hybrid_search("quick fox", &entries, 0.0, 1.0);
        assert!(!results.is_empty());
        // Both entries mentioning "quick" and "fox" should come first.
        assert!(results[0].id == "1" || results[0].id == "3");
    }

    #[test]
    fn test_empty_query_returns_all() {
        let entries = vec![make_entry("1", "hello"), make_entry("2", "world")];
        let results = hybrid_search("", &entries, 0.0, 1.0);
        assert_eq!(results.len(), 2);
    }
}
