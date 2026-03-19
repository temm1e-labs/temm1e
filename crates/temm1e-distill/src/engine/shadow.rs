//! Eigen-Tune Shadow Testing — SPRT on user behavior signals.
//!
//! During shadow phase, the local model serves the user directly.
//! User behavior (continue/retry/reject) provides the SPRT observations.
//! Zero LLM cost.

use crate::config::EigenTuneConfig;
use crate::stats::sprt::{Sprt, SprtDecision};
use crate::store::EigenTuneStore;
use crate::types::EigenTier;
use std::sync::Arc;
use tracing;

pub struct ShadowCoordinator {
    store: Arc<EigenTuneStore>,
    config: EigenTuneConfig,
}

impl ShadowCoordinator {
    pub fn new(store: Arc<EigenTuneStore>, config: EigenTuneConfig) -> Self {
        Self { store, config }
    }

    /// Process a user behavior observation during shadow testing.
    /// `agree` = true means user continued normally (implicit approval).
    /// `agree` = false means user retried, rejected, or tool failed.
    /// Returns the SPRT decision after incorporating this observation.
    pub async fn observe(
        &self,
        tier: EigenTier,
        agree: bool,
    ) -> Result<SprtDecision, temm1e_core::types::error::Temm1eError> {
        let mut record = self.store.get_tier(tier.as_str()).await?;

        // Restore SPRT from persisted state
        let mut sprt = Sprt::from_state(
            self.config.sprt_p0,
            self.config.sprt_p1,
            self.config.sprt_alpha,
            self.config.sprt_beta,
            self.config.sprt_max_samples as u32,
            record.sprt_lambda,
            record.sprt_n as u32,
        );

        // Process observation
        let decision = sprt.observe(agree);

        // Persist updated state
        record.sprt_lambda = sprt.lambda();
        record.sprt_n = sprt.n() as i32;
        self.store.update_tier(&record).await?;

        tracing::debug!(
            tier = %tier.as_str(),
            agree = agree,
            lambda = sprt.lambda(),
            n = sprt.n(),
            decision = ?decision,
            "Eigen-Tune: shadow observation processed"
        );

        Ok(decision)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_sprt_decision_debug() {
        assert!(format!("{:?}", SprtDecision::Continue).contains("Continue"));
        assert!(format!("{:?}", SprtDecision::AcceptH1).contains("AcceptH1"));
        assert!(format!("{:?}", SprtDecision::AcceptH0).contains("AcceptH0"));
    }
}
