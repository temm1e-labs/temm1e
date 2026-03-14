# Developer Guide: Architecture

This document describes TEMM1E's internal architecture: the crate dependency graph, data flow through the system, async runtime model, and extension points for adding new functionality.

## Design Principles

1. **Trait-based extensibility** -- every subsystem is defined by a trait in `temm1e-core`; implementations live in separate crates
2. **Deny-by-default security** -- all access controls, sandboxing, and encryption are mandatory
3. **Messaging-first UX** -- messaging apps are the primary control plane; file transfer is first-class
4. **Dual-mode runtime** -- same binary runs in cloud (headless, TLS) and local (localhost, optional GUI) modes
5. **Ecosystem compatibility** -- reads ZeroClaw TOML and OpenClaw YAML configs out of the box

These are codified in Architecture Decision Records at `docs/architecture/adr/001-006`.

## Crate Dependency Graph

```
temm1e (binary)
  |
  +-- temm1e-gateway
  |     +-- temm1e-core
  |     +-- axum, tower, tower-http, rustls
  |
  +-- temm1e-agent
  |     +-- temm1e-core
  |
  +-- temm1e-providers
  |     +-- temm1e-core
  |     +-- reqwest
  |
  +-- temm1e-channels
  |     +-- temm1e-core
  |     +-- teloxide (Telegram)
  |     +-- serenity, poise (Discord)
  |     +-- reqwest (Slack, WhatsApp)
  |
  +-- temm1e-memory
  |     +-- temm1e-core
  |     +-- sqlx (SQLite, PostgreSQL)
  |
  +-- temm1e-vault
  |     +-- temm1e-core
  |     +-- chacha20poly1305, ed25519-dalek
  |
  +-- temm1e-tools
  |     +-- temm1e-core
  |     +-- chromiumoxide (browser)
  |
  +-- temm1e-skills
  |     +-- temm1e-core
  |
  +-- temm1e-automation
  |     +-- temm1e-core
  |
  +-- temm1e-observable
  |     +-- temm1e-core
  |     +-- tracing, opentelemetry
  |
  +-- temm1e-filestore
        +-- temm1e-core
        +-- aws-sdk-s3
```

**Key rule**: all crates depend on `temm1e-core` for trait definitions and shared types. No implementation crate depends on another implementation crate. This keeps the dependency graph clean and enables independent compilation.

## Core Crate: temm1e-core

The `temm1e-core` crate contains:

- **12 trait definitions** (`traits/`) -- `Provider`, `Channel`, `FileTransfer`, `Tool`, `Memory`, `Vault`, `FileStore`, `Observable`, `Identity`, `Tunnel`, `Orchestrator`, `Tenant`, `Peripheral`
- **Shared types** (`types/`) -- `InboundMessage`, `OutboundMessage`, `CompletionRequest`, `CompletionResponse`, `Temm1eError`, `Temm1eConfig`, etc.
- **Config loading** (`config/`) -- TOML parser, YAML compat reader, environment variable expansion, `vault://` URI resolution

`temm1e-core` has minimal external dependencies: `serde`, `async-trait`, `thiserror`, `chrono`, `bytes`, `futures`. It defines interfaces only; no business logic.

## Data Flow

### Message Processing Pipeline

```
1. Platform Event
   Telegram/Discord/Slack/WhatsApp sends a webhook or gateway event

2. Channel::start()
   The channel listener receives the event and converts it to an InboundMessage

3. Gateway Router
   Routes the InboundMessage to the correct session
   Performs rate limiting and access control (Channel::is_allowed)

4. Session Manager
   Looks up or creates a SessionContext with:
     - session_id
     - channel + chat_id + user_id
     - conversation history (from Memory)
     - workspace_path

5. Agent Runtime Loop
   a. Context Assembly
      - Load session history from Memory
      - Load relevant long-term memory via Memory::search()
      - Load active skills from the skill registry
      - Build system prompt with workspace context

   b. Provider Call
      - Build CompletionRequest with messages + tools + system prompt
      - Call Provider::stream() for streaming response

   c. Tool Execution (may loop)
      - Parse tool calls from the model response (ContentPart::ToolUse)
      - Validate against ToolDeclarations (capability check)
      - Execute via Tool::execute() in sandboxed context
      - Return ToolOutput to the model for the next iteration

   d. Reply Streaming
      - Stream text deltas back through Channel::send_message()

   e. Persistence
      - Save conversation to Memory::store()
      - Update session state

6. File Handling (parallel path)
   - If InboundMessage has attachments:
     - FileTransfer::receive_file() downloads the file
     - Vault detector scans for API key patterns
     - Credentials are encrypted and stored via Vault::store_secret()
   - If reply includes files:
     - Small files: FileTransfer::send_file()
     - Large files: FileStore::store() + presigned_url()
```

### Credential Flow

```
1. User sends a .env file via Telegram
2. Channel receives the message with an AttachmentRef
3. FileTransfer::receive_file() downloads the file data
4. Vault detector parses key=value pairs and API key patterns
5. Each secret is encrypted: Vault::store_secret(key, plaintext)
6. Confirmation sent back: "Stored 3 secrets: AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY, ANTHROPIC_API_KEY"
7. Plaintext is zeroed from memory (zeroize crate)
```

## Async Runtime Model

TEMM1E uses **Tokio** as its async runtime with the multi-threaded scheduler.

| Component | Execution Model |
|-----------|----------------|
| Channel listeners | Each runs as a separate `tokio::spawn` task |
| Gateway server | axum server on its own Tokio task |
| Agent runtime | Spawned per-message; uses `spawn_blocking` for CPU-bound work |
| Memory operations | Async via sqlx (SQLite/PostgreSQL) |
| File I/O | Async via `tokio::fs` |
| Browser automation | Async via chromiumoxide |
| Provider API calls | Async via reqwest with streaming |

### Concurrency

- Multiple channels run concurrently as independent tasks
- Multiple messages can be processed in parallel
- Tool execution is serialized per session (to prevent workspace conflicts)
- Memory writes are serialized per session (to maintain ordering)

## Error Handling Strategy

| Layer | Approach |
|-------|----------|
| `temm1e-core` types | `Temm1eError` enum with `thiserror` -- domain-specific variants |
| Implementation crates | Return `Result<T, Temm1eError>` at crate boundaries |
| Binary entry point | `anyhow::Result` for ergonomic error propagation with backtraces |
| User-facing errors | Converted to friendly messages before reaching channels |

Error propagation path:
```
sqlx::Error --> Temm1eError::Memory("...") --> anyhow::Error (at binary level)
                                            --> "Sorry, I had trouble accessing my memory" (to user)
```

## Configuration System

### Resolution Order

```
Compiled defaults
  |
  v
/etc/temm1e/config.toml (system)
  |
  v
~/.temm1e/config.toml (user)
  |
  v
./config.toml (workspace)
  |
  v
TEMM1E_* environment variables
  |
  v
CLI flags (--mode, --config)
  |
  v
vault:// URI resolution
```

Later sources override earlier ones. The config module in `temm1e-core/src/config/` handles:
- `loader.rs` -- file discovery and loading
- `toml.rs` -- native TOML parser
- `yaml_compat.rs` -- OpenClaw YAML format reader
- `env.rs` -- `${ENV_VAR}` expansion and `TEMM1E_*` mapping

### Dual Mode Defaults

| Setting | Cloud Mode | Local Mode |
|---------|-----------|------------|
| `gateway.host` | `0.0.0.0` | `127.0.0.1` |
| `gateway.tls` | Required | Optional |
| `memory.backend` | PostgreSQL | SQLite |
| `vault.backend` | Cloud KMS | Local ChaCha20 |
| Browser | Headless only | Headed or headless |

`auto` mode detects the environment:
1. Container runtime present? (/.dockerenv, cgroup) -> cloud
2. Cloud metadata endpoint reachable? (169.254.169.254) -> cloud
3. Display server available? ($DISPLAY, $WAYLAND_DISPLAY) -> local with GUI
4. Otherwise -> local headless

## Extension Points

### Adding a New Component

| What | Where | How |
|------|-------|-----|
| AI Provider | `crates/temm1e-providers/src/` | Implement `Provider` trait |
| Messaging Channel | `crates/temm1e-channels/src/` | Implement `Channel` + `FileTransfer` traits |
| Tool | `crates/temm1e-tools/src/` | Implement `Tool` trait |
| Memory Backend | `crates/temm1e-memory/src/` | Implement `Memory` trait |
| File Storage Backend | `crates/temm1e-filestore/src/` | Implement `FileStore` trait |
| Tunnel Provider | (new crate or in gateway) | Implement `Tunnel` trait |

See the step-by-step tutorials:
- [Adding a Channel](adding-channel.md)
- [Adding a Provider](adding-provider.md)

### Feature Gating

Optional implementations are gated behind Cargo feature flags defined in the workspace `Cargo.toml`:

```toml
[features]
default = ["telegram", "discord", "slack", "whatsapp", "browser", "postgres"]
telegram = ["temm1e-channels/telegram"]
discord = ["temm1e-channels/discord"]
browser = ["temm1e-tools/browser"]
postgres = ["temm1e-memory/postgres"]
```

New extensions should follow the same pattern: add a feature flag so the component can be excluded from builds.
