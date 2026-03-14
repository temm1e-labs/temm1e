# TEMM1E -- Claude Code Project Guide

## Project overview

TEMM1E is a cloud-native Rust AI agent runtime. It connects to messaging channels (Telegram, Discord, Slack, CLI), routes messages through an agent loop that calls AI providers (Anthropic, OpenAI-compatible), executes tools (shell, browser, file ops), and persists conversation history to memory backends (SQLite, Markdown).

The codebase is a Cargo workspace with 15 crates plus a root binary.

## Build commands

```bash
# Quick compilation check (fastest feedback loop)
cargo check --workspace

# Build all crates in debug mode
cargo build --workspace

# Run all tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p temm1e-<crate>

# Clippy lints (CI gate -- treats warnings as errors)
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Format check
cargo fmt --all -- --check

# Format code
cargo fmt --all

# Build release binary
cargo build --release --bin temm1e

# Run the binary
cargo run -- start
cargo run -- chat
cargo run -- status
cargo run -- config validate
```

## Architecture

### Workspace structure

```
crates/
  temm1e-core        -- Shared traits, types, error enum, config loader
  temm1e-gateway     -- HTTP/WebSocket server, routing, session management
  temm1e-agent       -- Agent runtime loop, context, executor
  temm1e-providers   -- AI provider integrations (Anthropic, OpenAI-compatible)
  temm1e-channels    -- Messaging channels (CLI, Telegram, Discord, Slack)
  temm1e-memory      -- Persistent memory backends (SQLite, Markdown)
  temm1e-tools       -- Agent tool implementations (shell, browser, file ops)
  temm1e-vault       -- Secret storage with ChaCha20-Poly1305 encryption
  temm1e-skills      -- Skill registry and execution
  temm1e-automation  -- Cron jobs and scheduled tasks
  temm1e-observable  -- OpenTelemetry tracing and metrics
  temm1e-filestore   -- File storage (local, S3)
  temm1e-test-utils  -- Shared test utilities
src/
  main.rs             -- CLI entry point (clap)
```

### Architecture rules

1. **Traits in core, implementations in crates**: All shared traits (`Channel`, `Provider`, `Memory`, `Tool`, `FileTransfer`, etc.) are defined in `temm1e-core/src/traits/`. Implementations go in their respective crates.

2. **No cross-implementation dependencies**: Leaf crates (providers, channels, tools, memory backends) must never depend on each other. Shared types live in `temm1e-core`.

3. **Feature flags for optional dependencies**: Platform-specific channels (Telegram, Discord, Slack) and tools (browser) are behind Cargo feature flags. Never import their SDKs unconditionally.

4. **Factory pattern**: Each crate exposes a `create_*()` factory function (e.g., `create_channel()`, `create_provider()`, `create_memory_backend()`) that dispatches by name string.

### Message flow

```
Channel.start() -> inbound message via mpsc::channel
  -> Gateway router
    -> Agent runtime loop
      -> Provider.complete() or Provider.stream()
      <- CompletionResponse (may contain tool_use)
      -> Tool.execute() if tool_use
      <- ToolOutput fed back to provider
    <- Final response
  -> Channel.send_message(OutboundMessage)
```

## Code style conventions

- **Edition**: Rust 2021
- **Minimum Rust version**: 1.82
- **Async traits**: Use `#[async_trait]` from the `async_trait` crate for all async trait definitions and implementations
- **Error handling**: All fallible operations return `Result<T, Temm1eError>`. The `Temm1eError` enum is in `crates/temm1e-core/src/types/error.rs`. Use the appropriate variant (`Config`, `Provider`, `Channel`, `Memory`, `Tool`, `FileTransfer`, etc.)
- **Logging**: Use the `tracing` crate (`tracing::info!`, `tracing::debug!`, `tracing::error!`, `tracing::warn!`). Include structured fields (e.g., `tracing::info!(id = %entry.id, "Stored entry")`)
- **Serialization**: Use `serde` with `derive` for all data types. JSON via `serde_json`, TOML via `toml` for config
- **Naming**: Structs use PascalCase with the crate's domain prefix (e.g., `TelegramChannel`, `AnthropicProvider`, `SqliteMemory`). Trait names are bare (e.g., `Channel`, `Provider`, `Memory`, `Tool`)
- **Tests**: Place unit tests in a `#[cfg(test)] mod tests` block at the bottom of each file. Use `#[tokio::test]` for async tests

## Testing conventions

- Tests use `temm1e-test-utils` for shared test helpers
- SQLite tests use in-memory databases: `SqliteMemory::new("sqlite::memory:")`
- File-based tests use `tempfile::tempdir()` for temporary directories
- All channels and providers have creation/configuration tests
- Memory backends test the full CRUD cycle plus search and session operations
- Provider tests verify request body construction and SSE parsing without hitting real APIs

## Security conventions

- Empty allowlists deny all users (DF-16)
- Match only on numeric user IDs, never usernames (CA-04)
- Sanitize file names to prevent path traversal (strip directory components)
- Tools must declare resource needs in `ToolDeclarations`; the sandbox enforcer validates these
- Never log API keys or tokens at info level; use debug with masking
- Provider config redacts API keys in Debug output

## Configuration

Config is loaded from TOML files. See `crates/temm1e-core/src/types/config.rs` for the full schema. Key sections: `gateway`, `provider`, `memory`, `vault`, `channel.*`, `tools`, `security`, `observability`.

## Custom skills

Claude Code skills for common tasks are in `.claude/skills/`:
- `add-channel.md` -- Add a new messaging channel
- `add-provider.md` -- Add a new AI provider
- `add-memory-backend.md` -- Add a new memory backend
- `add-tool.md` -- Add a new agent tool
- `debug-temm1e.md` -- Debug and troubleshoot issues
