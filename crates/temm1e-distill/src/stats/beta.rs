//! Beta Distribution Utilities.
//!
//! Provides basic statistics and update operations for Beta distributions,
//! commonly used as conjugate priors for Bernoulli/binomial data.

/// Mean of Beta(alpha, beta) = alpha / (alpha + beta).
pub fn beta_mean(alpha: f64, beta: f64) -> f64 {
    alpha / (alpha + beta)
}

/// Variance of Beta(alpha, beta) = alpha * beta / ((alpha + beta)^2 * (alpha + beta + 1)).
pub fn beta_variance(alpha: f64, beta: f64) -> f64 {
    let sum = alpha + beta;
    (alpha * beta) / (sum * sum * (sum + 1.0))
}

/// Update Beta distribution parameters with a weighted observation.
///
/// - If `positive`: alpha += weight
/// - If not `positive`: beta += weight
///
/// Returns the updated (alpha, beta) pair.
pub fn beta_update(alpha: f64, beta: f64, weight: f64, positive: bool) -> (f64, f64) {
    if positive {
        (alpha + weight, beta)
    } else {
        (alpha, beta + weight)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn symmetric_beta_mean() {
        // Beta(2, 2) → mean = 0.5
        assert!((beta_mean(2.0, 2.0) - 0.5).abs() < 1e-12);
    }

    #[test]
    fn skewed_beta_mean() {
        // Beta(10, 2) → mean = 10/12 ≈ 0.8333
        let m = beta_mean(10.0, 2.0);
        assert!((m - 10.0 / 12.0).abs() < 1e-10);
    }

    #[test]
    fn positive_update_increases_mean() {
        let (a, b) = (5.0, 5.0);
        let mean_before = beta_mean(a, b);
        let (a2, b2) = beta_update(a, b, 1.0, true);
        let mean_after = beta_mean(a2, b2);
        assert!(mean_after > mean_before);
    }

    #[test]
    fn negative_update_decreases_mean() {
        let (a, b) = (5.0, 5.0);
        let mean_before = beta_mean(a, b);
        let (a2, b2) = beta_update(a, b, 1.0, false);
        let mean_after = beta_mean(a2, b2);
        assert!(mean_after < mean_before);
    }
}
