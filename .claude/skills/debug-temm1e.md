# Skill: Debug a TEMM1E issue

## When to use

Use this skill when the user reports a bug, unexpected behavior, test failure, or runtime error in TEMM1E and needs help diagnosing and fixing it.

## Diagnostic steps

### Step 1: Reproduce and gather information

Ask the user for:
- Error message or stack trace
- Which crate or component is affected
- Whether it happens at compile time, test time, or runtime
- Any recent changes that may be related

### Step 2: Check compilation

```bash
# Quick check -- catches type errors without full build
cargo check --workspace

# Full check with all features
cargo check --workspace --all-features

# Check a specific crate
cargo check -p temm1e-<crate_name>
```

### Step 3: Run tests

```bash
# Run all tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p temm1e-<crate_name>

# Run a specific test with output
cargo test -p temm1e-<crate_name> -- --nocapture <test_name>

# Run tests with all features
cargo test --workspace --all-features
```

### Step 4: Run linting

```bash
# Clippy with strict warnings
cargo clippy --workspace --all-targets --all-features -- -D warnings
```

### Step 5: Trace the message flow

The TEMM1E message flow goes through these crates in order:

```
Channel (inbound)
  -> Gateway (router.rs, session.rs)
    -> Agent (runtime.rs, executor.rs, context.rs)
      -> Provider (anthropic.rs / openai_compat.rs)
      <- Provider response (streaming or complete)
    -> Tool execution (if tool_use in response)
      -> Tools (execute, sandbox check)
      <- ToolOutput
    <- Agent loop (feed tool result back to provider)
  <- Gateway
-> Channel (outbound send_message)
```

Key files to inspect per stage:
- **Channel inbound**: `crates/temm1e-channels/src/<channel>.rs` -- the `start()` method and message handler
- **Gateway routing**: `crates/temm1e-gateway/src/router.rs` and `server.rs`
- **Session management**: `crates/temm1e-gateway/src/session.rs`
- **Agent loop**: `crates/temm1e-agent/src/runtime.rs` and `executor.rs`
- **Agent context**: `crates/temm1e-agent/src/context.rs`
- **Provider call**: `crates/temm1e-providers/src/<provider>.rs` -- `complete()` or `stream()`
- **Tool execution**: `crates/temm1e-tools/src/<tool>.rs`
- **Memory ops**: `crates/temm1e-memory/src/<backend>.rs`
- **Channel outbound**: `crates/temm1e-channels/src/<channel>.rs` -- `send_message()`

### Step 6: Check configuration

```bash
# Validate config
cargo run -- config validate

# Show resolved config
cargo run -- config show
```

Config file locations (checked in order):
1. Path passed via `--config` CLI flag
2. `./temm1e.toml`
3. `$HOME/.config/temm1e/config.toml`

Key config types are in `crates/temm1e-core/src/types/config.rs`.

### Step 7: Enable verbose logging

```bash
# Set log level to debug
RUST_LOG=debug cargo run -- start

# Set per-crate log levels
RUST_LOG=temm1e_agent=trace,temm1e_providers=debug,info cargo run -- start
```

## Common issues and fixes

### Issue: "Unknown channel: <name>"
**Cause**: The channel name in the config does not match any registered channel.
**Fix**: Check `crates/temm1e-channels/src/lib.rs` `create_channel()` match arms. Verify the feature flag is enabled in `Cargo.toml`.

### Issue: "Provider api_key is required"
**Cause**: No API key configured for the provider.
**Fix**: Set `provider.api_key` in `temm1e.toml` or the corresponding environment variable.

### Issue: Feature-gated code not compiling
**Cause**: Feature flag not enabled when building.
**Fix**: Check that the feature is in the `default` list in root `Cargo.toml`, or pass `--features <feature>` explicitly.

### Issue: "Telegram channel requires a bot token"
**Cause**: Channel config missing the `token` field.
**Fix**: Add `token = "bot123:ABC..."` under `[channel.telegram]` in config.

### Issue: Rate limiting (Temm1eError::RateLimited)
**Cause**: Too many requests to the AI provider.
**Fix**: Check `security.rate_limit` config. Add backoff/retry logic. Consider reducing request frequency.

### Issue: "Sandbox violation"
**Cause**: A tool tried to access a resource not declared in its `ToolDeclarations`.
**Fix**: Update the tool's `declarations()` method to include the required file paths, network domains, or shell access.

### Issue: Memory backend connection failure
**Cause**: Database URL is wrong or the database is not running.
**Fix**: Check `memory.connection_string` or `memory.path` in config. For SQLite, ensure the directory exists. For Postgres, check that the server is running and the connection string is correct.

### Issue: Serialization errors (Temm1eError::Serialization)
**Cause**: Malformed JSON in API responses, memory metadata, or config.
**Fix**: Enable debug logging to see the raw data. Check for unexpected null values, missing fields, or encoding issues.

### Issue: File transfer path traversal blocked
**Cause**: A received file name contained `../` components.
**Fix**: This is expected security behavior. The `file_transfer.rs` `save_received_file()` strips path components. No fix needed -- this is working correctly.

### Issue: SSE stream parsing errors
**Cause**: Provider API returned unexpected SSE event format.
**Fix**: Enable trace logging for the provider crate. Check `extract_sse_event()` in the provider file. The buffer may contain incomplete events or unexpected event types.

### Issue: Test failures after adding a new implementation
**Cause**: Usually missing trait method implementations or type mismatches.
**Fix**: Run `cargo check -p <crate>` first to see compiler errors. Ensure all trait methods are implemented. Check that error types use the correct `Temm1eError` variant.

## Key error types

All errors flow through `Temm1eError` defined in `crates/temm1e-core/src/types/error.rs`:

| Variant | When to use |
|---------|------------|
| `Config(String)` | Invalid or missing configuration |
| `Provider(String)` | AI provider API errors |
| `Channel(String)` | Messaging channel errors |
| `Memory(String)` | Memory backend errors |
| `Vault(String)` | Secret storage errors |
| `Tool(String)` | Tool execution failures |
| `FileTransfer(String)` | File upload/download errors |
| `Auth(String)` | Authentication failures (HTTP 401) |
| `PermissionDenied(String)` | Authorization failures |
| `SandboxViolation(String)` | Security sandbox breaches |
| `RateLimited(String)` | HTTP 429 / rate limiting |
| `NotFound(String)` | Resource not found |
| `Skill(String)` | Skill loading/execution errors |
| `Serialization(serde_json::Error)` | JSON parse errors |
| `Io(std::io::Error)` | Filesystem I/O errors |
| `Internal(String)` | Catch-all for unexpected errors |

## Crate dependency graph

```
temm1e (binary)
  |-- temm1e-gateway
  |     |-- temm1e-core
  |     |-- temm1e-agent
  |     |     |-- temm1e-core
  |     |     |-- temm1e-providers
  |     |     |     |-- temm1e-core
  |     |     |-- temm1e-tools
  |     |     |     |-- temm1e-core
  |     |     |-- temm1e-memory
  |     |           |-- temm1e-core
  |     |-- temm1e-channels
  |           |-- temm1e-core
  |-- temm1e-vault
  |     |-- temm1e-core
  |-- temm1e-observable
  |     |-- temm1e-core
  |-- temm1e-filestore
  |     |-- temm1e-core
  |-- temm1e-skills
  |     |-- temm1e-core
  |-- temm1e-automation
        |-- temm1e-core
```

Rule: Leaf crates (providers, channels, tools, memory) must never depend on each other. All shared types live in `temm1e-core`.
