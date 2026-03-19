//! Embedding Judge — cosine similarity via local Ollama embeddings.
//! Default evaluation method. Zero LLM cost.

/// Compute cosine similarity between two embedding vectors.
/// Returns a value between -1.0 and 1.0 (1.0 = identical direction).
pub fn cosine_similarity(a: &[f64], b: &[f64]) -> f64 {
    if a.len() != b.len() || a.is_empty() {
        return 0.0;
    }

    let dot: f64 = a.iter().zip(b).map(|(x, y)| x * y).sum();
    let norm_a: f64 = a.iter().map(|x| x * x).sum::<f64>().sqrt();
    let norm_b: f64 = b.iter().map(|x| x * x).sum::<f64>().sqrt();

    if norm_a == 0.0 || norm_b == 0.0 {
        return 0.0;
    }

    dot / (norm_a * norm_b)
}

/// Check if two responses are semantically equivalent using embedding similarity.
/// Threshold: >= 0.85 is considered equivalent.
pub fn is_equivalent(similarity: f64, threshold: f64) -> bool {
    similarity >= threshold
}

/// Tiered evaluation: try cheap checks before embedding.
/// Returns Some(true/false) if a tier resolved, None if embedding needed.
pub fn cheap_equivalence_check(local_response: &str, cloud_response: &str) -> Option<bool> {
    let local = local_response.trim();
    let cloud = cloud_response.trim();

    // Tier 0: Exact match
    if local == cloud {
        return Some(true);
    }

    // Tier 1: Normalized match (lowercase, collapse whitespace)
    let local_norm = normalize(local);
    let cloud_norm = normalize(cloud);
    if local_norm == cloud_norm {
        return Some(true);
    }

    // Tier 2: Extreme length divergence (10x difference)
    let len_ratio = local.len() as f64 / cloud.len().max(1) as f64;
    if !(0.1..=10.0).contains(&len_ratio) {
        return Some(false);
    }

    // Need embedding comparison
    None
}

fn normalize(s: &str) -> String {
    s.to_lowercase()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cosine_similarity_identical() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-10);
    }

    #[test]
    fn test_cosine_similarity_orthogonal() {
        let a = vec![1.0, 0.0, 0.0];
        let b = vec![0.0, 1.0, 0.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-10);
    }

    #[test]
    fn test_cosine_similarity_opposite() {
        let a = vec![1.0, 0.0];
        let b = vec![-1.0, 0.0];
        assert!((cosine_similarity(&a, &b) - (-1.0)).abs() < 1e-10);
    }

    #[test]
    fn test_cosine_similarity_empty() {
        assert_eq!(cosine_similarity(&[], &[]), 0.0);
    }

    #[test]
    fn test_cosine_similarity_different_lengths() {
        let a = vec![1.0, 0.0];
        let b = vec![1.0, 0.0, 0.0];
        assert_eq!(cosine_similarity(&a, &b), 0.0);
    }

    #[test]
    fn test_is_equivalent_above_threshold() {
        assert!(is_equivalent(0.90, 0.85));
    }

    #[test]
    fn test_is_equivalent_below_threshold() {
        assert!(!is_equivalent(0.80, 0.85));
    }

    #[test]
    fn test_cheap_check_exact_match() {
        assert_eq!(
            cheap_equivalence_check("hello world", "hello world"),
            Some(true)
        );
    }

    #[test]
    fn test_cheap_check_normalized_match() {
        assert_eq!(
            cheap_equivalence_check("Hello  World", "hello world"),
            Some(true)
        );
    }

    #[test]
    fn test_cheap_check_extreme_length() {
        assert_eq!(
            cheap_equivalence_check("hi", &"a".repeat(1000)),
            Some(false)
        );
    }

    #[test]
    fn test_cheap_check_needs_embedding() {
        assert_eq!(
            cheap_equivalence_check(
                "The capital of France is Paris.",
                "Paris is the capital city of France."
            ),
            None
        );
    }
}
