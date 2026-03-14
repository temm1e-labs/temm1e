//! OpenTelemetry exporter.
//!
//! Wraps [`MetricsCollector`] and additionally logs metric exports destined
//! for an OTLP endpoint. Real OTLP transport is stubbed — the struct records
//! the configured endpoint and delegates all metric storage to the inner
//! `MetricsCollector`.

use async_trait::async_trait;
use temm1e_core::traits::{ComponentHealth, HealthState, HealthStatus, Observable};
use temm1e_core::types::error::Temm1eError;

use crate::metrics::MetricsCollector;

/// OpenTelemetry-aware metrics exporter.
///
/// Delegates in-process storage to a [`MetricsCollector`] and annotates
/// operations with structured tracing spans so that `tracing-opentelemetry`
/// propagates trace context to any configured collector.
pub struct OtelExporter {
    inner: MetricsCollector,
    endpoint: String,
}

impl OtelExporter {
    /// Create a new exporter targeting the given OTLP endpoint.
    ///
    /// Returns an error if `endpoint` is empty.
    pub fn new(endpoint: &str) -> Result<Self, Temm1eError> {
        if endpoint.is_empty() {
            return Err(Temm1eError::Internal(
                "OTLP endpoint must not be empty".to_string(),
            ));
        }

        tracing::info!(endpoint, "OtelExporter initialised");

        Ok(Self {
            inner: MetricsCollector::new(),
            endpoint: endpoint.to_string(),
        })
    }

    /// Return a reference to the underlying [`MetricsCollector`].
    pub fn collector(&self) -> &MetricsCollector {
        &self.inner
    }

    /// Return the configured OTLP endpoint.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }
}

#[async_trait]
impl Observable for OtelExporter {
    async fn record_metric(
        &self,
        name: &str,
        value: f64,
        labels: &[(&str, &str)],
    ) -> Result<(), Temm1eError> {
        tracing::debug!(
            metric = name,
            value,
            endpoint = %self.endpoint,
            "otel: recording gauge"
        );
        self.inner.record_metric(name, value, labels).await
    }

    async fn increment_counter(
        &self,
        name: &str,
        labels: &[(&str, &str)],
    ) -> Result<(), Temm1eError> {
        tracing::debug!(
            metric = name,
            endpoint = %self.endpoint,
            "otel: incrementing counter"
        );
        self.inner.increment_counter(name, labels).await
    }

    async fn observe_histogram(
        &self,
        name: &str,
        value: f64,
        labels: &[(&str, &str)],
    ) -> Result<(), Temm1eError> {
        tracing::debug!(
            metric = name,
            value,
            endpoint = %self.endpoint,
            "otel: observing histogram"
        );
        self.inner.observe_histogram(name, value, labels).await
    }

    async fn health_status(&self) -> Result<HealthStatus, Temm1eError> {
        // For now we report healthy and note the configured endpoint.
        // A production version would ping the OTLP endpoint here.
        Ok(HealthStatus {
            status: HealthState::Healthy,
            components: vec![
                ComponentHealth {
                    name: "metrics_collector".to_string(),
                    status: HealthState::Healthy,
                    message: Some("In-process metrics operational".to_string()),
                },
                ComponentHealth {
                    name: "otlp_exporter".to_string(),
                    status: HealthState::Healthy,
                    message: Some(format!("Configured endpoint: {}", self.endpoint)),
                },
            ],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_endpoint_rejected() {
        let result = OtelExporter::new("");
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn otel_delegates_to_inner_collector() {
        let exporter = OtelExporter::new("http://localhost:4317").unwrap();
        exporter
            .increment_counter("test_counter", &[])
            .await
            .unwrap();
        exporter
            .increment_counter("test_counter", &[])
            .await
            .unwrap();

        assert_eq!(exporter.collector().counter_value("test_counter"), Some(2));
    }

    #[tokio::test]
    async fn otel_health_includes_exporter_component() {
        let exporter = OtelExporter::new("http://otel:4317").unwrap();
        let status = exporter.health_status().await.unwrap();

        assert!(matches!(status.status, HealthState::Healthy));
        assert_eq!(status.components.len(), 2);
        assert_eq!(status.components[1].name, "otlp_exporter");
    }

    #[test]
    fn endpoint_accessor() {
        let exporter = OtelExporter::new("http://otel:4317").unwrap();
        assert_eq!(exporter.endpoint(), "http://otel:4317");
    }
}
