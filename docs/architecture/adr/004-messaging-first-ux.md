# ADR-004: Messaging-First UX with Native File Transfer

## Status: Proposed

## Context
OpenClaw and ZeroClaw both require SSH + config file editing for setup. TEMM1E's primary differentiator is that messaging apps ARE the control plane. Users should be able to set up, configure, and operate their agent entirely through chat.

## Decision
Every Channel implementation must also implement the `FileTransfer` sub-trait:

```rust
pub trait Channel: Send + Sync {
    async fn start(&mut self) -> Result<()>;
    async fn send_message(&self, msg: OutboundMessage) -> Result<()>;
    fn name(&self) -> &str;
    fn file_transfer(&self) -> Option<&dyn FileTransfer>;
}

pub trait FileTransfer: Send + Sync {
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>>;
    async fn send_file(&self, chat_id: &str, file: OutboundFile) -> Result<()>;
    async fn send_file_stream(&self, chat_id: &str, stream: BoxStream<Bytes>, meta: FileMetadata) -> Result<()>;
    fn max_file_size(&self) -> usize;
}
```

Credential detection runs on every incoming message:
1. Check for file attachments (.env, .json, .yaml, .toml) → parse credentials
2. Check for API key patterns in text (sk-ant-*, sk-*, gsk_*) → extract and encrypt
3. Confirm to user what was detected (key names, not values)

## Consequences
- Zero SSH setup for users
- Credentials are encrypted immediately upon receipt
- File transfer is a first-class capability, not an afterthought
- Each channel must handle platform-specific file APIs
- Large file handling via presigned URLs to object storage
