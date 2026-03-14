use crate::types::error::Temm1eError;
use crate::types::file::{FileMetadata, OutboundFile, ReceivedFile};
use crate::types::message::{InboundMessage, OutboundMessage};
use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;

/// Messaging channel trait. Implement for each platform (Telegram, Discord, etc.)
#[async_trait]
pub trait Channel: Send + Sync {
    /// Channel name (e.g., "telegram", "discord", "cli")
    fn name(&self) -> &str;

    /// Start the channel listener (connect to platform API)
    async fn start(&mut self) -> Result<(), Temm1eError>;

    /// Stop the channel listener gracefully
    async fn stop(&mut self) -> Result<(), Temm1eError>;

    /// Send a text message to a specific chat
    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError>;

    /// Get the file transfer capability for this channel (None if not supported)
    fn file_transfer(&self) -> Option<&dyn FileTransfer>;

    /// Check if a user is allowed to use this channel
    fn is_allowed(&self, user_id: &str) -> bool;

    /// Delete a message from a chat by its ID.
    ///
    /// Used to remove sensitive content (API keys, credentials) from chat
    /// history after ingestion. Default implementation is a no-op for channels
    /// that don't support deletion (e.g. CLI).
    async fn delete_message(&self, _chat_id: &str, _message_id: &str) -> Result<(), Temm1eError> {
        Ok(())
    }
}

/// Bi-directional file transfer sub-trait. Every messaging channel should implement this.
#[async_trait]
pub trait FileTransfer: Send + Sync {
    /// Receive files attached to an inbound message
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>, Temm1eError>;

    /// Send a file to a user via the messaging platform
    async fn send_file(&self, chat_id: &str, file: OutboundFile) -> Result<(), Temm1eError>;

    /// Stream a large file with progress
    async fn send_file_stream(
        &self,
        chat_id: &str,
        stream: BoxStream<'_, Bytes>,
        metadata: FileMetadata,
    ) -> Result<(), Temm1eError>;

    /// Maximum file size this channel supports (in bytes)
    fn max_file_size(&self) -> usize;
}
