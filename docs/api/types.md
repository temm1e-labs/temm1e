# API Reference: Key Types

All shared types are defined in `crates/temm1e-core/src/types/`. They are re-exported from `temm1e_core::types`.

---

## Messages (`types/message.rs`)

### InboundMessage

Normalized inbound message from any channel. Every channel implementation converts platform-specific messages into this format.

```rust
pub struct InboundMessage {
    pub id: String,                              // Platform-specific message ID
    pub channel: String,                         // Channel name ("telegram", "discord", etc.)
    pub chat_id: String,                         // Chat/conversation ID
    pub user_id: String,                         // Sender user ID
    pub username: Option<String>,                // Human-readable username
    pub text: Option<String>,                    // Message text (None for file-only messages)
    pub attachments: Vec<AttachmentRef>,          // File attachments
    pub reply_to: Option<String>,                // ID of message being replied to
    pub timestamp: chrono::DateTime<chrono::Utc>, // When the message was sent
}
```

### AttachmentRef

Lazy reference to a file attachment. The actual file data is downloaded only when needed via the `FileTransfer` trait.

```rust
pub struct AttachmentRef {
    pub file_id: String,              // Platform-specific file ID
    pub file_name: Option<String>,    // Original file name
    pub mime_type: Option<String>,    // MIME type (e.g., "application/pdf")
    pub size: Option<usize>,         // File size in bytes
}
```

### OutboundMessage

Message to send back through a channel.

```rust
pub struct OutboundMessage {
    pub chat_id: String,              // Target chat/conversation ID
    pub text: String,                 // Message body
    pub reply_to: Option<String>,     // ID of message to reply to
    pub parse_mode: Option<ParseMode>, // Text formatting mode
}
```

### ParseMode

```rust
pub enum ParseMode {
    Markdown,
    Html,
    Plain,
}
```

### CompletionRequest

Request sent to an AI model provider.

```rust
pub struct CompletionRequest {
    pub model: String,                // Model identifier (e.g., "claude-sonnet-4-6")
    pub messages: Vec<ChatMessage>,   // Conversation history
    pub tools: Vec<ToolDefinition>,   // Available tool definitions
    pub max_tokens: Option<u32>,      // Maximum response tokens
    pub temperature: Option<f32>,     // Sampling temperature
    pub system: Option<String>,       // System prompt
}
```

### ChatMessage

A single message in the conversation history.

```rust
pub struct ChatMessage {
    pub role: Role,
    pub content: MessageContent,
}
```

### Role

```rust
pub enum Role {
    System,
    User,
    Assistant,
    Tool,
}
```

Serialized as lowercase strings: `"system"`, `"user"`, `"assistant"`, `"tool"`.

### MessageContent

```rust
pub enum MessageContent {
    Text(String),
    Parts(Vec<ContentPart>),
}
```

`Text` is used for simple text messages. `Parts` is used for multi-part messages containing tool calls and results.

### ContentPart

```rust
pub enum ContentPart {
    Text { text: String },
    ToolUse { id: String, name: String, input: serde_json::Value },
    ToolResult { tool_use_id: String, content: String, is_error: bool },
}
```

Serialized with a `"type"` discriminator field: `"text"`, `"tool_use"`, `"tool_result"`.

### ToolDefinition

Schema for a tool exposed to the AI model.

```rust
pub struct ToolDefinition {
    pub name: String,                   // Tool name
    pub description: String,            // Human-readable description
    pub parameters: serde_json::Value,  // JSON Schema for parameters
}
```

### CompletionResponse

Response from an AI model (non-streaming).

```rust
pub struct CompletionResponse {
    pub id: String,                     // Provider-specific response ID
    pub content: Vec<ContentPart>,      // Response content parts
    pub stop_reason: Option<String>,    // Why the model stopped ("end_turn", "tool_use", etc.)
    pub usage: Usage,                   // Token usage
}
```

### StreamChunk

A single chunk in a streaming response.

```rust
pub struct StreamChunk {
    pub delta: Option<String>,          // Text delta
    pub tool_use: Option<ContentPart>,  // Tool use content part (if emitting a tool call)
    pub stop_reason: Option<String>,    // Set on the final chunk
}
```

### Usage

Token usage statistics.

```rust
pub struct Usage {
    pub input_tokens: u32,
    pub output_tokens: u32,
}
```

---

## Files (`types/file.rs`)

### ReceivedFile

A file received from a user via a messaging channel. Contains the full file data.

```rust
pub struct ReceivedFile {
    pub name: String,       // Original file name
    pub mime_type: String,  // MIME type
    pub size: usize,        // Size in bytes
    pub data: Bytes,        // Raw file data
}
```

### OutboundFile

A file to send to a user via a messaging channel.

```rust
pub struct OutboundFile {
    pub name: String,            // File name
    pub mime_type: String,       // MIME type
    pub data: FileData,          // File content (bytes or URL)
    pub caption: Option<String>, // Optional caption/description
}
```

### FileData

File data can be raw bytes or a URL (e.g., a presigned S3 URL for large files).

```rust
pub enum FileData {
    Bytes(Bytes),
    Url(String),
}
```

### FileMetadata

Metadata about a file for storage and transfer.

```rust
pub struct FileMetadata {
    pub name: String,                  // File name
    pub mime_type: String,             // MIME type
    pub size: Option<usize>,          // Size in bytes
    pub content_hash: Option<String>, // Content hash for deduplication
}
```

---

## Sessions (`types/session.rs`)

### SessionContext

Active session context passed to the agent runtime.

```rust
pub struct SessionContext {
    pub session_id: String,                  // Unique session ID
    pub channel: String,                     // Originating channel name
    pub chat_id: String,                     // Chat/conversation ID
    pub user_id: String,                     // User ID
    pub history: Vec<ChatMessage>,           // Conversation history
    pub workspace_path: std::path::PathBuf,  // Sandboxed workspace directory
}
```

### SessionInfo

Session metadata for listing active and past sessions.

```rust
pub struct SessionInfo {
    pub id: String,                              // Session ID
    pub channel: String,                         // Channel name
    pub user_id: String,                         // User ID
    pub last_active: chrono::DateTime<chrono::Utc>, // Last activity timestamp
    pub message_count: u64,                      // Total messages in session
}
```

---

## Configuration (`types/config.rs`)

### Temm1eConfig

Top-level configuration struct. Deserialized from TOML.

```rust
pub struct Temm1eConfig {
    pub temm1e: Temm1eSection,
    pub gateway: GatewayConfig,
    pub provider: ProviderConfig,
    pub memory: MemoryConfig,
    pub vault: VaultConfig,
    pub filestore: FileStoreConfig,
    pub security: SecurityConfig,
    pub heartbeat: HeartbeatConfig,
    pub cron: CronConfig,
    pub channel: HashMap<String, ChannelConfig>,
    pub tools: ToolsConfig,
    pub tunnel: Option<TunnelConfig>,
    pub observability: ObservabilityConfig,
}
```

See the [Configuration Reference](config.md) for full details on each section.

### ChannelConfig

Per-channel configuration (used as `HashMap<String, ChannelConfig>` keyed by channel name).

```rust
pub struct ChannelConfig {
    pub enabled: bool,                   // Whether this channel is active
    pub token: Option<String>,           // Bot/API token
    pub allowlist: Vec<String>,          // Allowed user IDs or usernames
    pub file_transfer: bool,             // Enable file transfer (default true)
    pub max_file_size: Option<String>,   // Max file size override
}
```

### ProviderConfig

AI provider configuration. The `api_key` field is redacted in debug output.

```rust
pub struct ProviderConfig {
    pub name: Option<String>,       // Provider name
    pub api_key: Option<String>,    // API key (supports ${ENV_VAR} and vault://)
    pub model: Option<String>,      // Model identifier
    pub base_url: Option<String>,   // Custom API base URL
}
```

---

## Errors (`types/error.rs`)

### Temm1eError

Central error enum used across all crates. Built with `thiserror`.

```rust
pub enum Temm1eError {
    Config(String),           // Configuration errors
    Provider(String),         // AI provider errors
    Channel(String),          // Channel connection/communication errors
    Memory(String),           // Memory backend errors
    Vault(String),            // Vault encryption/decryption errors
    Tool(String),             // Tool execution errors
    FileTransfer(String),     // File transfer errors
    Auth(String),             // Authentication errors
    PermissionDenied(String), // Authorization errors
    SandboxViolation(String), // Sandbox policy violations
    RateLimited(String),      // Rate limiting errors
    NotFound(String),         // Resource not found
    Skill(String),            // Skill loading/execution errors
    Serialization(serde_json::Error), // JSON serialization errors (From impl)
    Io(std::io::Error),       // IO errors (From impl)
    Internal(String),         // Internal/unexpected errors
}
```

All variants implement `Display` via `thiserror` with descriptive prefixes (e.g., `"Configuration error: ..."`, `"Sandbox violation: ..."`).

The `Serialization` and `Io` variants have `From` implementations for automatic conversion from `serde_json::Error` and `std::io::Error` respectively.

At the binary level, `anyhow::Result` wraps `Temm1eError` for ergonomic error propagation with backtraces.
