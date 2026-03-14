//! Streaming response support — sends incremental text and tool status
//! updates to users as tokens arrive from the AI provider.
//!
//! # Architecture
//!
//! The module provides two main components:
//!
//! - [`StreamingNotifier`] — sends tool-lifecycle status updates (start/complete/fail)
//!   to the user via the messaging channel so they know what the agent is doing.
//!
//! - [`StreamBuffer`] — accumulates streamed tokens from the provider and flushes
//!   them at word boundaries (or after a configurable time threshold) to avoid
//!   sending partial words to the user.
//!
//! Both are designed for integration into the agent runtime loop but are
//! decoupled from it — they only depend on the `Channel` trait and config.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::OutboundMessage;
use temm1e_core::Channel;
use tracing::{debug, info, warn};

// ---------------------------------------------------------------------------
// Streaming Configuration
// ---------------------------------------------------------------------------

/// Configuration for streaming response behavior.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StreamingConfig {
    /// Whether streaming responses are enabled at all.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Minimum interval (in milliseconds) between flushing accumulated
    /// tokens to the user. Prevents overwhelming the messaging platform
    /// with rapid edits.
    #[serde(default = "default_min_flush_interval_ms")]
    pub min_flush_interval_ms: u64,

    /// Whether to send tool status updates ("Running shell...", "Complete").
    #[serde(default = "default_true")]
    pub tool_status_updates: bool,
}

impl Default for StreamingConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            min_flush_interval_ms: 1000,
            tool_status_updates: true,
        }
    }
}

fn default_true() -> bool {
    true
}

fn default_min_flush_interval_ms() -> u64 {
    1000
}

impl StreamingConfig {
    /// Validate that all values are within acceptable bounds.
    pub fn validate(&self) -> Result<(), Temm1eError> {
        if self.min_flush_interval_ms < 100 {
            return Err(Temm1eError::Config(
                "streaming.min_flush_interval_ms must be >= 100 to avoid rate limiting".to_string(),
            ));
        }
        if self.min_flush_interval_ms > 30_000 {
            return Err(Temm1eError::Config(
                "streaming.min_flush_interval_ms must be <= 30000".to_string(),
            ));
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// StreamingNotifier — tool lifecycle updates
// ---------------------------------------------------------------------------

/// Sends tool-lifecycle status messages to the user.
///
/// When a tool starts executing, the notifier sends a brief status message
/// like "Running shell..." so the user knows the agent is working. When the
/// tool completes, it sends a success or failure indicator.
///
/// If no channel is configured (e.g., running headless tests), all methods
/// are no-ops — the notifier degrades gracefully.
pub struct StreamingNotifier {
    /// The messaging channel to send updates through (None = silent/testing mode).
    channel: Option<Arc<dyn Channel>>,
    /// The chat ID to send updates to.
    chat_id: String,
    /// Whether tool status updates are enabled.
    tool_status_updates: bool,
    /// Whether the notifier is enabled at all.
    enabled: bool,
}

impl StreamingNotifier {
    /// Create a new `StreamingNotifier`.
    ///
    /// Pass `channel: None` for silent mode (useful in tests or headless runs).
    pub fn new(
        channel: Option<Arc<dyn Channel>>,
        chat_id: String,
        config: &StreamingConfig,
    ) -> Self {
        Self {
            channel,
            chat_id,
            tool_status_updates: config.tool_status_updates,
            enabled: config.enabled,
        }
    }

    /// Create a disabled (no-op) notifier that never sends messages.
    pub fn disabled() -> Self {
        Self {
            channel: None,
            chat_id: String::new(),
            tool_status_updates: false,
            enabled: false,
        }
    }

    /// Send a "tool started" notification to the user.
    ///
    /// Produces messages like: `"Running shell..."`, `"Reading file..."`.
    pub async fn notify_tool_start(&self, tool_name: &str) -> Result<(), Temm1eError> {
        if !self.enabled || !self.tool_status_updates {
            return Ok(());
        }

        let status_text = format_tool_start(tool_name);
        info!(tool = %tool_name, "Streaming tool-start notification");
        self.send_status(&status_text).await
    }

    /// Send a "tool completed" notification to the user.
    ///
    /// Produces messages like: `"shell complete"` or `"shell failed"`.
    pub async fn notify_tool_complete(
        &self,
        tool_name: &str,
        success: bool,
    ) -> Result<(), Temm1eError> {
        if !self.enabled || !self.tool_status_updates {
            return Ok(());
        }

        let status_text = format_tool_complete(tool_name, success);
        info!(
            tool = %tool_name,
            success = success,
            "Streaming tool-complete notification"
        );
        self.send_status(&status_text).await
    }

    /// Send a free-form status message to the user.
    ///
    /// Use sparingly — prefer `notify_tool_start`/`notify_tool_complete` for
    /// tool lifecycle events.
    pub async fn notify_status(&self, message: &str) -> Result<(), Temm1eError> {
        if !self.enabled {
            return Ok(());
        }

        debug!(message = %message, "Streaming status notification");
        self.send_status(message).await
    }

    /// Internal: send a status message through the channel.
    async fn send_status(&self, text: &str) -> Result<(), Temm1eError> {
        let channel = match &self.channel {
            Some(ch) => ch,
            None => {
                debug!(text = %text, "No channel configured — skipping status message");
                return Ok(());
            }
        };

        let msg = OutboundMessage {
            chat_id: self.chat_id.clone(),
            text: text.to_string(),
            reply_to: None,
            parse_mode: None,
        };

        match channel.send_message(msg).await {
            Ok(()) => Ok(()),
            Err(e) => {
                // Status updates are best-effort — log and continue, don't
                // fail the entire agent loop because of a notification failure.
                warn!(
                    error = %e,
                    text = %text,
                    "Failed to send streaming status update"
                );
                Ok(())
            }
        }
    }
}

// ---------------------------------------------------------------------------
// StreamBuffer — word-boundary token accumulator
// ---------------------------------------------------------------------------

/// Accumulates streamed tokens from the AI provider and flushes them at
/// word boundaries (or after a time threshold) to avoid sending partial
/// words to the user.
///
/// # Flush strategy
///
/// The buffer flushes when *either* condition is met:
/// 1. **Time threshold**: at least `min_flush_interval` has elapsed since the last flush.
/// 2. **Word boundary**: the accumulated text ends at a natural break point
///    (space, newline, or punctuation) *and* the time threshold has elapsed.
///
/// When flushed, the buffer returns the accumulated text and resets.
/// Any partial word at the end is retained for the next flush cycle.
pub struct StreamBuffer {
    /// Accumulated token text waiting to be flushed.
    buffer: String,
    /// When the last flush occurred (or when the buffer was created).
    last_flush: Instant,
    /// Minimum time between flushes.
    min_flush_interval: Duration,
    /// Total number of characters that have passed through this buffer.
    total_chars: usize,
    /// Number of flushes performed.
    flush_count: usize,
}

impl StreamBuffer {
    /// Create a new `StreamBuffer` with the given flush interval.
    pub fn new(min_flush_interval: Duration) -> Self {
        Self {
            buffer: String::new(),
            last_flush: Instant::now(),
            min_flush_interval,
            total_chars: 0,
            flush_count: 0,
        }
    }

    /// Create a `StreamBuffer` from a `StreamingConfig`.
    pub fn from_config(config: &StreamingConfig) -> Self {
        Self::new(Duration::from_millis(config.min_flush_interval_ms))
    }

    /// Append a token (text fragment) to the buffer.
    pub fn push(&mut self, token: &str) {
        self.buffer.push_str(token);
        self.total_chars += token.len();
    }

    /// Check whether the buffer should be flushed and, if so, return the
    /// accumulated text. The buffer is cleared (except for any trailing
    /// partial word) and the flush timer is reset.
    ///
    /// Returns `None` if the buffer is not ready to flush (either empty or
    /// the minimum interval hasn't elapsed).
    pub fn try_flush(&mut self) -> Option<String> {
        if self.buffer.is_empty() {
            return None;
        }

        // Don't flush until enough time has passed.
        if self.last_flush.elapsed() < self.min_flush_interval {
            return None;
        }

        // Find the last word boundary so we don't split mid-word.
        let flush_end = find_last_word_boundary(&self.buffer);

        if flush_end == 0 {
            // The entire buffer is a single unbroken token — only flush if
            // it's getting unreasonably large (> 500 chars). This prevents
            // holding a very long URL or base64 blob forever.
            if self.buffer.len() > 500 {
                let flushed = self.buffer.clone();
                self.buffer.clear();
                self.last_flush = Instant::now();
                self.flush_count += 1;
                return Some(flushed);
            }
            return None;
        }

        // Split at the word boundary.
        let flushed: String = self.buffer[..flush_end].to_string();
        let remainder: String = self.buffer[flush_end..].to_string();

        self.buffer = remainder;
        self.last_flush = Instant::now();
        self.flush_count += 1;

        Some(flushed)
    }

    /// Force-flush everything remaining in the buffer, regardless of word
    /// boundaries or time. Use this at the end of a streamed response to
    /// send the final fragment.
    pub fn flush_all(&mut self) -> Option<String> {
        if self.buffer.is_empty() {
            return None;
        }

        let flushed = std::mem::take(&mut self.buffer);
        self.last_flush = Instant::now();
        self.flush_count += 1;
        Some(flushed)
    }

    /// Returns `true` if the buffer contains any accumulated text.
    pub fn has_content(&self) -> bool {
        !self.buffer.is_empty()
    }

    /// Return the current buffered text (without flushing).
    pub fn peek(&self) -> &str {
        &self.buffer
    }

    /// Total characters that have passed through this buffer.
    pub fn total_chars(&self) -> usize {
        self.total_chars
    }

    /// Number of flushes performed so far.
    pub fn flush_count(&self) -> usize {
        self.flush_count
    }
}

// ---------------------------------------------------------------------------
// Formatting helpers
// ---------------------------------------------------------------------------

/// Format a tool-start status message.
fn format_tool_start(tool_name: &str) -> String {
    let verb = match tool_name {
        "shell" => "Running shell command",
        "browser" | "browse" => "Browsing page",
        "file_read" | "read_file" => "Reading file",
        "file_write" | "write_file" => "Writing file",
        "file_edit" | "edit_file" => "Editing file",
        "git" => "Running git operation",
        "http" | "http_request" => "Making HTTP request",
        "cron" => "Managing cron job",
        _ => tool_name,
    };
    format!("\u{29D6} {verb}...")
}

/// Format a tool-complete status message.
fn format_tool_complete(tool_name: &str, success: bool) -> String {
    if success {
        format!("\u{2713} {tool_name} complete")
    } else {
        format!("\u{2717} {tool_name} failed")
    }
}

/// Find the byte offset of the last word boundary in `text`.
///
/// A word boundary is a position immediately after a space, newline,
/// tab, or sentence-ending punctuation (`.`, `!`, `?`, `)`, `]`, `}`).
/// Returns 0 if no boundary is found.
fn find_last_word_boundary(text: &str) -> usize {
    // We search backwards for a character that constitutes a break point.
    // The flush position is *after* that character so the break char is
    // included in the flushed output.
    let boundary_chars: &[char] = &[' ', '\n', '\t', '.', '!', '?', ')', ']', '}', ',', ';', ':'];

    for (idx, ch) in text.char_indices().rev() {
        if boundary_chars.contains(&ch) {
            // Flush up to and including this character.
            return idx + ch.len_utf8();
        }
    }

    0
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    // ── StreamingConfig tests ─────────────────────────────────────────

    #[test]
    fn config_defaults_are_sensible() {
        let config = StreamingConfig::default();
        assert!(config.enabled);
        assert_eq!(config.min_flush_interval_ms, 1000);
        assert!(config.tool_status_updates);
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = StreamingConfig {
            enabled: false,
            min_flush_interval_ms: 2000,
            tool_status_updates: false,
        };

        let json = serde_json::to_string(&config).unwrap();
        let restored: StreamingConfig = serde_json::from_str(&json).unwrap();
        assert!(!restored.enabled);
        assert_eq!(restored.min_flush_interval_ms, 2000);
        assert!(!restored.tool_status_updates);
    }

    #[test]
    fn config_toml_roundtrip() {
        let config = StreamingConfig {
            enabled: true,
            min_flush_interval_ms: 500,
            tool_status_updates: true,
        };

        let toml_str = toml::to_string(&config).unwrap();
        let restored: StreamingConfig = toml::from_str(&toml_str).unwrap();
        assert!(restored.enabled);
        assert_eq!(restored.min_flush_interval_ms, 500);
        assert!(restored.tool_status_updates);
    }

    #[test]
    fn config_validate_ok() {
        let config = StreamingConfig::default();
        assert!(config.validate().is_ok());
    }

    #[test]
    fn config_validate_too_fast() {
        let config = StreamingConfig {
            min_flush_interval_ms: 50,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("min_flush_interval_ms"),
            "Error should mention min_flush_interval_ms: {}",
            err
        );
    }

    #[test]
    fn config_validate_too_slow() {
        let config = StreamingConfig {
            min_flush_interval_ms: 60_000,
            ..Default::default()
        };
        let err = config.validate().unwrap_err();
        assert!(
            err.to_string().contains("min_flush_interval_ms"),
            "Error should mention min_flush_interval_ms: {}",
            err
        );
    }

    #[test]
    fn config_validate_boundary_values() {
        // Exactly 100ms — should be valid
        let config = StreamingConfig {
            min_flush_interval_ms: 100,
            ..Default::default()
        };
        assert!(config.validate().is_ok());

        // Exactly 30000ms — should be valid
        let config = StreamingConfig {
            min_flush_interval_ms: 30_000,
            ..Default::default()
        };
        assert!(config.validate().is_ok());
    }

    // ── StreamingNotifier tests ───────────────────────────────────────

    /// A mock channel that records all sent messages for test assertions.
    struct MockChannel {
        sent_messages: Mutex<Vec<OutboundMessage>>,
        /// If set, send_message will return this error.
        fail_with: Option<String>,
    }

    impl MockChannel {
        fn new() -> Self {
            Self {
                sent_messages: Mutex::new(Vec::new()),
                fail_with: None,
            }
        }

        fn failing(error_msg: &str) -> Self {
            Self {
                sent_messages: Mutex::new(Vec::new()),
                fail_with: Some(error_msg.to_string()),
            }
        }

        fn sent_texts(&self) -> Vec<String> {
            self.sent_messages
                .lock()
                .unwrap()
                .iter()
                .map(|m| m.text.clone())
                .collect()
        }
    }

    #[async_trait::async_trait]
    impl Channel for MockChannel {
        fn name(&self) -> &str {
            "mock"
        }

        async fn start(&mut self) -> Result<(), Temm1eError> {
            Ok(())
        }

        async fn stop(&mut self) -> Result<(), Temm1eError> {
            Ok(())
        }

        async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
            if let Some(ref err) = self.fail_with {
                return Err(Temm1eError::Channel(err.clone()));
            }
            self.sent_messages.lock().unwrap().push(msg);
            Ok(())
        }

        fn file_transfer(&self) -> Option<&dyn temm1e_core::FileTransfer> {
            None
        }

        fn is_allowed(&self, _user_id: &str) -> bool {
            true
        }
    }

    #[tokio::test]
    async fn notifier_sends_tool_start() {
        let channel = Arc::new(MockChannel::new());
        let config = StreamingConfig::default();
        let notifier = StreamingNotifier::new(Some(channel.clone()), "chat-1".to_string(), &config);

        notifier.notify_tool_start("shell").await.unwrap();

        let texts = channel.sent_texts();
        assert_eq!(texts.len(), 1);
        assert!(
            texts[0].contains("Running shell command"),
            "Expected tool-start message, got: {}",
            texts[0]
        );
    }

    #[tokio::test]
    async fn notifier_sends_tool_complete_success() {
        let channel = Arc::new(MockChannel::new());
        let config = StreamingConfig::default();
        let notifier = StreamingNotifier::new(Some(channel.clone()), "chat-1".to_string(), &config);

        notifier.notify_tool_complete("shell", true).await.unwrap();

        let texts = channel.sent_texts();
        assert_eq!(texts.len(), 1);
        assert!(
            texts[0].contains("shell complete"),
            "Expected success message, got: {}",
            texts[0]
        );
    }

    #[tokio::test]
    async fn notifier_sends_tool_complete_failure() {
        let channel = Arc::new(MockChannel::new());
        let config = StreamingConfig::default();
        let notifier = StreamingNotifier::new(Some(channel.clone()), "chat-1".to_string(), &config);

        notifier
            .notify_tool_complete("browser", false)
            .await
            .unwrap();

        let texts = channel.sent_texts();
        assert_eq!(texts.len(), 1);
        assert!(
            texts[0].contains("browser failed"),
            "Expected failure message, got: {}",
            texts[0]
        );
    }

    #[tokio::test]
    async fn notifier_sends_status() {
        let channel = Arc::new(MockChannel::new());
        let config = StreamingConfig::default();
        let notifier = StreamingNotifier::new(Some(channel.clone()), "chat-1".to_string(), &config);

        notifier.notify_status("Analyzing output...").await.unwrap();

        let texts = channel.sent_texts();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0], "Analyzing output...");
    }

    #[tokio::test]
    async fn notifier_disabled_sends_nothing() {
        let channel = Arc::new(MockChannel::new());
        let config = StreamingConfig {
            enabled: false,
            ..Default::default()
        };
        let notifier = StreamingNotifier::new(Some(channel.clone()), "chat-1".to_string(), &config);

        notifier.notify_tool_start("shell").await.unwrap();
        notifier.notify_tool_complete("shell", true).await.unwrap();
        notifier.notify_status("Analyzing output...").await.unwrap();

        assert!(
            channel.sent_texts().is_empty(),
            "Disabled notifier should not send any messages"
        );
    }

    #[tokio::test]
    async fn notifier_tool_updates_disabled() {
        let channel = Arc::new(MockChannel::new());
        let config = StreamingConfig {
            enabled: true,
            tool_status_updates: false,
            ..Default::default()
        };
        let notifier = StreamingNotifier::new(Some(channel.clone()), "chat-1".to_string(), &config);

        // Tool updates are disabled...
        notifier.notify_tool_start("shell").await.unwrap();
        notifier.notify_tool_complete("shell", true).await.unwrap();

        // ...but free-form status should still work
        notifier
            .notify_status("Processing response...")
            .await
            .unwrap();

        let texts = channel.sent_texts();
        assert_eq!(texts.len(), 1);
        assert_eq!(texts[0], "Processing response...");
    }

    #[tokio::test]
    async fn notifier_no_channel_is_noop() {
        let config = StreamingConfig::default();
        let notifier = StreamingNotifier::new(None, "chat-1".to_string(), &config);

        // All calls should succeed silently with no channel
        notifier.notify_tool_start("shell").await.unwrap();
        notifier.notify_tool_complete("shell", true).await.unwrap();
        notifier.notify_status("hello").await.unwrap();
    }

    #[tokio::test]
    async fn notifier_disabled_constructor() {
        let notifier = StreamingNotifier::disabled();

        // All calls should succeed silently
        notifier.notify_tool_start("shell").await.unwrap();
        notifier.notify_tool_complete("shell", true).await.unwrap();
        notifier.notify_status("hello").await.unwrap();
    }

    #[tokio::test]
    async fn notifier_channel_error_is_swallowed() {
        let channel = Arc::new(MockChannel::failing("connection reset"));
        let config = StreamingConfig::default();
        let notifier = StreamingNotifier::new(Some(channel.clone()), "chat-1".to_string(), &config);

        // Should succeed (error is logged but not propagated)
        let result = notifier.notify_tool_start("shell").await;
        assert!(
            result.is_ok(),
            "Channel errors should be swallowed for status updates"
        );
    }

    #[tokio::test]
    async fn notifier_uses_correct_chat_id() {
        let channel = Arc::new(MockChannel::new());
        let config = StreamingConfig::default();
        let notifier =
            StreamingNotifier::new(Some(channel.clone()), "chat-42".to_string(), &config);

        notifier.notify_tool_start("shell").await.unwrap();

        let msgs = channel.sent_messages.lock().unwrap();
        assert_eq!(msgs[0].chat_id, "chat-42");
    }

    // ── StreamBuffer tests ────────────────────────────────────────────

    #[test]
    fn buffer_new_is_empty() {
        let buf = StreamBuffer::new(Duration::from_millis(100));
        assert!(!buf.has_content());
        assert_eq!(buf.peek(), "");
        assert_eq!(buf.total_chars(), 0);
        assert_eq!(buf.flush_count(), 0);
    }

    #[test]
    fn buffer_push_accumulates() {
        let mut buf = StreamBuffer::new(Duration::from_millis(100));
        buf.push("Hello ");
        buf.push("world");
        assert!(buf.has_content());
        assert_eq!(buf.peek(), "Hello world");
        assert_eq!(buf.total_chars(), 11);
    }

    #[test]
    fn buffer_try_flush_respects_interval() {
        let mut buf = StreamBuffer::new(Duration::from_secs(10));
        buf.push("Hello world ");

        // Should NOT flush — 10s haven't passed
        assert!(buf.try_flush().is_none());
        assert!(buf.has_content());
    }

    #[test]
    fn buffer_try_flush_after_interval() {
        let mut buf = StreamBuffer::new(Duration::from_millis(1));
        buf.push("Hello world ");

        // Wait for the interval to elapse
        std::thread::sleep(Duration::from_millis(5));

        let flushed = buf.try_flush();
        assert!(flushed.is_some());
        assert_eq!(flushed.unwrap(), "Hello world ");
        assert!(!buf.has_content());
        assert_eq!(buf.flush_count(), 1);
    }

    #[test]
    fn buffer_try_flush_at_word_boundary() {
        let mut buf = StreamBuffer::new(Duration::from_millis(1));
        buf.push("Hello world partial");

        std::thread::sleep(Duration::from_millis(5));

        // Should flush "Hello world " and keep "partial"
        let flushed = buf.try_flush().unwrap();
        assert_eq!(flushed, "Hello world ");
        assert_eq!(buf.peek(), "partial");
    }

    #[test]
    fn buffer_try_flush_at_punctuation() {
        let mut buf = StreamBuffer::new(Duration::from_millis(1));
        buf.push("Hello world. More text coming");

        std::thread::sleep(Duration::from_millis(5));

        let flushed = buf.try_flush().unwrap();
        assert_eq!(flushed, "Hello world. More text ");
        assert_eq!(buf.peek(), "coming");
    }

    #[test]
    fn buffer_try_flush_at_newline() {
        let mut buf = StreamBuffer::new(Duration::from_millis(1));
        buf.push("Line one\nLine two partial");

        std::thread::sleep(Duration::from_millis(5));

        let flushed = buf.try_flush().unwrap();
        assert_eq!(flushed, "Line one\nLine two ");
        assert_eq!(buf.peek(), "partial");
    }

    #[test]
    fn buffer_try_flush_empty_returns_none() {
        let mut buf = StreamBuffer::new(Duration::from_millis(1));
        std::thread::sleep(Duration::from_millis(5));
        assert!(buf.try_flush().is_none());
    }

    #[test]
    fn buffer_try_flush_single_long_token() {
        let mut buf = StreamBuffer::new(Duration::from_millis(1));
        // Push a single token with no word boundaries but within the 500-char limit
        buf.push("abcdefghijklmnop");

        std::thread::sleep(Duration::from_millis(5));

        // No word boundary found and under 500 chars — should NOT flush
        assert!(buf.try_flush().is_none());
        assert_eq!(buf.peek(), "abcdefghijklmnop");
    }

    #[test]
    fn buffer_try_flush_very_long_single_token() {
        let mut buf = StreamBuffer::new(Duration::from_millis(1));
        // Push a single token with no word boundaries, over 500 chars
        let long_token: String = "x".repeat(600);
        buf.push(&long_token);

        std::thread::sleep(Duration::from_millis(5));

        // Over 500 chars — should force-flush even without word boundary
        let flushed = buf.try_flush();
        assert!(flushed.is_some());
        assert_eq!(flushed.unwrap().len(), 600);
        assert!(!buf.has_content());
    }

    #[test]
    fn buffer_flush_all() {
        let mut buf = StreamBuffer::new(Duration::from_secs(60));
        buf.push("partial content without boundary");

        // flush_all should always flush, regardless of interval or boundaries
        let flushed = buf.flush_all().unwrap();
        assert_eq!(flushed, "partial content without boundary");
        assert!(!buf.has_content());
        assert_eq!(buf.flush_count(), 1);
    }

    #[test]
    fn buffer_flush_all_empty() {
        let mut buf = StreamBuffer::new(Duration::from_millis(100));
        assert!(buf.flush_all().is_none());
    }

    #[test]
    fn buffer_flush_all_preserves_stats() {
        let mut buf = StreamBuffer::new(Duration::from_millis(1));
        buf.push("hello ");

        std::thread::sleep(Duration::from_millis(5));
        buf.try_flush();

        buf.push("world");
        buf.flush_all();

        assert_eq!(buf.total_chars(), 11);
        assert_eq!(buf.flush_count(), 2);
    }

    #[test]
    fn buffer_from_config() {
        let config = StreamingConfig {
            min_flush_interval_ms: 2000,
            ..Default::default()
        };
        let buf = StreamBuffer::from_config(&config);
        assert!(!buf.has_content());
        assert_eq!(buf.total_chars(), 0);
    }

    #[test]
    fn buffer_multiple_flushes() {
        let mut buf = StreamBuffer::new(Duration::from_millis(1));

        buf.push("First sentence. ");
        std::thread::sleep(Duration::from_millis(5));
        let first = buf.try_flush().unwrap();
        assert_eq!(first, "First sentence. ");

        buf.push("Second sentence. ");
        std::thread::sleep(Duration::from_millis(5));
        let second = buf.try_flush().unwrap();
        assert_eq!(second, "Second sentence. ");

        assert_eq!(buf.flush_count(), 2);
        assert_eq!(buf.total_chars(), 33);
    }

    // ── Formatting helper tests ───────────────────────────────────────

    #[test]
    fn format_tool_start_known_tools() {
        assert!(format_tool_start("shell").contains("Running shell command"));
        assert!(format_tool_start("browser").contains("Browsing page"));
        assert!(format_tool_start("file_read").contains("Reading file"));
        assert!(format_tool_start("file_write").contains("Writing file"));
        assert!(format_tool_start("file_edit").contains("Editing file"));
        assert!(format_tool_start("git").contains("Running git operation"));
        assert!(format_tool_start("http").contains("Making HTTP request"));
        assert!(format_tool_start("cron").contains("Managing cron job"));
    }

    #[test]
    fn format_tool_start_unknown_tool() {
        let msg = format_tool_start("custom_tool");
        assert!(
            msg.contains("custom_tool"),
            "Unknown tools should use tool name directly: {}",
            msg
        );
    }

    #[test]
    fn format_tool_complete_success() {
        let msg = format_tool_complete("shell", true);
        assert!(msg.contains("shell"));
        assert!(msg.contains("complete"));
        // Should contain checkmark unicode
        assert!(msg.contains('\u{2713}'));
    }

    #[test]
    fn format_tool_complete_failure() {
        let msg = format_tool_complete("shell", false);
        assert!(msg.contains("shell"));
        assert!(msg.contains("failed"));
        // Should contain X mark unicode
        assert!(msg.contains('\u{2717}'));
    }

    // ── Word boundary tests ───────────────────────────────────────────

    #[test]
    fn find_boundary_space() {
        let text = "hello world partial";
        let boundary = find_last_word_boundary(text);
        assert_eq!(&text[..boundary], "hello world ");
    }

    #[test]
    fn find_boundary_newline() {
        let text = "line1\npartial";
        let boundary = find_last_word_boundary(text);
        assert_eq!(&text[..boundary], "line1\n");
    }

    #[test]
    fn find_boundary_period() {
        let text = "sentence.partial";
        let boundary = find_last_word_boundary(text);
        assert_eq!(&text[..boundary], "sentence.");
    }

    #[test]
    fn find_boundary_none() {
        let text = "nospaces";
        let boundary = find_last_word_boundary(text);
        assert_eq!(boundary, 0);
    }

    #[test]
    fn find_boundary_empty() {
        let boundary = find_last_word_boundary("");
        assert_eq!(boundary, 0);
    }

    #[test]
    fn find_boundary_comma() {
        let text = "item1, item2, partial";
        let boundary = find_last_word_boundary(text);
        assert_eq!(&text[..boundary], "item1, item2, ");
    }

    #[test]
    fn find_boundary_trailing_space() {
        let text = "full sentence ";
        let boundary = find_last_word_boundary(text);
        assert_eq!(&text[..boundary], "full sentence ");
    }

    #[test]
    fn find_boundary_unicode() {
        // Unicode text with a space boundary
        let text = "hello \u{00e9}l\u{00e8}ve partial";
        let boundary = find_last_word_boundary(text);
        // Should find the last space (before "partial")
        assert!(boundary > 0);
        let flushed = &text[..boundary];
        assert!(
            flushed.ends_with(' '),
            "Should end at space boundary: {:?}",
            flushed
        );
    }
}
