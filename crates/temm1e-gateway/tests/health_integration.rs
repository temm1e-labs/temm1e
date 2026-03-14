//! Integration test: start the gateway server and hit the /health endpoint.

use std::sync::Arc;

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::routing::get;
use axum::Router;
use tower::ServiceExt;

use temm1e_agent::AgentRuntime;
use temm1e_core::types::config::GatewayConfig;
use temm1e_gateway::health::health_handler;
use temm1e_gateway::server::AppState;
use temm1e_gateway::session::SessionManager;
use temm1e_test_utils::{MockMemory, MockProvider};

fn make_test_state() -> Arc<AppState> {
    let provider = Arc::new(MockProvider::with_text("test"));
    let memory = Arc::new(MockMemory::new());
    let agent = Arc::new(AgentRuntime::new(
        provider,
        memory,
        vec![],
        "test-model".to_string(),
        None,
    ));

    Arc::new(AppState {
        channels: vec![],
        agent,
        config: GatewayConfig::default(),
        sessions: SessionManager::new(),
        identity: None,
    })
}

#[tokio::test]
async fn health_endpoint_returns_200_json() {
    let state = make_test_state();

    let app = Router::new()
        .route("/health", get(health_handler))
        .with_state(state);

    let req = Request::builder()
        .uri("/health")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::OK);

    let body = axum::body::to_bytes(resp.into_body(), usize::MAX)
        .await
        .unwrap();
    let json: serde_json::Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(json["status"], "ok");
    assert!(json["version"].is_string());
    assert!(json["uptime_seconds"].is_number());
}

#[tokio::test]
async fn nonexistent_route_returns_404() {
    let state = make_test_state();

    let app = Router::new()
        .route("/health", get(health_handler))
        .with_state(state);

    let req = Request::builder()
        .uri("/nonexistent")
        .body(Body::empty())
        .unwrap();

    let resp = app.oneshot(req).await.unwrap();
    assert_eq!(resp.status(), StatusCode::NOT_FOUND);
}
