//! Eigen-Tune Production Monitor — CUSUM on user behavior signals.
//!
//! After graduation, continuously detects accuracy drift using CUSUM
//! on user behavior observations. Zero LLM cost.

use crate::config::EigenTuneConfig;
use crate::stats::cusum::Cusum;
use crate::store::EigenTuneStore;
use crate::types::EigenTier;
use rand::Rng;
use std::sync::Arc;
use tracing;

pub struct ProductionMonitor {
    store: Arc<EigenTuneStore>,
    config: EigenTuneConfig,
}

impl ProductionMonitor {
    pub fn new(store: Arc<EigenTuneStore>, config: EigenTuneConfig) -> Self {
        Self { store, config }
    }

    /// Should we monitor this query? (sample rate check)
    pub fn should_monitor(&self) -> bool {
        let mut rng = rand::thread_rng();
        rng.gen::<f64>() < self.config.monitor_sample_rate
    }

    /// Process a user behavior observation during production monitoring.
    /// `agree` = true means user continued normally.
    /// Returns true if CUSUM alarm triggered (demote this tier).
    pub async fn observe(
        &self,
        tier: EigenTier,
        agree: bool,
    ) -> Result<bool, temm1e_core::types::error::Temm1eError> {
        let mut record = self.store.get_tier(tier.as_str()).await?;

        // Restore CUSUM from persisted state
        // target = graduation_accuracy (in-control mean)
        // slack = cusum_k (allowance parameter)
        // fir = false (no FIR config field; FIR is applied at transition time)
        let mut cusum = Cusum::from_state(
            self.config.graduation_accuracy,
            self.config.cusum_k,
            self.config.cusum_threshold,
            false,
            record.cusum_s,
            record.cusum_n as u32,
        );

        // Observe (1.0 for agree, 0.0 for disagree)
        let value = if agree { 1.0 } else { 0.0 };
        let alarm = cusum.observe(value);

        // Persist updated state
        record.cusum_s = cusum.statistic();
        record.cusum_n = cusum.n() as i32;
        self.store.update_tier(&record).await?;

        if alarm {
            tracing::warn!(
                tier = %tier.as_str(),
                cusum_s = cusum.statistic(),
                n = cusum.n(),
                "Eigen-Tune: CUSUM alarm! Quality drift detected"
            );
        } else {
            tracing::debug!(
                tier = %tier.as_str(),
                agree = agree,
                cusum_s = cusum.statistic(),
                n = cusum.n(),
                "Eigen-Tune: monitor observation processed"
            );
        }

        Ok(alarm)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_cusum_config_defaults() {
        let config = EigenTuneConfig::default();
        // graduation_accuracy is the CUSUM target (in-control mean)
        assert!((config.graduation_accuracy - 0.95).abs() < 1e-10);
        // cusum_k is the allowance/slack parameter
        assert!((config.cusum_k - 0.5).abs() < 1e-10);
        // cusum_threshold is the alarm threshold
        assert!((config.cusum_threshold - 5.0).abs() < 1e-10);
    }
}
