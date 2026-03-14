//! Proactive Task Initiation — monitors triggers (file changes, cron schedules,
//! webhook events, metric thresholds) and evaluates whether action is needed.
//!
//! Users must opt-in to proactive behavior; sovereignty requires consent.
//! The global `enabled` flag defaults to `false`, and rate limits plus per-rule
//! cooldowns prevent runaway automation.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Mutex;
use std::time::Duration;
use temm1e_core::types::error::Temm1eError;
use tracing::{debug, info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Trigger types
// ---------------------------------------------------------------------------

/// The kind of filesystem event that occurred.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum FileEvent {
    Created,
    Modified,
    Deleted,
}

/// Condition for threshold-based triggers.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum ThresholdCondition {
    Above,
    Below,
    Equals,
}

/// A trigger that the proactive system can react to.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum Trigger {
    /// A file was created, modified, or deleted.
    FileChanged {
        path: PathBuf,
        event_type: FileEvent,
    },
    /// A cron schedule fired.
    CronSchedule { expression: String, name: String },
    /// An external webhook was received.
    Webhook {
        endpoint: String,
        payload: serde_json::Value,
    },
    /// A metric crossed a threshold.
    Threshold {
        metric: String,
        value: f64,
        condition: ThresholdCondition,
    },
}

// ---------------------------------------------------------------------------
// Rule & action structs
// ---------------------------------------------------------------------------

/// A rule that maps a trigger pattern to an agent action.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TriggerRule {
    /// Unique identifier (UUID).
    pub id: String,
    /// Human-readable name for the rule.
    pub name: String,
    /// The trigger pattern this rule matches against.
    pub trigger: Trigger,
    /// What the agent should do when the trigger fires.
    pub action_prompt: String,
    /// Whether the rule is active.
    pub enabled: bool,
    /// If `true`, the agent asks the user before acting.
    pub requires_confirmation: bool,
    /// Minimum time between activations of this rule.
    #[serde(with = "duration_serde")]
    pub cooldown: Duration,
    /// When this rule last fired (if ever).
    pub last_triggered: Option<DateTime<Utc>>,
    /// When this rule was created.
    pub created_at: DateTime<Utc>,
}

/// An action the proactive system decided to take.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProactiveAction {
    /// ID of the rule that produced this action.
    pub rule_id: String,
    /// Human-readable name of the rule.
    pub rule_name: String,
    /// The trigger that fired.
    pub trigger: Trigger,
    /// The prompt to send to the agent.
    pub action_prompt: String,
    /// Whether the user must confirm before execution.
    pub requires_confirmation: bool,
    /// When the action was created.
    pub timestamp: DateTime<Utc>,
}

// ---------------------------------------------------------------------------
// Duration serde helper (seconds as u64)
// ---------------------------------------------------------------------------

mod duration_serde {
    use serde::{Deserialize, Deserializer, Serialize, Serializer};
    use std::time::Duration;

    #[derive(Serialize, Deserialize)]
    struct DurationRepr {
        secs: u64,
    }

    pub fn serialize<S: Serializer>(d: &Duration, s: S) -> Result<S::Ok, S::Error> {
        DurationRepr { secs: d.as_secs() }.serialize(s)
    }

    pub fn deserialize<'de, D: Deserializer<'de>>(d: D) -> Result<Duration, D::Error> {
        let repr = DurationRepr::deserialize(d)?;
        Ok(Duration::from_secs(repr.secs))
    }
}

// ---------------------------------------------------------------------------
// ProactiveManager
// ---------------------------------------------------------------------------

/// Manages proactive trigger rules, rate limits, and action evaluation.
///
/// The manager is disabled by default — users must explicitly call [`enable`]
/// to opt-in. Rate limiting caps the total actions per hour, and per-rule
/// cooldowns prevent any single rule from firing too frequently.
pub struct ProactiveManager {
    rules: Vec<TriggerRule>,
    /// Global kill switch — defaults to `false`.
    enabled: bool,
    /// Maximum actions allowed per hour.
    max_actions_per_hour: usize,
    /// Counter of actions taken in the current hour window.
    actions_this_hour: AtomicUsize,
    /// Start of the current hour window.
    hour_start: Mutex<DateTime<Utc>>,
}

impl ProactiveManager {
    /// Create a new `ProactiveManager` with proactive behavior **disabled**.
    pub fn new() -> Self {
        info!("ProactiveManager created (disabled by default)");
        Self {
            rules: Vec::new(),
            enabled: false,
            max_actions_per_hour: 10,
            actions_this_hour: AtomicUsize::new(0),
            hour_start: Mutex::new(Utc::now()),
        }
    }

    /// Enable proactive behavior (user opt-in).
    pub fn enable(&mut self) {
        self.enabled = true;
        info!("ProactiveManager enabled — proactive triggers are now active");
    }

    /// Disable proactive behavior (global kill switch).
    pub fn disable(&mut self) {
        self.enabled = false;
        info!("ProactiveManager disabled — all proactive triggers paused");
    }

    /// Returns `true` if the manager is globally enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }

    /// Set the maximum number of actions allowed per hour.
    pub fn set_max_actions_per_hour(&mut self, max: usize) {
        self.max_actions_per_hour = max;
    }

    /// Add a trigger rule. Returns the rule ID on success.
    pub fn add_rule(&mut self, rule: TriggerRule) -> Result<String, Temm1eError> {
        if rule.name.is_empty() {
            return Err(Temm1eError::Config(
                "Trigger rule name must not be empty".to_string(),
            ));
        }
        if rule.action_prompt.is_empty() {
            return Err(Temm1eError::Config(
                "Trigger rule action_prompt must not be empty".to_string(),
            ));
        }
        let id = rule.id.clone();
        info!(rule_id = %id, rule_name = %rule.name, "Added proactive trigger rule");
        self.rules.push(rule);
        Ok(id)
    }

    /// Remove a trigger rule by ID.
    pub fn remove_rule(&mut self, rule_id: &str) -> Result<(), Temm1eError> {
        let before = self.rules.len();
        self.rules.retain(|r| r.id != rule_id);
        if self.rules.len() == before {
            return Err(Temm1eError::NotFound(format!(
                "Trigger rule not found: {rule_id}"
            )));
        }
        info!(rule_id = %rule_id, "Removed proactive trigger rule");
        Ok(())
    }

    /// List all registered trigger rules.
    pub fn list_rules(&self) -> &[TriggerRule] {
        &self.rules
    }

    /// Evaluate an incoming trigger against all rules and return actions to take.
    ///
    /// Respects the global enable flag, per-rule enable/cooldown, and the
    /// hourly rate limit.
    pub fn evaluate_trigger(&mut self, trigger: &Trigger) -> Vec<ProactiveAction> {
        if !self.enabled {
            debug!("ProactiveManager is disabled — ignoring trigger");
            return Vec::new();
        }

        let now = Utc::now();
        self.maybe_reset_hour_window(now);

        let mut actions = Vec::new();

        for rule in &mut self.rules {
            if !rule.enabled {
                debug!(rule_id = %rule.id, "Rule disabled — skipping");
                continue;
            }

            if !trigger_matches(trigger, &rule.trigger) {
                continue;
            }

            // Check per-rule cooldown
            if let Some(last) = rule.last_triggered {
                let elapsed = now.signed_duration_since(last);
                let cooldown_chrono =
                    chrono::Duration::from_std(rule.cooldown).unwrap_or(chrono::Duration::zero());
                if elapsed < cooldown_chrono {
                    info!(
                        rule_id = %rule.id,
                        rule_name = %rule.name,
                        cooldown_remaining_secs = (cooldown_chrono - elapsed).num_seconds(),
                        "Rule in cooldown — skipping"
                    );
                    continue;
                }
            }

            // Check rate limit
            let current_actions = self.actions_this_hour.load(Ordering::SeqCst);
            if current_actions >= self.max_actions_per_hour {
                warn!(
                    rule_id = %rule.id,
                    actions_this_hour = current_actions,
                    max = self.max_actions_per_hour,
                    "Hourly rate limit reached — skipping action"
                );
                continue;
            }

            info!(
                rule_id = %rule.id,
                rule_name = %rule.name,
                requires_confirmation = rule.requires_confirmation,
                "Trigger matched rule — generating action"
            );

            rule.last_triggered = Some(now);

            actions.push(ProactiveAction {
                rule_id: rule.id.clone(),
                rule_name: rule.name.clone(),
                trigger: trigger.clone(),
                action_prompt: rule.action_prompt.clone(),
                requires_confirmation: rule.requires_confirmation,
                timestamp: now,
            });
        }

        actions
    }

    /// Check whether the manager can currently take an action (enabled + under rate limit).
    pub fn can_act(&self) -> bool {
        if !self.enabled {
            return false;
        }

        // Check if the hour window needs resetting
        {
            let hour_start = self.hour_start.lock().unwrap();
            let elapsed = Utc::now().signed_duration_since(*hour_start);
            if elapsed >= chrono::Duration::hours(1) {
                // Window is stale, but we don't reset here (read-only check).
                // The counter will be reset on the next evaluate_trigger call.
                return true;
            }
        }

        let current = self.actions_this_hour.load(Ordering::SeqCst);
        current < self.max_actions_per_hour
    }

    /// Record that an action was taken (increments the hourly counter).
    pub fn record_action(&self) {
        let prev = self.actions_this_hour.fetch_add(1, Ordering::SeqCst);
        debug!(actions_this_hour = prev + 1, "Recorded proactive action");
    }

    /// Format a proactive action as a message the agent can process.
    pub fn format_action_request(action: &ProactiveAction) -> String {
        format!(
            "[Proactive] Rule \"{}\" triggered.\n\nAction: {}",
            action.rule_name, action.action_prompt
        )
    }

    /// Format a confirmation request message for the user.
    pub fn format_confirmation_request(action: &ProactiveAction) -> String {
        format!(
            "[Proactive] Rule \"{}\" wants to act.\n\n\
             Proposed action: {}\n\n\
             Reply /confirm to proceed or /deny to skip.",
            action.rule_name, action.action_prompt
        )
    }

    // -----------------------------------------------------------------------
    // Internal helpers
    // -----------------------------------------------------------------------

    /// Reset the hourly window if more than an hour has elapsed.
    fn maybe_reset_hour_window(&self, now: DateTime<Utc>) {
        let mut hour_start = self.hour_start.lock().unwrap();
        let elapsed = now.signed_duration_since(*hour_start);
        if elapsed >= chrono::Duration::hours(1) {
            debug!("Resetting hourly action counter");
            *hour_start = now;
            self.actions_this_hour.store(0, Ordering::SeqCst);
        }
    }
}

impl Default for ProactiveManager {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Trigger matching
// ---------------------------------------------------------------------------

/// Determine whether an incoming trigger matches a rule's trigger pattern.
fn trigger_matches(incoming: &Trigger, pattern: &Trigger) -> bool {
    match (incoming, pattern) {
        (
            Trigger::FileChanged {
                path: i_path,
                event_type: i_event,
            },
            Trigger::FileChanged {
                path: p_path,
                event_type: p_event,
            },
        ) => i_path == p_path && i_event == p_event,

        (
            Trigger::CronSchedule {
                expression: _,
                name: i_name,
            },
            Trigger::CronSchedule {
                expression: _,
                name: p_name,
            },
        ) => i_name == p_name,

        (Trigger::Webhook { endpoint: i_ep, .. }, Trigger::Webhook { endpoint: p_ep, .. }) => {
            i_ep == p_ep
        }

        (
            Trigger::Threshold {
                metric: i_metric,
                value: i_val,
                condition,
            },
            Trigger::Threshold {
                metric: p_metric, ..
            },
        ) => {
            if i_metric != p_metric {
                return false;
            }
            match condition {
                ThresholdCondition::Above => *i_val > 0.0, // threshold is in the pattern's value
                ThresholdCondition::Below => *i_val < f64::MAX,
                ThresholdCondition::Equals => true,
            }
        }

        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Helper to build rules with less boilerplate
// ---------------------------------------------------------------------------

/// Create a new `TriggerRule` with sensible defaults and a generated UUID.
pub fn new_rule(
    name: impl Into<String>,
    trigger: Trigger,
    action_prompt: impl Into<String>,
) -> TriggerRule {
    TriggerRule {
        id: Uuid::new_v4().to_string(),
        name: name.into(),
        trigger,
        action_prompt: action_prompt.into(),
        enabled: true,
        requires_confirmation: false,
        cooldown: Duration::from_secs(60),
        last_triggered: None,
        created_at: Utc::now(),
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a file-changed trigger for a given path.
    fn file_trigger(path: &str, event: FileEvent) -> Trigger {
        Trigger::FileChanged {
            path: PathBuf::from(path),
            event_type: event,
        }
    }

    /// Helper: build a cron trigger.
    fn cron_trigger(name: &str) -> Trigger {
        Trigger::CronSchedule {
            expression: "0 * * * *".to_string(),
            name: name.to_string(),
        }
    }

    /// Helper: build a webhook trigger.
    fn webhook_trigger(endpoint: &str) -> Trigger {
        Trigger::Webhook {
            endpoint: endpoint.to_string(),
            payload: serde_json::json!({"key": "value"}),
        }
    }

    /// Helper: build a threshold trigger.
    fn threshold_trigger(metric: &str, value: f64, cond: ThresholdCondition) -> Trigger {
        Trigger::Threshold {
            metric: metric.to_string(),
            value,
            condition: cond,
        }
    }

    /// Helper: build a rule with the given trigger.
    fn make_rule(name: &str, trigger: Trigger) -> TriggerRule {
        new_rule(name, trigger, format!("Handle {name}"))
    }

    // 1. ProactiveManager is disabled by default
    #[test]
    fn manager_disabled_by_default() {
        let mgr = ProactiveManager::new();
        assert!(!mgr.is_enabled());
        assert!(!mgr.can_act());
    }

    // 2. Enable and disable global toggle
    #[test]
    fn enable_disable_toggle() {
        let mut mgr = ProactiveManager::new();
        mgr.enable();
        assert!(mgr.is_enabled());
        assert!(mgr.can_act());

        mgr.disable();
        assert!(!mgr.is_enabled());
        assert!(!mgr.can_act());
    }

    // 3. Add rule
    #[test]
    fn add_rule_success() {
        let mut mgr = ProactiveManager::new();
        let rule = make_rule(
            "deploy-watcher",
            file_trigger("/app/deploy", FileEvent::Modified),
        );
        let id = mgr.add_rule(rule).unwrap();
        assert!(!id.is_empty());
        assert_eq!(mgr.list_rules().len(), 1);
    }

    // 4. Add rule with empty name fails
    #[test]
    fn add_rule_empty_name_fails() {
        let mut mgr = ProactiveManager::new();
        let mut rule = make_rule("temp", file_trigger("/tmp/f", FileEvent::Created));
        rule.name = String::new();
        let result = mgr.add_rule(rule);
        assert!(result.is_err());
    }

    // 5. Add rule with empty action_prompt fails
    #[test]
    fn add_rule_empty_action_prompt_fails() {
        let mut mgr = ProactiveManager::new();
        let mut rule = make_rule("temp", file_trigger("/tmp/f", FileEvent::Created));
        rule.action_prompt = String::new();
        let result = mgr.add_rule(rule);
        assert!(result.is_err());
    }

    // 6. Remove rule
    #[test]
    fn remove_rule_success() {
        let mut mgr = ProactiveManager::new();
        let rule = make_rule("r1", file_trigger("/a", FileEvent::Created));
        let id = mgr.add_rule(rule).unwrap();
        assert_eq!(mgr.list_rules().len(), 1);

        mgr.remove_rule(&id).unwrap();
        assert!(mgr.list_rules().is_empty());
    }

    // 7. Remove non-existent rule fails
    #[test]
    fn remove_nonexistent_rule_fails() {
        let mut mgr = ProactiveManager::new();
        let result = mgr.remove_rule("does-not-exist");
        assert!(result.is_err());
    }

    // 8. List rules returns all registered rules
    #[test]
    fn list_rules_returns_all() {
        let mut mgr = ProactiveManager::new();
        mgr.add_rule(make_rule("r1", cron_trigger("hourly-check")))
            .unwrap();
        mgr.add_rule(make_rule("r2", webhook_trigger("/hooks/deploy")))
            .unwrap();
        mgr.add_rule(make_rule(
            "r3",
            file_trigger("/etc/conf", FileEvent::Modified),
        ))
        .unwrap();

        assert_eq!(mgr.list_rules().len(), 3);
    }

    // 9. Trigger evaluation matches correct rules
    #[test]
    fn evaluate_trigger_matching() {
        let mut mgr = ProactiveManager::new();
        mgr.enable();

        let trigger = file_trigger("/app/config.toml", FileEvent::Modified);
        mgr.add_rule(make_rule("config-watcher", trigger.clone()))
            .unwrap();
        mgr.add_rule(make_rule(
            "other-watcher",
            file_trigger("/other/path", FileEvent::Modified),
        ))
        .unwrap();

        let actions = mgr.evaluate_trigger(&trigger);
        assert_eq!(actions.len(), 1);
        assert_eq!(actions[0].rule_name, "config-watcher");
    }

    // 10. Trigger evaluation returns nothing when disabled
    #[test]
    fn evaluate_trigger_when_disabled() {
        let mut mgr = ProactiveManager::new();
        // Manager is disabled by default
        let trigger = file_trigger("/app/config.toml", FileEvent::Modified);
        mgr.add_rule(make_rule("config-watcher", trigger.clone()))
            .unwrap();

        let actions = mgr.evaluate_trigger(&trigger);
        assert!(actions.is_empty());
    }

    // 11. Rate limiting enforced
    #[test]
    fn rate_limiting() {
        let mut mgr = ProactiveManager::new();
        mgr.enable();
        mgr.set_max_actions_per_hour(2);

        let trigger = cron_trigger("frequent-job");
        let mut rule = make_rule("job-runner", trigger.clone());
        rule.cooldown = Duration::from_secs(0); // no cooldown for this test
        mgr.add_rule(rule).unwrap();

        // First two should succeed
        let a1 = mgr.evaluate_trigger(&trigger);
        assert_eq!(a1.len(), 1);
        mgr.record_action();

        let a2 = mgr.evaluate_trigger(&trigger);
        assert_eq!(a2.len(), 1);
        mgr.record_action();

        // Third should be rate-limited
        let a3 = mgr.evaluate_trigger(&trigger);
        assert!(a3.is_empty());

        // can_act should also report false
        assert!(!mgr.can_act());
    }

    // 12. Per-rule cooldown enforced
    #[test]
    fn cooldown_enforcement() {
        let mut mgr = ProactiveManager::new();
        mgr.enable();

        let trigger = webhook_trigger("/hooks/deploy");
        let mut rule = make_rule("deploy-hook", trigger.clone());
        rule.cooldown = Duration::from_secs(3600); // 1 hour cooldown
        mgr.add_rule(rule).unwrap();

        // First evaluation should match
        let a1 = mgr.evaluate_trigger(&trigger);
        assert_eq!(a1.len(), 1);

        // Second evaluation should be in cooldown
        let a2 = mgr.evaluate_trigger(&trigger);
        assert!(a2.is_empty());
    }

    // 13. Disabled rules are skipped
    #[test]
    fn disabled_rule_skipped() {
        let mut mgr = ProactiveManager::new();
        mgr.enable();

        let trigger = file_trigger("/data/input.csv", FileEvent::Created);
        let mut rule = make_rule("data-processor", trigger.clone());
        rule.enabled = false;
        mgr.add_rule(rule).unwrap();

        let actions = mgr.evaluate_trigger(&trigger);
        assert!(actions.is_empty());
    }

    // 14. Format action request
    #[test]
    fn format_action_request_output() {
        let action = ProactiveAction {
            rule_id: "id-1".to_string(),
            rule_name: "config-watcher".to_string(),
            trigger: file_trigger("/app/config.toml", FileEvent::Modified),
            action_prompt: "Reload the application configuration".to_string(),
            requires_confirmation: false,
            timestamp: Utc::now(),
        };

        let msg = ProactiveManager::format_action_request(&action);
        assert!(msg.contains("config-watcher"));
        assert!(msg.contains("Reload the application configuration"));
        assert!(msg.starts_with("[Proactive]"));
    }

    // 15. Format confirmation request
    #[test]
    fn format_confirmation_request_output() {
        let action = ProactiveAction {
            rule_id: "id-2".to_string(),
            rule_name: "deploy-hook".to_string(),
            trigger: webhook_trigger("/hooks/deploy"),
            action_prompt: "Deploy latest build to staging".to_string(),
            requires_confirmation: true,
            timestamp: Utc::now(),
        };

        let msg = ProactiveManager::format_confirmation_request(&action);
        assert!(msg.contains("deploy-hook"));
        assert!(msg.contains("Deploy latest build to staging"));
        assert!(msg.contains("/confirm"));
        assert!(msg.contains("/deny"));
    }

    // 16. Multiple rules matching same trigger
    #[test]
    fn multiple_rules_match_same_trigger() {
        let mut mgr = ProactiveManager::new();
        mgr.enable();

        let trigger = file_trigger("/app/deploy.yaml", FileEvent::Modified);

        let mut r1 = make_rule("notify-slack", trigger.clone());
        r1.cooldown = Duration::from_secs(0);
        mgr.add_rule(r1).unwrap();

        let mut r2 = make_rule("run-tests", trigger.clone());
        r2.cooldown = Duration::from_secs(0);
        mgr.add_rule(r2).unwrap();

        let actions = mgr.evaluate_trigger(&trigger);
        assert_eq!(actions.len(), 2);

        let names: Vec<&str> = actions.iter().map(|a| a.rule_name.as_str()).collect();
        assert!(names.contains(&"notify-slack"));
        assert!(names.contains(&"run-tests"));
    }

    // 17. Non-matching trigger returns no actions
    #[test]
    fn non_matching_trigger() {
        let mut mgr = ProactiveManager::new();
        mgr.enable();

        mgr.add_rule(make_rule(
            "file-watcher",
            file_trigger("/watched/file.txt", FileEvent::Modified),
        ))
        .unwrap();

        // Different path
        let trigger = file_trigger("/other/file.txt", FileEvent::Modified);
        let actions = mgr.evaluate_trigger(&trigger);
        assert!(actions.is_empty());

        // Different event type
        let trigger = file_trigger("/watched/file.txt", FileEvent::Deleted);
        let actions = mgr.evaluate_trigger(&trigger);
        assert!(actions.is_empty());
    }

    // 18. Cross-type triggers don't match
    #[test]
    fn cross_type_triggers_dont_match() {
        let mut mgr = ProactiveManager::new();
        mgr.enable();

        mgr.add_rule(make_rule("cron-job", cron_trigger("daily-backup")))
            .unwrap();

        // A webhook trigger should not match a cron rule
        let trigger = webhook_trigger("daily-backup");
        let actions = mgr.evaluate_trigger(&trigger);
        assert!(actions.is_empty());
    }

    // 19. record_action increments counter
    #[test]
    fn record_action_increments() {
        let mgr = ProactiveManager::new();
        assert_eq!(mgr.actions_this_hour.load(Ordering::SeqCst), 0);

        mgr.record_action();
        assert_eq!(mgr.actions_this_hour.load(Ordering::SeqCst), 1);

        mgr.record_action();
        assert_eq!(mgr.actions_this_hour.load(Ordering::SeqCst), 2);
    }

    // 20. ProactiveAction captures requires_confirmation from rule
    #[test]
    fn action_inherits_requires_confirmation() {
        let mut mgr = ProactiveManager::new();
        mgr.enable();

        let trigger = file_trigger("/sensitive/data.db", FileEvent::Deleted);
        let mut rule = make_rule("dangerous-delete", trigger.clone());
        rule.requires_confirmation = true;
        rule.cooldown = Duration::from_secs(0);
        mgr.add_rule(rule).unwrap();

        let actions = mgr.evaluate_trigger(&trigger);
        assert_eq!(actions.len(), 1);
        assert!(actions[0].requires_confirmation);
    }

    // 21. Threshold trigger matching
    #[test]
    fn threshold_trigger_matching() {
        let mut mgr = ProactiveManager::new();
        mgr.enable();

        let pattern = threshold_trigger("cpu_usage", 90.0, ThresholdCondition::Above);
        let mut rule = make_rule("cpu-alert", pattern);
        rule.cooldown = Duration::from_secs(0);
        mgr.add_rule(rule).unwrap();

        // Same metric should match
        let incoming = threshold_trigger("cpu_usage", 95.0, ThresholdCondition::Above);
        let actions = mgr.evaluate_trigger(&incoming);
        assert_eq!(actions.len(), 1);

        // Different metric should not match
        let incoming = threshold_trigger("memory_usage", 95.0, ThresholdCondition::Above);
        let actions = mgr.evaluate_trigger(&incoming);
        assert!(actions.is_empty());
    }

    // 22. Serialization roundtrip for TriggerRule
    #[test]
    fn trigger_rule_serde_roundtrip() {
        let rule = make_rule(
            "serde-test",
            file_trigger("/tmp/test.txt", FileEvent::Created),
        );
        let json = serde_json::to_string(&rule).unwrap();
        let deserialized: TriggerRule = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.name, "serde-test");
        assert_eq!(deserialized.id, rule.id);
    }
}
