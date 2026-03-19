//! Power Analysis for sample size estimation.
//!
//! Computes the minimum sample size needed to detect a difference
//! between two proportions with specified significance and power.

use crate::stats::wilson::z_value;

/// Minimum sample size to detect a difference between proportions p0 and p1.
///
/// Uses the formula for comparing a proportion to a fixed value:
///   n = p_bar * (1 - p_bar) * ((z_alpha + z_beta) / delta)^2
///
/// where:
/// - delta = |p1 - p0| (effect size)
/// - p_bar = p0 (pooled proportion under H0)
/// - z_alpha = z-value for significance level alpha
/// - z_beta = z-value for power (1 - beta)
///
/// Returns the sample size rounded up to the nearest integer.
pub fn min_sample_size(p0: f64, p1: f64, alpha: f64, power: f64) -> u64 {
    let delta = (p1 - p0).abs();
    if delta == 0.0 {
        return u64::MAX; // Infinite sample needed for zero effect.
    }

    // Two-tailed significance: confidence = 1 - alpha.
    let z_alpha = z_value(1.0 - alpha);
    // Power: confidence = power (one-tailed, but we use the same z lookup).
    let z_beta = z_value(power);

    let p = p0;
    let numerator = p * (1.0 - p) * (z_alpha + z_beta) * (z_alpha + z_beta);
    let denominator = delta * delta;

    (numerator / denominator).ceil() as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn standard_case() {
        // p0=0.5, p1=0.55, alpha=0.05, power=0.80
        // delta=0.05, p=0.5, z_alpha=1.960, z_beta=1.282 (power=0.80 → conf=0.80)
        // n = 0.25 * (1.960 + 1.282)^2 / 0.0025 = 0.25 * 10.5165 / 0.0025 ≈ 1052
        // Actually: z for 0.80 from our table ≈ 1.282
        // n = 0.25 * (1.960 + 1.282)^2 / 0.05^2
        //   = 0.25 * (3.242)^2 / 0.0025
        //   = 0.25 * 10.5106 / 0.0025
        //   ≈ 1051
        let n = min_sample_size(0.5, 0.55, 0.05, 0.80);
        // Should be in the ballpark of ~1051.
        assert!(n > 900, "n={} too small", n);
        assert!(n < 1200, "n={} too large", n);
    }

    #[test]
    fn smaller_effect_requires_larger_n() {
        let n_large_effect = min_sample_size(0.5, 0.55, 0.05, 0.80);
        let n_small_effect = min_sample_size(0.5, 0.52, 0.05, 0.80);
        assert!(
            n_small_effect > n_large_effect,
            "smaller effect should need larger n: {} vs {}",
            n_small_effect,
            n_large_effect
        );
    }

    #[test]
    fn lower_confidence_requires_smaller_n() {
        // Lower alpha (0.10 vs 0.05) → smaller z_alpha → smaller n.
        let n_strict = min_sample_size(0.5, 0.55, 0.05, 0.80);
        let n_relaxed = min_sample_size(0.5, 0.55, 0.10, 0.80);
        assert!(
            n_relaxed < n_strict,
            "relaxed alpha should need smaller n: {} vs {}",
            n_relaxed,
            n_strict
        );
    }
}
