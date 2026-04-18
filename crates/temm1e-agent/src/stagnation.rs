//! Stagnation detector for the agent tool loop.
//!
//! Detects when the model gets stuck in a degenerate loop: same tool called
//! with same input, producing same result, N times in a row. Both halves
//! must hold — legitimate polling ("wait until status=ready") produces
//! identical calls with changing results, and legitimate iteration over
//! data produces different calls with potentially-similar results. Only the
//! intersection — same call AND same result — signals real pathology.
//!
//! When detected, the caller breaks the loop and the final-reply block
//! asks the model to synthesize what it has.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

/// Default window: 4 consecutive identical (call, result) pairs trigger
/// stagnation. Chosen to tolerate natural 2-3 step retry flows.
pub const DEFAULT_WINDOW: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagnationSignal {
    /// No stagnation detected at this observation.
    Ok,
    /// `count` consecutive observations were byte-identical on both call
    /// and result. The caller should break the loop.
    Stuck { count: usize },
}

/// Tracks recent tool-call/result hash pairs to detect degenerate loops.
pub struct StagnationDetector {
    window: usize,
    recent_calls: VecDeque<u64>,
    recent_results: VecDeque<u64>,
}

impl StagnationDetector {
    pub fn new() -> Self {
        Self::with_window(DEFAULT_WINDOW)
    }

    pub fn with_window(window: usize) -> Self {
        let window = window.max(2);
        Self {
            window,
            recent_calls: VecDeque::with_capacity(window),
            recent_results: VecDeque::with_capacity(window),
        }
    }

    fn hash_call(tool_name: &str, input: &serde_json::Value) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        tool_name.hash(&mut hasher);
        // serde_json::to_string gives a stable representation per value.
        let canonical = serde_json::to_string(input).unwrap_or_default();
        canonical.hash(&mut hasher);
        hasher.finish()
    }

    fn hash_result(result: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        result.hash(&mut hasher);
        hasher.finish()
    }

    /// Record a (tool_call, result) observation. Returns a signal indicating
    /// whether the loop should break.
    pub fn observe(
        &mut self,
        tool_name: &str,
        input: &serde_json::Value,
        result: &str,
    ) -> StagnationSignal {
        let call_hash = Self::hash_call(tool_name, input);
        let result_hash = Self::hash_result(result);

        self.recent_calls.push_back(call_hash);
        self.recent_results.push_back(result_hash);
        while self.recent_calls.len() > self.window {
            self.recent_calls.pop_front();
        }
        while self.recent_results.len() > self.window {
            self.recent_results.pop_front();
        }

        if self.recent_calls.len() < self.window {
            return StagnationSignal::Ok;
        }

        let all_calls_same = self.recent_calls.iter().all(|h| *h == call_hash);
        let all_results_same = self.recent_results.iter().all(|h| *h == result_hash);

        if all_calls_same && all_results_same {
            StagnationSignal::Stuck { count: self.window }
        } else {
            StagnationSignal::Ok
        }
    }

    pub fn reset(&mut self) {
        self.recent_calls.clear();
        self.recent_results.clear();
    }
}

impl Default for StagnationDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ok_when_empty() {
        let mut d = StagnationDetector::new();
        assert_eq!(d.observe("t", &json!({"a":1}), "r"), StagnationSignal::Ok);
    }

    #[test]
    fn stuck_after_window_identical_observations() {
        let mut d = StagnationDetector::with_window(3);
        assert_eq!(
            d.observe("file_read", &json!({"path":"x"}), "body"),
            StagnationSignal::Ok
        );
        assert_eq!(
            d.observe("file_read", &json!({"path":"x"}), "body"),
            StagnationSignal::Ok
        );
        assert!(matches!(
            d.observe("file_read", &json!({"path":"x"}), "body"),
            StagnationSignal::Stuck { .. }
        ));
    }

    #[test]
    fn same_call_different_result_is_ok() {
        // Legitimate polling pattern: same call, evolving result.
        let mut d = StagnationDetector::with_window(3);
        d.observe("http_get", &json!({"url":"x"}), "pending");
        d.observe("http_get", &json!({"url":"x"}), "pending");
        assert_eq!(
            d.observe("http_get", &json!({"url":"x"}), "ready"),
            StagnationSignal::Ok
        );
    }

    #[test]
    fn different_call_same_result_is_ok() {
        // Legitimate iteration: different files happening to have same body.
        let mut d = StagnationDetector::with_window(3);
        d.observe("file_read", &json!({"path":"a"}), "body");
        d.observe("file_read", &json!({"path":"b"}), "body");
        assert_eq!(
            d.observe("file_read", &json!({"path":"c"}), "body"),
            StagnationSignal::Ok
        );
    }

    #[test]
    fn reset_clears_state() {
        let mut d = StagnationDetector::with_window(2);
        d.observe("t", &json!({}), "r");
        d.observe("t", &json!({}), "r");
        d.reset();
        assert_eq!(d.observe("t", &json!({}), "r"), StagnationSignal::Ok);
    }

    #[test]
    fn window_minimum_of_two() {
        // Window of 1 should be clamped to 2.
        let mut d = StagnationDetector::with_window(1);
        assert_eq!(d.observe("t", &json!({}), "r"), StagnationSignal::Ok);
        assert!(matches!(
            d.observe("t", &json!({}), "r"),
            StagnationSignal::Stuck { .. }
        ));
    }

    #[test]
    fn window_recovers_when_pattern_breaks() {
        // Window: 3 identical observations, then something different, then
        // 3 identical again (but different from before) should trigger stuck.
        let mut d = StagnationDetector::with_window(3);
        d.observe("t", &json!({"a":1}), "r1");
        d.observe("t", &json!({"a":1}), "r1");
        d.observe("t", &json!({"a":2}), "r2"); // breaks pattern
        d.observe("t", &json!({"a":1}), "r1");
        d.observe("t", &json!({"a":1}), "r1");
        // Only 2 identical at the tail — not yet stuck.
        assert_eq!(d.observe("u", &json!({}), "other"), StagnationSignal::Ok);
    }
}
