//! Startup metrics and health verification for TEMM1E.
//!
//! This module tracks initialization timings for each subsystem so operators
//! can identify startup bottlenecks. It also provides a post-startup health
//! check that verifies all subsystems came up correctly.
//!
//! ## Binary size analysis
//!
//! The following crates contribute the most to the release binary size.
//! All are already feature-gated, which means a minimal build that excludes
//! unused channels and tools produces a significantly smaller binary:
//!
//! | Crate           | Approx. contribution | Feature gate          |
//! |-----------------|---------------------:|-----------------------|
//! | `teloxide`      |              ~2.0 MB | `telegram`            |
//! | `serenity`      |              ~1.5 MB | `discord`             |
//! | `reqwest`       |              ~1.0 MB | always (core HTTP)    |
//! | `sqlx`          |              ~0.8 MB | always (memory layer) |
//! | `chromiumoxide` |              ~0.6 MB | `browser`             |
//!
//! To produce the smallest possible binary, build with:
//! ```bash
//! cargo build --release --bin temm1e --no-default-features
//! ```
//! This drops `teloxide` and `chromiumoxide` entirely, saving ~2.6 MB.
//!
//! The release profile in the workspace `Cargo.toml` already applies:
//! - `opt-level = "z"` (optimize for size)
//! - `lto = true` (link-time optimization, eliminates dead code across crates)
//! - `codegen-units = 1` (maximum LTO effectiveness)
//! - `strip = true` (strip symbols from the binary)
//! - `panic = "abort"` (removes unwinding machinery, ~100 KB savings)

use std::fmt;
use std::time::Instant;

use temm1e_core::types::error::Temm1eError;
use tracing::info;

/// Timing breakdown of the startup sequence.
///
/// Each field records the wall-clock milliseconds spent initializing that
/// subsystem. Use [`StartupMetrics::measure`] for convenient scoped timing
/// and [`log_startup_metrics`] to emit structured tracing output.
#[derive(Debug, Clone, Default)]
pub struct StartupMetrics {
    /// Time spent loading and validating the TOML configuration.
    pub config_load_ms: u64,
    /// Time spent initializing the memory backend (SQLite migrations, etc.).
    pub memory_init_ms: u64,
    /// Time spent creating the AI provider client.
    pub provider_init_ms: u64,
    /// Time spent starting messaging channels (Telegram polling, etc.).
    pub channel_init_ms: u64,
    /// Total wall-clock time from process entry to "ready".
    pub total_startup_ms: u64,
}

impl StartupMetrics {
    /// Create a zeroed metrics struct.
    pub fn new() -> Self {
        Self::default()
    }

    /// Time a closure and return its result plus the elapsed milliseconds.
    ///
    /// # Example
    ///
    /// ```ignore
    /// let (result, ms) = StartupMetrics::measure(|| load_config());
    /// metrics.config_load_ms = ms;
    /// ```
    pub fn measure<F, T>(f: F) -> (T, u64)
    where
        F: FnOnce() -> T,
    {
        let start = Instant::now();
        let result = f();
        let elapsed_ms = start.elapsed().as_millis() as u64;
        (result, elapsed_ms)
    }

    /// Async version of [`measure`](Self::measure).
    pub async fn measure_async<F, Fut, T>(f: F) -> (T, u64)
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = T>,
    {
        let start = Instant::now();
        let result = f().await;
        let elapsed_ms = start.elapsed().as_millis() as u64;
        (result, elapsed_ms)
    }

    /// Returns `true` if any individual subsystem took longer than the
    /// given threshold (in milliseconds).
    pub fn has_slow_subsystem(&self, threshold_ms: u64) -> bool {
        self.config_load_ms > threshold_ms
            || self.memory_init_ms > threshold_ms
            || self.provider_init_ms > threshold_ms
            || self.channel_init_ms > threshold_ms
    }

    /// Return the name of the slowest subsystem and its duration.
    pub fn slowest_subsystem(&self) -> (&'static str, u64) {
        let candidates = [
            ("config", self.config_load_ms),
            ("memory", self.memory_init_ms),
            ("provider", self.provider_init_ms),
            ("channel", self.channel_init_ms),
        ];
        candidates
            .into_iter()
            .max_by_key(|(_, ms)| *ms)
            .unwrap_or(("config", 0))
    }
}

impl fmt::Display for StartupMetrics {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "config={}ms memory={}ms provider={}ms channel={}ms total={}ms",
            self.config_load_ms,
            self.memory_init_ms,
            self.provider_init_ms,
            self.channel_init_ms,
            self.total_startup_ms,
        )
    }
}

/// Emit structured startup timing via `tracing::info!`.
///
/// Each subsystem's duration is logged as a separate field so log aggregators
/// (Datadog, Loki, etc.) can index and alert on them individually.
pub fn log_startup_metrics(metrics: &StartupMetrics) {
    let (slowest_name, slowest_ms) = metrics.slowest_subsystem();

    info!(
        config_load_ms = metrics.config_load_ms,
        memory_init_ms = metrics.memory_init_ms,
        provider_init_ms = metrics.provider_init_ms,
        channel_init_ms = metrics.channel_init_ms,
        total_startup_ms = metrics.total_startup_ms,
        slowest_subsystem = slowest_name,
        slowest_subsystem_ms = slowest_ms,
        "Startup complete"
    );

    if metrics.has_slow_subsystem(5000) {
        tracing::warn!(
            subsystem = slowest_name,
            duration_ms = slowest_ms,
            "Slow startup detected — subsystem took >5s to initialize"
        );
    }
}

/// Verify that all critical subsystems are ready after startup.
///
/// Currently checks:
/// 1. Total startup did not exceed a reasonable ceiling (120s).
/// 2. No individual subsystem took an unreasonable amount of time (60s),
///    which would indicate a likely misconfiguration or network issue.
///
/// Returns `Ok(())` if all checks pass, or `Err(Temm1eError::Internal)`
/// with a description of what failed.
pub fn check_startup_health(metrics: &StartupMetrics) -> Result<(), Temm1eError> {
    const MAX_TOTAL_STARTUP_MS: u64 = 120_000;
    const MAX_SUBSYSTEM_MS: u64 = 60_000;

    if metrics.total_startup_ms > MAX_TOTAL_STARTUP_MS {
        return Err(Temm1eError::Internal(format!(
            "Total startup time ({}ms) exceeded maximum ({}ms) — possible misconfiguration",
            metrics.total_startup_ms, MAX_TOTAL_STARTUP_MS,
        )));
    }

    let subsystems = [
        ("config", metrics.config_load_ms),
        ("memory", metrics.memory_init_ms),
        ("provider", metrics.provider_init_ms),
        ("channel", metrics.channel_init_ms),
    ];

    for (name, duration_ms) in &subsystems {
        if *duration_ms > MAX_SUBSYSTEM_MS {
            return Err(Temm1eError::Internal(format!(
                "Subsystem '{}' took {}ms to initialize (max {}ms) — check network and configuration",
                name, duration_ms, MAX_SUBSYSTEM_MS,
            )));
        }
    }

    info!("Startup health check passed");
    Ok(())
}

/// Wrapper for lazy initialization of an expensive resource.
///
/// `LazyProvider` defers the creation of a value until the first call to
/// [`get_or_init`](Self::get_or_init). This is useful for subsystems that
/// are not needed on every code path (e.g., a provider that is only created
/// when the first message arrives, or a browser tool that is only spawned
/// on demand).
///
/// Unlike `std::sync::OnceLock`, this wrapper accepts an async initializer
/// and returns `Result`, making it suitable for fallible I/O-bound setup.
pub struct LazyResource<T> {
    inner: tokio::sync::OnceCell<T>,
    name: &'static str,
}

impl<T> LazyResource<T> {
    /// Create a new uninitialized lazy resource with a descriptive name
    /// (used in tracing output).
    pub fn new(name: &'static str) -> Self {
        Self {
            inner: tokio::sync::OnceCell::new(),
            name,
        }
    }

    /// Get a reference to the inner value, initializing it on first access.
    ///
    /// The initializer is called at most once, even under concurrent access.
    /// If the initializer fails, subsequent calls will retry.
    pub async fn get_or_init<F, Fut, E>(&self, init: F) -> Result<&T, E>
    where
        F: FnOnce() -> Fut,
        Fut: std::future::Future<Output = Result<T, E>>,
        E: fmt::Display,
    {
        self.inner
            .get_or_try_init(|| async {
                info!(resource = self.name, "Lazily initializing resource");
                let start = Instant::now();
                let result = init().await;
                let elapsed = start.elapsed();
                match &result {
                    Ok(_) => {
                        info!(
                            resource = self.name,
                            elapsed_ms = elapsed.as_millis() as u64,
                            "Resource initialized"
                        );
                    }
                    Err(e) => {
                        tracing::error!(
                            resource = self.name,
                            error = %e,
                            elapsed_ms = elapsed.as_millis() as u64,
                            "Resource initialization failed"
                        );
                    }
                }
                result
            })
            .await
    }

    /// Check whether the resource has been initialized.
    pub fn is_initialized(&self) -> bool {
        self.inner.initialized()
    }

    /// Get a reference to the inner value if already initialized.
    pub fn get(&self) -> Option<&T> {
        self.inner.get()
    }
}

impl<T: fmt::Debug> fmt::Debug for LazyResource<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LazyResource")
            .field("name", &self.name)
            .field("initialized", &self.is_initialized())
            .field("value", &self.inner.get())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_metrics_are_zeroed() {
        let m = StartupMetrics::new();
        assert_eq!(m.config_load_ms, 0);
        assert_eq!(m.memory_init_ms, 0);
        assert_eq!(m.provider_init_ms, 0);
        assert_eq!(m.channel_init_ms, 0);
        assert_eq!(m.total_startup_ms, 0);
    }

    #[test]
    fn default_metrics_equal_new() {
        let a = StartupMetrics::new();
        let b = StartupMetrics::default();
        assert_eq!(a.config_load_ms, b.config_load_ms);
        assert_eq!(a.memory_init_ms, b.memory_init_ms);
        assert_eq!(a.provider_init_ms, b.provider_init_ms);
        assert_eq!(a.channel_init_ms, b.channel_init_ms);
        assert_eq!(a.total_startup_ms, b.total_startup_ms);
    }

    #[test]
    fn measure_captures_elapsed_time() {
        let (result, ms) = StartupMetrics::measure(|| {
            std::thread::sleep(std::time::Duration::from_millis(50));
            42
        });
        assert_eq!(result, 42);
        assert!(ms >= 40, "Expected at least 40ms elapsed, got {}ms", ms);
    }

    #[test]
    fn has_slow_subsystem_detects_threshold() {
        let mut m = StartupMetrics::new();
        m.config_load_ms = 100;
        m.memory_init_ms = 200;
        m.provider_init_ms = 300;
        m.channel_init_ms = 400;

        assert!(!m.has_slow_subsystem(500));
        assert!(m.has_slow_subsystem(350)); // channel_init_ms = 400 > 350
        assert!(m.has_slow_subsystem(100)); // multiple subsystems above threshold
    }

    #[test]
    fn slowest_subsystem_identifies_bottleneck() {
        let mut m = StartupMetrics::new();
        m.config_load_ms = 10;
        m.memory_init_ms = 500;
        m.provider_init_ms = 200;
        m.channel_init_ms = 100;

        let (name, duration) = m.slowest_subsystem();
        assert_eq!(name, "memory");
        assert_eq!(duration, 500);
    }

    #[test]
    fn slowest_subsystem_when_all_zero() {
        let m = StartupMetrics::new();
        let (_, duration) = m.slowest_subsystem();
        assert_eq!(duration, 0);
    }

    #[test]
    fn display_format_is_readable() {
        let mut m = StartupMetrics::new();
        m.config_load_ms = 15;
        m.memory_init_ms = 120;
        m.provider_init_ms = 45;
        m.channel_init_ms = 300;
        m.total_startup_ms = 480;

        let s = format!("{}", m);
        assert_eq!(
            s,
            "config=15ms memory=120ms provider=45ms channel=300ms total=480ms"
        );
    }

    #[test]
    fn check_startup_health_passes_for_normal_timings() {
        let mut m = StartupMetrics::new();
        m.config_load_ms = 50;
        m.memory_init_ms = 200;
        m.provider_init_ms = 100;
        m.channel_init_ms = 500;
        m.total_startup_ms = 850;

        assert!(check_startup_health(&m).is_ok());
    }

    #[test]
    fn check_startup_health_fails_for_excessive_total() {
        let mut m = StartupMetrics::new();
        m.total_startup_ms = 130_000; // > 120s ceiling

        let err = check_startup_health(&m).unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("130000"),
            "Error should mention the actual duration: {}",
            msg
        );
        assert!(
            msg.contains("120000"),
            "Error should mention the max duration: {}",
            msg
        );
    }

    #[test]
    fn check_startup_health_fails_for_slow_subsystem() {
        let mut m = StartupMetrics::new();
        m.memory_init_ms = 70_000; // > 60s subsystem ceiling
        m.total_startup_ms = 70_000;

        let err = check_startup_health(&m).unwrap_err();
        let msg = format!("{}", err);
        assert!(
            msg.contains("memory"),
            "Error should identify the slow subsystem: {}",
            msg
        );
    }

    #[test]
    fn log_startup_metrics_does_not_panic() {
        // Verify that logging works without panicking, even with extreme values.
        let mut m = StartupMetrics::new();
        m.config_load_ms = u64::MAX;
        m.memory_init_ms = 0;
        m.provider_init_ms = 1;
        m.channel_init_ms = 999_999;
        m.total_startup_ms = u64::MAX;

        // This should not panic.
        log_startup_metrics(&m);
    }

    #[test]
    fn metrics_clone_is_independent() {
        let mut m = StartupMetrics::new();
        m.config_load_ms = 100;

        let m2 = m.clone();
        m.config_load_ms = 200;

        assert_eq!(m2.config_load_ms, 100);
        assert_eq!(m.config_load_ms, 200);
    }

    #[tokio::test]
    async fn measure_async_captures_elapsed_time() {
        let (result, ms) = StartupMetrics::measure_async(|| async {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            "hello"
        })
        .await;

        assert_eq!(result, "hello");
        assert!(ms >= 40, "Expected at least 40ms elapsed, got {}ms", ms);
    }

    #[tokio::test]
    async fn lazy_resource_initializes_once() {
        let resource = LazyResource::<String>::new("test");
        assert!(!resource.is_initialized());
        assert!(resource.get().is_none());

        let val = resource
            .get_or_init(|| async { Ok::<_, Temm1eError>("initialized".to_string()) })
            .await
            .unwrap();
        assert_eq!(val, "initialized");
        assert!(resource.is_initialized());
        assert_eq!(resource.get(), Some(&"initialized".to_string()));

        // Second call returns the same value without re-initializing.
        let val2 = resource
            .get_or_init(|| async { Ok::<_, Temm1eError>("should not run".to_string()) })
            .await
            .unwrap();
        assert_eq!(val2, "initialized");
    }

    #[tokio::test]
    async fn lazy_resource_retries_on_failure() {
        let resource = LazyResource::<String>::new("retry-test");

        // First attempt fails.
        let err = resource
            .get_or_init(|| async {
                Err::<String, Temm1eError>(Temm1eError::Internal("boom".to_string()))
            })
            .await;
        assert!(err.is_err());
        assert!(!resource.is_initialized());

        // Second attempt succeeds because OnceCell retries after failure.
        let val = resource
            .get_or_init(|| async { Ok::<_, Temm1eError>("recovered".to_string()) })
            .await
            .unwrap();
        assert_eq!(val, "recovered");
        assert!(resource.is_initialized());
    }
}
