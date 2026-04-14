//! Trust state machine.
//!
//! Tracks earned trust from a track record of successful cambium sessions.
//! Trust is graduated per-level: Level 3 (basic autonomous) graduates first
//! after 10 consecutive successes, Level 2 (full autonomous) after 25.
//! Failures reset streaks and accumulate rollbacks; 3+ rollbacks force all
//! levels back to approval-required.

use chrono::{DateTime, Utc};
use temm1e_core::types::cambium::{TrustLevel, TrustState};

/// The trust engine manages the `TrustState` and enforces trust transitions,
/// cooldowns, and daily limits.
pub struct TrustEngine {
    state: TrustState,
    /// Optional config override. If set to `"approval_required"`, all
    /// `is_autonomous` checks return false regardless of earned trust.
    config_override: Option<String>,
}

impl TrustEngine {
    /// Create a new trust engine with the given initial state and optional override.
    pub fn new(state: TrustState, config_override: Option<String>) -> Self {
        Self {
            state,
            config_override,
        }
    }

    /// Record a successful session at the given trust level.
    ///
    /// Increments the appropriate streak counter. Graduation thresholds:
    /// - Level 3: 10 consecutive successes -> `level3_autonomous = true`
    /// - Level 2: 25 consecutive successes -> `level2_autonomous = true`
    pub fn record_success(&mut self, level: TrustLevel) {
        match level {
            TrustLevel::AutonomousBasic => {
                self.state.level3_streak += 1;
                if self.state.level3_streak >= 10 {
                    self.state.level3_autonomous = true;
                    tracing::info!(
                        streak = self.state.level3_streak,
                        "cambium trust: Level 3 graduated to autonomous"
                    );
                }
            }
            TrustLevel::AutonomousFull => {
                self.state.level2_streak += 1;
                if self.state.level2_streak >= 25 {
                    self.state.level2_autonomous = true;
                    tracing::info!(
                        streak = self.state.level2_streak,
                        "cambium trust: Level 2 graduated to autonomous"
                    );
                }
            }
            // Level 0 and Level 1 don't have autonomous graduation.
            TrustLevel::Immutable | TrustLevel::ApprovalRequired => {}
        }
    }

    /// Record a failed session (rollback, zone violation, etc.).
    ///
    /// Resets both streaks and increments the rollback counter. If rollbacks
    /// reach 3, all levels are forced to approval-required mode.
    pub fn record_failure(&mut self) {
        self.state.level3_streak = 0;
        self.state.level2_streak = 0;
        self.state.recent_rollbacks += 1;
        self.state.last_failure_at = Some(Utc::now());

        if self.state.recent_rollbacks >= 3 {
            self.state.all_approval_required = true;
            tracing::warn!(
                rollbacks = self.state.recent_rollbacks,
                "cambium trust: too many rollbacks, all levels set to approval-required"
            );
        }
    }

    /// Record a Witness verdict outcome. Evidence-bound trust: the caller
    /// passes a boolean `passed` derived from a Witness `Verdict::is_pass()`
    /// plus the trust level at which the work was performed. `true` calls
    /// `record_success(level)`; `false` calls `record_failure()`.
    ///
    /// This is the integration point for Witness Phase 2+. Keeping it as a
    /// plain `bool` parameter avoids cambium depending on the witness crate
    /// — the caller (e.g. the agent runtime hook) is responsible for
    /// translating `Verdict` → `bool`.
    ///
    /// `Inconclusive` outcomes should NOT be mapped to this method. They
    /// represent "Witness couldn't decide" and should not move trust in
    /// either direction. The caller should skip `record_verdict` on
    /// inconclusive.
    pub fn record_verdict(&mut self, passed: bool, level: TrustLevel) {
        if passed {
            tracing::debug!(?level, "cambium trust: recording PASS verdict from witness");
            self.record_success(level);
        } else {
            tracing::debug!(?level, "cambium trust: recording FAIL verdict from witness");
            self.record_failure();
        }
    }

    /// Check whether the given trust level can operate autonomously.
    ///
    /// - Level 0 (Immutable): always `false` -- cannot be modified at all.
    /// - Level 1 (ApprovalRequired): always `false` -- requires human approval.
    /// - Level 2 (AutonomousFull): `true` if `level2_autonomous` and not
    ///   `all_approval_required`.
    /// - Level 3 (AutonomousBasic): `true` if `level3_autonomous` and not
    ///   `all_approval_required`.
    ///
    /// If `config_override` is `Some("approval_required")`, always returns `false`.
    pub fn is_autonomous(&self, level: TrustLevel) -> bool {
        if let Some(ref override_val) = self.config_override {
            if override_val == "approval_required" {
                return false;
            }
        }

        if self.state.all_approval_required {
            return false;
        }

        match level {
            TrustLevel::Immutable => false,
            TrustLevel::ApprovalRequired => false,
            TrustLevel::AutonomousFull => self.state.level2_autonomous,
            TrustLevel::AutonomousBasic => self.state.level3_autonomous,
        }
    }

    /// Check whether sufficient time has elapsed since the last session.
    ///
    /// Returns `true` if `last_session_at` is `None` (no prior session) or
    /// if `now - last_session_at >= cooldown_secs`.
    pub fn cooldown_elapsed(&self, now: DateTime<Utc>, cooldown_secs: i64) -> bool {
        match self.state.last_session_at {
            None => true,
            Some(last) => {
                let elapsed = now.signed_duration_since(last);
                elapsed.num_seconds() >= cooldown_secs
            }
        }
    }

    /// Check whether sufficient time has elapsed since the last failure.
    ///
    /// Returns `true` if `last_failure_at` is `None` or if
    /// `now - last_failure_at >= cooldown_secs`.
    pub fn failure_cooldown_elapsed(&self, now: DateTime<Utc>, cooldown_secs: i64) -> bool {
        match self.state.last_failure_at {
            None => true,
            Some(last) => {
                let elapsed = now.signed_duration_since(last);
                elapsed.num_seconds() >= cooldown_secs
            }
        }
    }

    /// Check whether the daily session limit has been reached.
    ///
    /// If the stored date differs from `today_str`, the counter is reset first.
    /// Returns `true` if `sessions_today >= max_per_day`.
    pub fn daily_limit_reached(&mut self, max_per_day: usize, today_str: &str) -> bool {
        // Reset counter if the day has changed.
        let stored_date = self.state.sessions_today_date.as_deref();
        if stored_date != Some(today_str) {
            self.state.sessions_today = 0;
            self.state.sessions_today_date = Some(today_str.to_string());
        }

        self.state.sessions_today >= max_per_day
    }

    /// Get a reference to the current trust state.
    pub fn state(&self) -> &TrustState {
        &self.state
    }

    /// Record the start of a new session.
    ///
    /// Updates `last_session_at` and increments `sessions_today`. Resets the
    /// daily counter if the date has changed.
    pub fn record_session_start(&mut self, now: DateTime<Utc>, today_str: &str) {
        self.state.last_session_at = Some(now);

        let stored_date = self.state.sessions_today_date.as_deref();
        if stored_date != Some(today_str) {
            self.state.sessions_today = 0;
            self.state.sessions_today_date = Some(today_str.to_string());
        }

        self.state.sessions_today += 1;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    fn fresh_engine() -> TrustEngine {
        TrustEngine::new(TrustState::default(), None)
    }

    // ── record_success ──────────────────────────────────────────────

    #[test]
    fn level3_graduates_after_10_successes() {
        let mut engine = fresh_engine();
        for _ in 0..9 {
            engine.record_success(TrustLevel::AutonomousBasic);
            assert!(!engine.state().level3_autonomous);
        }
        engine.record_success(TrustLevel::AutonomousBasic);
        assert!(engine.state().level3_autonomous);
        assert_eq!(engine.state().level3_streak, 10);
    }

    #[test]
    fn level2_graduates_after_25_successes() {
        let mut engine = fresh_engine();
        for _ in 0..24 {
            engine.record_success(TrustLevel::AutonomousFull);
            assert!(!engine.state().level2_autonomous);
        }
        engine.record_success(TrustLevel::AutonomousFull);
        assert!(engine.state().level2_autonomous);
        assert_eq!(engine.state().level2_streak, 25);
    }

    #[test]
    fn success_on_immutable_does_nothing() {
        let mut engine = fresh_engine();
        engine.record_success(TrustLevel::Immutable);
        assert_eq!(engine.state().level3_streak, 0);
        assert_eq!(engine.state().level2_streak, 0);
    }

    #[test]
    fn success_on_approval_required_does_nothing() {
        let mut engine = fresh_engine();
        engine.record_success(TrustLevel::ApprovalRequired);
        assert_eq!(engine.state().level3_streak, 0);
        assert_eq!(engine.state().level2_streak, 0);
    }

    // ── record_verdict (Witness integration) ────────────────────────

    #[test]
    fn record_verdict_pass_increments_level3_streak() {
        let mut engine = fresh_engine();
        engine.record_verdict(true, TrustLevel::AutonomousBasic);
        assert_eq!(engine.state().level3_streak, 1);
        assert_eq!(engine.state().recent_rollbacks, 0);
    }

    #[test]
    fn record_verdict_fail_resets_streak_and_increments_rollbacks() {
        let mut engine = fresh_engine();
        for _ in 0..5 {
            engine.record_success(TrustLevel::AutonomousBasic);
        }
        assert_eq!(engine.state().level3_streak, 5);
        engine.record_verdict(false, TrustLevel::AutonomousBasic);
        assert_eq!(engine.state().level3_streak, 0);
        assert_eq!(engine.state().recent_rollbacks, 1);
    }

    #[test]
    fn record_verdict_pass_after_failures_rebuilds_streak() {
        let mut engine = fresh_engine();
        engine.record_verdict(false, TrustLevel::AutonomousBasic);
        engine.record_verdict(true, TrustLevel::AutonomousBasic);
        engine.record_verdict(true, TrustLevel::AutonomousBasic);
        assert_eq!(engine.state().level3_streak, 2);
    }

    #[test]
    fn record_verdict_can_graduate_level3_autonomous() {
        let mut engine = fresh_engine();
        for _ in 0..10 {
            engine.record_verdict(true, TrustLevel::AutonomousBasic);
        }
        assert!(engine.state().level3_autonomous);
    }

    #[test]
    fn streaks_are_independent() {
        let mut engine = fresh_engine();
        for _ in 0..5 {
            engine.record_success(TrustLevel::AutonomousBasic);
        }
        for _ in 0..3 {
            engine.record_success(TrustLevel::AutonomousFull);
        }
        assert_eq!(engine.state().level3_streak, 5);
        assert_eq!(engine.state().level2_streak, 3);
    }

    // ── record_failure ──────────────────────────────────────────────

    #[test]
    fn failure_resets_both_streaks() {
        let mut engine = fresh_engine();
        for _ in 0..5 {
            engine.record_success(TrustLevel::AutonomousBasic);
        }
        for _ in 0..3 {
            engine.record_success(TrustLevel::AutonomousFull);
        }
        engine.record_failure();
        assert_eq!(engine.state().level3_streak, 0);
        assert_eq!(engine.state().level2_streak, 0);
    }

    #[test]
    fn failure_increments_rollbacks() {
        let mut engine = fresh_engine();
        engine.record_failure();
        assert_eq!(engine.state().recent_rollbacks, 1);
        engine.record_failure();
        assert_eq!(engine.state().recent_rollbacks, 2);
    }

    #[test]
    fn three_failures_force_all_approval_required() {
        let mut engine = fresh_engine();
        engine.record_failure();
        engine.record_failure();
        assert!(!engine.state().all_approval_required);
        engine.record_failure();
        assert!(engine.state().all_approval_required);
    }

    #[test]
    fn failure_sets_last_failure_at() {
        let mut engine = fresh_engine();
        assert!(engine.state().last_failure_at.is_none());
        engine.record_failure();
        assert!(engine.state().last_failure_at.is_some());
    }

    // ── is_autonomous ───────────────────────────────────────────────

    #[test]
    fn immutable_never_autonomous() {
        let mut engine = fresh_engine();
        // Even with graduated states.
        engine.state.level3_autonomous = true;
        engine.state.level2_autonomous = true;
        assert!(!engine.is_autonomous(TrustLevel::Immutable));
    }

    #[test]
    fn approval_required_never_autonomous() {
        let mut engine = fresh_engine();
        engine.state.level3_autonomous = true;
        engine.state.level2_autonomous = true;
        assert!(!engine.is_autonomous(TrustLevel::ApprovalRequired));
    }

    #[test]
    fn level3_autonomous_when_graduated() {
        let mut engine = fresh_engine();
        assert!(!engine.is_autonomous(TrustLevel::AutonomousBasic));
        for _ in 0..10 {
            engine.record_success(TrustLevel::AutonomousBasic);
        }
        assert!(engine.is_autonomous(TrustLevel::AutonomousBasic));
    }

    #[test]
    fn level2_autonomous_when_graduated() {
        let mut engine = fresh_engine();
        assert!(!engine.is_autonomous(TrustLevel::AutonomousFull));
        for _ in 0..25 {
            engine.record_success(TrustLevel::AutonomousFull);
        }
        assert!(engine.is_autonomous(TrustLevel::AutonomousFull));
    }

    #[test]
    fn all_approval_required_overrides_graduated() {
        let mut engine = fresh_engine();
        // Graduate both levels.
        for _ in 0..10 {
            engine.record_success(TrustLevel::AutonomousBasic);
        }
        for _ in 0..25 {
            engine.record_success(TrustLevel::AutonomousFull);
        }
        assert!(engine.is_autonomous(TrustLevel::AutonomousBasic));
        assert!(engine.is_autonomous(TrustLevel::AutonomousFull));

        // Force all approval required.
        engine.state.all_approval_required = true;
        assert!(!engine.is_autonomous(TrustLevel::AutonomousBasic));
        assert!(!engine.is_autonomous(TrustLevel::AutonomousFull));
    }

    #[test]
    fn config_override_forces_non_autonomous() {
        let state = TrustState {
            level3_autonomous: true,
            level2_autonomous: true,
            ..TrustState::default()
        };
        let engine = TrustEngine::new(state, Some("approval_required".to_string()));
        assert!(!engine.is_autonomous(TrustLevel::AutonomousBasic));
        assert!(!engine.is_autonomous(TrustLevel::AutonomousFull));
    }

    #[test]
    fn config_override_other_value_does_not_block() {
        let state = TrustState {
            level3_autonomous: true,
            ..TrustState::default()
        };
        let engine = TrustEngine::new(state, Some("something_else".to_string()));
        assert!(engine.is_autonomous(TrustLevel::AutonomousBasic));
    }

    // ── cooldowns ───────────────────────────────────────────────────

    #[test]
    fn cooldown_elapsed_when_no_prior_session() {
        let engine = fresh_engine();
        assert!(engine.cooldown_elapsed(Utc::now(), 300));
    }

    #[test]
    fn cooldown_not_elapsed_when_too_soon() {
        let mut engine = fresh_engine();
        let now = Utc::now();
        engine.state.last_session_at = Some(now);
        assert!(!engine.cooldown_elapsed(now, 300));
    }

    #[test]
    fn cooldown_elapsed_after_waiting() {
        let mut engine = fresh_engine();
        let past = Utc::now() - Duration::seconds(600);
        engine.state.last_session_at = Some(past);
        assert!(engine.cooldown_elapsed(Utc::now(), 300));
    }

    #[test]
    fn failure_cooldown_elapsed_when_no_prior_failure() {
        let engine = fresh_engine();
        assert!(engine.failure_cooldown_elapsed(Utc::now(), 3600));
    }

    #[test]
    fn failure_cooldown_not_elapsed_when_too_soon() {
        let mut engine = fresh_engine();
        let now = Utc::now();
        engine.state.last_failure_at = Some(now);
        assert!(!engine.failure_cooldown_elapsed(now, 3600));
    }

    #[test]
    fn failure_cooldown_elapsed_after_waiting() {
        let mut engine = fresh_engine();
        let past = Utc::now() - Duration::seconds(7200);
        engine.state.last_failure_at = Some(past);
        assert!(engine.failure_cooldown_elapsed(Utc::now(), 3600));
    }

    // ── daily limits ────────────────────────────────────────────────

    #[test]
    fn daily_limit_not_reached_initially() {
        let mut engine = fresh_engine();
        assert!(!engine.daily_limit_reached(5, "2026-04-08"));
    }

    #[test]
    fn daily_limit_reached_after_max_sessions() {
        let mut engine = fresh_engine();
        engine.state.sessions_today = 5;
        engine.state.sessions_today_date = Some("2026-04-08".to_string());
        assert!(engine.daily_limit_reached(5, "2026-04-08"));
    }

    #[test]
    fn daily_limit_resets_on_new_day() {
        let mut engine = fresh_engine();
        engine.state.sessions_today = 10;
        engine.state.sessions_today_date = Some("2026-04-07".to_string());
        // New day resets counter.
        assert!(!engine.daily_limit_reached(5, "2026-04-08"));
        assert_eq!(engine.state().sessions_today, 0);
        assert_eq!(
            engine.state().sessions_today_date.as_deref(),
            Some("2026-04-08")
        );
    }

    // ── record_session_start ────────────────────────────────────────

    #[test]
    fn record_session_start_updates_timestamp() {
        let mut engine = fresh_engine();
        let now = Utc::now();
        engine.record_session_start(now, "2026-04-08");
        assert_eq!(engine.state().last_session_at, Some(now));
    }

    #[test]
    fn record_session_start_increments_counter() {
        let mut engine = fresh_engine();
        let now = Utc::now();
        engine.record_session_start(now, "2026-04-08");
        assert_eq!(engine.state().sessions_today, 1);
        engine.record_session_start(now, "2026-04-08");
        assert_eq!(engine.state().sessions_today, 2);
    }

    #[test]
    fn record_session_start_resets_on_new_day() {
        let mut engine = fresh_engine();
        let now = Utc::now();
        engine.record_session_start(now, "2026-04-08");
        engine.record_session_start(now, "2026-04-08");
        assert_eq!(engine.state().sessions_today, 2);

        // New day resets counter, then increments.
        engine.record_session_start(now, "2026-04-09");
        assert_eq!(engine.state().sessions_today, 1);
        assert_eq!(
            engine.state().sessions_today_date.as_deref(),
            Some("2026-04-09")
        );
    }

    // ── integration scenarios ───────────────────────────────────────

    #[test]
    fn graduation_then_failure_then_recovery() {
        let mut engine = fresh_engine();

        // Graduate Level 3.
        for _ in 0..10 {
            engine.record_success(TrustLevel::AutonomousBasic);
        }
        assert!(engine.is_autonomous(TrustLevel::AutonomousBasic));

        // Failure resets streak but graduation flag persists.
        engine.record_failure();
        assert_eq!(engine.state().level3_streak, 0);
        // level3_autonomous remains true (graduation is persistent).
        assert!(engine.state().level3_autonomous);

        // But after 3 failures, all_approval_required kicks in.
        engine.record_failure();
        engine.record_failure();
        assert!(!engine.is_autonomous(TrustLevel::AutonomousBasic));
    }
}
