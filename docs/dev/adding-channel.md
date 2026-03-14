# Developer Guide: Adding a New Messaging Channel

This tutorial walks through adding a new messaging channel to TEMM1E. By the end, you will have a fully integrated channel that receives messages, sends replies, and transfers files.

## Overview

Adding a channel requires:

1. Implementing the `Channel` trait
2. Implementing the `FileTransfer` trait
3. Adding configuration support
4. Wiring the channel into the gateway
5. Adding a feature flag

## Step 1: Understand the Traits

The two traits you need to implement are defined in `crates/temm1e-core/src/traits/channel.rs`:

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;
    async fn start(&mut self) -> Result<(), Temm1eError>;
    async fn stop(&mut self) -> Result<(), Temm1eError>;
    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError>;
    fn file_transfer(&self) -> Option<&dyn FileTransfer>;
    fn is_allowed(&self, user_id: &str) -> bool;
}

#[async_trait]
pub trait FileTransfer: Send + Sync {
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>, Temm1eError>;
    async fn send_file(&self, chat_id: &str, file: OutboundFile) -> Result<(), Temm1eError>;
    async fn send_file_stream(
        &self,
        chat_id: &str,
        stream: BoxStream<'_, Bytes>,
        metadata: FileMetadata,
    ) -> Result<(), Temm1eError>;
    fn max_file_size(&self) -> usize;
}
```

## Step 2: Create the Channel Module

Create a new file in `crates/temm1e-channels/src/`. For this example, we will add a hypothetical "Matrix" channel.

**File**: `crates/temm1e-channels/src/matrix.rs`

```rust
use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use temm1e_core::traits::{Channel, FileTransfer};
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::file::{FileMetadata, OutboundFile, ReceivedFile};
use temm1e_core::types::message::{InboundMessage, OutboundMessage};

pub struct MatrixChannel {
    // Store configuration and client state
    homeserver: String,
    access_token: String,
    allowlist: Vec<String>,
    // ... platform SDK client
}

impl MatrixChannel {
    pub fn new(homeserver: String, access_token: String, allowlist: Vec<String>) -> Self {
        Self {
            homeserver,
            access_token,
            allowlist,
        }
    }
}

#[async_trait]
impl Channel for MatrixChannel {
    fn name(&self) -> &str {
        "matrix"
    }

    async fn start(&mut self) -> Result<(), Temm1eError> {
        // Connect to the Matrix homeserver
        // Set up event listeners for incoming messages
        // Convert platform events into InboundMessage and forward to the gateway
        tracing::info!(homeserver = %self.homeserver, "Matrix channel starting");
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Temm1eError> {
        // Gracefully disconnect from the homeserver
        tracing::info!("Matrix channel stopping");
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
        // Send a text message to the specified room (msg.chat_id)
        // Convert ParseMode to Matrix message format
        tracing::debug!(chat_id = %msg.chat_id, "Sending Matrix message");
        Ok(())
    }

    fn file_transfer(&self) -> Option<&dyn FileTransfer> {
        Some(self)
    }

    fn is_allowed(&self, user_id: &str) -> bool {
        self.allowlist.iter().any(|allowed| allowed == user_id)
    }
}

#[async_trait]
impl FileTransfer for MatrixChannel {
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>, Temm1eError> {
        let mut files = Vec::new();
        for attachment in &msg.attachments {
            // Download each attachment from the Matrix media API
            // Convert to ReceivedFile with name, mime_type, size, data
            let _ = attachment; // placeholder
        }
        Ok(files)
    }

    async fn send_file(&self, chat_id: &str, file: OutboundFile) -> Result<(), Temm1eError> {
        // Upload the file to the Matrix media API
        // Send a message with the media URL to the room
        let _ = (chat_id, file);
        Ok(())
    }

    async fn send_file_stream(
        &self,
        chat_id: &str,
        stream: BoxStream<'_, Bytes>,
        metadata: FileMetadata,
    ) -> Result<(), Temm1eError> {
        // For large files: stream the upload to Matrix media API
        let _ = (chat_id, stream, metadata);
        Ok(())
    }

    fn max_file_size(&self) -> usize {
        // Matrix default: 100 MB
        100 * 1024 * 1024
    }
}
```

## Step 3: Register the Module

Edit `crates/temm1e-channels/src/lib.rs` to include the new module:

```rust
pub mod cli;
pub mod telegram;
pub mod discord;
pub mod slack;
pub mod whatsapp;
pub mod file_transfer;

#[cfg(feature = "matrix")]
pub mod matrix;
```

## Step 4: Add Dependencies

If the channel requires a platform SDK, add it to `crates/temm1e-channels/Cargo.toml`:

```toml
[dependencies]
temm1e-core.workspace = true
async-trait.workspace = true
bytes.workspace = true
futures.workspace = true
tracing.workspace = true

# Optional: Matrix SDK
matrix-sdk = { version = "0.7", optional = true }

[features]
default = ["telegram", "discord", "slack", "whatsapp"]
telegram = ["teloxide"]
discord = ["serenity", "poise"]
slack = []
whatsapp = []
matrix = ["matrix-sdk"]
```

## Step 5: Add the Feature Flag to the Workspace

In the root `Cargo.toml`:

```toml
[features]
default = ["telegram", "discord", "slack", "whatsapp", "browser", "postgres"]
telegram = ["temm1e-channels/telegram"]
discord = ["temm1e-channels/discord"]
slack = ["temm1e-channels/slack"]
whatsapp = ["temm1e-channels/whatsapp"]
matrix = ["temm1e-channels/matrix"]          # <-- Add this
browser = ["temm1e-tools/browser"]
postgres = ["temm1e-memory/postgres"]
```

## Step 6: Wire into the Gateway

In `crates/temm1e-gateway/src/router.rs` (or wherever channels are instantiated), add logic to create the Matrix channel from config:

```rust
#[cfg(feature = "matrix")]
if let Some(channel_config) = config.channel.get("matrix") {
    if channel_config.enabled {
        let matrix = MatrixChannel::new(
            channel_config.token.clone().unwrap_or_default(),
            // ... other config
            channel_config.allowlist.clone(),
        );
        channels.push(Box::new(matrix));
    }
}
```

## Step 7: Add Configuration Documentation

Users can now configure the channel in their `config.toml`:

```toml
[channel.matrix]
enabled = true
token = "${MATRIX_ACCESS_TOKEN}"
allowlist = ["@user:matrix.org"]
file_transfer = true
```

## Step 8: Write Tests

Add tests in the channel module or in `crates/temm1e-channels/tests/`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_allowlist() {
        let channel = MatrixChannel::new(
            "https://matrix.org".into(),
            "token".into(),
            vec!["@alice:matrix.org".into()],
        );
        assert!(channel.is_allowed("@alice:matrix.org"));
        assert!(!channel.is_allowed("@bob:matrix.org"));
    }

    #[tokio::test]
    async fn test_send_message() {
        let channel = MatrixChannel::new(
            "https://matrix.org".into(),
            "token".into(),
            vec![],
        );
        let msg = OutboundMessage {
            chat_id: "!room:matrix.org".into(),
            text: "Hello".into(),
            reply_to: None,
            parse_mode: None,
        };
        // Should not error on the stub
        channel.send_message(msg).await.unwrap();
    }
}
```

## Step 9: Build and Verify

```bash
# Build with the new feature
cargo build --features matrix

# Run tests
cargo test -p temm1e-channels --features matrix

# Verify it compiles without the feature too
cargo build --no-default-features
```

## Checklist

- [ ] `Channel` trait implemented with `name()`, `start()`, `stop()`, `send_message()`, `is_allowed()`
- [ ] `FileTransfer` trait implemented with `receive_file()`, `send_file()`, `send_file_stream()`, `max_file_size()`
- [ ] `file_transfer()` returns `Some(self)` so the gateway can transfer files
- [ ] Allowlist enforced in `is_allowed()` -- empty list = deny all
- [ ] Feature flag added to `temm1e-channels/Cargo.toml` and root `Cargo.toml`
- [ ] Module gated with `#[cfg(feature = "...")]`
- [ ] Channel wired into gateway router
- [ ] Unit tests for allowlist logic and message conversion
- [ ] Integration tests for platform API (can be gated behind an env var)
- [ ] Configuration documented with example TOML
