//! SkyGate server — axum-based HTTP server with health/status routes,
//! WebSocket upgrade support, and shared application state.

use std::sync::Arc;

use axum::Router;
use axum::routing::get;
use skyclaw_agent::AgentRuntime;
use skyclaw_core::Channel;
use skyclaw_core::types::config::GatewayConfig;
use skyclaw_core::types::error::SkyclawError;
use tokio::net::TcpListener;
use tracing::info;

use crate::health::{health_handler, status_handler};
use crate::session::SessionManager;

/// Shared application state accessible from all handlers.
pub struct AppState {
    pub channels: Vec<Arc<dyn Channel>>,
    pub agent: Arc<AgentRuntime>,
    pub config: GatewayConfig,
    pub sessions: SessionManager,
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
        });
        Self { state }
    }

    /// Build the axum Router with all routes.
    fn build_router(&self) -> Router {
        Router::new()
            .route("/health", get(health_handler))
            .route("/status", get(status_handler))
            .with_state(self.state.clone())
    }

    /// Start the server, binding to the configured host and port.
    pub async fn start(&self) -> Result<(), SkyclawError> {
        let addr = format!("{}:{}", self.state.config.host, self.state.config.port);
        info!(addr = %addr, "Starting SkyGate server");

        let listener = TcpListener::bind(&addr)
            .await
            .map_err(|e| SkyclawError::Internal(format!("Failed to bind to {}: {}", addr, e)))?;

        let router = self.build_router();

        axum::serve(listener, router)
            .await
            .map_err(|e| SkyclawError::Internal(format!("Server error: {}", e)))?;

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
