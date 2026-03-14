//! Resilient memory backend with automatic failover.
//!
//! `ResilientMemory` wraps a primary `Box<dyn Memory>` and maintains an
//! in-memory fallback cache. When the primary backend fails, operations
//! transparently fall back to the cache. After a configurable number of
//! consecutive failures the wrapper attempts repair; on recovery it syncs
//! cached entries back to the primary.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::sync::Arc;
use temm1e_core::error::Temm1eError;
use temm1e_core::{Memory, MemoryEntry, SearchOpts};
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

// ---------------------------------------------------------------------------
// Health status
// ---------------------------------------------------------------------------

/// Current health of the resilient memory backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum MemoryHealthStatus {
    /// Primary backend is operating normally.
    Healthy,
    /// Primary backend failed; using in-memory fallback.
    Degraded { reason: String },
    /// Repair was attempted but the primary backend is still down.
    Failed { repair_attempted: bool },
}

// ---------------------------------------------------------------------------
// Configuration
// ---------------------------------------------------------------------------

/// Tunables for `ResilientMemory`.
#[derive(Debug, Clone)]
pub struct FailoverConfig {
    /// Number of consecutive failures before a repair attempt is triggered.
    pub max_consecutive_failures: u32,
}

impl Default for FailoverConfig {
    fn default() -> Self {
        Self {
            max_consecutive_failures: 3,
        }
    }
}

// ---------------------------------------------------------------------------
// Internal shared state
// ---------------------------------------------------------------------------

/// Mutable state protected by a `RwLock`.
struct InnerState {
    /// Consecutive failure counter.
    consecutive_failures: u32,
    /// Whether a repair has been attempted since the last healthy state.
    repair_attempted: bool,
    /// In-memory fallback cache keyed by entry ID.
    cache: HashMap<String, MemoryEntry>,
}

impl InnerState {
    fn new() -> Self {
        Self {
            consecutive_failures: 0,
            repair_attempted: false,
            cache: HashMap::new(),
        }
    }
}

// ---------------------------------------------------------------------------
// ResilientMemory
// ---------------------------------------------------------------------------

/// A decorator around any `Memory` implementation that adds automatic
/// failover to an in-memory cache and optional repair logic.
pub struct ResilientMemory {
    primary: Box<dyn Memory>,
    state: Arc<RwLock<InnerState>>,
    config: FailoverConfig,
}

impl ResilientMemory {
    /// Wrap a primary memory backend with default failover settings.
    pub fn new(primary: Box<dyn Memory>) -> Self {
        Self {
            primary,
            state: Arc::new(RwLock::new(InnerState::new())),
            config: FailoverConfig::default(),
        }
    }

    /// Wrap a primary memory backend with custom failover configuration.
    pub fn with_config(primary: Box<dyn Memory>, config: FailoverConfig) -> Self {
        Self {
            primary,
            state: Arc::new(RwLock::new(InnerState::new())),
            config,
        }
    }

    /// Return the current health status.
    pub async fn health_status(&self) -> MemoryHealthStatus {
        let state = self.state.read().await;
        if state.consecutive_failures == 0 {
            MemoryHealthStatus::Healthy
        } else if state.repair_attempted {
            MemoryHealthStatus::Failed {
                repair_attempted: true,
            }
        } else {
            MemoryHealthStatus::Degraded {
                reason: format!(
                    "{} consecutive failure(s) on primary backend",
                    state.consecutive_failures
                ),
            }
        }
    }

    /// Return the number of consecutive failures recorded so far.
    pub async fn failure_count(&self) -> u32 {
        self.state.read().await.consecutive_failures
    }

    /// Attempt to repair the primary backend.
    ///
    /// Currently this performs a simple health-probe by calling
    /// `list_sessions()` on the primary.  If the probe succeeds the backend
    /// is considered recovered and any cached entries are synced back.
    pub async fn attempt_repair(&self) -> Result<(), Temm1eError> {
        info!(
            backend = %self.primary.backend_name(),
            "Attempting repair of primary memory backend"
        );

        // Probe the primary with a lightweight operation.
        match self.primary.list_sessions().await {
            Ok(_) => {
                info!(
                    backend = %self.primary.backend_name(),
                    "Primary memory backend recovered"
                );
                self.sync_cache_to_primary().await?;

                let mut state = self.state.write().await;
                state.consecutive_failures = 0;
                state.repair_attempted = false;
                Ok(())
            }
            Err(e) => {
                error!(
                    backend = %self.primary.backend_name(),
                    error = %e,
                    "Repair probe failed — primary backend still unavailable"
                );
                let mut state = self.state.write().await;
                state.repair_attempted = true;
                // Ensure the failure counter is at least 1 so health_status()
                // reflects the broken state even when repair is called
                // manually without prior operation failures.
                if state.consecutive_failures == 0 {
                    state.consecutive_failures = 1;
                }
                Err(Temm1eError::Memory(format!(
                    "Primary backend repair failed: {e}"
                )))
            }
        }
    }

    // ----- internal helpers ------------------------------------------------

    /// Record a successful primary operation (resets the failure counter).
    async fn record_success(&self) {
        let mut state = self.state.write().await;
        if state.consecutive_failures > 0 {
            info!(
                backend = %self.primary.backend_name(),
                previous_failures = state.consecutive_failures,
                "Primary memory backend recovered"
            );
            state.consecutive_failures = 0;
            state.repair_attempted = false;
        }
    }

    /// Record a primary failure.  Returns `true` when a repair should be
    /// attempted (i.e. the failure threshold has just been crossed).
    async fn record_failure(&self, err: &Temm1eError) -> bool {
        let mut state = self.state.write().await;
        state.consecutive_failures += 1;
        let count = state.consecutive_failures;

        error!(
            backend = %self.primary.backend_name(),
            consecutive_failures = count,
            error = %err,
            "Primary memory backend operation failed"
        );

        count >= self.config.max_consecutive_failures && !state.repair_attempted
    }

    /// Try to push all cached entries back to the primary.
    async fn sync_cache_to_primary(&self) -> Result<(), Temm1eError> {
        let entries: Vec<MemoryEntry> = {
            let state = self.state.read().await;
            state.cache.values().cloned().collect()
        };

        if entries.is_empty() {
            return Ok(());
        }

        info!(
            count = entries.len(),
            "Syncing cached entries to primary backend"
        );

        let mut failed = 0u32;
        for entry in &entries {
            if let Err(e) = self.primary.store(entry.clone()).await {
                warn!(
                    id = %entry.id,
                    error = %e,
                    "Failed to sync cached entry to primary"
                );
                failed += 1;
            }
        }

        if failed > 0 {
            return Err(Temm1eError::Memory(format!(
                "Failed to sync {failed}/{} cached entries to primary",
                entries.len()
            )));
        }

        // Clear cache on full success.
        {
            let mut state = self.state.write().await;
            state.cache.clear();
        }

        info!("Cache sync to primary completed successfully");
        Ok(())
    }
}

#[async_trait]
impl Memory for ResilientMemory {
    async fn store(&self, entry: MemoryEntry) -> Result<(), Temm1eError> {
        // Always cache a copy so we don't lose it if primary fails.
        {
            let mut state = self.state.write().await;
            state.cache.insert(entry.id.clone(), entry.clone());
        }

        match self.primary.store(entry).await {
            Ok(()) => {
                self.record_success().await;
                // We keep the cache entry — it does no harm, makes get()
                // faster, and avoids a race where we remove the cache copy
                // just before a subsequent primary failure.  Cached entries
                // are cleared on successful sync after recovery.
                Ok(())
            }
            Err(e) => {
                let should_repair = self.record_failure(&e).await;
                if should_repair {
                    warn!("Failure threshold reached — attempting automatic repair");
                    if let Err(e) = self.attempt_repair().await {
                        tracing::warn!(error = %e, "Automatic repair attempt failed");
                    }
                }
                // The entry is already in the cache, so we can return Ok.
                debug!(
                    error = %e,
                    "Store fell back to in-memory cache"
                );
                Ok(())
            }
        }
    }

    async fn get(&self, id: &str) -> Result<Option<MemoryEntry>, Temm1eError> {
        // Check cache first — it may contain entries not yet synced.
        {
            let state = self.state.read().await;
            if let Some(entry) = state.cache.get(id) {
                return Ok(Some(entry.clone()));
            }
        }

        match self.primary.get(id).await {
            Ok(result) => {
                self.record_success().await;
                Ok(result)
            }
            Err(e) => {
                let should_repair = self.record_failure(&e).await;
                if should_repair {
                    warn!("Failure threshold reached — attempting automatic repair");
                    if let Err(e) = self.attempt_repair().await {
                        tracing::warn!(error = %e, "Automatic repair attempt failed");
                    }
                }
                // Return from cache (already checked, so None).
                debug!(
                    id = %id,
                    error = %e,
                    "Get fell back to in-memory cache (miss)"
                );
                Ok(None)
            }
        }
    }

    async fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<MemoryEntry>, Temm1eError> {
        match self.primary.search(query, opts.clone()).await {
            Ok(mut results) => {
                self.record_success().await;
                // Merge any cached entries that match the query.
                let state = self.state.read().await;
                for entry in state.cache.values() {
                    if entry.content.contains(query) && !results.iter().any(|r| r.id == entry.id) {
                        // Respect filters.
                        if let Some(ref session) = opts.session_filter {
                            if entry.session_id.as_deref() != Some(session.as_str()) {
                                continue;
                            }
                        }
                        results.push(entry.clone());
                    }
                }
                // Re-apply limit after merge.
                results.truncate(opts.limit);
                Ok(results)
            }
            Err(e) => {
                let should_repair = self.record_failure(&e).await;
                if should_repair {
                    warn!("Failure threshold reached — attempting automatic repair");
                    if let Err(e) = self.attempt_repair().await {
                        tracing::warn!(error = %e, "Automatic repair attempt failed");
                    }
                }
                // Fall back to searching the cache.
                let state = self.state.read().await;
                let mut results: Vec<MemoryEntry> = state
                    .cache
                    .values()
                    .filter(|entry| {
                        if !entry.content.contains(query) {
                            return false;
                        }
                        if let Some(ref session) = opts.session_filter {
                            if entry.session_id.as_deref() != Some(session.as_str()) {
                                return false;
                            }
                        }
                        true
                    })
                    .cloned()
                    .collect();
                results.sort_by(|a, b| b.timestamp.cmp(&a.timestamp));
                results.truncate(opts.limit);
                debug!(
                    cached_results = results.len(),
                    error = %e,
                    "Search fell back to in-memory cache"
                );
                Ok(results)
            }
        }
    }

    async fn delete(&self, id: &str) -> Result<(), Temm1eError> {
        // Always remove from cache.
        {
            let mut state = self.state.write().await;
            state.cache.remove(id);
        }

        match self.primary.delete(id).await {
            Ok(()) => {
                self.record_success().await;
                Ok(())
            }
            Err(e) => {
                let should_repair = self.record_failure(&e).await;
                if should_repair {
                    warn!("Failure threshold reached — attempting automatic repair");
                    if let Err(e) = self.attempt_repair().await {
                        tracing::warn!(error = %e, "Automatic repair attempt failed");
                    }
                }
                debug!(
                    id = %id,
                    error = %e,
                    "Delete fell back to in-memory cache (removed from cache only)"
                );
                Ok(())
            }
        }
    }

    async fn list_sessions(&self) -> Result<Vec<String>, Temm1eError> {
        match self.primary.list_sessions().await {
            Ok(sessions) => {
                self.record_success().await;
                Ok(sessions)
            }
            Err(e) => {
                let should_repair = self.record_failure(&e).await;
                if should_repair {
                    warn!("Failure threshold reached — attempting automatic repair");
                    if let Err(e) = self.attempt_repair().await {
                        tracing::warn!(error = %e, "Automatic repair attempt failed");
                    }
                }
                // Fall back to sessions derivable from cache.
                let state = self.state.read().await;
                let mut sessions: Vec<String> = state
                    .cache
                    .values()
                    .filter_map(|e| e.session_id.clone())
                    .collect::<std::collections::BTreeSet<_>>()
                    .into_iter()
                    .collect();
                sessions.sort();
                debug!(
                    cached_sessions = sessions.len(),
                    error = %e,
                    "list_sessions fell back to in-memory cache"
                );
                Ok(sessions)
            }
        }
    }

    async fn get_session_history(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, Temm1eError> {
        match self.primary.get_session_history(session_id, limit).await {
            Ok(history) => {
                self.record_success().await;
                Ok(history)
            }
            Err(e) => {
                let should_repair = self.record_failure(&e).await;
                if should_repair {
                    warn!("Failure threshold reached — attempting automatic repair");
                    if let Err(e) = self.attempt_repair().await {
                        tracing::warn!(error = %e, "Automatic repair attempt failed");
                    }
                }
                // Fall back to cache.
                let state = self.state.read().await;
                let mut history: Vec<MemoryEntry> = state
                    .cache
                    .values()
                    .filter(|entry| entry.session_id.as_deref() == Some(session_id))
                    .cloned()
                    .collect();
                history.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
                history.truncate(limit);
                debug!(
                    session_id = %session_id,
                    cached_entries = history.len(),
                    error = %e,
                    "get_session_history fell back to in-memory cache"
                );
                Ok(history)
            }
        }
    }

    fn backend_name(&self) -> &str {
        "resilient"
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use std::sync::atomic::{AtomicBool, Ordering};
    use temm1e_core::MemoryEntryType;

    // -- Test helpers -------------------------------------------------------

    fn make_entry(id: &str, content: &str, session: Option<&str>) -> MemoryEntry {
        MemoryEntry {
            id: id.to_string(),
            content: content.to_string(),
            metadata: serde_json::json!({"source": "test"}),
            timestamp: Utc::now(),
            session_id: session.map(String::from),
            entry_type: MemoryEntryType::Conversation,
        }
    }

    // -- Fake memory backend that can be told to fail -----------------------

    /// A controllable in-memory backend used exclusively in tests.
    struct FakeMemory {
        /// When `true` every operation returns an error.
        should_fail: Arc<AtomicBool>,
        /// The underlying store.
        store: Arc<RwLock<HashMap<String, MemoryEntry>>>,
    }

    impl FakeMemory {
        fn failing(&self) -> bool {
            self.should_fail.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl Memory for FakeMemory {
        async fn store(&self, entry: MemoryEntry) -> Result<(), Temm1eError> {
            if self.failing() {
                return Err(Temm1eError::Memory("fake store failure".into()));
            }
            self.store.write().await.insert(entry.id.clone(), entry);
            Ok(())
        }

        async fn search(
            &self,
            query: &str,
            opts: SearchOpts,
        ) -> Result<Vec<MemoryEntry>, Temm1eError> {
            if self.failing() {
                return Err(Temm1eError::Memory("fake search failure".into()));
            }
            let store = self.store.read().await;
            let mut results: Vec<MemoryEntry> = store
                .values()
                .filter(|e| e.content.contains(query))
                .cloned()
                .collect();
            if let Some(ref session) = opts.session_filter {
                results.retain(|e| e.session_id.as_deref() == Some(session.as_str()));
            }
            results.truncate(opts.limit);
            Ok(results)
        }

        async fn get(&self, id: &str) -> Result<Option<MemoryEntry>, Temm1eError> {
            if self.failing() {
                return Err(Temm1eError::Memory("fake get failure".into()));
            }
            Ok(self.store.read().await.get(id).cloned())
        }

        async fn delete(&self, id: &str) -> Result<(), Temm1eError> {
            if self.failing() {
                return Err(Temm1eError::Memory("fake delete failure".into()));
            }
            self.store.write().await.remove(id);
            Ok(())
        }

        async fn list_sessions(&self) -> Result<Vec<String>, Temm1eError> {
            if self.failing() {
                return Err(Temm1eError::Memory("fake list_sessions failure".into()));
            }
            let store = self.store.read().await;
            let mut sessions: Vec<String> = store
                .values()
                .filter_map(|e| e.session_id.clone())
                .collect::<std::collections::BTreeSet<_>>()
                .into_iter()
                .collect();
            sessions.sort();
            Ok(sessions)
        }

        async fn get_session_history(
            &self,
            session_id: &str,
            limit: usize,
        ) -> Result<Vec<MemoryEntry>, Temm1eError> {
            if self.failing() {
                return Err(Temm1eError::Memory(
                    "fake get_session_history failure".into(),
                ));
            }
            let store = self.store.read().await;
            let mut history: Vec<MemoryEntry> = store
                .values()
                .filter(|e| e.session_id.as_deref() == Some(session_id))
                .cloned()
                .collect();
            history.sort_by(|a, b| a.timestamp.cmp(&b.timestamp));
            history.truncate(limit);
            Ok(history)
        }

        fn backend_name(&self) -> &str {
            "fake"
        }
    }

    // We need a way to share the FakeMemory's fail flag with the test while
    // giving ownership of the FakeMemory to ResilientMemory.  We use Arc for
    // the flag so both sides can control it.

    struct FakeMemoryHandle {
        should_fail: Arc<AtomicBool>,
        store: Arc<RwLock<HashMap<String, MemoryEntry>>>,
    }

    fn create_fake() -> (FakeMemoryHandle, Box<dyn Memory>) {
        let should_fail = Arc::new(AtomicBool::new(false));
        let store = Arc::new(RwLock::new(HashMap::new()));
        let handle = FakeMemoryHandle {
            should_fail: Arc::clone(&should_fail),
            store: Arc::clone(&store),
        };
        let fake = FakeMemory { should_fail, store };
        (handle, Box::new(fake))
    }

    impl FakeMemoryHandle {
        fn set_failing(&self, fail: bool) {
            self.should_fail.store(fail, Ordering::SeqCst);
        }
    }

    // -- Actual tests -------------------------------------------------------

    #[tokio::test]
    async fn passthrough_store_and_get() {
        let (handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        let entry = make_entry("p1", "hello world", None);
        resilient.store(entry).await.unwrap();

        let fetched = resilient.get("p1").await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().content, "hello world");

        // Primary should have the entry too.
        let primary_store = handle.store.read().await;
        assert!(primary_store.contains_key("p1"));
    }

    #[tokio::test]
    async fn passthrough_search() {
        let (_handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        resilient
            .store(make_entry("s1", "Rust programming", None))
            .await
            .unwrap();
        resilient
            .store(make_entry("s2", "Python scripting", None))
            .await
            .unwrap();

        let results = resilient
            .search("Rust", SearchOpts::default())
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].id, "s1");
    }

    #[tokio::test]
    async fn passthrough_delete() {
        let (_handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        resilient
            .store(make_entry("d1", "delete me", None))
            .await
            .unwrap();
        resilient.delete("d1").await.unwrap();

        let fetched = resilient.get("d1").await.unwrap();
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn passthrough_list_sessions() {
        let (_handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        resilient
            .store(make_entry("ls1", "a", Some("alpha")))
            .await
            .unwrap();
        resilient
            .store(make_entry("ls2", "b", Some("beta")))
            .await
            .unwrap();

        let sessions = resilient.list_sessions().await.unwrap();
        assert_eq!(sessions.len(), 2);
        assert!(sessions.contains(&"alpha".to_string()));
        assert!(sessions.contains(&"beta".to_string()));
    }

    #[tokio::test]
    async fn passthrough_session_history() {
        let (_handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        for i in 0..3 {
            let mut entry = make_entry(&format!("h{i}"), &format!("msg {i}"), Some("sess"));
            entry.timestamp = Utc::now() + chrono::Duration::seconds(i as i64);
            resilient.store(entry).await.unwrap();
        }

        let history = resilient.get_session_history("sess", 10).await.unwrap();
        assert_eq!(history.len(), 3);
    }

    #[tokio::test]
    async fn healthy_status_initially() {
        let (_handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        assert_eq!(resilient.health_status().await, MemoryHealthStatus::Healthy);
        assert_eq!(resilient.failure_count().await, 0);
    }

    #[tokio::test]
    async fn store_falls_back_to_cache_on_primary_failure() {
        let (handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        // Make primary fail.
        handle.set_failing(true);

        // Store should still succeed (falls back to cache).
        let entry = make_entry("fb1", "cached content", None);
        resilient.store(entry).await.unwrap();

        // Get should return the cached entry.
        let fetched = resilient.get("fb1").await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().content, "cached content");

        // Failure count should be incremented.
        assert!(resilient.failure_count().await >= 1);
    }

    #[tokio::test]
    async fn get_falls_back_to_cache() {
        let (handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        // Store while healthy.
        resilient
            .store(make_entry("g1", "cached value", None))
            .await
            .unwrap();

        // Now make primary fail.
        handle.set_failing(true);

        // Get should still find the cached entry.
        let fetched = resilient.get("g1").await.unwrap();
        assert!(fetched.is_some());
        assert_eq!(fetched.unwrap().content, "cached value");
    }

    #[tokio::test]
    async fn search_falls_back_to_cache() {
        let (handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        // Store while healthy.
        resilient
            .store(make_entry("sc1", "Rust is great", None))
            .await
            .unwrap();
        resilient
            .store(make_entry("sc2", "Python is fine", None))
            .await
            .unwrap();

        // Now make primary fail.
        handle.set_failing(true);

        let results = resilient
            .search("Rust", SearchOpts::default())
            .await
            .unwrap();
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].content, "Rust is great");
    }

    #[tokio::test]
    async fn delete_removes_from_cache_even_on_failure() {
        let (handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        // Store while healthy.
        resilient
            .store(make_entry("dc1", "delete me", None))
            .await
            .unwrap();

        // Make primary fail, then delete.
        handle.set_failing(true);
        resilient.delete("dc1").await.unwrap();

        // The entry should be gone from cache.
        let fetched = resilient.get("dc1").await.unwrap();
        assert!(fetched.is_none());
    }

    #[tokio::test]
    async fn failure_counter_increments() {
        let (handle, primary) = create_fake();
        let config = FailoverConfig {
            max_consecutive_failures: 10, // high threshold so repair isn't triggered
        };
        let resilient = ResilientMemory::with_config(primary, config);

        handle.set_failing(true);

        // Each failing operation increments the counter.
        for i in 1..=5 {
            let _ = resilient
                .store(make_entry(&format!("fc{i}"), "x", None))
                .await;
            assert_eq!(resilient.failure_count().await, i as u32);
        }
    }

    #[tokio::test]
    async fn failure_counter_resets_on_success() {
        let (handle, primary) = create_fake();
        let config = FailoverConfig {
            max_consecutive_failures: 10,
        };
        let resilient = ResilientMemory::with_config(primary, config);

        handle.set_failing(true);
        resilient.store(make_entry("r1", "x", None)).await.unwrap();
        assert_eq!(resilient.failure_count().await, 1);

        // Recover.
        handle.set_failing(false);
        resilient.store(make_entry("r2", "y", None)).await.unwrap();
        assert_eq!(resilient.failure_count().await, 0);
    }

    #[tokio::test]
    async fn health_status_transitions() {
        let (handle, primary) = create_fake();
        let config = FailoverConfig {
            max_consecutive_failures: 2,
        };
        let resilient = ResilientMemory::with_config(primary, config);

        // Initially healthy.
        assert_eq!(resilient.health_status().await, MemoryHealthStatus::Healthy);

        // One failure -> degraded.
        handle.set_failing(true);
        resilient.store(make_entry("hs1", "x", None)).await.unwrap();
        match resilient.health_status().await {
            MemoryHealthStatus::Degraded { .. } => {}
            other => panic!("Expected Degraded, got {:?}", other),
        }

        // Second failure triggers repair attempt.  Repair will also fail
        // (primary is still broken), transitioning to Failed.
        resilient.store(make_entry("hs2", "y", None)).await.unwrap();
        match resilient.health_status().await {
            MemoryHealthStatus::Failed {
                repair_attempted: true,
            } => {}
            other => panic!("Expected Failed(repair_attempted=true), got {:?}", other),
        }
    }

    #[tokio::test]
    async fn recovery_after_repair() {
        let (handle, primary) = create_fake();
        let config = FailoverConfig {
            max_consecutive_failures: 2,
        };
        let resilient = ResilientMemory::with_config(primary, config);

        // Store one entry while healthy.
        resilient
            .store(make_entry("rec1", "important", None))
            .await
            .unwrap();

        // Break it and cause failures.
        handle.set_failing(true);
        resilient
            .store(make_entry("rec2", "cached only", None))
            .await
            .unwrap();

        assert!(resilient.failure_count().await >= 1);

        // Fix the primary.
        handle.set_failing(false);

        // Manual repair.
        resilient.attempt_repair().await.unwrap();

        assert_eq!(resilient.health_status().await, MemoryHealthStatus::Healthy);
        assert_eq!(resilient.failure_count().await, 0);

        // The cached entry should have been synced to primary.
        let primary_store = handle.store.read().await;
        assert!(primary_store.contains_key("rec2"));
    }

    #[tokio::test]
    async fn repair_fails_when_primary_still_down() {
        let (handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        handle.set_failing(true);

        let result = resilient.attempt_repair().await;
        assert!(result.is_err());

        match resilient.health_status().await {
            MemoryHealthStatus::Failed {
                repair_attempted: true,
            } => {}
            other => panic!("Expected Failed, got {:?}", other),
        }
    }

    #[tokio::test]
    async fn cache_sync_on_recovery() {
        let (handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        // Store multiple entries while primary is down.
        handle.set_failing(true);
        for i in 0..5 {
            resilient
                .store(make_entry(
                    &format!("sync{i}"),
                    &format!("content {i}"),
                    Some("sync_session"),
                ))
                .await
                .unwrap();
        }

        // Recover.
        handle.set_failing(false);
        resilient.attempt_repair().await.unwrap();

        // All entries should now be in the primary.
        let primary_store = handle.store.read().await;
        for i in 0..5 {
            assert!(
                primary_store.contains_key(&format!("sync{i}")),
                "Entry sync{i} missing from primary after sync"
            );
        }
    }

    #[tokio::test]
    async fn list_sessions_falls_back_to_cache() {
        let (handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        // Store while healthy.
        resilient
            .store(make_entry("lsf1", "a", Some("session_x")))
            .await
            .unwrap();

        handle.set_failing(true);

        let sessions = resilient.list_sessions().await.unwrap();
        assert!(sessions.contains(&"session_x".to_string()));
    }

    #[tokio::test]
    async fn session_history_falls_back_to_cache() {
        let (handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);

        for i in 0..3 {
            let mut entry = make_entry(
                &format!("shf{i}"),
                &format!("msg {i}"),
                Some("fallback_sess"),
            );
            entry.timestamp = Utc::now() + chrono::Duration::seconds(i as i64);
            resilient.store(entry).await.unwrap();
        }

        handle.set_failing(true);

        let history = resilient
            .get_session_history("fallback_sess", 10)
            .await
            .unwrap();
        assert_eq!(history.len(), 3);
    }

    #[tokio::test]
    async fn backend_name_is_resilient() {
        let (_handle, primary) = create_fake();
        let resilient = ResilientMemory::new(primary);
        assert_eq!(resilient.backend_name(), "resilient");
    }

    #[tokio::test]
    async fn custom_failover_config() {
        let (handle, primary) = create_fake();
        let config = FailoverConfig {
            max_consecutive_failures: 5,
        };
        let resilient = ResilientMemory::with_config(primary, config);

        handle.set_failing(true);

        // 4 failures should NOT trigger repair.
        for i in 1..=4 {
            resilient
                .store(make_entry(&format!("cf{i}"), "x", None))
                .await
                .unwrap();
        }
        // Should still be degraded, not failed (repair not attempted).
        match resilient.health_status().await {
            MemoryHealthStatus::Degraded { .. } => {}
            other => panic!("Expected Degraded after 4 failures, got {:?}", other),
        }

        // 5th failure should trigger repair.
        resilient.store(make_entry("cf5", "x", None)).await.unwrap();
        match resilient.health_status().await {
            MemoryHealthStatus::Failed {
                repair_attempted: true,
            } => {}
            other => panic!(
                "Expected Failed after 5 failures (threshold), got {:?}",
                other
            ),
        }
    }

    #[tokio::test]
    async fn concurrent_operations_during_failover() {
        let (handle, primary) = create_fake();
        let resilient = Arc::new(ResilientMemory::new(primary));

        // Start with healthy primary.
        let mut tasks = Vec::new();
        for i in 0..10 {
            let r = Arc::clone(&resilient);
            tasks.push(tokio::spawn(async move {
                r.store(make_entry(
                    &format!("conc{i}"),
                    &format!("concurrent {i}"),
                    None,
                ))
                .await
                .unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        // All entries should be accessible.
        for i in 0..10 {
            let fetched = resilient.get(&format!("conc{i}")).await.unwrap();
            assert!(fetched.is_some(), "Entry conc{i} should exist");
        }

        // Now fail primary and do concurrent stores.
        handle.set_failing(true);
        let mut tasks = Vec::new();
        for i in 10..20 {
            let r = Arc::clone(&resilient);
            tasks.push(tokio::spawn(async move {
                r.store(make_entry(
                    &format!("conc{i}"),
                    &format!("fallback {i}"),
                    None,
                ))
                .await
                .unwrap();
            }));
        }
        for t in tasks {
            t.await.unwrap();
        }

        // All fallback entries should be in cache.
        for i in 10..20 {
            let fetched = resilient.get(&format!("conc{i}")).await.unwrap();
            assert!(
                fetched.is_some(),
                "Fallback entry conc{i} should exist in cache"
            );
        }
    }
}
