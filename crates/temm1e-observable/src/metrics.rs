//! In-process metrics collector.
//!
//! Provides thread-safe counters, gauges, and histograms stored entirely
//! in-process. No external dependencies are needed — data lives in
//! `std::sync::atomic` integers and `std::sync::RwLock`-guarded vectors.

use std::collections::HashMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::RwLock;

use async_trait::async_trait;
use temm1e_core::traits::{ComponentHealth, HealthState, HealthStatus, Observable};
use temm1e_core::types::error::Temm1eError;

/// In-process metrics collector backed by atomics and RwLock-guarded vecs.
pub struct MetricsCollector {
    counters: RwLock<HashMap<String, AtomicU64>>,
    gauges: RwLock<HashMap<String, AtomicI64>>,
    histograms: RwLock<HashMap<String, Vec<f64>>>,
}

impl MetricsCollector {
    /// Create a new, empty metrics collector.
    pub fn new() -> Self {
        Self {
            counters: RwLock::new(HashMap::new()),
            gauges: RwLock::new(HashMap::new()),
            histograms: RwLock::new(HashMap::new()),
        }
    }

    // ── Helpers for label-qualified metric names ────────────────────────

    /// Build a composite key from the metric name and its labels.
    ///
    /// Example: `("latency", &[("provider", "anthropic")])` →
    /// `"latency{provider=anthropic}"`.
    fn qualified_name(name: &str, labels: &[(&str, &str)]) -> String {
        if labels.is_empty() {
            return name.to_string();
        }
        let pairs: Vec<String> = labels.iter().map(|(k, v)| format!("{k}={v}")).collect();
        format!("{name}{{{}}}", pairs.join(","))
    }

    // ── Public read accessors (for tests & OtelExporter) ───────────────

    /// Read the current value of a counter.
    pub fn counter_value(&self, key: &str) -> Option<u64> {
        let map = self.counters.read().unwrap_or_else(|e| e.into_inner());
        map.get(key).map(|v| v.load(Ordering::Relaxed))
    }

    /// Read the current value of a gauge.
    pub fn gauge_value(&self, key: &str) -> Option<i64> {
        let map = self.gauges.read().unwrap_or_else(|e| e.into_inner());
        map.get(key).map(|v| v.load(Ordering::Relaxed))
    }

    /// Read a snapshot of histogram observations.
    pub fn histogram_values(&self, key: &str) -> Option<Vec<f64>> {
        let map = self.histograms.read().unwrap_or_else(|e| e.into_inner());
        map.get(key).cloned()
    }

    /// Compute percentiles from a histogram's recorded values.
    ///
    /// Returns `None` if the histogram does not exist or has no observations.
    /// `percentile` must be in `[0.0, 100.0]`.
    pub fn histogram_percentile(&self, key: &str, percentile: f64) -> Option<f64> {
        if !(0.0..=100.0).contains(&percentile) {
            return None;
        }
        let map = self.histograms.read().unwrap_or_else(|e| e.into_inner());
        let values = map.get(key)?;
        if values.is_empty() {
            return None;
        }
        let mut sorted = values.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let idx = ((percentile / 100.0) * (sorted.len() as f64 - 1.0))
            .round()
            .max(0.0) as usize;
        let idx = idx.min(sorted.len() - 1);
        Some(sorted[idx])
    }
}

impl Default for MetricsCollector {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Observable for MetricsCollector {
    /// Record a gauge metric — stores `value * 1000` as an `i64`.
    async fn record_metric(
        &self,
        name: &str,
        value: f64,
        labels: &[(&str, &str)],
    ) -> Result<(), Temm1eError> {
        let key = Self::qualified_name(name, labels);
        let encoded = (value * 1000.0) as i64;

        let mut map = self
            .gauges
            .write()
            .map_err(|e| Temm1eError::Internal(format!("gauges lock poisoned: {e}")))?;

        map.entry(key)
            .and_modify(|v| v.store(encoded, Ordering::Relaxed))
            .or_insert_with(|| AtomicI64::new(encoded));

        tracing::debug!(metric = name, value, "gauge recorded");
        Ok(())
    }

    /// Increment a counter by 1.
    async fn increment_counter(
        &self,
        name: &str,
        labels: &[(&str, &str)],
    ) -> Result<(), Temm1eError> {
        let key = Self::qualified_name(name, labels);

        let map = self
            .counters
            .read()
            .map_err(|e| Temm1eError::Internal(format!("counters lock poisoned: {e}")))?;

        if let Some(counter) = map.get(&key) {
            counter.fetch_add(1, Ordering::Relaxed);
            tracing::debug!(metric = name, "counter incremented (existing)");
            return Ok(());
        }
        drop(map);

        let mut map = self
            .counters
            .write()
            .map_err(|e| Temm1eError::Internal(format!("counters lock poisoned: {e}")))?;

        // Double-check after acquiring write lock.
        map.entry(key)
            .and_modify(|v| {
                v.fetch_add(1, Ordering::Relaxed);
            })
            .or_insert_with(|| AtomicU64::new(1));

        tracing::debug!(metric = name, "counter incremented");
        Ok(())
    }

    /// Record a histogram observation.
    async fn observe_histogram(
        &self,
        name: &str,
        value: f64,
        labels: &[(&str, &str)],
    ) -> Result<(), Temm1eError> {
        let key = Self::qualified_name(name, labels);

        let mut map = self
            .histograms
            .write()
            .map_err(|e| Temm1eError::Internal(format!("histograms lock poisoned: {e}")))?;

        map.entry(key).or_default().push(value);

        tracing::debug!(metric = name, value, "histogram observation recorded");
        Ok(())
    }

    /// In-process metrics are always healthy.
    async fn health_status(&self) -> Result<HealthStatus, Temm1eError> {
        Ok(HealthStatus {
            status: HealthState::Healthy,
            components: vec![ComponentHealth {
                name: "metrics_collector".to_string(),
                status: HealthState::Healthy,
                message: Some("In-process metrics operational".to_string()),
            }],
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn increment_counter_creates_and_increments() {
        let mc = MetricsCollector::new();
        mc.increment_counter("requests", &[]).await.unwrap();
        mc.increment_counter("requests", &[]).await.unwrap();
        mc.increment_counter("requests", &[]).await.unwrap();

        assert_eq!(mc.counter_value("requests"), Some(3));
    }

    #[tokio::test]
    async fn increment_counter_with_labels() {
        let mc = MetricsCollector::new();
        mc.increment_counter("requests", &[("method", "GET")])
            .await
            .unwrap();
        mc.increment_counter("requests", &[("method", "POST")])
            .await
            .unwrap();
        mc.increment_counter("requests", &[("method", "GET")])
            .await
            .unwrap();

        assert_eq!(mc.counter_value("requests{method=GET}"), Some(2));
        assert_eq!(mc.counter_value("requests{method=POST}"), Some(1));
    }

    #[tokio::test]
    async fn counter_missing_returns_none() {
        let mc = MetricsCollector::new();
        assert_eq!(mc.counter_value("nonexistent"), None);
    }

    #[tokio::test]
    async fn record_metric_sets_gauge() {
        let mc = MetricsCollector::new();
        mc.record_metric("cpu_usage", 72.5, &[]).await.unwrap();

        let raw = mc.gauge_value("cpu_usage").unwrap();
        // 72.5 * 1000 = 72500
        assert_eq!(raw, 72500);
    }

    #[tokio::test]
    async fn record_metric_overwrites_gauge() {
        let mc = MetricsCollector::new();
        mc.record_metric("temperature", 20.0, &[]).await.unwrap();
        mc.record_metric("temperature", 25.5, &[]).await.unwrap();

        let raw = mc.gauge_value("temperature").unwrap();
        assert_eq!(raw, 25500);
    }

    #[tokio::test]
    async fn gauge_with_labels() {
        let mc = MetricsCollector::new();
        mc.record_metric("cpu", 50.0, &[("host", "a")])
            .await
            .unwrap();
        mc.record_metric("cpu", 80.0, &[("host", "b")])
            .await
            .unwrap();

        assert_eq!(mc.gauge_value("cpu{host=a}"), Some(50000));
        assert_eq!(mc.gauge_value("cpu{host=b}"), Some(80000));
    }

    #[tokio::test]
    async fn observe_histogram_records_values() {
        let mc = MetricsCollector::new();
        mc.observe_histogram("latency", 100.0, &[]).await.unwrap();
        mc.observe_histogram("latency", 200.0, &[]).await.unwrap();
        mc.observe_histogram("latency", 150.0, &[]).await.unwrap();

        let vals = mc.histogram_values("latency").unwrap();
        assert_eq!(vals, vec![100.0, 200.0, 150.0]);
    }

    #[tokio::test]
    async fn histogram_percentile_p50() {
        let mc = MetricsCollector::new();
        for v in [10.0, 20.0, 30.0, 40.0, 50.0] {
            mc.observe_histogram("lat", v, &[]).await.unwrap();
        }

        let p50 = mc.histogram_percentile("lat", 50.0).unwrap();
        assert!((p50 - 30.0).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn histogram_percentile_p99() {
        let mc = MetricsCollector::new();
        for v in 1..=100 {
            mc.observe_histogram("lat", v as f64, &[]).await.unwrap();
        }

        let p99 = mc.histogram_percentile("lat", 99.0).unwrap();
        // With 100 values, p99 index ≈ 98 → value 99.0
        assert!((p99 - 99.0).abs() < 1.0);
    }

    #[tokio::test]
    async fn histogram_percentile_empty_returns_none() {
        let mc = MetricsCollector::new();
        assert!(mc.histogram_percentile("nonexistent", 50.0).is_none());
    }

    #[tokio::test]
    async fn health_status_is_healthy() {
        let mc = MetricsCollector::new();
        let status = mc.health_status().await.unwrap();

        assert!(matches!(status.status, HealthState::Healthy));
        assert_eq!(status.components.len(), 1);
        assert_eq!(status.components[0].name, "metrics_collector");
        assert!(matches!(status.components[0].status, HealthState::Healthy));
    }

    #[tokio::test]
    async fn concurrent_counter_increments() {
        use std::sync::Arc;

        let mc = Arc::new(MetricsCollector::new());
        let mut handles = vec![];

        for _ in 0..10 {
            let mc = Arc::clone(&mc);
            handles.push(tokio::spawn(async move {
                for _ in 0..100 {
                    mc.increment_counter("concurrent", &[]).await.unwrap();
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        assert_eq!(mc.counter_value("concurrent"), Some(1000));
    }

    #[tokio::test]
    async fn concurrent_histogram_observations() {
        use std::sync::Arc;

        let mc = Arc::new(MetricsCollector::new());
        let mut handles = vec![];

        for i in 0..5 {
            let mc = Arc::clone(&mc);
            handles.push(tokio::spawn(async move {
                for j in 0..20 {
                    mc.observe_histogram("conc_hist", (i * 20 + j) as f64, &[])
                        .await
                        .unwrap();
                }
            }));
        }

        for h in handles {
            h.await.unwrap();
        }

        let vals = mc.histogram_values("conc_hist").unwrap();
        assert_eq!(vals.len(), 100);
    }

    #[tokio::test]
    async fn qualified_name_no_labels() {
        let key = MetricsCollector::qualified_name("metric", &[]);
        assert_eq!(key, "metric");
    }

    #[tokio::test]
    async fn qualified_name_with_labels() {
        let key = MetricsCollector::qualified_name("metric", &[("env", "prod"), ("region", "us")]);
        assert_eq!(key, "metric{env=prod,region=us}");
    }
}
