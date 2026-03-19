//! Eigen-Tune Scorer — Beta-Binomial quality scoring for training pairs.
//!
//! Each training pair gets a quality score from observed user behavior signals.
//! The Beta-Binomial model handles uncertainty naturally: a pair with 1 positive
//! signal is NOT the same as a pair with 10 positive and 4 negative (even if
//! the point estimate is similar).

use crate::stats::beta;
use crate::store::EigenTuneStore;
use crate::types::QualitySignal;
use std::sync::Arc;
use tracing;

pub struct EigenTuneScorer {
    store: Arc<EigenTuneStore>,
}

impl EigenTuneScorer {
    pub fn new(store: Arc<EigenTuneStore>) -> Self {
        Self { store }
    }

    /// Compute quality score from Beta parameters.
    pub fn compute_score(alpha: f64, beta_param: f64) -> f64 {
        beta::beta_mean(alpha, beta_param)
    }

    /// Compute uncertainty from Beta parameters.
    pub fn compute_uncertainty(alpha: f64, beta_param: f64) -> f64 {
        beta::beta_variance(alpha, beta_param)
    }

    /// Apply a quality signal to a pair, updating its Beta parameters.
    /// Returns the new quality score.
    pub async fn apply_signal(
        &self,
        pair_id: &str,
        signal: QualitySignal,
    ) -> Result<f64, temm1e_core::types::error::Temm1eError> {
        // Get current pair quality
        let pair = self.store.get_recent_pair(pair_id).await?.ok_or_else(|| {
            temm1e_core::types::error::Temm1eError::NotFound(format!(
                "Training pair not found: {}",
                pair_id
            ))
        })?;

        let weight = signal.weight();
        let (new_alpha, new_beta) = beta::beta_update(
            pair.quality_alpha,
            pair.quality_beta,
            weight,
            signal.is_positive(),
        );
        let new_score = beta::beta_mean(new_alpha, new_beta);

        self.store
            .update_quality(pair_id, new_alpha, new_beta, new_score)
            .await?;

        tracing::debug!(
            pair_id = %pair_id,
            signal = ?signal,
            alpha = new_alpha,
            beta = new_beta,
            score = new_score,
            "Eigen-Tune: quality updated"
        );

        Ok(new_score)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_initial_score() {
        // Beta(2,2) mean = 0.5
        let score = EigenTuneScorer::compute_score(2.0, 2.0);
        assert!((score - 0.5).abs() < 1e-10);
    }

    #[test]
    fn test_positive_signal_increases_score() {
        let (alpha, beta_p) = beta::beta_update(2.0, 2.0, 1.0, true);
        let new_score = beta::beta_mean(alpha, beta_p);
        assert!(new_score > 0.5);
    }

    #[test]
    fn test_negative_signal_decreases_score() {
        let (alpha, beta_p) = beta::beta_update(2.0, 2.0, 1.0, false);
        let new_score = beta::beta_mean(alpha, beta_p);
        assert!(new_score < 0.5);
    }

    #[test]
    fn test_score_bounded() {
        // Even with many positive signals, score stays <= 1.0
        let mut alpha = 2.0;
        let beta_p = 2.0;
        for _ in 0..100 {
            alpha += 1.0;
        }
        let score = beta::beta_mean(alpha, beta_p);
        assert!(score <= 1.0);
        assert!(score > 0.9); // Should be very high after 100 positive signals
    }

    #[test]
    fn test_uncertainty_decreases_with_evidence() {
        let var_initial = EigenTuneScorer::compute_uncertainty(2.0, 2.0);
        let var_after = EigenTuneScorer::compute_uncertainty(10.0, 10.0);
        assert!(var_after < var_initial);
    }
}
