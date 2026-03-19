//! Cumulative Sum control chart (Page, 1954).
//!
//! Detects sustained shifts in a process mean. The one-sided lower CUSUM
//! accumulates deviations from `target`, offset by a `slack` parameter,
//! and signals when the cumulative sum exceeds `threshold`.

use serde::{Deserialize, Serialize};

/// CUSUM detector state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Cusum {
    /// Target value (in-control mean).
    target: f64,
    /// Slack parameter (allowable slack, typically delta/2).
    slack: f64,
    /// Decision threshold (alarm limit).
    threshold: f64,
    /// Current cumulative sum statistic.
    s: f64,
    /// Number of observations.
    n: u32,
    /// Fast Initial Response: start at threshold/2 instead of 0.
    fir: bool,
}

impl Cusum {
    /// Create a new CUSUM detector.
    ///
    /// - `target`: in-control process mean
    /// - `slack`: allowable slack (k), typically half the shift to detect
    /// - `threshold`: alarm threshold (h)
    /// - `fir`: if true, initialize statistic at threshold/2 (Fast Initial Response)
    pub fn new(target: f64, slack: f64, threshold: f64, fir: bool) -> Self {
        let s = if fir { threshold / 2.0 } else { 0.0 };
        Self {
            target,
            slack,
            threshold,
            s,
            n: 0,
            fir,
        }
    }

    /// Restore from persisted state.
    ///
    /// Argument order matches the engine's calling convention:
    /// config values first (target, slack, threshold, fir), then
    /// persisted state (s, n).
    pub fn from_state(target: f64, slack: f64, threshold: f64, fir: bool, s: f64, n: u32) -> Self {
        Self {
            target,
            slack,
            threshold,
            s,
            n,
            fir,
        }
    }

    /// Observe a new value. Returns `true` if alarm threshold is exceeded.
    ///
    /// The upper CUSUM is: S_n = max(0, S_{n-1} + (target - x) - slack)
    /// This detects negative shifts (values lower than target).
    pub fn observe(&mut self, value: f64) -> bool {
        self.n += 1;
        self.s = f64::max(0.0, self.s + (self.target - value) - self.slack);
        self.s > self.threshold
    }

    /// Reset the detector to initial state.
    pub fn reset(&mut self) {
        self.s = if self.fir { self.threshold / 2.0 } else { 0.0 };
        self.n = 0;
    }

    /// Current CUSUM statistic.
    pub fn statistic(&self) -> f64 {
        self.s
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
    fn in_control_no_alarm() {
        // Values at target: deviation is 0, minus slack makes it negative, clamped to 0.
        let mut cusum = Cusum::new(1.0, 0.1, 5.0, false);
        for _ in 0..100 {
            assert!(!cusum.observe(1.0));
        }
        assert!((cusum.statistic() - 0.0).abs() < 1e-12);
    }

    #[test]
    fn sustained_drift_triggers_alarm() {
        // target=1.0, slack=0.1, threshold=5.0.
        // Observing 0.5 each time: increment = (1.0 - 0.5) - 0.1 = 0.4 per step.
        // After 13 steps: 13 * 0.4 = 5.2 > 5.0 → alarm.
        let mut cusum = Cusum::new(1.0, 0.1, 5.0, false);
        let mut alarm_at = None;
        for i in 1..=20 {
            if cusum.observe(0.5) {
                alarm_at = Some(i);
                break;
            }
        }
        assert_eq!(alarm_at, Some(13));
    }

    #[test]
    fn single_outlier_no_alarm() {
        // A single moderate deviation should not alarm; the statistic
        // rises but stays under the threshold, then decays back to 0.
        // target=1.0, slack=0.1, threshold=5.0
        // Observing -3.0: increment = (1.0 - (-3.0)) - 0.1 = 3.9, under 5.0.
        let mut cusum = Cusum::new(1.0, 0.1, 5.0, false);
        assert!(!cusum.observe(-3.0));
        assert!(cusum.statistic() > 0.0);
        // Then back to normal — in-control observations drive s toward 0.
        for _ in 0..100 {
            cusum.observe(1.0);
        }
        assert!((cusum.statistic() - 0.0).abs() < 1e-12);
    }

    #[test]
    fn fir_starts_at_half_threshold() {
        let cusum = Cusum::new(1.0, 0.1, 5.0, true);
        assert!((cusum.statistic() - 2.5).abs() < 1e-12);

        // FIR means fewer bad observations needed to trigger alarm.
        // Need (5.0 - 2.5) / 0.4 = 6.25 → 7 observations of 0.5.
        let mut cusum_fir = Cusum::new(1.0, 0.1, 5.0, true);
        let mut alarm_at = None;
        for i in 1..=20 {
            if cusum_fir.observe(0.5) {
                alarm_at = Some(i);
                break;
            }
        }
        assert_eq!(alarm_at, Some(7));
    }

    #[test]
    fn reset_restores_initial() {
        let mut cusum = Cusum::new(1.0, 0.1, 5.0, false);
        for _ in 0..5 {
            cusum.observe(0.5);
        }
        assert!(cusum.statistic() > 0.0);
        assert_eq!(cusum.n(), 5);

        cusum.reset();
        assert!((cusum.statistic() - 0.0).abs() < 1e-12);
        assert_eq!(cusum.n(), 0);
    }

    #[test]
    fn state_restore() {
        let mut cusum = Cusum::new(1.0, 0.1, 5.0, false);
        for _ in 0..5 {
            cusum.observe(0.8);
        }

        let restored = Cusum::from_state(1.0, 0.1, 5.0, false, cusum.statistic(), cusum.n());
        assert!((restored.statistic() - cusum.statistic()).abs() < 1e-12);
        assert_eq!(restored.n(), cusum.n());
    }

    #[test]
    fn exact_boundary_not_alarm() {
        // S must be *strictly greater than* threshold to alarm.
        // target=1.0, slack=0.0, threshold=5.0.
        // 5 observations of 0.0: each adds 1.0, total = 5.0 exactly.
        let mut cusum = Cusum::new(1.0, 0.0, 5.0, false);
        for _ in 0..5 {
            let alarm = cusum.observe(0.0);
            if cusum.n() < 5 {
                assert!(!alarm);
            }
        }
        // At exactly 5.0, should NOT alarm (strictly greater than).
        assert!((cusum.statistic() - 5.0).abs() < 1e-12);
        assert!(!cusum.observe(1.0)); // Next in-control obs brings it back down.
    }
}
