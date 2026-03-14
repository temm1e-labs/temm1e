# ZeroClaw — Full Architecture Documentation

> **Project**: ZeroClaw
> **Organization**: zeroclaw-labs
> **Language**: Rust (100%)
> **License**: Open Source
> **GitHub**: github.com/zeroclaw-labs/zeroclaw (~3,400+ stars in first 2 days)
> **Binary Size**: ~3.4 MB static binary
> **Boot Time**: <10 ms (even on 0.6 GHz cores)
> **Memory**: <5 MB RAM typical

---

## 1. Overview

ZeroClaw is the **runtime operating system for agentic workflows** — infrastructure that abstracts models, tools, memory, and execution so agents can be built once and run anywhere. It is a lightweight, security-first autonomous AI agent framework built entirely in Rust.

ZeroClaw is designed as a direct alternative to OpenClaw, trading the Node.js ecosystem's breadth for Rust's performance, safety, and minimal resource footprint. It can run on everything from a $10 Raspberry Pi to cloud VMs.

**Core philosophy**: Zero overhead, zero compromise. Every subsystem is a Rust trait — swap any component by changing a single line in `config.toml`, with zero code changes and zero recompilation.

---

## 2. High-Level Architecture (Trait-Based Modular)

```
┌──────────────────────────────────────────────────────┐
│                     Channels (17+)                    │
│  CLI · Telegram · Discord · Slack · WhatsApp · Signal │
│  iMessage · Matrix · Mattermost · IRC · Lark · Email  │
│  DingTalk · QQ · Nostr · Linq · Webhook              │
└──────────────────────┬───────────────────────────────┘
                       │ Channel trait
                       ▼
┌──────────────────────────────────────────────────────┐
│                      GATEWAY                          │
│  ┌───────────┐ ┌──────────┐ ┌──────────┐ ┌────────┐ │
│  │ Channel   │ │ Session  │ │ Security │ │ Tunnel │ │
│  │ Dispatch  │ │ Manager  │ │ Policy   │ │ Trait  │ │
│  └───────────┘ └──────────┘ └──────────┘ └────────┘ │
│  Refuses 0.0.0.0 unless tunnel or allow_public_bind  │
└──────────────────────┬───────────────────────────────┘
                       │
                       ▼
┌──────────────────────────────────────────────────────┐
│                   AGENT RUNTIME                       │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐             │
│  │ Context  │ │ Provider │ │   Tool   │             │
│  │ Assembly │→│  Trait   │→│  Trait   │             │
│  └──────────┘ └──────────┘ └──────────┘             │
│       ↕              ↕            ↕                   │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐             │
│  │ Memory   │ │ Identity │ │Observable│             │
│  │  Trait   │ │  Trait   │ │  Trait   │             │
│  └──────────┘ └──────────┘ └──────────┘             │
└──────────────────────────────────────────────────────┘
                       ↕
┌──────────────────────────────────────────────────────┐
│               PERIPHERAL TRAIT (Hardware)             │
│           GPIO · Camera · Sensors · etc.              │
└──────────────────────────────────────────────────────┘
```

---

## 3. The Eight Core Traits

ZeroClaw's entire architecture is defined by **eight Rust traits**. Every subsystem implements one of these traits, and any implementation can be swapped via `config.toml`.

| # | Trait | Purpose | Example Implementations |
|---|-------|---------|------------------------|
| 1 | **Provider** | AI model backends | OpenAI, Anthropic, OpenAI-compatible endpoints, local models |
| 2 | **Channel** | Messaging platform adapters | Telegram, Discord, Slack, CLI, WhatsApp, 17+ total |
| 3 | **Tool** | Agent capabilities / actions | Shell, file I/O, browser, git, cron, HTTP, screenshot |
| 4 | **Memory** | Persistence backends | InMemory, SQLite, Markdown, PostgreSQL, Lucid Bridge, None |
| 5 | **Tunnel** | Secure external access | Cloudflare, Tailscale, ngrok, custom command |
| 6 | **Identity** | Authentication / pairing | Device pairing, channel allowlists |
| 7 | **Peripheral** | Hardware integration | GPIO, camera, sensors |
| 8 | **Observable** | Monitoring / telemetry | Logging, metrics, tracing |

### Trait Pattern (Rust)
```rust
#[async_trait]
pub trait Memory: Send + Sync {
    async fn store(&self, entry: MemoryEntry) -> Result<()>;
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<MemoryEntry>>;
    async fn get(&self, id: &str) -> Result<Option<MemoryEntry>>;
    async fn delete(&self, id: &str) -> Result<()>;
}

#[async_trait]
pub trait Channel: Send + Sync {
    async fn start(&mut self) -> Result<()>;
    async fn send(&self, message: OutboundMessage) -> Result<()>;
    fn name(&self) -> &str;
}

#[async_trait]
pub trait Provider: Send + Sync {
    async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse>;
    async fn stream(&self, request: CompletionRequest) -> Result<StreamingResponse>;
    fn name(&self) -> &str;
}

#[async_trait]
pub trait Tool: Send + Sync {
    fn name(&self) -> &str;
    fn description(&self) -> &str;
    fn declarations(&self) -> ToolDeclarations; // What resources it needs
    async fn execute(&self, input: ToolInput, ctx: &ToolContext) -> Result<ToolOutput>;
}
```

---

## 4. Core Components — Detailed

### 4.1 Provider System

Providers connect ZeroClaw to AI model backends.

**Supported Providers (22+)**:
- OpenAI (GPT-4, GPT-4o, o1, o3, etc.)
- Anthropic (Claude Opus, Sonnet, Haiku)
- Any OpenAI-compatible endpoint (vLLM, Ollama, LM Studio, etc.)
- Google (Gemini)
- And more via the OpenAI-compatible trait

**Configuration** (`config.toml`):
```toml
[provider]
name = "anthropic"
api_key = "${ANTHROPIC_API_KEY}"
model = "claude-sonnet-4-20250514"

# Or OpenAI-compatible
[provider]
name = "openai-compatible"
base_url = "http://localhost:11434/v1"
model = "llama3"
api_key = "not-needed"
```

### 4.2 Memory System

ZeroClaw has the **most sophisticated memory system** in the Claw ecosystem, with hybrid vector + full-text search.

#### Memory Backends

| Backend | Description | Config |
|---------|-------------|--------|
| **InMemory** | Ephemeral, in-process storage | `backend = "memory"` |
| **SQLite** | Local database file | `backend = "sqlite"` |
| **Markdown** | Human-readable file-based storage with hygiene system | `backend = "markdown"` |
| **PostgreSQL** | Remote database for centralized multi-instance storage | `backend = "postgres"` |
| **Lucid Bridge** | External context management via `lucid` CLI | `backend = "lucid"` |
| **None** | Explicit no-op (disables persistence) | `backend = "none"` |

#### Markdown Backend Detail
Organizes memory into two file types:
- **Daily files**: Aggregate all conversations from a calendar day into a single Markdown document
- **Session files**: Isolate conversation contexts by unique identifiers

#### Hybrid Search
Combines **vector similarity** (0.7 weight) with **keyword matching** (0.3 weight) for optimal recall:
```toml
[memory]
backend = "sqlite"

[memory.search]
vector_weight = 0.7
keyword_weight = 0.3
```

#### PostgreSQL Backend
Enables centralized storage across multiple ZeroClaw instances:
```toml
[memory]
backend = "postgres"
connection_string = "postgres://user:pass@host:5432/zeroclaw"
```

### 4.3 Channel System

Each channel implements the `Channel` trait with deny-by-default allowlisting.

#### Supported Channels (17+)
CLI, Telegram, Discord, Slack, WhatsApp, Signal, iMessage, Matrix, Mattermost, IRC, Lark, DingTalk, QQ, Nostr, Email, Linq, Webhook

#### Allowlist Security
```toml
[channel.telegram]
enabled = true
token = "${TELEGRAM_BOT_TOKEN}"
allowlist = ["username1", "123456789"]  # Empty = deny all

[channel.discord]
enabled = true
token = "${DISCORD_BOT_TOKEN}"
allowlist = ["987654321"]
```

Every channel runs as a plugin with its own authentication and message handling. An **empty allowlist equals deny-all**.

### 4.4 Tool System

Tools declare their resource requirements **before execution**. The runtime enforces allowlists based on those declarations.

#### Built-in Tools
- **Shell/Exec**: Command execution with allowlisting
- **File I/O**: Workspace-scoped read/write/search
- **Browser**: Automation with domain allowlists
- **Git**: Version control operations
- **Cron/Schedule**: Job scheduling and management
- **HTTP**: Web requests with domain restrictions
- **Screenshot/Image**: Visual capture tools
- **Pushover**: Notification delivery
- **GPIO**: Hardware control (for edge devices)
- **Composio**: External integration (opt-in)

#### Tool Declaration Pattern
```rust
fn declarations(&self) -> ToolDeclarations {
    ToolDeclarations {
        file_access: vec![PathAccess::Read("~/workspace".into())],
        network_access: vec![DomainAccess::Allow("api.example.com".into())],
        shell_access: false,
    }
}
```

A tool that claims read access to `~/documents` **cannot** silently access `~/.ssh`.

### 4.5 Security Model

ZeroClaw's security is **defense-in-depth** — enforced at every layer.

#### Filesystem Security
- Workspace scoping enabled by default
- 14 system directories + 4 sensitive dotfiles blocked
- Null byte injection blocked
- Symlink escape detection via canonicalization + resolved-path workspace checks
- File read/write tools enforce workspace boundaries

#### Channel Security
- Deny-by-default allowlists on all channels
- Per-channel authentication
- Empty allowlist = deny all

#### Secrets
- Tunnel credentials encrypted with **ChaCha20-Poly1305 AEAD** when `secrets.encrypt = true`
- Local key file for encryption

#### Sandboxing
- Docker as sandboxed runtime environment
- Agent/gateway/daemon can run inside Docker containers for isolation
- Built-in tools are sandboxed and workspace-scoped by default

#### No Plugin Supply Chain
> "You can't have a supply chain attack if you don't have a supply chain."

Extensions are **compiled in**, not downloaded. No marketplace to compromise, no packages to typosquat, no trust decisions at install time. This directly addresses the ClawHub vulnerability problem (41.7% of skills had vulnerabilities).

### 4.6 Tunnel System

Secure external access without public IP binding.

| Provider | Config Key | Notes |
|----------|-----------|-------|
| **Cloudflare** | `tunnel.provider = "cloudflare"` | Requires cloudflared daemon; token from Zero Trust dashboard |
| **Tailscale** | `tunnel.provider = "tailscale"` | `serve` (tailnet-only) or `funnel` (public HTTPS) modes |
| **ngrok** | `tunnel.provider = "ngrok"` | Quick public URL |
| **Custom** | `tunnel.provider = "custom"` | Any binary via command template with `{port}` and `{host}` placeholders |

```toml
[tunnel]
provider = "cloudflare"
token = "${CLOUDFLARE_TUNNEL_TOKEN}"

# Or custom
[tunnel]
provider = "custom"
command = "bore local {port} --to bore.pub"
```

Gateway refuses to bind to public addresses unless tunnel is configured or `allow_public_bind` is explicitly set.

---

## 5. Configuration System

ZeroClaw uses **TOML** for all configuration (vs OpenClaw's YAML).

### Full Config Reference (`config.toml`)
```toml
[gateway]
host = "127.0.0.1"
port = 18789
allow_public_bind = false

[provider]
name = "anthropic"
api_key = "${ANTHROPIC_API_KEY}"
model = "claude-sonnet-4-20250514"

[memory]
backend = "sqlite"
path = "~/.zeroclaw/memory.db"

[memory.search]
vector_weight = 0.7
keyword_weight = 0.3

[channel.cli]
enabled = true

[channel.telegram]
enabled = true
token = "${TELEGRAM_BOT_TOKEN}"
allowlist = []

[channel.discord]
enabled = true
token = "${DISCORD_BOT_TOKEN}"
allowlist = []

[tools]
shell = true
browser = true
file = true
git = true
cron = true
http = true

[tools.shell]
allowlist = ["ls", "cat", "grep", "git"]

[tools.browser]
domain_allowlist = ["*.google.com", "github.com"]

[tools.file]
workspace = "~/workspace"

[tunnel]
provider = "cloudflare"
token = "${CLOUDFLARE_TUNNEL_TOKEN}"

[secrets]
encrypt = true

[security]
workspace_scoping = true
block_system_dirs = true
block_dotfiles = true
symlink_escape_detection = true

[observability]
log_level = "info"
```

### Environment Variable Expansion
Config values support `${ENV_VAR}` syntax for secrets, keeping sensitive data out of config files.

### OpenClaw Compatibility
ZeroClaw can **read OpenClaw configuration and memory data**, enabling migration with minimal rewriting. This is achieved by parsing OpenClaw's YAML config format and Markdown memory files.

---

## 6. Comparison with OpenClaw

| Dimension | OpenClaw | ZeroClaw |
|-----------|----------|----------|
| **Language** | TypeScript / Node.js | Rust |
| **Binary Size** | ~200+ MB (with node_modules) | 3.4 MB static binary |
| **RAM Usage** | 1 GB+ | <5 MB |
| **Boot Time** | Several seconds | <10 ms |
| **Config Format** | YAML | TOML |
| **Extensibility** | Marketplace (ClawHub) + plugins | Compiled-in traits |
| **Skills** | 3,286+ on ClawHub | Write Rust, compile in |
| **Channels** | 25+ | 17+ |
| **Memory** | Markdown + SQLite vectors | Markdown, SQLite, PostgreSQL, Lucid, hybrid search |
| **Security** | Opt-in sandbox, ClawHub risks | Deny-by-default everything, no supply chain |
| **Cloud Deployment** | Afterthought (tunnels, SSH) | First-class tunnel trait, Railway/Docker support |
| **Hardware/Edge** | Not designed for it | Runs on Raspberry Pi ($10 board) |
| **GitHub Stars** | ~247k | ~3.4k (2 days old) |
| **Migration** | N/A | Can read OpenClaw config + memory |

---

## 7. Strengths & Weaknesses

### Strengths
- **Extreme performance**: 3.4 MB, <10 ms boot, <5 MB RAM
- **Security-first**: Deny-by-default everything, no plugin supply chain
- **Trait-based modularity**: Swap any subsystem via config, no recompilation
- **Hybrid memory search**: Vector + keyword for better recall
- **PostgreSQL support**: Centralized memory across instances
- **OpenClaw migration path**: Reads OpenClaw config and memory
- **Edge-ready**: Runs on $10 Raspberry Pi boards
- **Rust safety**: Memory safety at compile time, type-safe extensibility

### Weaknesses
- **No marketplace**: Adding extensions requires writing Rust and recompiling
- **Smaller ecosystem**: ~3.4k stars vs OpenClaw's ~247k
- **Fewer channels**: 17+ vs OpenClaw's 25+
- **Steeper learning curve**: Rust required for custom extensions
- **No skill portability**: Can't use OpenClaw's Markdown skills directly as capabilities
- **Young project**: Less battle-tested in production

---

## 8. Key Takeaways for TEMM1E

1. **Rust trait-based architecture is excellent** — adopt it for TEMM1E's core
2. **Hybrid search (vector + keyword) is superior** — use it for TEMM1E's memory
3. **Deny-by-default security** is the right model — make it TEMM1E's default
4. **No-marketplace has trade-offs** — TEMM1E should find a middle ground (verified/signed skills?)
5. **PostgreSQL memory backend** enables multi-instance — essential for cloud-native
6. **TOML config with env var expansion** is clean — adopt it
7. **OpenClaw compatibility** for migration is smart — TEMM1E should support both ecosystems
8. **Cloud-native is missing** — ZeroClaw has tunnels but no native cloud orchestration, no OAuth flows, no headless-first design
9. **ChaCha20-Poly1305 for secrets** is solid cryptography — adopt it
10. **Single static binary** deployment model is ideal for cloud containers

---

## Sources

- [ZeroClaw GitHub](https://github.com/zeroclaw-labs/zeroclaw)
- [ZeroClaw README](https://github.com/zeroclaw-labs/zeroclaw/blob/main/README.md)
- [ZeroClaw AGENTS.md](https://github.com/zeroclaw-labs/zeroclaw/blob/main/AGENTS.md)
- [ZeroClaw Docs](https://zeroclaws.io/docs/)
- [ZeroClaw Official Site](https://zeroclaw.net/)
- [ZeroClaw Blog: Trait-Driven Architecture](https://zeroclaws.io/blog/trait-driven-architecture-extensible-agents/)
- [ZeroClaw vs OpenClaw vs PicoClaw 2026](https://zeroclaws.io/blog/zeroclaw-vs-openclaw-vs-picoclaw-2026/)
- [ZeroClaw DEV Community](https://dev.to/brooks_wilson_36fbefbbae4/zeroclaw-a-lightweight-secure-rust-agent-runtime-redefining-openclaw-infrastructure-2cl0)
- [ZeroClaw on Raspberry Pi](https://pbxscience.com/how-zeroclaw-a-3-4-mb-rust-binary-is-turning-a-10-raspberry-pi-into-a-fully-autonomous/)
- [ZeroClaw DeepWiki](https://deepwiki.com/zeroclaw-labs/zeroclaw/1.1-what-is-zeroclaw)
- [ZeroClaw Config Reference](https://github.com/zeroclaw-labs/zeroclaw/blob/main/docs/config-reference.md)
- [ZeroClaw Tunnel Configuration (DeepWiki)](https://deepwiki.com/zeroclaw-labs/zeroclaw/10.5-tunnel-configuration)
- [ZeroClaw Provider Configuration (DeepWiki)](https://deepwiki.com/zeroclaw-labs/zeroclaw/3.3-provider-configuration)
- [Claw Ecosystem Comparison (Medium)](https://pchojecki.medium.com/the-claw-family-top-5-openclaw-variants-compared-to-the-original-64d8342712dd)
- [Claw Ecosystem Complexity (Sean Weldon)](https://www.sean-weldon.com/blog/2026-02-19-claw-ecosystem-complexity-gradients-agentic-runtimes)
- [ZeroClaw Cloudron Forum](https://forum.cloudron.io/topic/15080/zeroclaw-rust-based-alternative-to-openclaw-picoclaw-nanobot-agentzero)
- [ZeroClaw lib.rs](https://lib.rs/crates/zeroclaw)
