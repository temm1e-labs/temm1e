# TEMM1E Developer Protocol

> The single source of truth for building TEMM1E. Every contributor — human or AI — follows this protocol.

---

## Principles

### I. Traits in Core, Implementations in Crates

All shared abstractions live in `temm1e-core/src/traits/`. Implementations live in their respective crates. A channel implementation never imports a provider. A tool never imports a memory backend. If two crates need the same type, it belongs in `temm1e-core/src/types/`.

### II. No Cross-Implementation Dependencies

Leaf crates (providers, channels, tools, memory backends) must never depend on each other. The dependency graph is a star: core at the center, everything else at the edges. Violations create coupling that makes the system impossible to extend.

### III. Feature Flags for Optional Dependencies

Platform-specific SDKs (teloxide, serenity, chromiumoxide) are behind Cargo feature flags. Never `use teloxide::*` unconditionally. The binary must compile with zero optional features enabled.

### IV. Factory Dispatch by Name String

Each crate exposes a `create_*()` function that takes a config and returns `Box<dyn Trait>`. The gateway and main.rs never construct implementations directly — they call the factory with a name string from config.

### V. Every Error is a Temm1eError

No `unwrap()` in production code. No `Box<dyn Error>`. Every fallible operation returns `Result<T, Temm1eError>` using the appropriate variant. The caller always knows what domain the error came from.

### VI. Security is Structural, Not Optional

Empty allowlists deny everyone. Numeric IDs only — never usernames. Path traversal protection on every file operation. API keys redacted in Debug output. Vault key files are 0600. These rules are not suggestions — they are enforced by the code.

---

## Architecture

```
temm1e (binary)                    src/main.rs — CLI, onboarding, agent init
├── temm1e-core         (traits)   13 trait definitions, types, errors, config
├── temm1e-gateway      (http)     axum server, health, identity, OAuth
├── temm1e-agent        (brain)    TEM'S MIND — 20 autonomy modules
├── temm1e-providers    (llm)      Anthropic, OpenAI-compat (6 providers)
├── temm1e-channels     (io)       Telegram, Discord, Slack, CLI
├── temm1e-memory       (storage)  SQLite + Markdown with failover
├── temm1e-vault        (crypto)   ChaCha20-Poly1305 encrypted secrets
├── temm1e-tools        (actions)  Shell, browser, file ops, web fetch, git
├── temm1e-skills       (extend)   Skill registry (TemHub v1)
├── temm1e-automation   (cron)     Heartbeat, scheduled tasks
├── temm1e-observable   (telemetry) OpenTelemetry, 6 predefined metrics
├── temm1e-filestore    (files)    Local + S3/R2 storage
└── temm1e-test-utils   (testing)  Shared test helpers
```

### Message Flow

```
Channel.start() → inbound message via mpsc::channel
  → Gateway router
    → Agent runtime loop
      → Provider.complete() or Provider.stream()
      ← CompletionResponse (may contain tool_use)
      → Tool.execute() if tool_use
      ← ToolOutput fed back to provider
    ← Final response
  → Channel.send_message(OutboundMessage)
```

### Key Files

| File | Purpose |
|------|---------|
| `src/main.rs` | CLI entry, onboarding flow, credential detection, agent init |
| `crates/temm1e-core/src/types/config.rs` | Full config schema, validation, agent-accessible subset |
| `crates/temm1e-core/src/types/error.rs` | Unified Temm1eError enum |
| `crates/temm1e-core/src/traits/` | All 13 trait definitions |
| `crates/temm1e-agent/src/runtime.rs` | Agent loop, tool execution, streaming |
| `crates/temm1e-agent/src/context.rs` | Context building, token budgeting, history management |
| `crates/temm1e-providers/src/lib.rs` | Provider factory |
| `crates/temm1e-channels/src/lib.rs` | Channel factory |
| `crates/temm1e-memory/src/lib.rs` | Memory backend factory |

---

## Patterns

### Error Handling

All errors use `Temm1eError` from `temm1e-core/src/types/error.rs`:

```rust
// 15 domain-specific variants
Temm1eError::Config(String)          // Bad config
Temm1eError::Provider(String)        // LLM API error
Temm1eError::Channel(String)         // Messaging channel error
Temm1eError::Memory(String)          // Storage error
Temm1eError::Vault(String)           // Encryption error
Temm1eError::Tool(String)            // Tool execution error
Temm1eError::FileTransfer(String)    // File ops error
Temm1eError::Auth(String)            // Authentication error
Temm1eError::PermissionDenied(String)
Temm1eError::SandboxViolation(String)
Temm1eError::RateLimited(String)
Temm1eError::NotFound(String)
Temm1eError::Skill(String)
Temm1eError::Serialization(#[from] serde_json::Error)  // auto-convert
Temm1eError::Io(#[from] std::io::Error)                // auto-convert
Temm1eError::Internal(String)
```

**Rules:**
- Pick the most specific variant. `Provider` for LLM errors, not `Internal`.
- Include actionable context: `"Anthropic API error (429): rate limited"`, not `"request failed"`.
- HTTP status codes map to variants: 401/403 → `Auth`, 429 → `RateLimited`, 404 → `NotFound`.
- Use `.map_err()` to convert upstream errors: `.map_err(|e| Temm1eError::Tool(format!("Browser failed: {e}")))?`

### Trait Definitions

All traits follow this structure:

```rust
#[async_trait]
pub trait Channel: Send + Sync {
    fn name(&self) -> &str;                                           // Identity
    async fn start(&mut self) -> Result<(), Temm1eError>;            // Lifecycle
    async fn stop(&mut self) -> Result<(), Temm1eError>;             // Lifecycle
    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError>;  // Core
    fn file_transfer(&self) -> Option<&dyn FileTransfer>;             // Optional capability
    fn is_allowed(&self, user_id: &str) -> bool;                      // Security
    async fn delete_message(&self, _chat_id: &str, _message_id: &str) -> Result<(), Temm1eError> {
        Ok(())  // Default no-op
    }
}
```

**Rules:**
- Always `#[async_trait]` + `Send + Sync`
- Every method returns `Result<T, Temm1eError>` (except `name()`, `is_allowed()`)
- Default no-op implementations for optional capabilities
- `name()` returns a static `&str` identifying the implementation

### Factory Functions

```rust
pub fn create_channel(
    name: &str,
    config: &ChannelConfig,
    workspace: PathBuf,
) -> Result<Box<dyn Channel>, Temm1eError> {
    match name {
        "cli" => Ok(Box::new(CliChannel::new(workspace))),

        #[cfg(feature = "telegram")]
        "telegram" => Ok(Box::new(TelegramChannel::new(config)?)),

        #[cfg(not(feature = "telegram"))]
        "telegram" => Err(Temm1eError::Config(
            "Telegram support not enabled. Compile with --features telegram".into(),
        )),

        other => Err(Temm1eError::Config(format!("Unknown channel: {other}"))),
    }
}
```

**Rules:**
- Return `Box<dyn Trait>`
- Feature-gated with both `#[cfg(feature)]` and `#[cfg(not(feature))]` arms
- Disabled features return a descriptive `Temm1eError::Config` explaining how to enable
- Catch-all `other` arm for unknown names

### Builder Pattern

```rust
impl OpenAICompatProvider {
    pub fn new(api_key: String) -> Self { /* required fields */ }
    pub fn with_base_url(mut self, url: String) -> Self { self.base_url = url; self }
    pub fn with_extra_headers(mut self, h: HashMap<String, String>) -> Self { self.extra_headers = h; self }
}

// Usage:
let provider = OpenAICompatProvider::new(key)
    .with_base_url(url)
    .with_extra_headers(headers);
```

**Rules:**
- `new()` takes only required fields
- `with_*()` methods take `mut self`, return `Self` for chaining
- Used consistently across providers, channels, tools

### Config Sections

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GatewayConfig {
    #[serde(default = "default_host")]
    pub host: String,
    #[serde(default = "default_port")]
    pub port: u16,
    #[serde(default)]
    pub tls: bool,
    pub tls_cert: Option<String>,  // Optional fields are Option<T>
}

impl Default for GatewayConfig {
    fn default() -> Self {
        Self { host: "127.0.0.1".to_string(), port: 8080, tls: false, tls_cert: None, tls_key: None }
    }
}

fn default_host() -> String { "127.0.0.1".to_string() }
fn default_port() -> u16 { 8080 }
```

**Rules:**
- Always derive `Debug, Clone, Serialize, Deserialize`
- Use `#[serde(default = "fn_name")]` for non-trivial defaults
- Use `#[serde(default)]` for `bool`, `Vec`, `Option`, `HashMap`
- Implement `Default` explicitly
- Add validation in `AgentAccessibleConfig::validate()` for bounds checking
- API keys get custom `Debug` impl that redacts: `sk-an...xyz9`

### Logging

```rust
// Lifecycle events — info level
tracing::info!("Telegram channel started");
tracing::info!(id = %entry.id, "Stored entry");

// Implementation details — debug level
tracing::debug!(provider = "anthropic", model = %request.model, "Sending request");

// Recoverable issues — warn level
tracing::warn!("CLI channel receiver dropped, stopping stdin reader");

// Failures — error level
tracing::error!(error = %e, "Error reading stdin");
```

**Rules:**
- `%` for Display formatting, `?` for Debug formatting
- Always include structured fields for searchability
- Never log API keys, tokens, or passwords at info level
- Use `debug` with the redacting `Debug` impl for sensitive structs

---

## Security Checklist

Every PR must satisfy these. Non-negotiable.

| Rule | Code Pattern | Reference |
|------|-------------|-----------|
| Empty allowlist = deny all | `if list.is_empty() { return false; }` | DF-16 |
| Match numeric user IDs only | `list.iter().any(\|a\| a == user_id)` — never match username | CA-04 |
| Sanitize filenames | `Path::new(&name).file_name().unwrap_or("unnamed")` | Path traversal |
| Validate resolved paths | `full.starts_with(&self.base_dir)` | Path traversal |
| Redact keys in Debug | Custom `fmt::Debug` impl: `sk-an...xyz9` | Key leaks |
| Vault keys are 0600 | `Permissions::from_mode(0o600)` on Unix | Key material |
| Zeroize key material | `Zeroizing<[u8; 32]>` from zeroize crate | Memory safety |
| Tools declare resources | `ToolDeclarations { file_access, network_access, shell_access }` | Sandbox |
| Validate config bounds | `max_turns > 0`, `context_tokens >= 1000`, `timeout <= 86400` | DoS prevention |
| Delete credential messages | `channel.delete_message()` after reading API keys/passwords | Chat hygiene |

---

## Testing Conventions

### Structure

```rust
#[cfg(test)]
mod tests {
    use super::*;

    // Helper to reduce boilerplate
    fn make_entry(id: &str, content: &str) -> MemoryEntry {
        MemoryEntry { id: id.to_string(), content: content.to_string(), /* ... */ }
    }

    #[tokio::test]
    async fn store_and_get() {
        let mem = SqliteMemory::new("sqlite::memory:").await.unwrap();
        let entry = make_entry("e1", "hello");
        mem.store(entry).await.unwrap();
        let fetched = mem.get("e1").await.unwrap();
        assert_eq!(fetched.unwrap().content, "hello");
    }
}
```

### Rules

| Convention | Example |
|-----------|---------|
| Async tests | `#[tokio::test]` |
| In-memory SQLite | `SqliteMemory::new("sqlite::memory:")` |
| Temp directories | `tempfile::tempdir()` |
| Test helpers | `make_entry()`, `make_config()` — reduce boilerplate |
| Test CRUD cycle | store → get → update → delete → verify gone |
| Test edge cases | nonexistent keys, empty inputs, boundary values |
| Config roundtrip | serialize → deserialize → assert equal |
| Provider tests | Verify request body construction, NOT real API calls |

### Running Tests

```bash
cargo test --workspace                    # All 1012 tests
cargo test -p temm1e-agent               # Single crate
cargo test -p temm1e-tools -- browser    # Filter by name
cargo clippy --workspace --all-targets --all-features -- -D warnings  # 0 warnings
cargo fmt --all -- --check                # Format check
```

---

## How to Add Things

### Add a New AI Provider

**Touchpoints:** 6 files

1. **`crates/temm1e-providers/src/lib.rs`** — Add match arm in `create_provider()`
2. **`crates/temm1e-providers/src/openai_compat.rs`** — If OpenAI-compatible, add preset base URL (like Gemini/Grok). If not, create new file.
3. **`src/main.rs`** — Add to `detect_api_key()` with key prefix pattern
4. **`src/main.rs`** — Add default model in onboarding message
5. **`src/main.rs`** — Add to vault credential detector if applicable
6. **`README.md`** — Add to Supported Providers table

**Key prefix detection order matters:** Specific prefixes (sk-ant-, sk-or-, xai-) before generic (sk-).

### Add a New Messaging Channel

**Touchpoints:** 5 files

1. **`crates/temm1e-channels/Cargo.toml`** — Add feature flag + optional dependency
2. **`crates/temm1e-channels/src/{name}.rs`** — Implement `Channel` trait
3. **`crates/temm1e-channels/src/lib.rs`** — Add `#[cfg(feature)]` module + re-export + factory arm
4. **`crates/temm1e-core/src/types/config.rs`** — Add channel-specific config fields if needed
5. **`Cargo.toml` (root)** — Add feature to workspace default features

**Channel must implement:**
- `start()` — connect to messaging platform, spawn receiver loop, send to `mpsc::Sender`
- `send_message()` — format and deliver `OutboundMessage`
- `is_allowed()` — check numeric user ID against allowlist
- `delete_message()` — delete messages by ID (default no-op if unsupported)
- `file_transfer()` — return `Some(&dyn FileTransfer)` if platform supports files

### Add a New Agent Tool

**Touchpoints:** 3 files

1. **`crates/temm1e-tools/src/{name}.rs`** — Implement `Tool` trait
2. **`crates/temm1e-tools/src/lib.rs`** — Register in `create_tools()` vec
3. **`crates/temm1e-tools/Cargo.toml`** — Add feature flag if tool has optional dependencies

**Tool must declare:**
```rust
fn declarations(&self) -> ToolDeclarations {
    ToolDeclarations {
        file_access: vec![PathAccess::ReadWrite("~/.temm1e/sessions".into())],
        network_access: vec!["*.example.com".into()],
        shell_access: false,
    }
}
```

### Add a New Memory Backend

**Touchpoints:** 3 files

1. **`crates/temm1e-memory/src/{name}.rs`** — Implement `Memory` trait
2. **`crates/temm1e-memory/src/lib.rs`** — Add factory arm in `create_memory_backend()`
3. **`crates/temm1e-core/src/types/config.rs`** — Add config fields if needed

**Memory must implement:** `store()`, `get()`, `delete()`, `search()`, `get_session_history()`, `clear_session()`

---

## Context & History Management

Understanding how the agent maintains conversation context is critical for debugging "forgetfulness."

### Token Budget (default: 30,000)

Priority-based allocation in `crates/temm1e-agent/src/context.rs`:

| Priority | Category | Budget | Notes |
|----------|---------|--------|-------|
| 1 | System prompt + tool defs | Fixed (~5-8K) | Always included |
| 2 | Recent messages | Min 30, max 60 | Always kept regardless of budget |
| 3 | Memory search results | 15% (~4,500) | Up to 5 results, keyword LIKE search |
| 4 | Cross-task learnings | 5% (~1,500) | Up to 5 learnings, cross-session |
| 5 | Older history | Remaining | Dropped oldest-first when budget exceeded |

### Key Constants

| Constant | Value | File | Purpose |
|----------|-------|------|---------|
| `MIN_RECENT_MESSAGES` | 30 | context.rs:29 | Always kept in context |
| `MAX_RECENT_MESSAGES` | 60 | context.rs:32 | Before budget trimming |
| `MEMORY_BUDGET_FRACTION` | 0.15 | context.rs:35 | Memory search allocation |
| `LEARNING_BUDGET_FRACTION` | 0.05 | context.rs:38 | Cross-task learning allocation |
| `MAX_TOOL_OUTPUT_CHARS` | 30,000 | runtime.rs | Per-tool output truncation |
| `max_context_tokens` | 30,000 | config.rs | Total token budget (configurable) |
| `max_turns` | 200 | config.rs | Max message pairs to keep |
| `max_tool_rounds` | 200 | config.rs | Max tool calls per message |
| `max_task_duration_secs` | 1,800 | config.rs | 30-minute wall-clock limit |

### Tool Output Compression

Large tool outputs are compressed in `output_compression.rs`:
- **shell**: Errors/warnings extracted + last 20 lines + exit code
- **file_read**: First 50 + last 20 lines with omission marker
- **web_fetch**: Status code + content-type + truncated body
- **git**: Changed files + errors + status markers
- **default**: First 66% + last 33% with truncation marker
- **Error lines always preserved** regardless of truncation

---

## Provider-Specific Notes

| Provider | Base URL | Token Param | Key Prefix |
|----------|----------|-------------|------------|
| Anthropic | `api.anthropic.com` | `max_tokens` | `sk-ant-*` |
| OpenAI | `api.openai.com/v1` | `max_completion_tokens` | `sk-*` |
| Gemini | `generativelanguage.googleapis.com/v1beta/openai` | `max_tokens` | `AIzaSy*` |
| Grok | `api.x.ai/v1` | `max_tokens` | `xai-*` |
| OpenRouter | `openrouter.ai/api/v1` | `max_tokens` | `sk-or-*` |
| MiniMax | `api.minimax.chat/v1` | `max_tokens` | config only |

**Key detection order in `detect_api_key()` (main.rs):** `sk-ant-` → `sk-or-` → `AIzaSy` → `xai-` → `sk-` (generic OpenAI last, since it's the shortest prefix).

---

## Onboarding Flow

The fresh-user experience in `src/main.rs`:

```
1. No API key found (no env var, no credentials.toml, no config)
   → Start in onboarding mode
2. User sends any message to Telegram
   → Auto-whitelist first user as admin (numeric ID)
   → Persist allowlist to ~/.temm1e/allowlist.toml
3. Prompt user for API key
4. User pastes API key in chat
   → detect_api_key() identifies provider from prefix
   → Validate key against real API (health_check)
   → Save to ~/.temm1e/credentials.toml
   → Delete the credential message from chat
   → Initialize agent with detected provider + default model
5. Agent online — subsequent messages route to LLM
```

**Credential resolution priority chain:**
1. Config file (`temm1e.toml` with `${ENV_VAR}` expansion)
2. Saved credentials (`~/.temm1e/credentials.toml`)
3. Onboarding mode (no credentials anywhere → ask user)

---

## Local Development

### Build & Test

```bash
cargo check --workspace                  # Fast compilation check
cargo build --workspace                  # Debug build
cargo test --workspace                   # All tests
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all                          # Format
cargo build --release --bin temm1e      # Release binary
```

### Run Locally (Normal)

```bash
# Source all env vars
grep -E "^[A-Z_]+=" .env | sed 's/^/export /' > /tmp/temm1e_env.sh
source /tmp/temm1e_env.sh
./target/release/temm1e start > /tmp/temm1e.log 2>&1 &
tail -f /tmp/temm1e.log
```

### Run Locally (Fresh User — Ad-Hoc Testing Only)

For testing the onboarding flow from scratch. This is NOT in any code path — it's a manual protocol.

```bash
# 1. Kill existing
pkill -f "temm1e start"

# 2. Reset user state
echo 'admin = ""\nusers = []' > ~/.temm1e/allowlist.toml
rm -f ~/.temm1e/credentials.toml
rm -f ~/.temm1e/memory.db

# 3. Source env WITHOUT API key (so onboarding triggers)
grep -E "^[A-Z_]+=" .env | grep -v "ANTHROPIC_API_KEY" | sed 's/^/export /' > /tmp/temm1e_env.sh
source /tmp/temm1e_env.sh

# 4. Launch & tail
./target/release/temm1e start > /tmp/temm1e.log 2>&1 &
tail -f /tmp/temm1e.log
```

**Why exclude ANTHROPIC_API_KEY:** The config loader expands `${ANTHROPIC_API_KEY}` from env, which bypasses onboarding entirely. Excluding it forces the fresh-user path.

**This does NOT affect production.** Normal launches source all env vars. The service auto-recovers on restart because `credentials.toml` and `allowlist.toml` persist on disk.

---

## Common Mistakes

| Mistake | Fix |
|---------|-----|
| `unwrap()` in production code | Use `.map_err()` + `?` operator |
| Cross-crate dependency (e.g., tools → channels) | Move shared type to `temm1e-core` |
| Unconditional `use teloxide::*` | Gate behind `#[cfg(feature = "telegram")]` |
| Logging API keys at info level | Use `debug` with redacting `Debug` impl |
| Matching on username in allowlist | Match numeric user ID only (CA-04) |
| `let _ = fs::set_permissions(...)` | Propagate the error with `.map_err()?` |
| `&text[..max_chars]` for truncation | Use `char_indices()` for UTF-8 safety |
| `max_tokens` for OpenAI provider | Use `max_completion_tokens` (deprecated param) |
| Empty credentials.toml bypasses checks | `load_saved_credentials()` rejects empty name/key |
| Adding tool without `ToolDeclarations` | Declare file_access, network_access, shell_access |
| Test hits real API | Mock the HTTP layer or verify request body only |

---

## Version History

| Version | Date | Highlights |
|---------|------|-----------|
| 1.2.0 | 2026-03-09 | Stealth browser, session persistence, credential deletion, 1012 tests |
| 1.1.0 | 2026-03-08 | 6 LLM providers, hot-reload, channel docs |
| 1.0.0 | 2026-03-08 | TEM'S MIND, 35 features, vision support, 905 tests |
