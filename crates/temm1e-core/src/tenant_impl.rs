use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::RwLock;

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use crate::traits::{Tenant, TenantId};
use crate::types::config::Temm1eConfig;
use crate::types::error::Temm1eError;

// ---------------------------------------------------------------------------
// Configuration types
// ---------------------------------------------------------------------------

/// Per-tenant configuration loaded from the application config.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantConfig {
    /// Unique identifier for this tenant.
    pub tenant_id: String,
    /// List of (channel, user_id) pairs allowed to act as this tenant.
    pub allowed_users: Vec<(String, String)>,
    /// Directory name under `base_path` for this tenant's workspace.
    pub workspace_name: String,
    /// Maximum number of tasks (API calls) allowed per day.
    #[serde(default = "default_max_tasks_per_day")]
    pub max_tasks_per_day: u32,
    /// Maximum storage in megabytes for this tenant.
    #[serde(default = "default_max_storage_mb")]
    pub max_storage_mb: u64,
    /// Maximum API calls allowed per day.
    #[serde(default = "default_max_api_calls_per_day")]
    pub max_api_calls_per_day: u32,
}

fn default_max_tasks_per_day() -> u32 {
    1000
}

fn default_max_storage_mb() -> u64 {
    1024
}

fn default_max_api_calls_per_day() -> u32 {
    5000
}

// ---------------------------------------------------------------------------
// Rate-limit tracking
// ---------------------------------------------------------------------------

/// In-memory rate-limit state for a single tenant.
#[derive(Debug, Clone)]
struct RateLimitState {
    /// Number of API calls made in the current day window.
    call_count: u32,
    /// The day (as days-since-epoch) this counter belongs to.
    day_epoch: u64,
}

impl RateLimitState {
    fn new() -> Self {
        Self {
            call_count: 0,
            day_epoch: current_day_epoch(),
        }
    }
}

/// Returns a monotonic "day number" — seconds since UNIX epoch divided by 86400.
fn current_day_epoch() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        / 86_400
}

// ---------------------------------------------------------------------------
// TenantManager
// ---------------------------------------------------------------------------

/// Concrete implementation of the [`Tenant`] trait providing multi-tenant
/// workspace isolation, user-to-tenant resolution, and per-tenant rate limiting.
pub struct TenantManager {
    /// Root directory under which all tenant workspaces are created.
    base_path: PathBuf,
    /// Map from `tenant_id` to its configuration.
    tenants: HashMap<String, TenantConfig>,
    /// In-memory rate-limit counters keyed by tenant ID string.
    rate_limits: RwLock<HashMap<String, RateLimitState>>,
    /// Fallback tenant ID for users not mapped to any tenant.
    default_tenant: TenantId,
}

impl std::fmt::Debug for TenantManager {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TenantManager")
            .field("base_path", &self.base_path)
            .field("tenants", &self.tenants.keys().collect::<Vec<_>>())
            .field("default_tenant", &self.default_tenant)
            .finish()
    }
}

impl TenantManager {
    /// Create a new `TenantManager` from explicit parts.
    pub fn new(base_path: PathBuf, tenants: Vec<TenantConfig>, default_tenant: TenantId) -> Self {
        let tenant_map: HashMap<String, TenantConfig> = tenants
            .into_iter()
            .map(|t| (t.tenant_id.clone(), t))
            .collect();

        tracing::info!(
            tenant_count = tenant_map.len(),
            base_path = %base_path.display(),
            default = %default_tenant.0,
            "TenantManager initialised"
        );

        Self {
            base_path,
            tenants: tenant_map,
            rate_limits: RwLock::new(HashMap::new()),
            default_tenant,
        }
    }

    /// Return the base path for all tenant workspaces.
    pub fn base_path(&self) -> &Path {
        &self.base_path
    }

    /// Return the default tenant ID.
    pub fn default_tenant_id(&self) -> &TenantId {
        &self.default_tenant
    }

    /// Look up the [`TenantConfig`] for a given tenant ID.
    pub fn tenant_config(&self, tenant_id: &TenantId) -> Option<&TenantConfig> {
        self.tenants.get(&tenant_id.0)
    }

    // -- Workspace isolation ------------------------------------------------

    /// Return the path to the tenant's SQLite memory database.
    pub fn memory_db_path(&self, tenant_id: &TenantId) -> PathBuf {
        self.base_path.join(&tenant_id.0).join("memory.db")
    }

    /// Return the path to the tenant's vault namespace directory.
    pub fn vault_path(&self, tenant_id: &TenantId) -> PathBuf {
        self.base_path.join(&tenant_id.0).join("vault")
    }

    /// Ensure that the workspace directory structure exists for a tenant.
    ///
    /// Creates:
    /// - `{base_path}/{tenant_id}/workspace/`
    /// - `{base_path}/{tenant_id}/memory.db` (parent dir)
    /// - `{base_path}/{tenant_id}/vault/`
    pub fn ensure_workspace(&self, tenant_id: &TenantId) -> Result<(), Temm1eError> {
        let workspace = self.workspace_path(tenant_id);
        let vault = self.vault_path(tenant_id);

        std::fs::create_dir_all(&workspace).map_err(|e| {
            Temm1eError::Io(std::io::Error::new(
                e.kind(),
                format!(
                    "Failed to create workspace directory {}: {e}",
                    workspace.display()
                ),
            ))
        })?;

        std::fs::create_dir_all(&vault).map_err(|e| {
            Temm1eError::Io(std::io::Error::new(
                e.kind(),
                format!("Failed to create vault directory {}: {e}", vault.display()),
            ))
        })?;

        tracing::info!(
            tenant_id = %tenant_id.0,
            workspace = %workspace.display(),
            "Ensured workspace directories exist"
        );

        Ok(())
    }

    // -- Rate-limit helpers -------------------------------------------------

    /// Return the configured daily API-call limit for a tenant, or `u32::MAX`
    /// if the tenant is not in the config (i.e. the default tenant with no
    /// explicit limit).
    fn daily_limit(&self, tenant_id: &TenantId) -> u32 {
        self.tenants
            .get(&tenant_id.0)
            .map(|c| c.max_api_calls_per_day)
            .unwrap_or(u32::MAX)
    }

    /// Return the current call count for a tenant in the current day.
    pub fn current_call_count(&self, tenant_id: &TenantId) -> u32 {
        let limits = self.rate_limits.read().unwrap_or_else(|e| e.into_inner());
        let today = current_day_epoch();
        limits
            .get(&tenant_id.0)
            .filter(|s| s.day_epoch == today)
            .map(|s| s.call_count)
            .unwrap_or(0)
    }
}

#[async_trait]
impl Tenant for TenantManager {
    /// Resolve a (channel, user_id) pair to a [`TenantId`].
    ///
    /// Iterates over all tenant configs and returns the first match.
    /// Falls back to `default_tenant` if no mapping is found.
    async fn resolve_tenant(&self, channel: &str, user_id: &str) -> Result<TenantId, Temm1eError> {
        for config in self.tenants.values() {
            for (ch, uid) in &config.allowed_users {
                if ch == channel && uid == user_id {
                    tracing::debug!(
                        channel,
                        user_id,
                        tenant_id = %config.tenant_id,
                        "Resolved tenant"
                    );
                    return Ok(TenantId(config.tenant_id.clone()));
                }
            }
        }

        tracing::debug!(
            channel,
            user_id,
            tenant_id = %self.default_tenant.0,
            "No tenant mapping found, using default"
        );
        Ok(self.default_tenant.clone())
    }

    /// Return the workspace directory path for a tenant:
    /// `{base_path}/{tenant_id}/workspace`.
    fn workspace_path(&self, tenant_id: &TenantId) -> PathBuf {
        self.base_path.join(&tenant_id.0).join("workspace")
    }

    /// Check whether the tenant is still within its daily API-call rate limit.
    ///
    /// Increments the counter and returns `true` if under the limit, `false`
    /// if the limit has been reached. Counters reset automatically at the
    /// start of a new UTC day.
    async fn check_rate_limit(&self, tenant_id: &TenantId) -> Result<bool, Temm1eError> {
        let limit = self.daily_limit(tenant_id);
        let today = current_day_epoch();

        let mut limits = self
            .rate_limits
            .write()
            .map_err(|e| Temm1eError::Internal(format!("Rate-limit lock poisoned: {e}")))?;

        let state = limits
            .entry(tenant_id.0.clone())
            .or_insert_with(RateLimitState::new);

        // Reset counter if the day has rolled over.
        if state.day_epoch != today {
            state.call_count = 0;
            state.day_epoch = today;
        }

        if state.call_count >= limit {
            tracing::warn!(
                tenant_id = %tenant_id.0,
                limit,
                "Tenant rate limit exceeded"
            );
            return Ok(false);
        }

        state.call_count += 1;
        Ok(true)
    }
}

// ---------------------------------------------------------------------------
// Factory
// ---------------------------------------------------------------------------

/// Create a [`TenantManager`] from the application configuration.
///
/// If no explicit tenant section is present in the config, the manager is
/// initialised with no tenant mappings and the default tenant as fallback.
/// The base path defaults to `./tenants` relative to the current working
/// directory.
pub fn create_tenant_manager(config: &Temm1eConfig) -> TenantManager {
    // For now, we derive the base path and tenants from the config.
    // The TOML config doesn't yet have a `[tenants]` table, so we
    // bootstrap with sensible defaults.  Real tenant configs will be
    // added to Temm1eConfig in a follow-up.
    let base_path = PathBuf::from("./tenants");

    let default_tenant = TenantId::default_tenant();

    // Build a default-tenant config so that `ensure_workspace` works even
    // when tenant_isolation is disabled.
    let default_config = TenantConfig {
        tenant_id: default_tenant.0.clone(),
        allowed_users: Vec::new(),
        workspace_name: default_tenant.0.clone(),
        max_tasks_per_day: default_max_tasks_per_day(),
        max_storage_mb: default_max_storage_mb(),
        max_api_calls_per_day: default_max_api_calls_per_day(),
    };

    let tenants = vec![default_config];

    let manager = TenantManager::new(base_path, tenants, default_tenant);

    if config.temm1e.tenant_isolation {
        tracing::info!("Tenant isolation enabled");
    } else {
        tracing::info!("Tenant isolation disabled — all users map to default tenant");
    }

    manager
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    /// Helper: build a TenantManager with two tenants under a temp directory.
    fn test_manager(base: &Path) -> TenantManager {
        let tenants = vec![
            TenantConfig {
                tenant_id: "acme".to_string(),
                allowed_users: vec![
                    ("telegram".to_string(), "111".to_string()),
                    ("discord".to_string(), "222".to_string()),
                ],
                workspace_name: "acme".to_string(),
                max_tasks_per_day: 100,
                max_storage_mb: 512,
                max_api_calls_per_day: 50,
            },
            TenantConfig {
                tenant_id: "globex".to_string(),
                allowed_users: vec![("slack".to_string(), "333".to_string())],
                workspace_name: "globex".to_string(),
                max_tasks_per_day: 200,
                max_storage_mb: 1024,
                max_api_calls_per_day: 100,
            },
        ];

        TenantManager::new(base.to_path_buf(), tenants, TenantId::default_tenant())
    }

    // -- resolve_tenant tests -----------------------------------------------

    #[tokio::test]
    async fn resolve_tenant_exact_match() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let tid = mgr.resolve_tenant("telegram", "111").await.unwrap();
        assert_eq!(tid.0, "acme");
    }

    #[tokio::test]
    async fn resolve_tenant_second_tenant() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let tid = mgr.resolve_tenant("slack", "333").await.unwrap();
        assert_eq!(tid.0, "globex");
    }

    #[tokio::test]
    async fn resolve_tenant_multiple_channels_same_tenant() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let t1 = mgr.resolve_tenant("telegram", "111").await.unwrap();
        let t2 = mgr.resolve_tenant("discord", "222").await.unwrap();
        assert_eq!(t1.0, t2.0);
        assert_eq!(t1.0, "acme");
    }

    #[tokio::test]
    async fn resolve_tenant_falls_back_to_default() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let tid = mgr.resolve_tenant("whatsapp", "999").await.unwrap();
        assert_eq!(tid.0, "default");
    }

    #[tokio::test]
    async fn resolve_tenant_wrong_channel_falls_back() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        // User 111 is mapped on telegram, NOT on slack
        let tid = mgr.resolve_tenant("slack", "111").await.unwrap();
        assert_eq!(tid.0, "default");
    }

    // -- workspace_path tests -----------------------------------------------

    #[test]
    fn workspace_path_format() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let tid = TenantId("acme".to_string());
        let path = mgr.workspace_path(&tid);

        assert_eq!(path, dir.path().join("acme").join("workspace"));
    }

    #[test]
    fn workspace_paths_are_isolated() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let p1 = mgr.workspace_path(&TenantId("acme".to_string()));
        let p2 = mgr.workspace_path(&TenantId("globex".to_string()));
        let p3 = mgr.workspace_path(&TenantId::default_tenant());

        assert_ne!(p1, p2);
        assert_ne!(p1, p3);
        assert_ne!(p2, p3);

        // Each contains its own tenant ID segment
        assert!(p1.to_str().unwrap().contains("acme"));
        assert!(p2.to_str().unwrap().contains("globex"));
        assert!(p3.to_str().unwrap().contains("default"));
    }

    // -- ensure_workspace tests ---------------------------------------------

    #[test]
    fn ensure_workspace_creates_directories() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let tid = TenantId("acme".to_string());
        mgr.ensure_workspace(&tid).unwrap();

        assert!(mgr.workspace_path(&tid).is_dir());
        assert!(mgr.vault_path(&tid).is_dir());
        // memory.db parent dir should exist
        assert!(mgr.memory_db_path(&tid).parent().unwrap().is_dir());
    }

    #[test]
    fn ensure_workspace_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let tid = TenantId("globex".to_string());
        mgr.ensure_workspace(&tid).unwrap();
        // Second call must not fail
        mgr.ensure_workspace(&tid).unwrap();

        assert!(mgr.workspace_path(&tid).is_dir());
    }

    // -- rate_limit tests ---------------------------------------------------

    #[tokio::test]
    async fn rate_limit_allows_under_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let tid = TenantId("acme".to_string()); // limit = 50

        for _ in 0..50 {
            assert!(mgr.check_rate_limit(&tid).await.unwrap());
        }
    }

    #[tokio::test]
    async fn rate_limit_blocks_at_limit() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let tid = TenantId("acme".to_string()); // limit = 50

        // Exhaust the limit
        for _ in 0..50 {
            mgr.check_rate_limit(&tid).await.unwrap();
        }

        // 51st call should be blocked
        assert!(!mgr.check_rate_limit(&tid).await.unwrap());
    }

    #[tokio::test]
    async fn rate_limit_independent_per_tenant() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let acme = TenantId("acme".to_string()); // limit = 50
        let globex = TenantId("globex".to_string()); // limit = 100

        // Exhaust acme's limit
        for _ in 0..50 {
            mgr.check_rate_limit(&acme).await.unwrap();
        }
        assert!(!mgr.check_rate_limit(&acme).await.unwrap());

        // globex should still be fine
        assert!(mgr.check_rate_limit(&globex).await.unwrap());
    }

    #[tokio::test]
    async fn rate_limit_default_tenant_unlimited() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        // default tenant is not in the config map of test_manager,
        // so daily_limit returns u32::MAX
        let tid = TenantId("unknown".to_string());

        // Should never hit the limit in practice
        for _ in 0..1000 {
            assert!(mgr.check_rate_limit(&tid).await.unwrap());
        }
    }

    // -- memory_db_path / vault_path tests ----------------------------------

    #[test]
    fn memory_db_path_format() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let tid = TenantId("acme".to_string());
        let path = mgr.memory_db_path(&tid);
        assert_eq!(path, dir.path().join("acme").join("memory.db"));
    }

    #[test]
    fn vault_path_format() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let tid = TenantId("acme".to_string());
        let path = mgr.vault_path(&tid);
        assert_eq!(path, dir.path().join("acme").join("vault"));
    }

    // -- factory tests ------------------------------------------------------

    #[test]
    fn factory_creates_manager_with_defaults() {
        let config = Temm1eConfig::default();
        let mgr = create_tenant_manager(&config);

        assert_eq!(mgr.default_tenant_id().0, "default");
        // Default tenant config should be present
        let default_cfg = mgr.tenant_config(&TenantId::default_tenant());
        assert!(default_cfg.is_some());
    }

    #[test]
    fn factory_with_tenant_isolation_enabled() {
        let config = Temm1eConfig {
            temm1e: crate::types::config::Temm1eSection {
                tenant_isolation: true,
                ..Default::default()
            },
            ..Default::default()
        };

        let mgr = create_tenant_manager(&config);
        assert_eq!(mgr.default_tenant_id().0, "default");
    }

    // -- TenantConfig serde tests -------------------------------------------

    #[test]
    fn tenant_config_serde_roundtrip() {
        let cfg = TenantConfig {
            tenant_id: "acme".to_string(),
            allowed_users: vec![("telegram".to_string(), "111".to_string())],
            workspace_name: "acme-workspace".to_string(),
            max_tasks_per_day: 500,
            max_storage_mb: 2048,
            max_api_calls_per_day: 10_000,
        };

        let json = serde_json::to_string(&cfg).unwrap();
        let restored: TenantConfig = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.tenant_id, "acme");
        assert_eq!(restored.allowed_users.len(), 1);
        assert_eq!(restored.max_api_calls_per_day, 10_000);
    }

    // -- current_call_count -------------------------------------------------

    #[tokio::test]
    async fn current_call_count_tracks_calls() {
        let dir = tempfile::tempdir().unwrap();
        let mgr = test_manager(dir.path());

        let tid = TenantId("acme".to_string());
        assert_eq!(mgr.current_call_count(&tid), 0);

        mgr.check_rate_limit(&tid).await.unwrap();
        mgr.check_rate_limit(&tid).await.unwrap();
        mgr.check_rate_limit(&tid).await.unwrap();

        assert_eq!(mgr.current_call_count(&tid), 3);
    }
}
