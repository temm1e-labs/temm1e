//! Shannon Entropy (Shannon, 1948).
//!
//! Measures the uncertainty or information content in a discrete
//! probability distribution. Uses natural logarithm (nats).

/// Compute Shannon entropy H = -sum(p_i * ln(p_i)) from raw counts.
///
/// Convention: 0 * ln(0) = 0.
/// Returns 0.0 for empty input or all-zero counts.
pub fn shannon_entropy(counts: &[u64]) -> f64 {
    let total: u64 = counts.iter().sum();
    if total == 0 {
        return 0.0;
    }
    let n = total as f64;
    let mut h = 0.0;
    for &c in counts {
        if c > 0 {
            let p = c as f64 / n;
            h -= p * p.ln();
        }
    }
    h
}

/// Compute normalized entropy J = H / ln(K) where K is the number
/// of non-zero categories.
///
/// Returns 0.0 if there are fewer than 2 non-zero categories (entropy
/// is trivially 0 or undefined for normalization).
pub fn normalized_entropy(counts: &[u64]) -> f64 {
    let k = counts.iter().filter(|&&c| c > 0).count();
    if k < 2 {
        return 0.0;
    }
    let h = shannon_entropy(counts);
    let max_h = (k as f64).ln();
    if max_h == 0.0 {
        return 0.0;
    }
    h / max_h
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn uniform_normalized_is_one() {
        // Four equal categories → maximum entropy.
        let counts = [25, 25, 25, 25];
        let j = normalized_entropy(&counts);
        assert!((j - 1.0).abs() < 1e-10);
    }

    #[test]
    fn single_category_is_zero() {
        // All mass on one category → zero entropy.
        let counts = [100, 0, 0, 0];
        let h = shannon_entropy(&counts);
        assert!((h - 0.0).abs() < 1e-12);
        let j = normalized_entropy(&counts);
        assert!((j - 0.0).abs() < 1e-12);
    }

    #[test]
    fn two_equal_normalized_is_one() {
        // Two equal categories → normalized entropy = 1.0.
        let counts = [50, 50];
        let j = normalized_entropy(&counts);
        assert!((j - 1.0).abs() < 1e-10);
    }

    #[test]
    fn skewed_between_zero_and_one() {
        // Skewed distribution → normalized entropy between 0 and 1.
        let counts = [90, 5, 3, 2];
        let j = normalized_entropy(&counts);
        assert!(j > 0.0);
        assert!(j < 1.0);
    }

    #[test]
    fn empty_is_zero() {
        let h = shannon_entropy(&[]);
        assert!((h - 0.0).abs() < 1e-12);
        let j = normalized_entropy(&[]);
        assert!((j - 0.0).abs() < 1e-12);

        // All-zero counts also return 0.
        let h2 = shannon_entropy(&[0, 0, 0]);
        assert!((h2 - 0.0).abs() < 1e-12);
    }
}
