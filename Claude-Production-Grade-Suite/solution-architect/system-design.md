# TEMM1E v0.1 — System Design

## Crate Structure (Rust Workspace)

```
temm1e/
├── Cargo.toml                    # Workspace root
├── crates/
│   ├── temm1e-core/             # Core traits, types, error handling
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── traits/           # All 12 trait definitions
│   │       │   ├── mod.rs
│   │       │   ├── provider.rs   # AI provider trait
│   │       │   ├── channel.rs    # Messaging channel trait + FileTransfer
│   │       │   ├── tool.rs       # Tool execution trait
│   │       │   ├── memory.rs     # Memory backend trait
│   │       │   ├── tunnel.rs     # External access trait
│   │       │   ├── identity.rs   # Auth/pairing trait
│   │       │   ├── peripheral.rs # Hardware trait (stub for v0.1)
│   │       │   ├── observable.rs # Monitoring trait
│   │       │   ├── filestore.rs  # File storage trait
│   │       │   ├── vault.rs      # Secrets management trait
│   │       │   ├── orchestrator.rs # Container lifecycle trait (stub)
│   │       │   └── tenant.rs     # Multi-tenancy trait (stub)
│   │       ├── types/            # Shared types
│   │       │   ├── mod.rs
│   │       │   ├── message.rs    # InboundMessage, OutboundMessage, etc.
│   │       │   ├── file.rs       # ReceivedFile, OutboundFile, FileMetadata
│   │       │   ├── config.rs     # Configuration types
│   │       │   ├── session.rs    # Session, Conversation, Context
│   │       │   └── error.rs      # Error types (thiserror)
│   │       └── config/           # Config loading (TOML + YAML compat)
│   │           ├── mod.rs
│   │           ├── loader.rs     # Config file discovery & loading
│   │           ├── toml.rs       # Native TOML parser
│   │           ├── yaml_compat.rs # OpenClaw YAML compat reader
│   │           └── env.rs        # Environment variable expansion
│   │
│   ├── temm1e-gateway/          # SkyGate: the cloud gateway
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── server.rs         # axum HTTP/WS server
│   │       ├── router.rs         # Channel message routing
│   │       ├── session.rs        # Session management
│   │       ├── health.rs         # /health endpoint
│   │       └── tls.rs            # TLS configuration (rustls)
│   │
│   ├── temm1e-agent/            # Agent runtime (the brain)
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── runtime.rs        # Agent loop: context → LLM → tools → reply
│   │       ├── context.rs        # Context assembly (history + memory + skills)
│   │       ├── executor.rs       # Tool call execution with sandboxing
│   │       └── streaming.rs      # Response streaming back to channel
│   │
│   ├── temm1e-providers/        # AI provider implementations
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── anthropic.rs      # Anthropic Claude API
│   │       ├── openai_compat.rs  # OpenAI-compatible (covers OpenAI, Ollama, vLLM, etc.)
│   │       ├── google.rs         # Google Gemini API
│   │       ├── mistral.rs        # Mistral API
│   │       └── groq.rs           # Groq API
│   │
│   ├── temm1e-channels/         # Channel implementations
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── cli.rs            # CLI REPL channel
│   │       ├── telegram.rs       # Telegram via teloxide
│   │       ├── discord.rs        # Discord via serenity/poise
│   │       ├── slack.rs          # Slack via custom HTTP client
│   │       ├── whatsapp.rs       # WhatsApp Business API
│   │       └── file_transfer.rs  # FileTransfer trait impls per channel
│   │
│   ├── temm1e-memory/           # Memory backend implementations
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── sqlite.rs         # SQLite + vector search
│   │       ├── postgres.rs       # PostgreSQL backend
│   │       ├── markdown.rs       # Markdown files (OpenClaw compat)
│   │       ├── search.rs         # Hybrid search engine (vector + keyword)
│   │       └── migration.rs      # OpenClaw/ZeroClaw memory import
│   │
│   ├── temm1e-vault/            # Secrets management
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── local.rs          # ChaCha20-Poly1305 local vault
│   │       ├── resolver.rs       # vault:// URI resolver
│   │       └── detector.rs       # API key pattern detection in messages
│   │
│   ├── temm1e-tools/            # Built-in tool implementations
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── shell.rs          # Shell command execution
│   │       ├── file_ops.rs       # File read/write/search
│   │       ├── browser.rs        # Browser automation (chromiumoxide)
│   │       ├── git.rs            # Git operations
│   │       ├── http.rs           # HTTP requests
│   │       └── screenshot.rs     # Screen capture
│   │
│   ├── temm1e-skills/           # Skill loading & management
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── loader.rs         # SKILL.md parser
│   │       ├── registry.rs       # Local skill registry
│   │       ├── openclaw_compat.rs # OpenClaw skill format parser
│   │       └── capability.rs     # Capability declaration & enforcement
│   │
│   ├── temm1e-automation/       # Heartbeat & Cron
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── heartbeat.rs      # HEARTBEAT.md periodic checker
│   │       └── cron.rs           # Persistent cron scheduler
│   │
│   ├── temm1e-observable/       # Observability
│   │   └── src/
│   │       ├── lib.rs
│   │       ├── logging.rs        # Structured JSON logging (tracing)
│   │       ├── metrics.rs        # Metrics collection
│   │       └── otel.rs           # OpenTelemetry export
│   │
│   └── temm1e-filestore/        # File storage backends
│       └── src/
│           ├── lib.rs
│           ├── local.rs          # Local filesystem storage
│           └── s3.rs             # S3/R2/GCS compatible storage
│
├── src/
│   └── main.rs                   # Binary entry point, CLI (clap)
│
├── config/
│   └── default.toml              # Default configuration
│
└── docs/                         # Architecture & user docs
```

## Data Flow

```
                    ┌─────────────────┐
                    │  Messaging App   │
                    │  (Telegram/etc.) │
                    └────────┬────────┘
                             │ Platform API (HTTP/WS)
                             ▼
┌─────────────────────────────────────────────────────────┐
│  temm1e-channels (Channel trait + FileTransfer trait)    │
│  ┌──────────┐ ┌──────────┐ ┌────────┐ ┌──────────────┐ │
│  │ telegram │ │ discord  │ │  slack │ │   whatsapp   │ │
│  └────┬─────┘ └────┬─────┘ └───┬────┘ └──────┬───────┘ │
│       └─────────────┴───────────┴─────────────┘         │
│                     InboundMessage                       │
└─────────────────────────┬───────────────────────────────┘
                          │
                          ▼
┌─────────────────────────────────────────────────────────┐
│  temm1e-gateway (SkyGate)                               │
│  ┌────────────┐  ┌──────────────┐  ┌─────────────────┐ │
│  │  Router    │→ │  Session Mgr │→ │  Rate Limiter   │ │
│  └────────────┘  └──────────────┘  └─────────────────┘ │
└─────────────────────────┬───────────────────────────────┘
                          │ SessionContext
                          ▼
┌─────────────────────────────────────────────────────────┐
│  temm1e-agent (SkyAgent Runtime)                        │
│                                                          │
│  1. Context Assembly                                     │
│     ├── Session history (from memory)                    │
│     ├── Long-term memory (MEMORY.md / DB)                │
│     ├── Active skills (from skill registry)              │
│     └── System prompt + workspace context                │
│                                                          │
│  2. Provider Call (via Provider trait)                    │
│     └── Streaming response from AI model                 │
│                                                          │
│  3. Tool Execution (via Tool trait)                       │
│     ├── Parse tool calls from model response             │
│     ├── Validate against capability declarations         │
│     ├── Execute in sandbox (workspace-scoped)            │
│     └── Return results to model for next iteration       │
│                                                          │
│  4. Reply Streaming                                      │
│     └── Stream response back via originating channel     │
│                                                          │
│  5. Persistence                                          │
│     ├── Save conversation to memory                      │
│     └── Update session state                             │
└─────────────────────────────────────────────────────────┘
          │                    │                    │
          ▼                    ▼                    ▼
┌──────────────┐  ┌──────────────────┐  ┌──────────────────┐
│ temm1e-     │  │ temm1e-tools    │  │ temm1e-vault    │
│ memory       │  │ ┌──────────────┐ │  │ ┌──────────────┐ │
│ ┌──────────┐ │  │ │ shell        │ │  │ │ ChaCha20     │ │
│ │ sqlite   │ │  │ │ file_ops     │ │  │ │ vault.enc    │ │
│ │ postgres │ │  │ │ browser      │ │  │ │ vault://     │ │
│ │ markdown │ │  │ │ git          │ │  │ │ resolver     │ │
│ └──────────┘ │  │ │ http         │ │  │ └──────────────┘ │
│              │  │ │ screenshot   │ │  │                  │
│ hybrid search│  │ └──────────────┘ │  │                  │
└──────────────┘  └──────────────────┘  └──────────────────┘
```

## Async Runtime Model

- **Tokio** multi-threaded runtime as the foundation
- Each channel runs as a separate tokio task
- Gateway server runs as axum on its own tokio task
- Agent runtime uses spawn_blocking for CPU-bound operations
- Memory operations are async (sqlx for SQLite/PostgreSQL)
- File I/O uses tokio::fs for async filesystem operations
- Browser automation uses chromiumoxide's async API

## Error Handling Strategy

- **thiserror** for defining error types in each crate
- **anyhow** at the binary/CLI level for ergonomic error propagation
- Every crate defines its own `Error` enum implementing `std::error::Error`
- Errors propagate through `Result<T, Temm1eError>` at crate boundaries
- User-facing errors are converted to friendly messages before reaching channels

## Configuration Resolution Order

1. Default values (compiled in)
2. System config: `/etc/temm1e/config.toml`
3. User config: `~/.temm1e/config.toml`
4. Workspace config: `./config.toml`
5. Environment variables: `TEMM1E_*` prefix
6. CLI flags: `--provider`, `--mode`, etc.
7. vault:// URIs resolved from vault at runtime

Later sources override earlier ones. ZeroClaw TOML and OpenClaw YAML configs are detected and converted at load time.
