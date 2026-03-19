//! Sequential Probability Ratio Test (Wald, 1945).
//!
//! Tests H0: p = p0 vs H1: p = p1, controlling type I error alpha
//! and type II error beta, with optional truncation at max_n.

use serde::{Deserialize, Serialize};

/// Decision from SPRT observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SprtDecision {
    /// Accept H1 (p = p1).
    AcceptH1,
    /// Accept H0 (p = p0).
    AcceptH0,
    /// Continue sampling.
    Continue,
}

/// Sequential Probability Ratio Test state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Sprt {
    /// Null hypothesis proportion.
    p0: f64,
    /// Alternative hypothesis proportion.
    p1: f64,
    /// Upper boundary: ln((1 - beta) / alpha).
    log_a: f64,
    /// Lower boundary: ln(beta / (1 - alpha)).
    log_b: f64,
    /// Maximum observations before forced decision (truncation).
    max_n: u32,
    /// Cumulative log-likelihood ratio.
    lambda: f64,
    /// Number of observations so far.
    n: u32,
}

impl Sprt {
    /// Create a new SPRT.
    ///
    /// - `p0`: null hypothesis proportion (e.g., 0.5)
    /// - `p1`: alternative hypothesis proportion (e.g., 0.7)
    /// - `alpha`: type I error rate
    /// - `beta`: type II error rate
    /// - `max_n`: maximum observations before truncation
    pub fn new(p0: f64, p1: f64, alpha: f64, beta: f64, max_n: u32) -> Self {
        let log_a = ((1.0 - beta) / alpha).ln();
        let log_b = (beta / (1.0 - alpha)).ln();
        Self {
            p0,
            p1,
            log_a,
            log_b,
            max_n,
            lambda: 0.0,
            n: 0,
        }
    }

    /// Restore from persisted state.
    ///
    /// Accepts raw error rates `alpha` and `beta` (not pre-computed log
    /// boundaries) so callers can use config values directly.
    pub fn from_state(
        p0: f64,
        p1: f64,
        alpha: f64,
        beta: f64,
        max_n: u32,
        lambda: f64,
        n: u32,
    ) -> Self {
        let log_a = ((1.0 - beta) / alpha).ln();
        let log_b = (beta / (1.0 - alpha)).ln();
        Self {
            p0,
            p1,
            log_a,
            log_b,
            max_n,
            lambda,
            n,
        }
    }

    /// Observe a single Bernoulli outcome.
    ///
    /// `agree == true` means the outcome supports H1.
    /// Returns the current decision after this observation.
    pub fn observe(&mut self, agree: bool) -> SprtDecision {
        self.n += 1;
        if agree {
            self.lambda += (self.p1 / self.p0).ln();
        } else {
            self.lambda += ((1.0 - self.p1) / (1.0 - self.p0)).ln();
        }
        self.decision()
    }

    /// Current decision based on lambda and boundaries.
    pub fn decision(&self) -> SprtDecision {
        if self.lambda >= self.log_a {
            SprtDecision::AcceptH1
        } else if self.lambda <= self.log_b {
            SprtDecision::AcceptH0
        } else if self.n >= self.max_n {
            // Truncation: decide based on which boundary is closer.
            if self.lambda > 0.0 {
                SprtDecision::AcceptH1
            } else {
                SprtDecision::AcceptH0
            }
        } else {
            SprtDecision::Continue
        }
    }

    /// Reset to initial state.
    pub fn reset(&mut self) {
        self.lambda = 0.0;
        self.n = 0;
    }

    /// Current log-likelihood ratio.
    pub fn lambda(&self) -> f64 {
        self.lambda
    }

    /// Number of observations so far.
    pub fn n(&self) -> u32 {
        self.n
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_starts_at_zero() {
        let sprt = Sprt::new(0.5, 0.7, 0.05, 0.10, 1000);
        assert_eq!(sprt.lambda(), 0.0);
        assert_eq!(sprt.n(), 0);
        assert_eq!(sprt.decision(), SprtDecision::Continue);
    }

    #[test]
    fn all_agree_accepts_h1() {
        let mut sprt = Sprt::new(0.5, 0.7, 0.05, 0.10, 1000);
        let mut decision = SprtDecision::Continue;
        for _ in 0..1000 {
            decision = sprt.observe(true);
            if decision != SprtDecision::Continue {
                break;
            }
        }
        assert_eq!(decision, SprtDecision::AcceptH1);
    }

    #[test]
    fn all_disagree_accepts_h0() {
        let mut sprt = Sprt::new(0.5, 0.7, 0.05, 0.10, 1000);
        let mut decision = SprtDecision::Continue;
        for _ in 0..1000 {
            decision = sprt.observe(false);
            if decision != SprtDecision::Continue {
                break;
            }
        }
        assert_eq!(decision, SprtDecision::AcceptH0);
    }

    #[test]
    fn mixed_continues() {
        let mut sprt = Sprt::new(0.5, 0.7, 0.05, 0.10, 1000);
        // Alternate true/false to stay near zero.
        for _ in 0..5 {
            sprt.observe(true);
            sprt.observe(false);
        }
        assert_eq!(sprt.decision(), SprtDecision::Continue);
    }

    #[test]
    fn truncation_forces_decision() {
        let mut sprt = Sprt::new(0.5, 0.7, 0.05, 0.10, 10);
        // Alternate to stay indeterminate.
        for _ in 0..5 {
            sprt.observe(true);
            sprt.observe(false);
        }
        // After 10 observations, truncation fires.
        let d = sprt.decision();
        assert!(d == SprtDecision::AcceptH1 || d == SprtDecision::AcceptH0);
    }

    #[test]
    fn single_disagree_offsets_many_agrees() {
        // With p0=0.5, p1=0.7:
        //   agree  increment = ln(0.7/0.5) ≈ 0.3365
        //   disagree increment = ln(0.3/0.5) ≈ -0.5108
        // One disagree offsets about 1.5 agrees.
        // So 19 agrees + 1 disagree should still be strongly positive but
        // less than 20 agrees alone.
        let mut sprt_pure = Sprt::new(0.5, 0.7, 0.05, 0.10, 1000);
        for _ in 0..20 {
            sprt_pure.observe(true);
        }
        let lambda_pure = sprt_pure.lambda();

        let mut sprt_mixed = Sprt::new(0.5, 0.7, 0.05, 0.10, 1000);
        for _ in 0..19 {
            sprt_mixed.observe(true);
        }
        sprt_mixed.observe(false);
        let lambda_mixed = sprt_mixed.lambda();

        assert!(lambda_mixed < lambda_pure);
        assert!(lambda_mixed > 0.0); // Still positive overall.
    }

    #[test]
    fn state_restore() {
        let mut sprt = Sprt::new(0.5, 0.7, 0.05, 0.10, 1000);
        for _ in 0..5 {
            sprt.observe(true);
        }

        // from_state takes raw alpha/beta error rates, not pre-computed log boundaries.
        let restored = Sprt::from_state(0.5, 0.7, 0.05, 0.10, 1000, sprt.lambda(), sprt.n());
        assert!((restored.lambda() - sprt.lambda()).abs() < 1e-12);
        assert_eq!(restored.n(), sprt.n());
        assert_eq!(restored.decision(), sprt.decision());
    }

    #[test]
    fn boundary_values() {
        // Verify log_a and log_b are computed correctly.
        let sprt = Sprt::new(0.5, 0.7, 0.05, 0.10, 100);
        let expected_log_a = ((1.0 - 0.10) / 0.05_f64).ln();
        let expected_log_b = (0.10 / (1.0 - 0.05_f64)).ln();
        assert!((sprt.log_a - expected_log_a).abs() < 1e-12);
        assert!((sprt.log_b - expected_log_b).abs() < 1e-12);
    }
}
