//! Web dashboard — read-only HTML overview with HTMX live updates and JSON API endpoints.

use std::sync::Arc;

use axum::extract::State;
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse};
use axum::Json;
use serde::Serialize;

use crate::server::AppState;

// ---------------------------------------------------------------------------
// JSON response types
// ---------------------------------------------------------------------------

#[derive(Debug, Serialize)]
pub struct DashboardHealthResponse {
    pub status: &'static str,
    pub version: &'static str,
    pub provider: ProviderStatus,
    pub memory: MemoryStatus,
    pub channels: Vec<ChannelStatus>,
}

#[derive(Debug, Serialize)]
pub struct ProviderStatus {
    pub name: String,
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct MemoryStatus {
    pub backend: String,
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct ChannelStatus {
    pub name: String,
    pub status: &'static str,
}

#[derive(Debug, Serialize)]
pub struct TaskEntry {
    pub id: String,
    pub chat_id: String,
    pub description: String,
    pub status: String,
}

#[derive(Debug, Serialize)]
pub struct DashboardTasksResponse {
    pub tasks: Vec<TaskEntry>,
}

#[derive(Debug, Serialize)]
pub struct DashboardConfigResponse {
    pub provider: String,
    pub model: String,
    pub memory_backend: String,
    pub agent: AgentConfigSummary,
    pub tools: Vec<String>,
    pub channels: Vec<String>,
    pub gateway: GatewayConfigSummary,
}

#[derive(Debug, Serialize)]
pub struct AgentConfigSummary {
    pub max_turns: usize,
    pub max_context_tokens: usize,
    pub max_tool_rounds: usize,
    pub max_task_duration_secs: u64,
}

#[derive(Debug, Serialize)]
pub struct GatewayConfigSummary {
    pub host: String,
    pub port: u16,
    pub tls: bool,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

/// GET /dashboard — serves the HTML dashboard page with embedded HTMX.
pub async fn dashboard_page(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let version = env!("CARGO_PKG_VERSION");
    let provider_name = state.agent.provider().name().to_string();
    let memory_backend = state.agent.memory().backend_name().to_string();
    let model = state.agent.model().to_string();

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

    let channels_html: String = if channel_names.is_empty() {
        "<li class=\"item\">No channels configured</li>".to_string()
    } else {
        channel_names
            .iter()
            .map(|name| {
                format!(
                    "<li class=\"item\"><span class=\"badge ok\">connected</span> {}</li>",
                    html_escape(name)
                )
            })
            .collect::<Vec<_>>()
            .join("\n              ")
    };

    let tools_html: String = if tool_names.is_empty() {
        "<li class=\"item\">No tools registered</li>".to_string()
    } else {
        tool_names
            .iter()
            .map(|name| {
                format!(
                    "<li class=\"item\"><span class=\"badge ok\">active</span> {}</li>",
                    html_escape(name)
                )
            })
            .collect::<Vec<_>>()
            .join("\n              ")
    };

    let html = format!(
        r##"<!DOCTYPE html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>TEMM1E Dashboard</title>
  <script src="https://unpkg.com/htmx.org@1.9.12"></script>
  <style>
    :root {{
      --bg: #0f1117;
      --card: #1a1d27;
      --border: #2a2d3a;
      --text: #e0e0e8;
      --muted: #8888a0;
      --accent: #6c8cff;
      --ok: #22c55e;
      --warn: #eab308;
      --err: #ef4444;
    }}
    * {{ margin: 0; padding: 0; box-sizing: border-box; }}
    body {{
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", Roboto, monospace;
      background: var(--bg);
      color: var(--text);
      padding: 2rem;
      max-width: 1200px;
      margin: 0 auto;
    }}
    h1 {{ font-size: 1.5rem; margin-bottom: 0.25rem; }}
    .subtitle {{ color: var(--muted); font-size: 0.875rem; margin-bottom: 2rem; }}
    .grid {{
      display: grid;
      grid-template-columns: repeat(auto-fit, minmax(340px, 1fr));
      gap: 1.25rem;
    }}
    .card {{
      background: var(--card);
      border: 1px solid var(--border);
      border-radius: 8px;
      padding: 1.25rem;
    }}
    .card h2 {{
      font-size: 1rem;
      margin-bottom: 1rem;
      color: var(--accent);
    }}
    .item {{
      display: flex;
      align-items: center;
      gap: 0.5rem;
      padding: 0.4rem 0;
      font-size: 0.875rem;
      list-style: none;
    }}
    .badge {{
      display: inline-block;
      padding: 0.15rem 0.5rem;
      border-radius: 4px;
      font-size: 0.75rem;
      font-weight: 600;
      text-transform: uppercase;
    }}
    .badge.ok {{ background: #22c55e22; color: var(--ok); }}
    .badge.warn {{ background: #eab30822; color: var(--warn); }}
    .badge.err {{ background: #ef444422; color: var(--err); }}
    dl {{ display: grid; grid-template-columns: auto 1fr; gap: 0.3rem 1rem; font-size: 0.875rem; }}
    dt {{ color: var(--muted); }}
    dd {{ font-weight: 500; }}
    .health-indicator {{
      display: inline-block;
      width: 8px; height: 8px;
      border-radius: 50%;
      background: var(--ok);
      margin-right: 0.5rem;
    }}
    .health-indicator.err {{ background: var(--err); }}
    #health-status {{ font-size: 0.875rem; }}
    .footer {{ margin-top: 2rem; color: var(--muted); font-size: 0.75rem; text-align: center; }}
  </style>
</head>
<body>
  <h1>TEMM1E Dashboard</h1>
  <p class="subtitle">v{version} &mdash; read-only system overview</p>

  <div class="grid">

    <div class="card">
      <h2>System Health</h2>
      <div id="health-status"
           hx-get="/dashboard/api/health"
           hx-trigger="load, every 10s"
           hx-swap="innerHTML">
        Loading...
      </div>
    </div>

    <div class="card">
      <h2>Channels</h2>
      <ul>
        {channels_html}
      </ul>
    </div>

    <div class="card">
      <h2>Agent Configuration</h2>
      <dl>
        <dt>Provider</dt>  <dd>{provider_escaped}</dd>
        <dt>Model</dt>     <dd>{model_escaped}</dd>
        <dt>Memory</dt>    <dd>{memory_escaped}</dd>
        <dt>Max Turns</dt> <dd>{max_turns}</dd>
        <dt>Max Context Tokens</dt> <dd>{max_context_tokens}</dd>
        <dt>Max Tool Rounds</dt>    <dd>{max_tool_rounds}</dd>
        <dt>Max Task Duration</dt>  <dd>{max_task_duration_secs}s</dd>
      </dl>
    </div>

    <div class="card">
      <h2>Tools</h2>
      <ul>
        {tools_html}
      </ul>
    </div>

    <div class="card">
      <h2>Active Tasks</h2>
      <div id="tasks-list"
           hx-get="/dashboard/api/tasks"
           hx-trigger="load, every 10s"
           hx-swap="innerHTML">
        Loading...
      </div>
    </div>

    <div class="card">
      <h2>Gateway</h2>
      <dl>
        <dt>Host</dt> <dd>{gw_host}</dd>
        <dt>Port</dt> <dd>{gw_port}</dd>
        <dt>TLS</dt>  <dd>{gw_tls}</dd>
      </dl>
    </div>

  </div>

  <p class="footer">TEMM1E Agent Runtime &mdash; Dashboard is read-only</p>
</body>
</html>"##,
        version = html_escape(version),
        channels_html = channels_html,
        tools_html = tools_html,
        provider_escaped = html_escape(&provider_name),
        model_escaped = html_escape(&model),
        memory_escaped = html_escape(&memory_backend),
        max_turns = state.agent.max_turns(),
        max_context_tokens = state.agent.max_context_tokens(),
        max_tool_rounds = state.agent.max_tool_rounds(),
        max_task_duration_secs = state.agent.max_task_duration().as_secs(),
        gw_host = html_escape(&state.config.host),
        gw_port = state.config.port,
        gw_tls = if state.config.tls {
            "enabled"
        } else {
            "disabled"
        },
    );

    (StatusCode::OK, Html(html))
}

/// GET /dashboard/api/health — JSON health data for HTMX polling.
pub async fn dashboard_health(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let provider_name = state.agent.provider().name().to_string();
    let memory_backend = state.agent.memory().backend_name().to_string();

    let channels: Vec<ChannelStatus> = state
        .channels
        .iter()
        .map(|c| ChannelStatus {
            name: c.name().to_string(),
            status: "connected",
        })
        .collect();

    let resp = DashboardHealthResponse {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
        provider: ProviderStatus {
            name: provider_name,
            status: "ok",
        },
        memory: MemoryStatus {
            backend: memory_backend,
            status: "ok",
        },
        channels,
    };

    (StatusCode::OK, Json(resp))
}

/// GET /dashboard/api/tasks — JSON list of active tasks.
pub async fn dashboard_tasks(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let tasks = if let Some(tq) = state.agent.task_queue() {
        match tq.load_incomplete().await {
            Ok(entries) => entries
                .into_iter()
                .map(|t| TaskEntry {
                    id: t.task_id,
                    chat_id: t.chat_id,
                    description: t.goal,
                    status: format!("{:?}", t.status),
                })
                .collect(),
            Err(_) => Vec::new(),
        }
    } else {
        Vec::new()
    };

    let resp = DashboardTasksResponse { tasks };
    (StatusCode::OK, Json(resp))
}

/// GET /dashboard/api/config — JSON config overview with secrets redacted.
pub async fn dashboard_config(State(state): State<Arc<AppState>>) -> impl IntoResponse {
    let provider_name = state.agent.provider().name().to_string();
    let model = state.agent.model().to_string();
    let memory_backend = state.agent.memory().backend_name().to_string();

    let tool_names: Vec<String> = state
        .agent
        .tools()
        .iter()
        .map(|t| t.name().to_string())
        .collect();

    let channel_names: Vec<String> = state
        .channels
        .iter()
        .map(|c| c.name().to_string())
        .collect();

    let resp = DashboardConfigResponse {
        provider: provider_name,
        model,
        memory_backend,
        agent: AgentConfigSummary {
            max_turns: state.agent.max_turns(),
            max_context_tokens: state.agent.max_context_tokens(),
            max_tool_rounds: state.agent.max_tool_rounds(),
            max_task_duration_secs: state.agent.max_task_duration().as_secs(),
        },
        tools: tool_names,
        channels: channel_names,
        gateway: GatewayConfigSummary {
            host: state.config.host.clone(),
            port: state.config.port,
            tls: state.config.tls,
        },
    };

    (StatusCode::OK, Json(resp))
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Minimal HTML escaping to prevent XSS in rendered values.
fn html_escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    use axum::body::Body;
    use axum::http::Request;
    use axum::routing::get;
    use axum::Router;
    use tower::ServiceExt;

    use serde_json::Value;
    use temm1e_agent::AgentRuntime;
    use temm1e_core::types::config::GatewayConfig;
    use temm1e_test_utils::{MockChannel, MockMemory, MockProvider};

    use crate::session::SessionManager;

    /// Build a test AppState with mocks.
    fn test_state() -> Arc<AppState> {
        let provider = Arc::new(MockProvider::with_text("test"));
        let memory = Arc::new(MockMemory::new());
        let tools: Vec<Arc<dyn temm1e_core::Tool>> = Vec::new();
        let agent = Arc::new(AgentRuntime::new(
            provider,
            memory,
            tools,
            "test-model".to_string(),
            None,
        ));
        let channel: Arc<dyn temm1e_core::Channel> = Arc::new(MockChannel::new("telegram"));
        Arc::new(AppState {
            channels: vec![channel],
            agent,
            config: GatewayConfig {
                host: "127.0.0.1".to_string(),
                port: 8080,
                tls: false,
                tls_cert: None,
                tls_key: None,
            },
            sessions: SessionManager::new(),
            identity: None,
        })
    }

    /// Build a test router with all dashboard routes.
    fn test_router(state: Arc<AppState>) -> Router {
        Router::new()
            .route("/dashboard", get(dashboard_page))
            .route("/dashboard/api/health", get(dashboard_health))
            .route("/dashboard/api/tasks", get(dashboard_tasks))
            .route("/dashboard/api/config", get(dashboard_config))
            .with_state(state)
    }

    // ── Test 1: Dashboard HTML page returns 200 ─────────────────────────

    #[tokio::test]
    async fn dashboard_page_returns_200_with_html() {
        let state = test_state();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/dashboard")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        let html = String::from_utf8(body.to_vec()).unwrap();
        assert!(html.contains("<!DOCTYPE html>"));
        assert!(html.contains("TEMM1E Dashboard"));
        assert!(html.contains("htmx.org"));
        assert!(html.contains("telegram"));
    }

    // ── Test 2: Dashboard HTML is under 50KB ────────────────────────────

    #[tokio::test]
    async fn dashboard_page_is_under_50kb() {
        let state = test_state();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/dashboard")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(resp.into_body(), 1_000_000)
            .await
            .unwrap();
        assert!(
            body.len() < 50_000,
            "Dashboard HTML is {} bytes, expected < 50000",
            body.len()
        );
    }

    // ── Test 3: Health API returns valid JSON ────────────────────────────

    #[tokio::test]
    async fn dashboard_health_returns_valid_json() {
        let state = test_state();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/dashboard/api/health")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 100_000)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert_eq!(json["status"], "ok");
        assert!(json["version"].is_string());
        assert_eq!(json["provider"]["name"], "mock");
        assert_eq!(json["provider"]["status"], "ok");
        assert_eq!(json["memory"]["backend"], "mock");
        assert!(json["channels"].is_array());
        assert_eq!(json["channels"][0]["name"], "telegram");
    }

    // ── Test 4: Tasks API returns valid JSON ────────────────────────────

    #[tokio::test]
    async fn dashboard_tasks_returns_valid_json() {
        let state = test_state();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/dashboard/api/tasks")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 100_000)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();
        assert!(json["tasks"].is_array());
    }

    // ── Test 5: Config API returns valid JSON with no secrets ───────────

    #[tokio::test]
    async fn dashboard_config_returns_valid_json_without_secrets() {
        let state = test_state();
        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/dashboard/api/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(resp.status(), StatusCode::OK);
        let body = axum::body::to_bytes(resp.into_body(), 100_000)
            .await
            .unwrap();
        let body_str = String::from_utf8(body.to_vec()).unwrap();
        let json: Value = serde_json::from_str(&body_str).unwrap();

        // Verify structure
        assert_eq!(json["provider"], "mock");
        assert_eq!(json["model"], "test-model");
        assert_eq!(json["memory_backend"], "mock");
        assert!(json["agent"]["max_turns"].is_number());
        assert!(json["agent"]["max_context_tokens"].is_number());
        assert!(json["agent"]["max_tool_rounds"].is_number());
        assert!(json["agent"]["max_task_duration_secs"].is_number());
        assert!(json["tools"].is_array());
        assert!(json["channels"].is_array());
        assert_eq!(json["gateway"]["host"], "127.0.0.1");
        assert_eq!(json["gateway"]["port"], 8080);

        // No API keys or secret tokens anywhere in the response
        assert!(!body_str.contains("api_key"));
        assert!(!body_str.contains("\"token\""));
        assert!(!body_str.contains("sk-"));
    }

    // ── Test 6: Config API shows correct agent limits ───────────────────

    #[tokio::test]
    async fn dashboard_config_shows_correct_agent_limits() {
        let provider = Arc::new(MockProvider::with_text("test"));
        let memory = Arc::new(MockMemory::new());
        let tools: Vec<Arc<dyn temm1e_core::Tool>> = Vec::new();
        let agent = Arc::new(AgentRuntime::with_limits(
            provider,
            memory,
            tools,
            "custom-model".to_string(),
            None,
            50,
            20_000,
            100,
            600,
            1.0,
        ));
        let state = Arc::new(AppState {
            channels: Vec::new(),
            agent,
            config: GatewayConfig::default(),
            sessions: SessionManager::new(),
            identity: None,
        });

        let app = test_router(state);

        let resp = app
            .oneshot(
                Request::builder()
                    .uri("/dashboard/api/config")
                    .body(Body::empty())
                    .unwrap(),
            )
            .await
            .unwrap();

        let body = axum::body::to_bytes(resp.into_body(), 100_000)
            .await
            .unwrap();
        let json: Value = serde_json::from_slice(&body).unwrap();

        assert_eq!(json["model"], "custom-model");
        assert_eq!(json["agent"]["max_turns"], 50);
        assert_eq!(json["agent"]["max_context_tokens"], 20_000);
        assert_eq!(json["agent"]["max_tool_rounds"], 100);
        assert_eq!(json["agent"]["max_task_duration_secs"], 600);
    }

    // ── Test 7: HTML escaping prevents XSS ──────────────────────────────

    #[test]
    fn html_escape_prevents_xss() {
        let malicious = "<script>alert('xss')</script>";
        let escaped = html_escape(malicious);
        assert!(!escaped.contains('<'));
        assert!(!escaped.contains('>'));
        assert!(escaped.contains("&lt;"));
        assert!(escaped.contains("&gt;"));
    }

    // ── Test 8: Health response serializes correctly ─────────────────────

    #[test]
    fn health_response_serializes_correctly() {
        let resp = DashboardHealthResponse {
            status: "ok",
            version: "1.0.0",
            provider: ProviderStatus {
                name: "anthropic".to_string(),
                status: "ok",
            },
            memory: MemoryStatus {
                backend: "sqlite".to_string(),
                status: "ok",
            },
            channels: vec![ChannelStatus {
                name: "telegram".to_string(),
                status: "connected",
            }],
        };

        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["provider"]["name"], "anthropic");
        assert_eq!(json["memory"]["backend"], "sqlite");
        assert_eq!(json["channels"][0]["name"], "telegram");
    }
}
