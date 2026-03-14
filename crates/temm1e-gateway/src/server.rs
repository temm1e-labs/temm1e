//! SkyGate server — axum-based HTTP server with health/status routes,
//! WebSocket upgrade support, and shared application state.

use std::sync::Arc;

use axum::routing::get;
use axum::Router;
use temm1e_agent::AgentRuntime;
use temm1e_core::types::config::GatewayConfig;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::Channel;
use tokio::net::TcpListener;
use tracing::info;

use crate::dashboard::{dashboard_config, dashboard_health, dashboard_page, dashboard_tasks};
use crate::health::{health_handler, status_handler};
use crate::identity::{oauth_callback_handler, OAuthIdentityManager};
use crate::session::SessionManager;

/// Shared application state accessible from all handlers.
pub struct AppState {
    pub channels: Vec<Arc<dyn Channel>>,
    pub agent: Arc<AgentRuntime>,
    pub config: GatewayConfig,
    pub sessions: SessionManager,
    pub identity: Option<Arc<OAuthIdentityManager>>,
}

/// The main SkyGate server.
pub struct SkyGate {
    state: Arc<AppState>,
}

impl SkyGate {
    /// Create a new SkyGate server.
    pub fn new(
        channels: Vec<Arc<dyn Channel>>,
        agent: Arc<AgentRuntime>,
        config: GatewayConfig,
    ) -> Self {
        let state = Arc::new(AppState {
            channels,
            agent,
            config,
            sessions: SessionManager::new(),
            identity: None,
        });
        Self { state }
    }

    /// Create a new SkyGate server with an OAuth identity manager.
    pub fn with_identity(
        channels: Vec<Arc<dyn Channel>>,
        agent: Arc<AgentRuntime>,
        config: GatewayConfig,
        identity: OAuthIdentityManager,
    ) -> Self {
        let state = Arc::new(AppState {
            channels,
            agent,
            config,
            sessions: SessionManager::new(),
            identity: Some(Arc::new(identity)),
        });
        Self { state }
    }

    /// Build the axum Router with all routes.
    fn build_router(&self) -> Router {
        let mut router = Router::new()
            .route("/health", get(health_handler))
            .route("/status", get(status_handler))
            .route("/dashboard", get(dashboard_page))
            .route("/dashboard/api/health", get(dashboard_health))
            .route("/dashboard/api/tasks", get(dashboard_tasks))
            .route("/dashboard/api/config", get(dashboard_config))
            .with_state(self.state.clone());

        // Mount OAuth callback when identity is configured
        if let Some(ref identity) = self.state.identity {
            let auth_router = Router::new()
                .route("/auth/callback", get(oauth_callback_handler))
                .with_state(identity.clone());
            router = router.merge(auth_router);
        }

        router
    }

    /// Start the server, binding to the configured host and port.
    pub async fn start(&self) -> Result<(), Temm1eError> {
        let addr = format!("{}:{}", self.state.config.host, self.state.config.port);
        info!(addr = %addr, "Starting SkyGate server");

        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| Temm1eError::Internal(format!("Failed to bind to {}: {}", addr, e)))?;

        let router = self.build_router();

        axum::serve(listener, router)
            .await
            .map_err(|e| Temm1eError::Internal(format!("Server error: {}", e)))?;

        Ok(())
    }

    /// Get a reference to the shared application state.
    pub fn state(&self) -> &Arc<AppState> {
        &self.state
    }

    /// Get a reference to the session manager.
    pub fn sessions(&self) -> &SessionManager {
        &self.state.sessions
    }
}
