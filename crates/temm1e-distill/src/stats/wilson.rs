//! Wilson Score Interval (Wilson, 1927).
//!
//! Provides confidence intervals for binomial proportions that are
//! well-behaved even with small samples or extreme proportions.

/// Z-value lookup for common confidence levels.
///
/// Supports exact lookup for 0.90, 0.95, 0.99, and linear interpolation
/// for values in between. Returns the closest endpoint for values outside
/// the range.
pub fn z_value(confidence: f64) -> f64 {
    // Known z-values for two-tailed confidence intervals.
    const TABLE: [(f64, f64); 5] = [
        (0.80, 1.282),
        (0.90, 1.645),
        (0.95, 1.960),
        (0.98, 2.326),
        (0.99, 2.576),
    ];

    // Clamp to table range.
    if confidence <= TABLE[0].0 {
        return TABLE[0].1;
    }
    if confidence >= TABLE[TABLE.len() - 1].0 {
        return TABLE[TABLE.len() - 1].1;
    }

    // Find bracketing entries and linearly interpolate.
    for i in 0..TABLE.len() - 1 {
        let (c0, z0) = TABLE[i];
        let (c1, z1) = TABLE[i + 1];
        if confidence >= c0 && confidence <= c1 {
            let t = (confidence - c0) / (c1 - c0);
            return z0 + t * (z1 - z0);
        }
    }

    // Fallback (should not reach here due to clamping).
    1.960
}

/// Compute Wilson score confidence interval for a binomial proportion.
///
/// Returns (lower, upper) bounds.
///
/// - `successes`: number of successes
/// - `total`: total number of trials (must be > 0)
/// - `confidence`: confidence level (e.g., 0.95 for 95% CI)
pub fn wilson_interval(successes: u64, total: u64, confidence: f64) -> (f64, f64) {
    if total == 0 {
        return (0.0, 1.0);
    }

    let n = total as f64;
    let p = successes as f64 / n;
    let z = z_value(confidence);
    let z2 = z * z;

    let denominator = n + z2;
    let center = (n * p + z2 / 2.0) / denominator;
    let margin = z * ((n * p * (1.0 - p) + z2 / 4.0) / (denominator * denominator)).sqrt();

    let lower = (center - margin).max(0.0);
    let upper = (center + margin).min(1.0);
    (lower, upper)
}

/// Compute the lower bound of the Wilson score interval.
pub fn wilson_lower(successes: u64, total: u64, confidence: f64) -> f64 {
    wilson_interval(successes, total, confidence).0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn perfect_score_near_one() {
        let (lower, upper) = wilson_interval(100, 100, 0.95);
        assert!(lower > 0.95);
        assert!((upper - 1.0).abs() < 0.01);
    }

    #[test]
    fn zero_score_near_zero() {
        let (lower, upper) = wilson_interval(0, 100, 0.95);
        assert!(lower.abs() < 0.01);
        assert!(upper < 0.05);
    }

    #[test]
    fn high_proportion_at_99_ci() {
        // 95/100 at 99% CI should have lower > 0.85ish.
        let (lower, upper) = wilson_interval(95, 100, 0.99);
        assert!(lower > 0.85);
        assert!(upper > 0.95);
        assert!(upper <= 1.0);
    }

    #[test]
    fn large_sample() {
        // 950/1000 ≈ 0.95 with tighter interval.
        let (lower, upper) = wilson_interval(950, 1000, 0.95);
        assert!(lower > 0.93);
        assert!(upper < 0.97);
    }

    #[test]
    fn small_sample() {
        // 3/5 = 0.6, wide interval expected.
        let (lower, upper) = wilson_interval(3, 5, 0.95);
        assert!(lower > 0.15);
        assert!(lower < 0.40);
        assert!(upper > 0.75);
        assert!(upper < 0.95);
    }

    #[test]
    fn z_value_99_approx() {
        let z = z_value(0.99);
        assert!((z - 2.576).abs() < 0.001);
    }
}
