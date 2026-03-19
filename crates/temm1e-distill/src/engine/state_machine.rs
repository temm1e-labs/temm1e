//! Per-tier state machine for Eigen-Tune.
//!
//! Each complexity tier (Simple, Standard, Complex) has an independent state machine.
//! Transitions are governed by statistical tests — no transition without mathematical proof.

use crate::config::EigenTuneConfig;
use crate::stats::{entropy, sprt::SprtDecision, wilson};
use crate::store::EigenTuneStore;
use crate::types::{EigenTier, TierState};
use std::sync::Arc;
use tracing;

pub struct EigenTuneStateMachine {
    store: Arc<EigenTuneStore>,
    config: EigenTuneConfig,
}

impl EigenTuneStateMachine {
    pub fn new(store: Arc<EigenTuneStore>, config: EigenTuneConfig) -> Self {
        Self { store, config }
    }

    /// Check if a tier should transition to a new state.
    /// Returns the new state if a transition should occur, None otherwise.
    pub async fn check_transition(
        &self,
        tier: EigenTier,
    ) -> Result<Option<TierState>, temm1e_core::types::error::Temm1eError> {
        let record = self.store.get_tier(tier.as_str()).await?;

        match record.state {
            TierState::Collecting => self.check_collecting_transition(tier, &record).await,
            TierState::Training => Ok(None), // Training transitions handled by trainer
            TierState::Evaluating => self.check_evaluating_transition(tier, &record).await,
            TierState::Shadowing => self.check_shadowing_transition(tier, &record).await,
            TierState::Graduated => self.check_graduated_transition(tier, &record).await,
        }
    }

    /// Collecting → Training: enough data AND diverse enough.
    async fn check_collecting_transition(
        &self,
        tier: EigenTier,
        _record: &crate::types::TierRecord,
    ) -> Result<Option<TierState>, temm1e_core::types::error::Temm1eError> {
        // Check minimum pair count
        let pair_count = self
            .store
            .count_high_quality_pairs(tier.as_str(), self.config.quality_threshold)
            .await?;

        if pair_count < self.config.min_pairs as i64 {
            return Ok(None);
        }

        // Check dataset diversity (Shannon entropy)
        let category_counts = self.store.get_category_counts(tier.as_str()).await?;
        let counts: Vec<u64> = category_counts.iter().map(|(_, c)| *c as u64).collect();
        let j = entropy::normalized_entropy(&counts);

        if j < self.config.diversity_target {
            tracing::info!(
                tier = %tier.as_str(),
                entropy = j,
                threshold = self.config.diversity_target,
                "Eigen-Tune: dataset diversity too low, continuing to collect"
            );
            return Ok(None);
        }

        tracing::info!(
            tier = %tier.as_str(),
            pairs = pair_count,
            entropy = j,
            "Eigen-Tune: transition Collecting → Training"
        );

        Ok(Some(TierState::Training))
    }

    /// Evaluating → Shadowing or Collecting.
    /// Uses Wilson score interval at configured confidence level.
    async fn check_evaluating_transition(
        &self,
        tier: EigenTier,
        record: &crate::types::TierRecord,
    ) -> Result<Option<TierState>, temm1e_core::types::error::Temm1eError> {
        let (accuracy, n) = match (record.eval_accuracy, record.eval_n) {
            (Some(acc), Some(n)) => (acc, n),
            _ => return Ok(None), // Not enough eval data yet
        };

        if n < self.config.shadow_min_n {
            return Ok(None);
        }

        let successes = (accuracy * n as f64).round() as u64;
        let lower = wilson::wilson_lower(successes, n as u64, self.config.graduation_confidence);

        if lower >= self.config.graduation_accuracy {
            tracing::info!(
                tier = %tier.as_str(),
                accuracy = accuracy,
                wilson_lower = lower,
                threshold = self.config.graduation_accuracy,
                "Eigen-Tune: eval passed, transition Evaluating → Shadowing"
            );
            Ok(Some(TierState::Shadowing))
        } else {
            tracing::info!(
                tier = %tier.as_str(),
                accuracy = accuracy,
                wilson_lower = lower,
                threshold = self.config.graduation_accuracy,
                "Eigen-Tune: eval failed, transition Evaluating → Collecting"
            );
            Ok(Some(TierState::Collecting))
        }
    }

    /// Shadowing → Graduated or Collecting.
    /// Uses persisted SPRT state.
    async fn check_shadowing_transition(
        &self,
        tier: EigenTier,
        record: &crate::types::TierRecord,
    ) -> Result<Option<TierState>, temm1e_core::types::error::Temm1eError> {
        let sprt = crate::stats::sprt::Sprt::from_state(
            self.config.sprt_p0,
            self.config.sprt_p1,
            self.config.sprt_alpha,
            self.config.sprt_beta,
            self.config.sprt_max_samples as u32,
            record.sprt_lambda,
            record.sprt_n as u32,
        );

        match sprt.decision() {
            SprtDecision::AcceptH1 => {
                tracing::info!(
                    tier = %tier.as_str(),
                    lambda = record.sprt_lambda,
                    n = record.sprt_n,
                    "Eigen-Tune: SPRT accepted H1, transition Shadowing → Graduated"
                );
                Ok(Some(TierState::Graduated))
            }
            SprtDecision::AcceptH0 => {
                tracing::info!(
                    tier = %tier.as_str(),
                    lambda = record.sprt_lambda,
                    n = record.sprt_n,
                    "Eigen-Tune: SPRT accepted H0, transition Shadowing → Collecting"
                );
                Ok(Some(TierState::Collecting))
            }
            SprtDecision::Continue => Ok(None),
        }
    }

    /// Graduated → Collecting if CUSUM alarms.
    async fn check_graduated_transition(
        &self,
        tier: EigenTier,
        record: &crate::types::TierRecord,
    ) -> Result<Option<TierState>, temm1e_core::types::error::Temm1eError> {
        // CUSUM alarm check — the monitor updates cusum_s in the record
        if record.cusum_s > self.config.cusum_threshold {
            tracing::warn!(
                tier = %tier.as_str(),
                cusum_s = record.cusum_s,
                threshold = self.config.cusum_threshold,
                "Eigen-Tune: CUSUM alarm! Demoting tier"
            );
            Ok(Some(TierState::Collecting))
        } else {
            Ok(None)
        }
    }

    /// Execute a state transition for a tier.
    pub async fn transition(
        &self,
        tier: EigenTier,
        from: TierState,
        to: TierState,
    ) -> Result<(), temm1e_core::types::error::Temm1eError> {
        let mut record = self.store.get_tier(tier.as_str()).await?;

        // Validate transition
        if record.state != from {
            return Err(temm1e_core::types::error::Temm1eError::Internal(format!(
                "Eigen-Tune: invalid transition {} → {} for tier {} (current state: {})",
                from.as_str(),
                to.as_str(),
                tier.as_str(),
                record.state.as_str()
            )));
        }

        record.state = to;

        // Reset state-specific fields on transition
        match to {
            TierState::Collecting => {
                // Reset SPRT and CUSUM on demotion
                record.sprt_lambda = 0.0;
                record.sprt_n = 0;
                record.cusum_s = 0.0;
                record.cusum_n = 0;
                if from == TierState::Graduated {
                    record.last_demoted_at = Some(chrono::Utc::now());
                    record.serving_run_id = None;
                    record.serving_since = None;
                }
            }
            TierState::Training => {
                record.last_trained_at = Some(chrono::Utc::now());
            }
            TierState::Evaluating => {
                record.eval_accuracy = None;
                record.eval_n = None;
            }
            TierState::Shadowing => {
                record.sprt_lambda = 0.0;
                record.sprt_n = 0;
            }
            TierState::Graduated => {
                record.last_graduated_at = Some(chrono::Utc::now());
                record.serving_since = Some(chrono::Utc::now());
                // CUSUM starts with FIR if configured
                // Use FIR (Fast Initial Response) at graduation:
                // start CUSUM at threshold/2 for faster drift detection.
                if true {
                    record.cusum_s = self.config.cusum_threshold / 2.0;
                } else {
                    record.cusum_s = 0.0;
                }
                record.cusum_n = 0;
            }
        }

        self.store.update_tier(&record).await?;

        tracing::info!(
            tier = %tier.as_str(),
            from = %from.as_str(),
            to = %to.as_str(),
            "Eigen-Tune: state transition complete"
        );

        Ok(())
    }

    /// Get current state for a tier.
    pub async fn state(
        &self,
        tier: EigenTier,
    ) -> Result<TierState, temm1e_core::types::error::Temm1eError> {
        let record = self.store.get_tier(tier.as_str()).await?;
        Ok(record.state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_tier_state_str_roundtrip() {
        for state in [
            TierState::Collecting,
            TierState::Training,
            TierState::Evaluating,
            TierState::Shadowing,
            TierState::Graduated,
        ] {
            assert_eq!(TierState::from_str(state.as_str()), state);
        }
    }
}
