//! Stdio transport — spawns an MCP server as a child process,
//! communicates via stdin/stdout with newline-delimited JSON-RPC.
//!
//! Resilience features:
//! - Background reader task detects process exit (EOF on stdout)
//! - Pending requests are cleaned up on timeout
//! - Dead process detected via `is_alive()` flag
//! - Max line length (10 MB) prevents OOM from misbehaving servers
//! - Stderr is captured and logged (never parsed as JSON-RPC)

use crate::config::McpServerConfig;
use crate::jsonrpc::{
    parse_incoming, IncomingMessage, JsonRpcNotification, JsonRpcRequest, JsonRpcResponse,
};
use crate::transport::Transport;
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;
use temm1e_core::types::error::Temm1eError;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tokio::sync::{oneshot, Mutex};
use tracing::{debug, error, warn};

/// Maximum line length from an MCP server (10 MB). Lines longer than this
/// are skipped to prevent OOM from misbehaving servers.
const MAX_LINE_LENGTH: usize = 10 * 1024 * 1024;

pub struct StdioTransport {
    stdin: Arc<Mutex<tokio::process::ChildStdin>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>>,
    next_id: AtomicU64,
    alive: Arc<AtomicBool>,
    timeout: Duration,
    child: Arc<Mutex<Option<Child>>>,
    server_name: String,
    _reader_handle: tokio::task::JoinHandle<()>,
    _stderr_handle: tokio::task::JoinHandle<()>,
}

impl StdioTransport {
    /// Spawn an MCP server subprocess and set up stdin/stdout pipes.
    pub async fn spawn(config: &McpServerConfig, timeout: Duration) -> Result<Self, Temm1eError> {
        let command = config.command.as_deref().ok_or_else(|| {
            Temm1eError::Tool(format!(
                "MCP server '{}': no command specified",
                config.name
            ))
        })?;

        debug!(
            server = %config.name,
            command = %command,
            args = ?config.args,
            "Spawning MCP server subprocess"
        );

        let mut cmd = Command::new(command);
        cmd.args(&config.args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());

        // Pass environment variables
        for (key, value) in &config.env {
            cmd.env(key, value);
        }

        // On Windows, create detached process to avoid console window
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x00000008); // DETACHED_PROCESS
        }

        let mut child = cmd.spawn().map_err(|e| {
            Temm1eError::Tool(format!(
                "Failed to spawn MCP server '{}' ({}): {}",
                config.name, command, e
            ))
        })?;

        let stdin = child.stdin.take().ok_or_else(|| {
            Temm1eError::Tool(format!(
                "MCP server '{}': failed to capture stdin",
                config.name
            ))
        })?;
        let stdout = child.stdout.take().ok_or_else(|| {
            Temm1eError::Tool(format!(
                "MCP server '{}': failed to capture stdout",
                config.name
            ))
        })?;
        let stderr = child.stderr.take().ok_or_else(|| {
            Temm1eError::Tool(format!(
                "MCP server '{}': failed to capture stderr",
                config.name
            ))
        })?;

        let alive = Arc::new(AtomicBool::new(true));
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<JsonRpcResponse>>>> =
            Arc::new(Mutex::new(HashMap::new()));

        let server_name = config.name.clone();

        // Background task: read stdout lines, dispatch responses
        let reader_alive = alive.clone();
        let reader_pending = pending.clone();
        let reader_name = server_name.clone();
        let reader_handle = tokio::spawn(async move {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        if line.len() > MAX_LINE_LENGTH {
                            warn!(
                                server = %reader_name,
                                len = line.len(),
                                "MCP server sent oversized line — skipping"
                            );
                            continue;
                        }
                        match parse_incoming(&line) {
                            Some(IncomingMessage::Response(resp)) => {
                                if let Some(id) = resp.id {
                                    let mut map = reader_pending.lock().await;
                                    if let Some(sender) = map.remove(&id) {
                                        let _ = sender.send(resp);
                                    } else {
                                        debug!(
                                            server = %reader_name,
                                            id = id,
                                            "Response for unknown request ID"
                                        );
                                    }
                                }
                            }
                            Some(IncomingMessage::Notification(notif)) => {
                                debug!(
                                    server = %reader_name,
                                    method = %notif.method,
                                    "MCP notification received"
                                );
                            }
                            None => {
                                // Not valid JSON-RPC — could be startup banner or debug output
                                if !line.trim().is_empty() {
                                    debug!(
                                        server = %reader_name,
                                        line = %line,
                                        "Unparseable MCP server output (not JSON-RPC)"
                                    );
                                }
                            }
                        }
                    }
                    Ok(None) => {
                        // EOF — process exited
                        reader_alive.store(false, Ordering::Relaxed);
                        warn!(server = %reader_name, "MCP server stdout closed — process exited");
                        // Wake up all pending requests with an error
                        let mut map = reader_pending.lock().await;
                        for (_, sender) in map.drain() {
                            let _ = sender.send(JsonRpcResponse {
                                jsonrpc: "2.0".to_string(),
                                id: None,
                                result: None,
                                error: Some(crate::jsonrpc::JsonRpcError {
                                    code: -1,
                                    message: "MCP server process exited".to_string(),
                                    data: None,
                                }),
                            });
                        }
                        break;
                    }
                    Err(e) => {
                        reader_alive.store(false, Ordering::Relaxed);
                        error!(
                            server = %reader_name,
                            error = %e,
                            "Error reading MCP server stdout"
                        );
                        break;
                    }
                }
            }
        });

        // Background task: read stderr and log it
        let stderr_name = server_name.clone();
        let stderr_handle = tokio::spawn(async move {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Ok(Some(line)) = lines.next_line().await {
                if !line.trim().is_empty() {
                    debug!(server = %stderr_name, stderr = %line, "MCP server stderr");
                }
            }
        });

        Ok(Self {
            stdin: Arc::new(Mutex::new(stdin)),
            pending,
            next_id: AtomicU64::new(1),
            alive,
            timeout,
            child: Arc::new(Mutex::new(Some(child))),
            server_name,
            _reader_handle: reader_handle,
            _stderr_handle: stderr_handle,
        })
    }

    /// Get the child process handle (for killing on shutdown).
    pub async fn take_child(&self) -> Option<Child> {
        self.child.lock().await.take()
    }
}

#[async_trait]
impl Transport for StdioTransport {
    async fn send(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<JsonRpcResponse, Temm1eError> {
        if !self.alive.load(Ordering::Relaxed) {
            return Err(Temm1eError::Tool(format!(
                "MCP server '{}' is not running",
                self.server_name
            )));
        }

        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let request = JsonRpcRequest::new(id, method, params);

        let (tx, rx) = oneshot::channel();
        self.pending.lock().await.insert(id, tx);

        // Serialize and write to stdin
        let json = serde_json::to_string(&request).map_err(|e| {
            Temm1eError::Tool(format!("Failed to serialize JSON-RPC request: {}", e))
        })?;

        {
            let mut stdin = self.stdin.lock().await;
            if let Err(e) = stdin.write_all(json.as_bytes()).await {
                self.pending.lock().await.remove(&id);
                return Err(Temm1eError::Tool(format!(
                    "Failed to write to MCP server '{}' stdin: {}",
                    self.server_name, e
                )));
            }
            if let Err(e) = stdin.write_all(b"\n").await {
                self.pending.lock().await.remove(&id);
                return Err(Temm1eError::Tool(format!(
                    "Failed to write newline to MCP server '{}': {}",
                    self.server_name, e
                )));
            }
            if let Err(e) = stdin.flush().await {
                self.pending.lock().await.remove(&id);
                return Err(Temm1eError::Tool(format!(
                    "Failed to flush MCP server '{}' stdin: {}",
                    self.server_name, e
                )));
            }
        }

        // Wait for response with timeout
        match tokio::time::timeout(self.timeout, rx).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(_)) => {
                // Channel dropped — reader died
                Err(Temm1eError::Tool(format!(
                    "MCP server '{}' response channel dropped — server may have crashed",
                    self.server_name
                )))
            }
            Err(_) => {
                // Timeout
                self.pending.lock().await.remove(&id);
                Err(Temm1eError::Tool(format!(
                    "MCP call to '{}' method '{}' timed out after {}s",
                    self.server_name,
                    method,
                    self.timeout.as_secs()
                )))
            }
        }
    }

    async fn notify(
        &self,
        method: &str,
        params: Option<serde_json::Value>,
    ) -> Result<(), Temm1eError> {
        if !self.alive.load(Ordering::Relaxed) {
            return Err(Temm1eError::Tool(format!(
                "MCP server '{}' is not running",
                self.server_name
            )));
        }

        let notification = JsonRpcNotification::new(method, params);
        let json = serde_json::to_string(&notification)
            .map_err(|e| Temm1eError::Tool(format!("Failed to serialize notification: {}", e)))?;

        let mut stdin = self.stdin.lock().await;
        stdin.write_all(json.as_bytes()).await.map_err(|e| {
            Temm1eError::Tool(format!(
                "Failed to write notification to MCP server '{}': {}",
                self.server_name, e
            ))
        })?;
        stdin
            .write_all(b"\n")
            .await
            .map_err(|e| Temm1eError::Tool(format!("Failed to write newline: {}", e)))?;
        stdin
            .flush()
            .await
            .map_err(|e| Temm1eError::Tool(format!("Failed to flush: {}", e)))?;
        Ok(())
    }

    fn is_alive(&self) -> bool {
        self.alive.load(Ordering::Relaxed)
    }

    async fn close(&self) -> Result<(), Temm1eError> {
        self.alive.store(false, Ordering::Relaxed);
        // Kill child process
        if let Some(mut child) = self.child.lock().await.take() {
            let _ = child.start_kill();
            // Give it a moment to exit gracefully
            match tokio::time::timeout(Duration::from_secs(5), child.wait()).await {
                Ok(Ok(status)) => {
                    debug!(
                        server = %self.server_name,
                        status = %status,
                        "MCP server process exited"
                    );
                }
                Ok(Err(e)) => {
                    warn!(
                        server = %self.server_name,
                        error = %e,
                        "Error waiting for MCP server to exit"
                    );
                }
                Err(_) => {
                    warn!(
                        server = %self.server_name,
                        "MCP server did not exit within 5s — force killed"
                    );
                }
            }
        }
        // Cancel all pending requests
        let mut map = self.pending.lock().await;
        map.clear();
        Ok(())
    }
}
