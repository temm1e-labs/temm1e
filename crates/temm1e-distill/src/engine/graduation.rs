//! Eigen-Tune Graduation — manages tier promotion and demotion.

use crate::config::EigenTuneConfig;
use crate::engine::state_machine::EigenTuneStateMachine;
use crate::store::EigenTuneStore;
use crate::types::{EigenTier, TierState};
use std::sync::Arc;
use tracing;

pub struct GraduationManager {
    state_machine: EigenTuneStateMachine,
    #[allow(dead_code)]
    config: EigenTuneConfig,
}

impl GraduationManager {
    pub fn new(store: Arc<EigenTuneStore>, config: EigenTuneConfig) -> Self {
        let state_machine = EigenTuneStateMachine::new(store, config.clone());
        Self {
            state_machine,
            config,
        }
    }

    /// Check all tiers for possible transitions and execute them.
    /// Returns a list of transitions that occurred.
    pub async fn tick(
        &self,
    ) -> Result<Vec<(EigenTier, TierState, TierState)>, temm1e_core::types::error::Temm1eError>
    {
        let mut transitions = Vec::new();

        for tier in [EigenTier::Simple, EigenTier::Standard, EigenTier::Complex] {
            let current = self.state_machine.state(tier).await?;

            if let Some(new_state) = self.state_machine.check_transition(tier).await? {
                self.state_machine
                    .transition(tier, current, new_state)
                    .await?;

                tracing::info!(
                    tier = %tier.as_str(),
                    from = %current.as_str(),
                    to = %new_state.as_str(),
                    "Eigen-Tune: tier transitioned"
                );

                transitions.push((tier, current, new_state));
            }
        }

        Ok(transitions)
    }

    /// Force-demote a tier back to Collecting (e.g., on CUSUM alarm).
    pub async fn demote(
        &self,
        tier: EigenTier,
    ) -> Result<(), temm1e_core::types::error::Temm1eError> {
        let current = self.state_machine.state(tier).await?;
        if current != TierState::Collecting {
            self.state_machine
                .transition(tier, current, TierState::Collecting)
                .await?;
            tracing::warn!(
                tier = %tier.as_str(),
                from = %current.as_str(),
                "Eigen-Tune: tier demoted to Collecting"
            );
        }
        Ok(())
    }
}
