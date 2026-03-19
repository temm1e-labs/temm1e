//! Eigen-Tune Router — decides cloud vs local model per query.

use crate::config::EigenTuneConfig;
use crate::store::EigenTuneStore;
use crate::types::{EigenTier, ModelEndpoint, RouteDecision, TierState};
use rand::Rng as _;
use std::sync::Arc;

pub struct EigenTuneRouter {
    store: Arc<EigenTuneStore>,
    config: EigenTuneConfig,
}

impl EigenTuneRouter {
    pub fn new(store: Arc<EigenTuneStore>, config: EigenTuneConfig) -> Self {
        Self { store, config }
    }

    /// Decide whether to route to local or cloud model.
    pub async fn route(
        &self,
        complexity: &str,
    ) -> Result<RouteDecision, temm1e_core::types::error::Temm1eError> {
        let tier = EigenTier::from_str(complexity);
        let record = self.store.get_tier(tier.as_str()).await?;

        match record.state {
            TierState::Graduated => {
                if let Some(ref run_id) = record.serving_run_id {
                    let run = self.store.get_run(run_id).await?;
                    if let Some(run) = run {
                        if let Some(ref model_name) = run.ollama_model_name {
                            let endpoint = ModelEndpoint {
                                base_url: "http://localhost:11434/v1".to_string(),
                                model_name: model_name.clone(),
                            };

                            // 5% monitoring sample
                            let mut rng = rand::thread_rng();
                            if rng.gen::<f64>() < self.config.monitor_sample_rate {
                                return Ok(RouteDecision::Monitor(endpoint));
                            }

                            return Ok(RouteDecision::Local(endpoint));
                        }
                    }
                }
                // Fallback: no model endpoint found, use cloud
                Ok(RouteDecision::Cloud)
            }
            TierState::Shadowing => {
                if let Some(ref run_id) = record.serving_run_id {
                    let run = self.store.get_run(run_id).await?;
                    if let Some(run) = run {
                        if let Some(ref model_name) = run.ollama_model_name {
                            let endpoint = ModelEndpoint {
                                base_url: "http://localhost:11434/v1".to_string(),
                                model_name: model_name.clone(),
                            };
                            return Ok(RouteDecision::Shadow(endpoint));
                        }
                    }
                }
                Ok(RouteDecision::Cloud)
            }
            _ => Ok(RouteDecision::Cloud),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_route_decision_debug() {
        let cloud = RouteDecision::Cloud;
        assert!(format!("{:?}", cloud).contains("Cloud"));

        let local = RouteDecision::Local(ModelEndpoint {
            base_url: "http://localhost:11434/v1".to_string(),
            model_name: "eigentune-v1".to_string(),
        });
        assert!(format!("{:?}", local).contains("Local"));
    }
}
