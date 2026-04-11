//! Circuit breaker for provider calls — prevents cascading failures
//! when the AI provider is down or rate-limited.

use std::sync::atomic::{AtomicU64, AtomicU8, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// Maximum recovery timeout (5 minutes).
const MAX_RECOVERY_TIMEOUT: Duration = Duration::from_secs(300);

/// Maximum backoff duration (30 seconds).
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Circuit breaker states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum CircuitState {
    /// Normal operation — requests pass through.
    Closed = 0,
    /// Provider is failing — requests are rejected immediately.
    Open = 1,
    /// Testing recovery — one request allowed through.
    HalfOpen = 2,
}

impl From<u8> for CircuitState {
    fn from(v: u8) -> Self {
        match v {
            0 => CircuitState::Closed,
            1 => CircuitState::Open,
            2 => CircuitState::HalfOpen,
            _ => CircuitState::Closed,
        }
    }
}

/// Circuit breaker that tracks consecutive provider failures and prevents
/// cascading failures by rejecting requests when the provider appears down.
///
/// State machine:
/// ```text
/// Closed --(failures >= threshold)--> Open
/// Open   --(recovery timeout elapsed)--> HalfOpen
/// HalfOpen --(success)--> Closed
/// HalfOpen --(failure)--> Open (doubled timeout)
/// ```
pub struct CircuitBreaker {
    state: AtomicU8,
    consecutive_failures: AtomicU64,
    failure_threshold: u64,
    /// Base recovery timeout (doubles on repeated failures).
    base_recovery_timeout: Duration,
    /// When the circuit opened (for recovery timing).
    opened_at: Mutex<Option<Instant>>,
    /// Current recovery timeout (doubles each time half-open fails).
    current_recovery_timeout: Mutex<Duration>,
}

impl CircuitBreaker {
    /// Create a new circuit breaker with custom thresholds.
    pub fn new(failure_threshold: u64, base_recovery_timeout: Duration) -> Self {
        Self {
            state: AtomicU8::new(CircuitState::Closed as u8),
            consecutive_failures: AtomicU64::new(0),
            failure_threshold,
            base_recovery_timeout,
            opened_at: Mutex::new(None),
            current_recovery_timeout: Mutex::new(base_recovery_timeout),
        }
    }

    /// Check whether a request is allowed to proceed.
    ///
    /// - **Closed**: always returns `true`.
    /// - **Open**: returns `true` (and transitions to HalfOpen) only if the
    ///   recovery timeout has elapsed.
    /// - **HalfOpen**: returns `true` — the test request is allowed through.
    pub fn can_execute(&self) -> bool {
        let state: CircuitState = self.state.load(Ordering::SeqCst).into();
        match state {
            CircuitState::Closed => true,
            CircuitState::Open => {
                let timeout = {
                    let t = self.current_recovery_timeout.lock().unwrap_or_else(|e| {
                        error!("Mutex poisoned in circuit breaker, recovering");
                        e.into_inner()
                    });
                    *t
                };
                let should_try = {
                    let opened = self.opened_at.lock().unwrap_or_else(|e| {
                        error!("Mutex poisoned in circuit breaker, recovering");
                        e.into_inner()
                    });
                    match *opened {
                        Some(at) => at.elapsed() >= timeout,
                        None => true,
                    }
                };
                if should_try {
                    // CAS: only one caller transitions Open → HalfOpen.
                    // Losers see Err (another caller already transitioned)
                    // and return false — only one test request goes through.
                    let transitioned = self.state.compare_exchange(
                        CircuitState::Open as u8,
                        CircuitState::HalfOpen as u8,
                        Ordering::SeqCst,
                        Ordering::SeqCst,
                    );
                    if transitioned.is_ok() {
                        info!(
                            timeout_secs = timeout.as_secs(),
                            "Circuit breaker transitioning from Open to HalfOpen — allowing test request"
                        );
                        true
                    } else {
                        // Another caller already transitioned — reject this one.
                        false
                    }
                } else {
                    false
                }
            }
            CircuitState::HalfOpen => true,
        }
    }

    /// Record a successful provider call. Resets the circuit to Closed.
    pub fn record_success(&self) {
        let prev: CircuitState = self.state.load(Ordering::SeqCst).into();
        self.state
            .store(CircuitState::Closed as u8, Ordering::SeqCst);
        self.consecutive_failures.store(0, Ordering::SeqCst);

        // Reset recovery timeout to base
        {
            let mut t = self.current_recovery_timeout.lock().unwrap_or_else(|e| {
                error!("Mutex poisoned in circuit breaker, recovering");
                e.into_inner()
            });
            *t = self.base_recovery_timeout;
        }

        if prev != CircuitState::Closed {
            info!(
                previous_state = ?prev,
                "Circuit breaker closed — provider recovered"
            );
        }
    }

    /// Record a failed provider call. May transition the circuit to Open.
    pub fn record_failure(&self) {
        let failures = self.consecutive_failures.fetch_add(1, Ordering::SeqCst) + 1;
        let state: CircuitState = self.state.load(Ordering::SeqCst).into();

        match state {
            CircuitState::Closed => {
                if failures >= self.failure_threshold {
                    self.state.store(CircuitState::Open as u8, Ordering::SeqCst);
                    {
                        let mut opened = self.opened_at.lock().unwrap_or_else(|e| {
                            error!("Mutex poisoned in circuit breaker, recovering");
                            e.into_inner()
                        });
                        *opened = Some(Instant::now());
                    }
                    let timeout = {
                        let t = self.current_recovery_timeout.lock().unwrap_or_else(|e| {
                            error!("Mutex poisoned in circuit breaker, recovering");
                            e.into_inner()
                        });
                        *t
                    };
                    warn!(
                        failures = failures,
                        threshold = self.failure_threshold,
                        recovery_timeout_secs = timeout.as_secs(),
                        "Circuit breaker opened — provider has {} consecutive failures",
                        failures
                    );
                }
            }
            CircuitState::HalfOpen => {
                // Test request failed — reopen with doubled timeout
                self.state.store(CircuitState::Open as u8, Ordering::SeqCst);
                {
                    let mut opened = self.opened_at.lock().unwrap_or_else(|e| {
                        error!("Mutex poisoned in circuit breaker, recovering");
                        e.into_inner()
                    });
                    *opened = Some(Instant::now());
                }
                let new_timeout = {
                    let mut t = self.current_recovery_timeout.lock().unwrap_or_else(|e| {
                        error!("Mutex poisoned in circuit breaker, recovering");
                        e.into_inner()
                    });
                    *t = (*t * 2).min(MAX_RECOVERY_TIMEOUT);
                    *t
                };
                warn!(
                    new_recovery_timeout_secs = new_timeout.as_secs(),
                    "Circuit breaker re-opened from HalfOpen — doubled recovery timeout"
                );
            }
            CircuitState::Open => {
                // Already open, nothing to do
            }
        }
    }

    /// Return the current circuit state.
    pub fn state(&self) -> CircuitState {
        self.state.load(Ordering::SeqCst).into()
    }

    /// Compute an exponential backoff duration with deterministic jitter.
    ///
    /// Formula: `min(30s, 1s * 2^attempt)` with ±25% jitter.
    /// Jitter is deterministic (based on attempt number) — no `rand` dependency.
    pub fn backoff_duration(attempt: u32) -> Duration {
        let base_ms = 1000u64.saturating_mul(1u64.checked_shl(attempt).unwrap_or(u64::MAX));
        let capped_ms = base_ms.min(MAX_BACKOFF.as_millis() as u64);

        // Deterministic jitter: ±25% based on attempt number.
        // Use a simple hash-like function of the attempt to get a value in [-0.25, 0.25].
        let jitter_pattern: [i64; 8] = [-25, 15, -10, 20, -5, 25, -20, 10];
        let jitter_pct = jitter_pattern[(attempt as usize) % jitter_pattern.len()];
        let jitter_ms = (capped_ms as i64 * jitter_pct) / 100;
        let final_ms = (capped_ms as i64 + jitter_ms).max(1) as u64;

        Duration::from_millis(final_ms)
    }
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::new(5, Duration::from_secs(30))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_closed_allows_execution() {
        let cb = CircuitBreaker::default();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.can_execute());
    }

    #[test]
    fn test_opens_after_threshold_failures() {
        let cb = CircuitBreaker::new(3, Duration::from_secs(30));
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Closed);

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);
    }

    #[test]
    fn test_open_rejects_execution() {
        let cb = CircuitBreaker::new(2, Duration::from_secs(60));

        // Trip the breaker
        cb.record_failure();
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Should reject — timeout hasn't elapsed (60s)
        assert!(!cb.can_execute());
    }

    #[test]
    fn test_half_open_after_recovery_timeout() {
        // Use a very short timeout so the test passes instantly
        let cb = CircuitBreaker::new(1, Duration::from_millis(1));

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for the recovery timeout to elapse
        std::thread::sleep(Duration::from_millis(5));

        assert!(cb.can_execute());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_half_open_success_closes() {
        let cb = CircuitBreaker::new(1, Duration::from_millis(1));

        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        std::thread::sleep(Duration::from_millis(5));
        assert!(cb.can_execute());
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        cb.record_success();
        assert_eq!(cb.state(), CircuitState::Closed);
        assert!(cb.can_execute());
    }

    #[test]
    fn test_half_open_failure_reopens_with_doubled_timeout() {
        let base_timeout = Duration::from_millis(10);
        let cb = CircuitBreaker::new(1, base_timeout);

        // Trip the breaker
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Wait for recovery timeout, transition to HalfOpen
        std::thread::sleep(Duration::from_millis(15));
        assert!(cb.can_execute());
        assert_eq!(cb.state(), CircuitState::HalfOpen);

        // Fail the test request — should reopen with doubled timeout
        cb.record_failure();
        assert_eq!(cb.state(), CircuitState::Open);

        // Check that the recovery timeout has doubled
        let current_timeout = {
            let t = cb.current_recovery_timeout.lock().unwrap_or_else(|e| {
                tracing::error!("Mutex poisoned in circuit breaker, recovering");
                e.into_inner()
            });
            *t
        };
        assert_eq!(current_timeout, base_timeout * 2);

        // The short original timeout shouldn't be enough — need to wait for doubled
        std::thread::sleep(Duration::from_millis(12));
        // Doubled timeout is 20ms, we only slept 12ms — should still be Open
        assert!(!cb.can_execute());
        assert_eq!(cb.state(), CircuitState::Open);

        // Now wait long enough for the doubled timeout
        std::thread::sleep(Duration::from_millis(15));
        assert!(cb.can_execute());
        assert_eq!(cb.state(), CircuitState::HalfOpen);
    }

    #[test]
    fn test_backoff_duration_increases() {
        let d0 = CircuitBreaker::backoff_duration(0);
        let d1 = CircuitBreaker::backoff_duration(1);
        let d2 = CircuitBreaker::backoff_duration(2);
        let d3 = CircuitBreaker::backoff_duration(3);

        // Each should roughly double (within jitter bounds)
        assert!(d1 > d0, "d1 ({:?}) should be > d0 ({:?})", d1, d0);
        assert!(d2 > d1, "d2 ({:?}) should be > d1 ({:?})", d2, d1);
        assert!(d3 > d2, "d3 ({:?}) should be > d2 ({:?})", d3, d2);
    }

    #[test]
    fn test_backoff_caps_at_30_seconds() {
        // attempt=20 → 2^20 seconds ≈ 1,048,576 seconds → capped at 30s
        let d = CircuitBreaker::backoff_duration(20);
        assert!(
            d <= Duration::from_millis(37_500), // 30s + 25% jitter
            "Backoff should be capped near 30s, got {:?}",
            d
        );
        assert!(
            d >= Duration::from_millis(22_500), // 30s - 25% jitter
            "Backoff should be at least 22.5s at attempt 20, got {:?}",
            d
        );
    }
}
