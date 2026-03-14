use crate::types::error::Temm1eError;
use async_trait::async_trait;

/// Observable trait — monitoring, logging, and metrics
#[async_trait]
pub trait Observable: Send + Sync {
    /// Record a metric
    async fn record_metric(
        &self,
        name: &str,
        value: f64,
        labels: &[(&str, &str)],
    ) -> Result<(), Temm1eError>;

    /// Record a counter increment
    async fn increment_counter(
        &self,
        name: &str,
        labels: &[(&str, &str)],
    ) -> Result<(), Temm1eError>;

    /// Record a histogram observation
    async fn observe_histogram(
        &self,
        name: &str,
        value: f64,
        labels: &[(&str, &str)],
    ) -> Result<(), Temm1eError>;

    /// Report health status
    async fn health_status(&self) -> Result<HealthStatus, Temm1eError>;
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct HealthStatus {
    pub status: HealthState,
    pub components: Vec<ComponentHealth>,
}

#[derive(Debug, Clone, serde::Serialize)]
pub enum HealthState {
    Healthy,
    Degraded,
    Unhealthy,
}

#[derive(Debug, Clone, serde::Serialize)]
pub struct ComponentHealth {
    pub name: String,
    pub status: HealthState,
    pub message: Option<String>,
}
