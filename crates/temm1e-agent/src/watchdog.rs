//! Process Watchdog — monitors subsystem health and tracks activity
//! to detect stalls, failures, and degraded operation.

use chrono::{DateTime, Utc};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tracing::{error, info, warn};

/// Health status of a subsystem.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SubsystemStatus {
    /// Subsystem is operating normally.
    Healthy,
    /// Subsystem is operational but experiencing issues.
    Degraded(String),
    /// Subsystem has failed.
    Unhealthy(String),
}

impl SubsystemStatus {
    /// Returns `true` if the status is `Healthy`.
    pub fn is_healthy(&self) -> bool {
        matches!(self, SubsystemStatus::Healthy)
    }
}

/// Snapshot of the agent's health at a point in time.
#[derive(Debug, Clone)]
pub struct HealthReport {
    /// Health of the AI provider subsystem.
    pub provider_status: SubsystemStatus,
    /// Health of the memory backend subsystem.
    pub memory_status: SubsystemStatus,
    /// Health of the messaging channel subsystem.
    pub channel_status: SubsystemStatus,
    /// When the agent last processed a message.
    pub last_activity: DateTime<Utc>,
    /// How long the agent process has been running.
    pub uptime: Duration,
    /// When this health check was performed.
    pub checked_at: DateTime<Utc>,
}

/// Configuration for the watchdog.
#[derive(Debug, Clone)]
pub struct WatchdogConfig {
    /// How often to check health (default: 60s).
    pub check_interval: Duration,
    /// If no activity in this window, the agent is considered idle (default: 30min).
    /// Being idle is not the same as being unhealthy — it just gets noted.
    pub activity_timeout: Duration,
    /// After this many consecutive health check failures, recommend shutdown (default: 5).
    pub max_consecutive_failures: u32,
}

impl Default for WatchdogConfig {
    fn default() -> Self {
        Self {
            check_interval: Duration::from_secs(60),
            activity_timeout: Duration::from_secs(30 * 60),
            max_consecutive_failures: 5,
        }
    }
}

/// Internal state for a single subsystem's reported health.
#[derive(Debug)]
struct SubsystemState {
    status: SubsystemStatus,
}

impl Default for SubsystemState {
    fn default() -> Self {
        Self {
            status: SubsystemStatus::Healthy,
        }
    }
}

/// Process watchdog that tracks subsystem health and consecutive failures.
///
/// The watchdog does not poll external systems itself — instead, callers
/// report health via `report_*_health()` methods, and the watchdog
/// aggregates the results into a `HealthReport`.
pub struct Watchdog {
    config: WatchdogConfig,
    start_time: Instant,
    last_activity: Arc<Mutex<DateTime<Utc>>>,
    consecutive_failures: Arc<AtomicU32>,
    provider_state: Arc<Mutex<SubsystemState>>,
    memory_state: Arc<Mutex<SubsystemState>>,
    channel_state: Arc<Mutex<SubsystemState>>,
}

impl Watchdog {
    /// Create a new watchdog with the given configuration.
    pub fn new(config: WatchdogConfig) -> Self {
        Self {
            config,
            start_time: Instant::now(),
            last_activity: Arc::new(Mutex::new(Utc::now())),
            consecutive_failures: Arc::new(AtomicU32::new(0)),
            provider_state: Arc::new(Mutex::new(SubsystemState::default())),
            memory_state: Arc::new(Mutex::new(SubsystemState::default())),
            channel_state: Arc::new(Mutex::new(SubsystemState::default())),
        }
    }

    /// Record that the agent just processed a message or performed work.
    /// This resets the idle timer.
    pub fn record_activity(&self) {
        let mut last = self.last_activity.lock().unwrap();
        *last = Utc::now();
    }

    /// Report the health of the AI provider subsystem.
    pub fn report_provider_health(&self, healthy: bool, detail: Option<String>) {
        let new_status = to_status(healthy, detail);
        let mut state = self.provider_state.lock().unwrap();
        log_transition("provider", &state.status, &new_status);
        state.status = new_status;
    }

    /// Report the health of the memory backend subsystem.
    pub fn report_memory_health(&self, healthy: bool, detail: Option<String>) {
        let new_status = to_status(healthy, detail);
        let mut state = self.memory_state.lock().unwrap();
        log_transition("memory", &state.status, &new_status);
        state.status = new_status;
    }

    /// Report the health of the messaging channel subsystem.
    pub fn report_channel_health(&self, healthy: bool, detail: Option<String>) {
        let new_status = to_status(healthy, detail);
        let mut state = self.channel_state.lock().unwrap();
        log_transition("channel", &state.status, &new_status);
        state.status = new_status;
    }

    /// Produce a health report reflecting current subsystem statuses.
    pub fn check_health(&self) -> HealthReport {
        let provider_status = {
            let state = self.provider_state.lock().unwrap();
            state.status.clone()
        };
        let memory_status = {
            let state = self.memory_state.lock().unwrap();
            state.status.clone()
        };
        let channel_status = {
            let state = self.channel_state.lock().unwrap();
            state.status.clone()
        };
        let last_activity = {
            let last = self.last_activity.lock().unwrap();
            *last
        };

        // Count any unhealthy subsystem as a failure
        let any_unhealthy = matches!(provider_status, SubsystemStatus::Unhealthy(_))
            || matches!(memory_status, SubsystemStatus::Unhealthy(_))
            || matches!(channel_status, SubsystemStatus::Unhealthy(_));

        if any_unhealthy {
            let prev = self.consecutive_failures.fetch_add(1, Ordering::SeqCst);
            warn!(
                consecutive_failures = prev + 1,
                max = self.config.max_consecutive_failures,
                "Watchdog detected unhealthy subsystem"
            );
        } else {
            let prev = self.consecutive_failures.load(Ordering::SeqCst);
            if prev > 0 {
                self.consecutive_failures.store(0, Ordering::SeqCst);
                info!(
                    previous_failures = prev,
                    "Watchdog: all subsystems healthy — resetting failure counter"
                );
            }
        }

        HealthReport {
            provider_status,
            memory_status,
            channel_status,
            last_activity,
            uptime: self.start_time.elapsed(),
            checked_at: Utc::now(),
        }
    }

    /// Returns `true` if consecutive failures exceed the configured maximum,
    /// indicating the process should be shut down.
    pub fn should_shutdown(&self) -> bool {
        let failures = self.consecutive_failures.load(Ordering::SeqCst);
        if failures > self.config.max_consecutive_failures {
            error!(
                consecutive_failures = failures,
                max = self.config.max_consecutive_failures,
                "Watchdog recommends shutdown — too many consecutive failures"
            );
            return true;
        }
        false
    }

    /// Reset the consecutive failure counter (e.g. after manual intervention).
    pub fn reset_failures(&self) {
        self.consecutive_failures.store(0, Ordering::SeqCst);
        info!("Watchdog: consecutive failure counter reset");
    }

    /// Return the watchdog configuration.
    pub fn config(&self) -> &WatchdogConfig {
        &self.config
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Convert a (healthy, detail) pair into a `SubsystemStatus`.
fn to_status(healthy: bool, detail: Option<String>) -> SubsystemStatus {
    match (healthy, detail) {
        (true, None) => SubsystemStatus::Healthy,
        (true, Some(msg)) => SubsystemStatus::Degraded(msg),
        (false, Some(msg)) => SubsystemStatus::Unhealthy(msg),
        (false, None) => SubsystemStatus::Unhealthy("unknown error".to_string()),
    }
}

/// Log health transitions at the appropriate level.
fn log_transition(subsystem: &str, old: &SubsystemStatus, new: &SubsystemStatus) {
    if old == new {
        return;
    }
    match (old, new) {
        // Recovery
        (
            SubsystemStatus::Unhealthy(_) | SubsystemStatus::Degraded(_),
            SubsystemStatus::Healthy,
        ) => {
            info!(subsystem = subsystem, "Subsystem recovered to healthy");
        }
        // Degradation
        (_, SubsystemStatus::Degraded(msg)) => {
            warn!(subsystem = subsystem, detail = %msg, "Subsystem degraded");
        }
        // Failure
        (_, SubsystemStatus::Unhealthy(msg)) => {
            error!(subsystem = subsystem, detail = %msg, "Subsystem unhealthy");
        }
        // Healthy to healthy (no-op, already handled by equality check)
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_watchdog_is_healthy() {
        let wd = Watchdog::new(WatchdogConfig::default());
        let report = wd.check_health();

        assert!(report.provider_status.is_healthy());
        assert!(report.memory_status.is_healthy());
        assert!(report.channel_status.is_healthy());
        assert!(!wd.should_shutdown());
    }

    #[test]
    fn record_activity_updates_timestamp() {
        let wd = Watchdog::new(WatchdogConfig::default());

        let before = {
            let last = wd.last_activity.lock().unwrap();
            *last
        };

        // Small sleep to ensure time advances
        std::thread::sleep(Duration::from_millis(10));
        wd.record_activity();

        let after = {
            let last = wd.last_activity.lock().unwrap();
            *last
        };

        assert!(after > before, "Activity timestamp should advance");
    }

    #[test]
    fn consecutive_failures_trigger_shutdown() {
        let config = WatchdogConfig {
            max_consecutive_failures: 2,
            ..Default::default()
        };
        let wd = Watchdog::new(config);

        // Report unhealthy provider
        wd.report_provider_health(false, Some("connection refused".to_string()));

        // Each check_health call with an unhealthy subsystem increments failures
        let _ = wd.check_health(); // failure 1
        assert!(!wd.should_shutdown());

        let _ = wd.check_health(); // failure 2
        assert!(!wd.should_shutdown());

        let _ = wd.check_health(); // failure 3 — exceeds max of 2
        assert!(wd.should_shutdown());
    }

    #[test]
    fn reset_failures_works() {
        let config = WatchdogConfig {
            max_consecutive_failures: 1,
            ..Default::default()
        };
        let wd = Watchdog::new(config);

        wd.report_memory_health(false, Some("disk full".to_string()));
        let _ = wd.check_health(); // failure 1
        let _ = wd.check_health(); // failure 2 — exceeds max of 1

        assert!(wd.should_shutdown());

        wd.reset_failures();
        assert!(!wd.should_shutdown());
    }

    #[test]
    fn health_report_includes_correct_uptime() {
        let wd = Watchdog::new(WatchdogConfig::default());

        std::thread::sleep(Duration::from_millis(50));

        let report = wd.check_health();
        assert!(
            report.uptime >= Duration::from_millis(50),
            "Uptime should be at least 50ms, got {:?}",
            report.uptime
        );
    }

    #[test]
    fn provider_health_reporting() {
        let wd = Watchdog::new(WatchdogConfig::default());

        // Initially healthy
        let report = wd.check_health();
        assert!(report.provider_status.is_healthy());

        // Report degraded (healthy=true with a detail message)
        wd.report_provider_health(true, Some("high latency".to_string()));
        let report = wd.check_health();
        assert_eq!(
            report.provider_status,
            SubsystemStatus::Degraded("high latency".to_string())
        );

        // Report unhealthy
        wd.report_provider_health(false, Some("timeout".to_string()));
        let report = wd.check_health();
        assert_eq!(
            report.provider_status,
            SubsystemStatus::Unhealthy("timeout".to_string())
        );

        // Recover
        wd.report_provider_health(true, None);
        let report = wd.check_health();
        assert!(report.provider_status.is_healthy());
    }

    #[test]
    fn memory_health_reporting() {
        let wd = Watchdog::new(WatchdogConfig::default());

        wd.report_memory_health(false, Some("sqlite locked".to_string()));
        let report = wd.check_health();
        assert_eq!(
            report.memory_status,
            SubsystemStatus::Unhealthy("sqlite locked".to_string())
        );

        wd.report_memory_health(true, None);
        let report = wd.check_health();
        assert!(report.memory_status.is_healthy());
    }

    #[test]
    fn channel_health_reporting() {
        let wd = Watchdog::new(WatchdogConfig::default());

        wd.report_channel_health(false, Some("telegram api down".to_string()));
        let report = wd.check_health();
        assert_eq!(
            report.channel_status,
            SubsystemStatus::Unhealthy("telegram api down".to_string())
        );

        wd.report_channel_health(true, None);
        let report = wd.check_health();
        assert!(report.channel_status.is_healthy());
    }

    #[test]
    fn failures_reset_when_all_healthy() {
        let config = WatchdogConfig {
            max_consecutive_failures: 10,
            ..Default::default()
        };
        let wd = Watchdog::new(config);

        // Accumulate some failures
        wd.report_provider_health(false, Some("error".to_string()));
        let _ = wd.check_health();
        let _ = wd.check_health();
        let _ = wd.check_health();

        assert_eq!(wd.consecutive_failures.load(Ordering::SeqCst), 3);

        // Recover — next check_health should reset counter
        wd.report_provider_health(true, None);
        let _ = wd.check_health();

        assert_eq!(wd.consecutive_failures.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn unhealthy_without_detail_uses_default_message() {
        let wd = Watchdog::new(WatchdogConfig::default());

        wd.report_channel_health(false, None);
        let report = wd.check_health();
        assert_eq!(
            report.channel_status,
            SubsystemStatus::Unhealthy("unknown error".to_string())
        );
    }

    #[test]
    fn default_config_values() {
        let config = WatchdogConfig::default();
        assert_eq!(config.check_interval, Duration::from_secs(60));
        assert_eq!(config.activity_timeout, Duration::from_secs(30 * 60));
        assert_eq!(config.max_consecutive_failures, 5);
    }
}
