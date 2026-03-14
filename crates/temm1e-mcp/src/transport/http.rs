//! HTTP transport — connects to a remote MCP server via Streamable HTTP.
//!
//! Uses HTTP POST for JSON-RPC requests and receives responses directly.
//! Session management via `Mcp-Session-Id` header.

use crate::jsonrpc::{JsonRpcNotification, JsonRpcRequest, JsonRpcResponse};
use crate::transport::Transport;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use temm1e_core::types::error::Temm1eError;
use tokio::sync::RwLock;
use tracing::{debug, warn};

pub struct HttpTransport {
    url: String,
    client: reqwest::Client,
    next_id: AtomicU64,
    session_id: Arc<RwLock<Option<String>>>,
    alive: AtomicBool,
    extra_headers: HashMap<String, String>,
    server_name: String,
}

impl HttpTransport {
    pub fn new(
        server_name: &str,
        url: &str,
        timeout: Duration,
        extra_headers: HashMap<String, String>,
    ) -> Result<Self, Temm1eError> {
        let client = reqwest::Client::builder()
            .timeout(timeout)
            .build()
            .map_err(|e| Temm1eError::Tool(format!("Failed to create HTTP client: {}", e)))?;

        Ok(Self {
            url: url.to_string(),
            client,
            next_id: AtomicU64::new(1),
            session_id: Arc::new(RwLock::new(None)),
            alive: AtomicBool::new(true),
            extra_headers,
            server_name: server_name.to_string(),
        })
    }

    /// Build the HTTP request with common headers.
    fn build_request(&self, body: &str) -> Result<reqwest::RequestBuilder, Temm1eError> {
        let mut builder = self
            .client
            .post(&self.url)
            .header("Content-Type", "application/json")
            .header("Accept", "application/json, text/event-stream");

        for (key, value) in &self.extra_headers {
            builder = builder.header(key.as_str(), value.as_str());
        }

        // Don't block on reading the session_id — use try_read
        // If we can't read it, skip the header (it's optional for the first request)

        Ok(builder.body(body.to_string()))
    }
}

#[async_trait]
impl Transport for HttpTransport {
    async fn send(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<JsonRpcResponse, Temm1eError> {
        if !self.alive.load(Ordering::Relaxed) {
            return Err(Temm1eError::Tool(format!(
                "MCP HTTP transport for '{}' is closed",
                self.server_name
            )));
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest::new(id, method, params);
        let json = serde_json::to_string(&request)
            .map_err(|e| Temm1eError::Tool(format!("Failed to serialize request: {}", e)))?;

        let mut http_req = self.build_request(&json)?;

        // Add session ID if we have one
        if let Some(sid) = self.session_id.read().await.as_ref() {
            http_req = http_req.header("Mcp-Session-Id", sid.as_str());
        }

        debug!(
            server = %self.server_name,
            method = %method,
            "Sending MCP HTTP request"
        );

        let http_resp = http_req.send().await.map_err(|e| {
            Temm1eError::Tool(format!(
                "MCP HTTP request to '{}' failed: {}",
                self.server_name, e
            ))
        })?;

        // Store session ID from response
        if let Some(sid) = http_resp.headers().get("mcp-session-id") {
            if let Ok(sid_str) = sid.to_str() {
                *self.session_id.write().await = Some(sid_str.to_string());
            }
        }

        let status = http_resp.status();
        if !status.is_success() {
            let body = http_resp.text().await.unwrap_or_default();
            return Err(Temm1eError::Tool(format!(
                "MCP HTTP request to '{}' returned {}: {}",
                self.server_name, status, body
            )));
        }

        let body = http_resp.text().await.map_err(|e| {
            Temm1eError::Tool(format!("Failed to read MCP HTTP response body: {}", e))
        })?;

        let response: JsonRpcResponse = serde_json::from_str(&body).map_err(|e| {
            Temm1eError::Tool(format!(
                "Failed to parse MCP HTTP response as JSON-RPC: {} (body: {})",
                e,
                &body[..body.len().min(200)]
            ))
        })?;

        Ok(response)
    }

    async fn notify(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), Temm1eError> {
        if !self.alive.load(Ordering::Relaxed) {
            return Err(Temm1eError::Tool(format!(
                "MCP HTTP transport for '{}' is closed",
                self.server_name
            )));
        }

        let notification = JsonRpcNotification::new(method, params);
        let json = serde_json::to_string(&notification)
            .map_err(|e| Temm1eError::Tool(format!("Failed to serialize notification: {}", e)))?;

        let mut http_req = self.build_request(&json)?;

        if let Some(sid) = self.session_id.read().await.as_ref() {
            http_req = http_req.header("Mcp-Session-Id", sid.as_str());
        }

        let resp = http_req.send().await.map_err(|e| {
            Temm1eError::Tool(format!(
                "MCP HTTP notification to '{}' failed: {}",
                self.server_name, e
            ))
        })?;

        if !resp.status().is_success() {
            warn!(
                server = %self.server_name,
                status = %resp.status(),
                "MCP HTTP notification returned non-success status"
            );
        }

        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    async fn close(&self) -> Result<(), Temm1eError> {
        self.alive.store(false, Ordering::Relaxed);
        Ok(())
    }
}
