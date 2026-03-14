//! HeartbeatRunner — periodically reads a task checklist and sends
//! synthetic messages to the agent for processing.
//!
//! Protocol (OpenClaw-compatible):
//!   1. Every `interval`, read `HEARTBEAT.md` from the workspace.
//!   2. If the file is missing or empty, skip (no work).
//!   3. If `HEARTBEAT_OK` exists in the workspace, delete it and skip
//!      that cycle (agent signalled "nothing to do, save tokens").
//!   4. Otherwise, send the checklist content as a synthetic inbound
//!      message through the unified message channel.
//!   5. If the channel is full (previous heartbeat still processing),
//!      skip — never pile up heartbeat ticks.
//!
//! Health-Aware (Phase 2.3):
//!   - Before each tick, run an optional health check callback.
//!   - If health is degraded, include a health summary in the message
//!     so the agent can self-diagnose.
//!   - Track metrics: total runs, successes, failures, last run time.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use chrono::{DateTime, Timelike, Utc};
use temm1e_core::types::config::HeartbeatConfig;
use temm1e_core::types::message::InboundMessage;
use tokio::sync::{mpsc, Mutex};
use tracing::{debug, info, warn};

use crate::duration::parse_duration;

/// Health status returned by a health check callback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HealthStatus {
    /// All subsystems operational.
    Healthy,
    /// Some subsystems degraded but operational.
    Degraded(String),
    /// Critical subsystem failure — agent should prioritize recovery.
    Unhealthy(String),
}

impl std::fmt::Display for HealthStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Healthy => write!(f, "Healthy"),
            Self::Degraded(detail) => write!(f, "Degraded: {detail}"),
            Self::Unhealthy(detail) => write!(f, "UNHEALTHY: {detail}"),
        }
    }
}

/// Callback that checks system health before each heartbeat.
pub type HealthCheckFn = Arc<dyn Fn() -> HealthStatus + Send + Sync>;

/// Metrics tracked across heartbeat runs.
#[derive(Debug)]
pub struct HeartbeatMetrics {
    pub total_runs: AtomicU64,
    pub successful_sends: AtomicU64,
    pub skipped_no_checklist: AtomicU64,
    pub skipped_ok_suppressed: AtomicU64,
    pub skipped_outside_hours: AtomicU64,
    pub skipped_channel_full: AtomicU64,
    pub health_degraded_count: AtomicU64,
    pub health_unhealthy_count: AtomicU64,
    pub last_run: Mutex<Option<DateTime<Utc>>>,
    pub last_success: Mutex<Option<DateTime<Utc>>>,
}

impl Default for HeartbeatMetrics {
    fn default() -> Self {
        Self::new()
    }
}

impl HeartbeatMetrics {
    pub fn new() -> Self {
        Self {
            total_runs: AtomicU64::new(0),
            successful_sends: AtomicU64::new(0),
            skipped_no_checklist: AtomicU64::new(0),
            skipped_ok_suppressed: AtomicU64::new(0),
            skipped_outside_hours: AtomicU64::new(0),
            skipped_channel_full: AtomicU64::new(0),
            health_degraded_count: AtomicU64::new(0),
            health_unhealthy_count: AtomicU64::new(0),
            last_run: Mutex::new(None),
            last_success: Mutex::new(None),
        }
    }

    /// Format a compact summary for logging or diagnostic display.
    pub async fn summary(&self) -> String {
        let last_run = self.last_run.lock().await;
        let last_success = self.last_success.lock().await;
        format!(
            "runs={}, sent={}, skipped(no_checklist={}, ok={}, hours={}, full={}), \
             health(degraded={}, unhealthy={}), last_run={}, last_success={}",
            self.total_runs.load(Ordering::Relaxed),
            self.successful_sends.load(Ordering::Relaxed),
            self.skipped_no_checklist.load(Ordering::Relaxed),
            self.skipped_ok_suppressed.load(Ordering::Relaxed),
            self.skipped_outside_hours.load(Ordering::Relaxed),
            self.skipped_channel_full.load(Ordering::Relaxed),
            self.health_degraded_count.load(Ordering::Relaxed),
            self.health_unhealthy_count.load(Ordering::Relaxed),
            last_run
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "never".to_string()),
            last_success
                .map(|t| t.to_rfc3339())
                .unwrap_or_else(|| "never".to_string()),
        )
    }
}

/// Parse an "HH:MM-HH:MM" active hours window into (start_hour, start_min, end_hour, end_min).
fn parse_active_hours(s: &str) -> Option<(u32, u32, u32, u32)> {
    let parts: Vec<&str> = s.split('-').collect();
    if parts.len() != 2 {
        return None;
    }
    let start: Vec<&str> = parts[0].trim().split(':').collect();
    let end: Vec<&str> = parts[1].trim().split(':').collect();
    if start.len() != 2 || end.len() != 2 {
        return None;
    }
    Some((
        start[0].parse().ok()?,
        start[1].parse().ok()?,
        end[0].parse().ok()?,
        end[1].parse().ok()?,
    ))
}

/// Check if the current local time is within the active hours window.
fn is_within_active_hours(active_hours: &str) -> bool {
    let (sh, sm, eh, em) = match parse_active_hours(active_hours) {
        Some(v) => v,
        None => {
            warn!(window = %active_hours, "Invalid active_hours format, ignoring");
            return true; // Invalid format → don't block
        }
    };

    let now = chrono::Local::now();
    let current = now.hour() * 60 + now.minute();
    let start = sh * 60 + sm;
    let end = eh * 60 + em;

    if start <= end {
        // Normal window: e.g. 08:00-22:00
        current >= start && current < end
    } else {
        // Overnight window: e.g. 22:00-06:00
        current >= start || current < end
    }
}

/// Heartbeat runner that produces synthetic agent messages on a timer.
pub struct HeartbeatRunner {
    config: HeartbeatConfig,
    workspace_path: PathBuf,
    /// Chat ID to attribute heartbeat messages to (for response routing).
    chat_id: String,
    /// Optional health check callback — invoked before each tick.
    health_check: Option<HealthCheckFn>,
    /// Shared metrics tracked across runs.
    metrics: Arc<HeartbeatMetrics>,
}

impl HeartbeatRunner {
    pub fn new(config: HeartbeatConfig, workspace_path: PathBuf, chat_id: String) -> Self {
        Self {
            config,
            workspace_path,
            chat_id,
            health_check: None,
            metrics: Arc::new(HeartbeatMetrics::new()),
        }
    }

    /// Attach a health check callback. Called before each heartbeat tick.
    pub fn with_health_check(mut self, check: HealthCheckFn) -> Self {
        self.health_check = Some(check);
        self
    }

    /// Get a shared reference to the metrics.
    pub fn metrics(&self) -> Arc<HeartbeatMetrics> {
        self.metrics.clone()
    }

    /// Start the heartbeat loop. Messages are sent to `tx`.
    ///
    /// Uses `try_send` — if the channel buffer is full (previous heartbeat
    /// still being processed), the tick is silently skipped.
    ///
    /// This method runs forever; spawn it as a tokio task.
    pub async fn run(self, tx: mpsc::Sender<InboundMessage>) {
        let interval = match parse_duration(&self.config.interval) {
            Ok(d) => d,
            Err(e) => {
                warn!(error = %e, interval = %self.config.interval, "Invalid heartbeat interval, defaulting to 30m");
                std::time::Duration::from_secs(30 * 60)
            }
        };

        info!(
            interval_secs = interval.as_secs(),
            checklist = %self.config.checklist,
            "Heartbeat started"
        );

        let mut timer = tokio::time::interval(interval);
        // Skip the first immediate tick — let the system warm up
        timer.tick().await;

        loop {
            timer.tick().await;

            self.metrics.total_runs.fetch_add(1, Ordering::Relaxed);
            *self.metrics.last_run.lock().await = Some(Utc::now());

            // 0. Active hours check (ZeroClaw pattern — save tokens at night)
            if let Some(ref window) = self.config.active_hours {
                if !is_within_active_hours(window) {
                    debug!(window = %window, "Outside active hours — skipping heartbeat");
                    self.metrics
                        .skipped_outside_hours
                        .fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            }

            // 1. HEARTBEAT_OK suppression
            let ok_path = self.workspace_path.join("HEARTBEAT_OK");
            if ok_path.exists() {
                match tokio::fs::remove_file(&ok_path).await {
                    Ok(()) => {
                        info!("Heartbeat suppressed by HEARTBEAT_OK — skipping cycle");
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to remove HEARTBEAT_OK");
                    }
                }
                self.metrics
                    .skipped_ok_suppressed
                    .fetch_add(1, Ordering::Relaxed);
                continue;
            }

            // 2. Read the checklist
            let checklist_path = self.workspace_path.join(&self.config.checklist);
            let checklist = match tokio::fs::read_to_string(&checklist_path).await {
                Ok(content) if !content.trim().is_empty() => content,
                Ok(_) => {
                    debug!("Heartbeat checklist is empty — skipping");
                    self.metrics
                        .skipped_no_checklist
                        .fetch_add(1, Ordering::Relaxed);
                    continue;
                }
                Err(_) => {
                    debug!(path = %checklist_path.display(), "No heartbeat checklist found — skipping");
                    self.metrics
                        .skipped_no_checklist
                        .fetch_add(1, Ordering::Relaxed);
                    continue;
                }
            };

            // 3. Health check (Phase 2.3)
            let health_section = if let Some(ref check) = self.health_check {
                let status = check();
                match &status {
                    HealthStatus::Healthy => String::new(),
                    HealthStatus::Degraded(detail) => {
                        self.metrics
                            .health_degraded_count
                            .fetch_add(1, Ordering::Relaxed);
                        warn!(detail = %detail, "Health degraded during heartbeat");
                        format!(
                            "\n\n⚠ SYSTEM HEALTH: DEGRADED\n{detail}\n\
                             Consider investigating before proceeding with tasks.\n"
                        )
                    }
                    HealthStatus::Unhealthy(detail) => {
                        self.metrics
                            .health_unhealthy_count
                            .fetch_add(1, Ordering::Relaxed);
                        tracing::error!(detail = %detail, "System unhealthy during heartbeat");
                        format!(
                            "\n\n🚨 SYSTEM HEALTH: UNHEALTHY\n{detail}\n\
                             PRIORITY: Diagnose and attempt recovery before task execution.\n\
                             Use available tools to check connectivity, disk space, and service status.\n"
                        )
                    }
                }
            } else {
                String::new()
            };

            // 4. Build metrics summary for context
            let metrics_line = format!(
                "\nHeartbeat #{} | Sent: {} | Last success: {}",
                self.metrics.total_runs.load(Ordering::Relaxed),
                self.metrics.successful_sends.load(Ordering::Relaxed),
                self.metrics
                    .last_success
                    .lock()
                    .await
                    .map(|t| t.format("%H:%M:%S UTC").to_string())
                    .unwrap_or_else(|| "never".to_string()),
            );

            // 5. Build synthetic inbound message
            let now = Utc::now();
            let msg = InboundMessage {
                id: format!("heartbeat-{}", now.timestamp()),
                channel: "heartbeat".to_string(),
                chat_id: self.chat_id.clone(),
                user_id: "system".to_string(),
                username: None,
                text: Some(format!(
                    "HEARTBEAT — You are running autonomously. \
                     Review your task checklist below and take action on any pending items. \
                     Use tools to execute tasks. When all tasks are done or you need to wait, \
                     write 'HEARTBEAT_OK' to the file HEARTBEAT_OK in your workspace to skip \
                     the next heartbeat cycle.\
                     {health_section}\n\n\
                     ---\n\n\
                     {checklist}\n\n\
                     ---\n\n\
                     Instructions:\n\
                     - Execute the next pending task (marked with `- [ ]`)\n\
                     - Mark completed tasks with `- [x]` by rewriting the checklist\n\
                     - If all tasks are done, write HEARTBEAT_OK to pause\n\
                     - If a task fails, note the error and move to the next one\n\
                     - Be concise in responses — this is autonomous execution\
                     {metrics_line}",
                )),
                attachments: Vec::new(),
                reply_to: None,
                timestamp: now,
            };

            // 6. Send — skip if channel full (previous heartbeat still processing)
            match tx.try_send(msg) {
                Ok(()) => {
                    info!("Heartbeat tick sent to agent");
                    self.metrics
                        .successful_sends
                        .fetch_add(1, Ordering::Relaxed);
                    *self.metrics.last_success.lock().await = Some(Utc::now());
                }
                Err(mpsc::error::TrySendError::Full(_)) => {
                    debug!("Heartbeat skipped — agent still processing previous tick");
                    self.metrics
                        .skipped_channel_full
                        .fetch_add(1, Ordering::Relaxed);
                }
                Err(mpsc::error::TrySendError::Closed(_)) => {
                    warn!("Heartbeat channel closed — stopping");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config() -> HeartbeatConfig {
        HeartbeatConfig {
            enabled: true,
            interval: "1s".to_string(),
            checklist: "HEARTBEAT.md".to_string(),
            report_to: None,
            active_hours: None,
        }
    }

    #[test]
    fn parse_active_hours_normal() {
        assert_eq!(parse_active_hours("08:00-22:00"), Some((8, 0, 22, 0)));
        assert_eq!(parse_active_hours("09:30-17:45"), Some((9, 30, 17, 45)));
    }

    #[test]
    fn parse_active_hours_overnight() {
        assert_eq!(parse_active_hours("22:00-06:00"), Some((22, 0, 6, 0)));
    }

    #[test]
    fn parse_active_hours_invalid() {
        assert_eq!(parse_active_hours("invalid"), None);
        assert_eq!(parse_active_hours("08:00"), None);
        assert_eq!(parse_active_hours(""), None);
    }

    #[test]
    fn active_hours_always_true_for_bad_format() {
        // Invalid format should not block heartbeats
        assert!(is_within_active_hours("garbage"));
    }

    #[tokio::test]
    async fn heartbeat_skips_when_no_checklist() {
        let dir = tempfile::tempdir().unwrap();
        let runner = HeartbeatRunner::new(
            test_config(),
            dir.path().to_path_buf(),
            "test-chat".to_string(),
        );

        let (tx, mut rx) = mpsc::channel(1);

        // Run heartbeat in background, give it 2 seconds
        let handle = tokio::spawn(runner.run(tx));

        // Wait a bit — should NOT receive anything (no checklist file)
        let result = tokio::time::timeout(std::time::Duration::from_millis(2500), rx.recv()).await;

        assert!(
            result.is_err(),
            "Should timeout — no messages without checklist"
        );
        handle.abort();
    }

    #[tokio::test]
    async fn heartbeat_sends_when_checklist_exists() {
        let dir = tempfile::tempdir().unwrap();
        let checklist_path = dir.path().join("HEARTBEAT.md");
        std::fs::write(
            &checklist_path,
            "- [ ] Do something\n- [ ] Do another thing",
        )
        .unwrap();

        let runner = HeartbeatRunner::new(
            test_config(),
            dir.path().to_path_buf(),
            "test-chat".to_string(),
        );

        let (tx, mut rx) = mpsc::channel(2);
        let handle = tokio::spawn(runner.run(tx));

        // Should receive a heartbeat message
        let msg = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("Should receive heartbeat")
            .expect("Channel should not close");

        assert_eq!(msg.channel, "heartbeat");
        assert!(msg.text.unwrap().contains("Do something"));
        handle.abort();
    }

    #[tokio::test]
    async fn heartbeat_ok_suppression() {
        let dir = tempfile::tempdir().unwrap();
        let checklist_path = dir.path().join("HEARTBEAT.md");
        std::fs::write(&checklist_path, "- [ ] Task").unwrap();

        // Write HEARTBEAT_OK to suppress
        let ok_path = dir.path().join("HEARTBEAT_OK");
        std::fs::write(&ok_path, "ok").unwrap();

        let runner = HeartbeatRunner::new(
            test_config(),
            dir.path().to_path_buf(),
            "test-chat".to_string(),
        );

        let (tx, mut rx) = mpsc::channel(2);
        let handle = tokio::spawn(runner.run(tx));

        // First tick should be suppressed, HEARTBEAT_OK should be deleted
        tokio::time::sleep(std::time::Duration::from_millis(1500)).await;
        assert!(!ok_path.exists(), "HEARTBEAT_OK should be deleted");

        // Second tick should send (HEARTBEAT_OK is gone)
        let msg = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("Should receive heartbeat after HEARTBEAT_OK cleared")
            .expect("Channel should not close");

        assert_eq!(msg.channel, "heartbeat");
        handle.abort();
    }

    // ── Health-Aware Heartbeat tests (Phase 2.3) ──────────────────────

    #[test]
    fn health_status_display() {
        assert_eq!(format!("{}", HealthStatus::Healthy), "Healthy");
        assert_eq!(
            format!("{}", HealthStatus::Degraded("slow db".to_string())),
            "Degraded: slow db"
        );
        assert_eq!(
            format!("{}", HealthStatus::Unhealthy("db down".to_string())),
            "UNHEALTHY: db down"
        );
    }

    #[tokio::test]
    async fn heartbeat_metrics_default() {
        let metrics = HeartbeatMetrics::new();
        assert_eq!(metrics.total_runs.load(Ordering::Relaxed), 0);
        assert_eq!(metrics.successful_sends.load(Ordering::Relaxed), 0);
        assert!(metrics.last_run.lock().await.is_none());
        assert!(metrics.last_success.lock().await.is_none());
    }

    #[tokio::test]
    async fn heartbeat_metrics_summary() {
        let metrics = HeartbeatMetrics::new();
        metrics.total_runs.store(5, Ordering::Relaxed);
        metrics.successful_sends.store(3, Ordering::Relaxed);
        metrics.skipped_no_checklist.store(2, Ordering::Relaxed);
        let summary = metrics.summary().await;
        assert!(summary.contains("runs=5"));
        assert!(summary.contains("sent=3"));
        assert!(summary.contains("no_checklist=2"));
    }

    #[tokio::test]
    async fn heartbeat_with_healthy_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("HEARTBEAT.md"), "- [ ] Task").unwrap();

        let runner = HeartbeatRunner::new(
            test_config(),
            dir.path().to_path_buf(),
            "test-chat".to_string(),
        )
        .with_health_check(Arc::new(|| HealthStatus::Healthy));

        let metrics = runner.metrics();
        let (tx, mut rx) = mpsc::channel(2);
        let handle = tokio::spawn(runner.run(tx));

        let msg = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("Should receive heartbeat")
            .expect("Channel should not close");

        // Healthy status should NOT add health section
        let text = msg.text.unwrap();
        assert!(!text.contains("SYSTEM HEALTH"));
        assert!(text.contains("Task"));
        assert_eq!(metrics.health_degraded_count.load(Ordering::Relaxed), 0);
        handle.abort();
    }

    #[tokio::test]
    async fn heartbeat_with_degraded_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("HEARTBEAT.md"), "- [ ] Task").unwrap();

        let runner = HeartbeatRunner::new(
            test_config(),
            dir.path().to_path_buf(),
            "test-chat".to_string(),
        )
        .with_health_check(Arc::new(|| {
            HealthStatus::Degraded("Provider latency high".to_string())
        }));

        let metrics = runner.metrics();
        let (tx, mut rx) = mpsc::channel(2);
        let handle = tokio::spawn(runner.run(tx));

        let msg = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("Should receive heartbeat")
            .expect("Channel should not close");

        let text = msg.text.unwrap();
        assert!(text.contains("DEGRADED"));
        assert!(text.contains("Provider latency high"));
        assert_eq!(metrics.health_degraded_count.load(Ordering::Relaxed), 1);
        handle.abort();
    }

    #[tokio::test]
    async fn heartbeat_with_unhealthy_check() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("HEARTBEAT.md"), "- [ ] Task").unwrap();

        let runner = HeartbeatRunner::new(
            test_config(),
            dir.path().to_path_buf(),
            "test-chat".to_string(),
        )
        .with_health_check(Arc::new(|| {
            HealthStatus::Unhealthy("Memory backend unreachable".to_string())
        }));

        let metrics = runner.metrics();
        let (tx, mut rx) = mpsc::channel(2);
        let handle = tokio::spawn(runner.run(tx));

        let msg = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .expect("Should receive heartbeat")
            .expect("Channel should not close");

        let text = msg.text.unwrap();
        assert!(text.contains("UNHEALTHY"));
        assert!(text.contains("Memory backend unreachable"));
        assert!(text.contains("recovery"));
        assert_eq!(metrics.health_unhealthy_count.load(Ordering::Relaxed), 1);
        handle.abort();
    }

    #[tokio::test]
    async fn heartbeat_tracks_metrics() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("HEARTBEAT.md"), "- [ ] Task").unwrap();

        let runner = HeartbeatRunner::new(
            test_config(),
            dir.path().to_path_buf(),
            "test-chat".to_string(),
        );

        let metrics = runner.metrics();
        let (tx, mut rx) = mpsc::channel(4);
        let handle = tokio::spawn(runner.run(tx));

        // Receive first heartbeat
        let _ = tokio::time::timeout(std::time::Duration::from_secs(3), rx.recv())
            .await
            .unwrap()
            .unwrap();

        assert!(metrics.total_runs.load(Ordering::Relaxed) >= 1);
        assert!(metrics.successful_sends.load(Ordering::Relaxed) >= 1);
        assert!(metrics.last_run.lock().await.is_some());
        assert!(metrics.last_success.lock().await.is_some());
        handle.abort();
    }
}
