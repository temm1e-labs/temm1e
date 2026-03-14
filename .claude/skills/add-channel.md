# Skill: Add a new messaging channel to TEMM1E

## When to use

Use this skill when the user asks to add a new messaging channel (e.g., Discord, Slack, WhatsApp, Matrix, LINE, Signal) to TEMM1E.

## Reference implementation

Study the existing Telegram channel as a complete example:
- `crates/temm1e-channels/src/telegram.rs` -- full Channel + FileTransfer implementation
- `crates/temm1e-channels/src/cli.rs` -- simpler Channel + FileTransfer without external API
- `crates/temm1e-core/src/traits/channel.rs` -- the `Channel` and `FileTransfer` trait definitions

## Steps

### 1. Create the channel source file

Create `crates/temm1e-channels/src/<channel_name>.rs` using the template below.

### 2. Add the module to lib.rs

Edit `crates/temm1e-channels/src/lib.rs`:
- Add `#[cfg(feature = "<channel_name>")] pub mod <channel_name>;` alongside the other channel modules
- Add `#[cfg(feature = "<channel_name>")] pub use <channel_name>::<ChannelName>Channel;` for re-export
- Add a match arm in `create_channel()` for the new channel name, including both the `#[cfg(feature)]` and `#[cfg(not(feature))]` variants (see the telegram pattern)

### 3. Add the feature flag to Cargo.toml files

Edit `crates/temm1e-channels/Cargo.toml`:
- Add the feature under `[features]`, e.g., `<channel_name> = ["dep:reqwest"]` (add any channel-specific deps)
- Add any new optional dependencies under `[dependencies]`

Edit the root `Cargo.toml`:
- Add the feature flag: `<channel_name> = ["temm1e-channels/<channel_name>"]`
- Add it to the `default` features list if it should be enabled by default

### 4. Write tests

Add tests in the channel source file or in `crates/temm1e-channels/src/lib.rs`:
- Test channel creation via `create_channel("<channel_name>", &config, workspace)`
- Test `name()` returns the correct string
- Test `is_allowed()` with empty and non-empty allowlists
- Test that creation fails when required config is missing (e.g., no token)

### 5. Verify

```bash
cargo check -p temm1e-channels --features <channel_name>
cargo test -p temm1e-channels --features <channel_name>
cargo clippy -p temm1e-channels --features <channel_name> -- -D warnings
```

## Template

```rust
//! <ChannelName> channel -- <brief description>.

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use tokio::sync::mpsc;

use temm1e_core::types::config::ChannelConfig;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::file::{FileData, FileMetadata, OutboundFile, ReceivedFile};
use temm1e_core::types::message::{AttachmentRef, InboundMessage, OutboundMessage, ParseMode};
use temm1e_core::{Channel, FileTransfer};

/// Maximum file size supported by <ChannelName> (adjust per platform).
const UPLOAD_LIMIT: usize = 50 * 1024 * 1024;

/// <ChannelName> messaging channel.
pub struct <ChannelName>Channel {
    /// API token / bot token.
    token: String,
    /// Allowlist of user IDs. Empty = deny all (DF-16).
    allowlist: Vec<String>,
    /// Sender for forwarding inbound messages to the gateway.
    tx: mpsc::Sender<InboundMessage>,
    /// Receiver the gateway drains. Taken once via `take_receiver()`.
    rx: Option<mpsc::Receiver<InboundMessage>>,
    // TODO: Add platform-specific client/connection fields
}

impl <ChannelName>Channel {
    /// Create a new <ChannelName> channel from a `ChannelConfig`.
    pub fn new(config: &ChannelConfig) -> Result<Self, Temm1eError> {
        let token = config
            .token
            .clone()
            .ok_or_else(|| Temm1eError::Config("<ChannelName> channel requires a token".into()))?;

        let (tx, rx) = mpsc::channel(256);

        Ok(Self {
            token,
            allowlist: config.allowlist.clone(),
            tx,
            rx: Some(rx),
        })
    }

    /// Take the inbound message receiver. The gateway should call this once.
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<InboundMessage>> {
        self.rx.take()
    }
}

#[async_trait]
impl Channel for <ChannelName>Channel {
    fn name(&self) -> &str {
        "<channel_name>"
    }

    async fn start(&mut self) -> Result<(), Temm1eError> {
        // TODO: Connect to platform API, start event listener
        tracing::info!("<ChannelName> channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Temm1eError> {
        // TODO: Disconnect cleanly
        tracing::info!("<ChannelName> channel stopped");
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
        // TODO: Send message via platform API
        todo!("Implement send_message for <ChannelName>")
    }

    fn file_transfer(&self) -> Option<&dyn FileTransfer> {
        Some(self)
    }

    fn is_allowed(&self, user_id: &str) -> bool {
        // Empty allowlist denies all users (DF-16).
        if self.allowlist.is_empty() {
            return false;
        }
        self.allowlist.iter().any(|a| a == user_id)
    }
}

#[async_trait]
impl FileTransfer for <ChannelName>Channel {
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>, Temm1eError> {
        // TODO: Download files from platform API using msg.attachments
        todo!("Implement receive_file for <ChannelName>")
    }

    async fn send_file(&self, chat_id: &str, file: OutboundFile) -> Result<(), Temm1eError> {
        // TODO: Upload file via platform API
        todo!("Implement send_file for <ChannelName>")
    }

    async fn send_file_stream(
        &self,
        _chat_id: &str,
        _stream: BoxStream<'_, Bytes>,
        metadata: FileMetadata,
    ) -> Result<(), Temm1eError> {
        Err(Temm1eError::FileTransfer(
            format!(
                "<ChannelName> does not support streaming uploads. \
                 Buffer the file ({}) and use send_file() instead.",
                metadata.name
            ),
        ))
    }

    fn max_file_size(&self) -> usize {
        UPLOAD_LIMIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(token: Option<&str>) -> ChannelConfig {
        ChannelConfig {
            enabled: true,
            token: token.map(String::from),
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        }
    }

    #[test]
    fn new_requires_token() {
        let config = test_config(None);
        assert!(<ChannelName>Channel::new(&config).is_err());
    }

    #[test]
    fn new_with_token_succeeds() {
        let config = test_config(Some("test-token"));
        let channel = <ChannelName>Channel::new(&config).unwrap();
        assert_eq!(channel.name(), "<channel_name>");
    }

    #[test]
    fn empty_allowlist_denies_all() {
        let config = test_config(Some("tok"));
        let channel = <ChannelName>Channel::new(&config).unwrap();
        assert!(!channel.is_allowed("12345"));
    }

    #[test]
    fn allowlist_allows_listed_user() {
        let mut config = test_config(Some("tok"));
        config.allowlist = vec!["12345".to_string()];
        let channel = <ChannelName>Channel::new(&config).unwrap();
        assert!(channel.is_allowed("12345"));
        assert!(!channel.is_allowed("99999"));
    }
}
```

## Key conventions

- **Allowlist security**: Empty allowlist must deny all users (DF-16). Match only on numeric user IDs, not usernames (CA-04).
- **Error types**: Use `Temm1eError::Channel(...)` for channel errors, `Temm1eError::FileTransfer(...)` for file ops.
- **mpsc pattern**: Use `tokio::sync::mpsc` for forwarding inbound messages to the gateway, with a `take_receiver()` method.
- **Feature gates**: All platform-specific channels must be behind feature flags. Never import platform SDKs unconditionally.
- **File transfer**: Always implement `FileTransfer` even if the platform has limited support -- return `Temm1eError::FileTransfer(...)` for unsupported operations.
