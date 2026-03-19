//! Tab completion with fuzzy matching.

/// Simple prefix-based completion (nucleo integration deferred).
pub fn fuzzy_match(query: &str, candidates: &[(&str, &str)]) -> Vec<(String, String)> {
    let lower = query.to_lowercase();
    candidates
        .iter()
        .filter(|(name, _)| name.to_lowercase().starts_with(&lower))
        .map(|(name, desc)| (name.to_string(), desc.to_string()))
        .collect()
}
