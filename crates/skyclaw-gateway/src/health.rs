//! Health endpoint handler — returns JSON health/status information.

use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use serde::Serialize;

#[derive(Serialize)]
pub struct HealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub uptime_seconds: u64,
}

#[derive(Serialize)]
pub struct StatusResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub provider: String,
    pub channels: Vec<String>,
    pub tools: Vec<String>,
    pub memory_backend: String,
}

/// Handler for GET /health
pub async fn health_handler() -> impl IntoResponse {
    let resp = HealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        uptime_seconds: 0, // placeholder; real uptime would come from shared state
    };
    (StatusCode::OK, Json(resp))
}

/// Handler for GET /status — provides detailed status including provider/channels/tools.
/// This version uses the shared AppState.
pub async fn status_handler(
    axum::extract::State(state): axum::extract::State<std::sync::Arc<crate::server::AppState>>,
) -> impl IntoResponse {
    let channel_names: Vec<String> = state
        .channels
        .iter()
        .map(|c| c.name().to_string())
        .collect();

    let tool_names: Vec<String> = state
        .agent
        .tools()
        .iter()
        .map(|t| t.name().to_string())
        .collect();

    let resp = StatusResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        provider: state.agent.provider().name().to_string(),
        channels: channel_names,
        tools: tool_names,
        memory_backend: state.agent.memory().backend_name().to_string(),
    };
    (StatusCode::OK, Json(resp))
}
