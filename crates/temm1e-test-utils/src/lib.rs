//! Test utilities and mocks for TEMM1E.
//!
//! Provides mock implementations of core traits, factory helpers for test data,
//! and a fluent config builder for test scenarios.

use std::sync::Arc;

use async_trait::async_trait;
use futures::stream::BoxStream;
use tokio::sync::Mutex;

use temm1e_core::types::config::*;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::*;
use temm1e_core::types::session::SessionContext;
use temm1e_core::{
    Channel, FileTransfer, Memory, MemoryEntry, MemoryEntryType, Provider, SearchOpts, Tool,
    ToolContext, ToolDeclarations, ToolInput, ToolOutput,
};

// ---------------------------------------------------------------------------
// MockProvider
// ---------------------------------------------------------------------------

/// A mock AI provider that returns canned responses.
pub struct MockProvider {
    /// The response to return from `complete()`.
    pub response: CompletionResponse,
    /// Number of times `complete()` was called.
    pub call_count: Arc<Mutex<usize>>,
    /// Captured requests for assertion.
    pub captured_requests: Arc<Mutex<Vec<CompletionRequest>>>,
}

impl MockProvider {
    /// Create a mock provider that returns a simple text response.
    pub fn with_text(text: &str) -> Self {
        Self {
            response: CompletionResponse {
                id: "mock-resp-1".to_string(),
                content: vec![ContentPart::Text {
                    text: text.to_string(),
                }],
                stop_reason: Some("end_turn".to_string()),
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 20,
                    cost_usd: 0.0,
                },
            },
            call_count: Arc::new(Mutex::new(0)),
            captured_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Create a mock provider that returns a tool_use response.
    pub fn with_tool_use(tool_id: &str, tool_name: &str, input: serde_json::Value) -> Self {
        Self {
            response: CompletionResponse {
                id: "mock-resp-tool".to_string(),
                content: vec![ContentPart::ToolUse {
                    id: tool_id.to_string(),
                    name: tool_name.to_string(),
                    input,
                    thought_signature: None,
                }],
                stop_reason: Some("tool_use".to_string()),
                usage: Usage {
                    input_tokens: 10,
                    output_tokens: 30,
                    cost_usd: 0.0,
                },
            },
            call_count: Arc::new(Mutex::new(0)),
            captured_requests: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Get the number of times `complete()` was called.
    pub async fn calls(&self) -> usize {
        *self.call_count.lock().await
    }
}

#[async_trait]
impl Provider for MockProvider {
    fn name(&self) -> &str {
        "mock"
    }

    async fn complete(
        &self,
        request: CompletionRequest,
    ) -> Result<CompletionResponse, Temm1eError> {
        let mut count = self.call_count.lock().await;
        *count += 1;
        let mut reqs = self.captured_requests.lock().await;
        reqs.push(request);
        Ok(self.response.clone())
    }

    async fn stream(
        &self,
        _request: CompletionRequest,
    ) -> Result<BoxStream<'_, Result<StreamChunk, Temm1eError>>, Temm1eError> {
        let chunks = vec![
            Ok(StreamChunk {
                delta: Some("mock stream".to_string()),
                tool_use: None,
                stop_reason: None,
            }),
            Ok(StreamChunk {
                delta: None,
                tool_use: None,
                stop_reason: Some("end_turn".to_string()),
            }),
        ];
        Ok(Box::pin(futures::stream::iter(chunks)))
    }

    async fn health_check(&self) -> Result<bool, Temm1eError> {
        Ok(true)
    }

    async fn list_models(&self) -> Result<Vec<String>, Temm1eError> {
        Ok(vec!["mock-model".to_string()])
    }
}

// ---------------------------------------------------------------------------
// MockMemory
// ---------------------------------------------------------------------------

/// A mock memory backend backed by an in-memory Vec.
pub struct MockMemory {
    entries: Arc<Mutex<Vec<MemoryEntry>>>,
}

impl MockMemory {
    pub fn new() -> Self {
        Self {
            entries: Arc::new(Mutex::new(Vec::new())),
        }
    }

    pub fn with_entries(entries: Vec<MemoryEntry>) -> Self {
        Self {
            entries: Arc::new(Mutex::new(entries)),
        }
    }

    pub async fn len(&self) -> usize {
        self.entries.lock().await.len()
    }

    pub async fn is_empty(&self) -> bool {
        self.entries.lock().await.is_empty()
    }
}

impl Default for MockMemory {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Memory for MockMemory {
    async fn store(&self, entry: MemoryEntry) -> Result<(), Temm1eError> {
        let mut entries = self.entries.lock().await;
        entries.push(entry);
        Ok(())
    }

    async fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<MemoryEntry>, Temm1eError> {
        let entries = self.entries.lock().await;
        let results: Vec<MemoryEntry> = entries
            .iter()
            .filter(|e| {
                let matches_query =
                    query.is_empty() || e.content.to_lowercase().contains(&query.to_lowercase());
                let matches_session = opts
                    .session_filter
                    .as_ref()
                    .is_none_or(|s| e.session_id.as_deref() == Some(s.as_str()));
                matches_query && matches_session
            })
            .take(opts.limit)
            .cloned()
            .collect();
        Ok(results)
    }

    async fn get(&self, id: &str) -> Result<Option<MemoryEntry>, Temm1eError> {
        let entries = self.entries.lock().await;
        Ok(entries.iter().find(|e| e.id == id).cloned())
    }

    async fn delete(&self, id: &str) -> Result<(), Temm1eError> {
        let mut entries = self.entries.lock().await;
        entries.retain(|e| e.id != id);
        Ok(())
    }

    async fn list_sessions(&self) -> Result<Vec<String>, Temm1eError> {
        let entries = self.entries.lock().await;
        let sessions: std::collections::BTreeSet<String> = entries
            .iter()
            .filter_map(|e| e.session_id.clone())
            .collect();
        Ok(sessions.into_iter().collect())
    }

    async fn get_session_history(
        &self,
        session_id: &str,
        limit: usize,
    ) -> Result<Vec<MemoryEntry>, Temm1eError> {
        let entries = self.entries.lock().await;
        let history: Vec<MemoryEntry> = entries
            .iter()
            .filter(|e| e.session_id.as_deref() == Some(session_id))
            .take(limit)
            .cloned()
            .collect();
        Ok(history)
    }

    fn backend_name(&self) -> &str {
        "mock"
    }
}

// ---------------------------------------------------------------------------
// MockChannel
// ---------------------------------------------------------------------------

/// A mock messaging channel that records sent messages.
pub struct MockChannel {
    name: String,
    allowlist: Vec<String>,
    pub sent_messages: Arc<Mutex<Vec<OutboundMessage>>>,
    started: Arc<Mutex<bool>>,
}

impl MockChannel {
    pub fn new(name: &str) -> Self {
        Self {
            name: name.to_string(),
            allowlist: Vec::new(),
            sent_messages: Arc::new(Mutex::new(Vec::new())),
            started: Arc::new(Mutex::new(false)),
        }
    }

    pub fn with_allowlist(mut self, allowlist: Vec<String>) -> Self {
        self.allowlist = allowlist;
        self
    }

    pub async fn sent_count(&self) -> usize {
        self.sent_messages.lock().await.len()
    }
}

#[async_trait]
impl Channel for MockChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&mut self) -> Result<(), Temm1eError> {
        let mut started = self.started.lock().await;
        *started = true;
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Temm1eError> {
        let mut started = self.started.lock().await;
        *started = false;
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
        let mut msgs = self.sent_messages.lock().await;
        msgs.push(msg);
        Ok(())
    }

    fn file_transfer(&self) -> Option<&dyn FileTransfer> {
        None
    }

    fn is_allowed(&self, user_id: &str) -> bool {
        if self.allowlist.is_empty() {
            return true;
        }
        self.allowlist.iter().any(|a| a == user_id)
    }
}

// ---------------------------------------------------------------------------
// MockTool
// ---------------------------------------------------------------------------

/// A mock tool for testing the executor/sandbox.
pub struct MockTool {
    tool_name: String,
    declarations: ToolDeclarations,
    output: ToolOutput,
}

impl MockTool {
    pub fn new(name: &str) -> Self {
        Self {
            tool_name: name.to_string(),
            declarations: ToolDeclarations {
                file_access: Vec::new(),
                network_access: Vec::new(),
                shell_access: false,
            },
            output: ToolOutput {
                content: "mock output".to_string(),
                is_error: false,
            },
        }
    }

    pub fn with_declarations(mut self, declarations: ToolDeclarations) -> Self {
        self.declarations = declarations;
        self
    }

    pub fn with_output(mut self, output: ToolOutput) -> Self {
        self.output = output;
        self
    }
}

#[async_trait]
impl Tool for MockTool {
    fn name(&self) -> &str {
        &self.tool_name
    }

    fn description(&self) -> &str {
        "A mock tool for testing"
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {}
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        self.declarations.clone()
    }

    async fn execute(
        &self,
        _input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        Ok(self.output.clone())
    }
}

// ---------------------------------------------------------------------------
// TestConfigBuilder
// ---------------------------------------------------------------------------

/// Fluent builder for `Temm1eConfig` in tests.
pub struct TestConfigBuilder {
    config: Temm1eConfig,
}

impl TestConfigBuilder {
    pub fn new() -> Self {
        Self {
            config: Temm1eConfig::default(),
        }
    }

    pub fn with_gateway_port(mut self, port: u16) -> Self {
        self.config.gateway.port = port;
        self
    }

    pub fn with_gateway_host(mut self, host: &str) -> Self {
        self.config.gateway.host = host.to_string();
        self
    }

    pub fn with_provider(mut self, name: &str, api_key: &str) -> Self {
        self.config.provider.name = Some(name.to_string());
        self.config.provider.api_key = Some(api_key.to_string());
        self
    }

    pub fn with_memory_backend(mut self, backend: &str) -> Self {
        self.config.memory.backend = backend.to_string();
        self
    }

    pub fn with_channel(mut self, name: &str, config: ChannelConfig) -> Self {
        self.config.channel.insert(name.to_string(), config);
        self
    }

    pub fn with_sandbox(mut self, mode: &str) -> Self {
        self.config.security.sandbox = mode.to_string();
        self
    }

    pub fn build(self) -> Temm1eConfig {
        self.config
    }
}

impl Default for TestConfigBuilder {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Factory helpers
// ---------------------------------------------------------------------------

/// Create a test `MemoryEntry` with sensible defaults.
pub fn make_test_entry(id: &str, content: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        content: content.to_string(),
        metadata: serde_json::json!({}),
        timestamp: chrono::Utc::now(),
        session_id: None,
        entry_type: MemoryEntryType::Conversation,
    }
}

/// Create a test `MemoryEntry` with a specific session.
pub fn make_test_entry_with_session(id: &str, content: &str, session: &str) -> MemoryEntry {
    MemoryEntry {
        id: id.to_string(),
        content: content.to_string(),
        metadata: serde_json::json!({}),
        timestamp: chrono::Utc::now(),
        session_id: Some(session.to_string()),
        entry_type: MemoryEntryType::Conversation,
    }
}

/// Create a test `InboundMessage`.
pub fn make_inbound_msg(text: &str) -> InboundMessage {
    InboundMessage {
        id: uuid::Uuid::new_v4().to_string(),
        channel: "test".to_string(),
        chat_id: "test-chat".to_string(),
        user_id: "test-user".to_string(),
        username: Some("tester".to_string()),
        text: Some(text.to_string()),
        attachments: Vec::new(),
        reply_to: None,
        timestamp: chrono::Utc::now(),
    }
}

/// Create a test `SessionContext`.
pub fn make_session() -> SessionContext {
    SessionContext {
        session_id: "test:test-chat:test-user".to_string(),
        channel: "test".to_string(),
        chat_id: "test-chat".to_string(),
        user_id: "test-user".to_string(),
        history: Vec::new(),
        workspace_path: std::env::temp_dir().join("temm1e-test"),
    }
}

// ---------------------------------------------------------------------------
// Tests for test-utils themselves
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_config_builder() {
        let config = TestConfigBuilder::new()
            .with_gateway_port(9090)
            .with_provider("anthropic", "test-key")
            .with_memory_backend("markdown")
            .build();

        assert_eq!(config.gateway.port, 9090);
        assert_eq!(config.provider.name.as_deref(), Some("anthropic"));
        assert_eq!(config.provider.api_key.as_deref(), Some("test-key"));
        assert_eq!(config.memory.backend, "markdown");
    }

    #[test]
    fn test_make_test_entry() {
        let entry = make_test_entry("e1", "hello world");
        assert_eq!(entry.id, "e1");
        assert_eq!(entry.content, "hello world");
        assert!(entry.session_id.is_none());
    }

    #[test]
    fn test_make_inbound_msg() {
        let msg = make_inbound_msg("test message");
        assert_eq!(msg.text.as_deref(), Some("test message"));
        assert_eq!(msg.channel, "test");
    }

    #[tokio::test]
    async fn test_mock_provider() {
        let provider = MockProvider::with_text("hello from mock");
        let request = CompletionRequest {
            model: "test".to_string(),
            messages: Vec::new(),
            tools: Vec::new(),
            max_tokens: None,
            temperature: None,
            system: None,
        };
        let resp = provider.complete(request).await.unwrap();
        assert_eq!(resp.content.len(), 1);
        assert_eq!(provider.calls().await, 1);
    }

    #[tokio::test]
    async fn test_mock_memory() {
        let mem = MockMemory::new();
        let entry = make_test_entry("m1", "test content");
        mem.store(entry).await.unwrap();
        assert_eq!(mem.len().await, 1);

        let found = mem.get("m1").await.unwrap();
        assert!(found.is_some());
        assert_eq!(found.unwrap().content, "test content");

        mem.delete("m1").await.unwrap();
        assert_eq!(mem.len().await, 0);
    }

    #[tokio::test]
    async fn test_mock_channel() {
        let chan = MockChannel::new("test-chan");
        assert!(chan.is_allowed("anyone"));
        assert_eq!(chan.name(), "test-chan");

        let chan_restricted =
            MockChannel::new("restricted").with_allowlist(vec!["user1".to_string()]);
        assert!(chan_restricted.is_allowed("user1"));
        assert!(!chan_restricted.is_allowed("user2"));
    }
}
