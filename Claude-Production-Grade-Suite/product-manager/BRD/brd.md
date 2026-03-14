# TEMM1E v0.1 — Business Requirements Document

## Executive Summary

TEMM1E is a cloud-native, Rust-based autonomous AI agent runtime. Users interact with their agent entirely through messaging apps (Telegram, Discord, Slack, WhatsApp, CLI) — sending credentials, files, and commands as naturally as chatting. No SSH, no config files, no setup complexity.

**v0.1 Goal**: Ship a functional, performant runtime that proves the "messaging app as control plane" thesis with full bi-directional file transfer, multi-provider AI support, and ecosystem compatibility with both OpenClaw and ZeroClaw.

---

## Stakeholders

| Role | Description | Needs |
|------|-------------|-------|
| **Developer User** | Primary target. Wants a cloud AI agent without SSH/setup | Zero-friction onboarding via messaging app |
| **Project Owner** | TEMM1E maintainer | Clean Rust codebase, trait-based extensibility, <10 MB binary |
| **Claw Ecosystem** | OpenClaw/ZeroClaw community | Migration path, config/skill compatibility |
| **Cloud Operators** | Anyone deploying TEMM1E on Docker/VPS | Single binary, Docker-first, easy provisioning |

---

## Constraints

| Constraint | Value |
|-----------|-------|
| **Language** | Rust (100%) |
| **Binary Size** | <10 MB static binary |
| **Boot Time** | <50 ms cold start |
| **RAM Usage** | <20 MB idle |
| **Config Format** | TOML (ZeroClaw compatible) + YAML reader (OpenClaw compat) |
| **Security** | Deny-by-default everything. Mandatory sandboxing. |
| **Deployment** | Cloud-native first, Docker-based. Must also run on user's local machine. |
| **Runtime Mode** | `cloud` (headless on VPS/cloud) or `local` (user's laptop/desktop) — same binary |
| **Tenancy** | Single-tenant v0.1 with Tenant trait designed for future multi-tenant |
| **License** | MIT (matching OpenClaw) |

---

## User Stories

### Epic 1: Core Runtime & Agent Loop

**US-1.1: Agent Loop Execution**
> As a developer, I want TEMM1E to receive my message, assemble context, call an AI model, execute tool calls, and stream the reply back — so I have a working AI agent.

Acceptance Criteria:
- [ ] Gateway receives inbound message from any configured channel
- [ ] Context assembler loads session history + memory + active skills
- [ ] Provider trait dispatches to configured AI model
- [ ] Tool calls from model response are executed in sandboxed environment
- [ ] Reply is streamed back through the originating channel
- [ ] Conversation is persisted to memory backend
- [ ] Full cycle completes in <2s for simple queries (excluding model latency)

**US-1.1b: Dual-Mode Runtime (Cloud + Local)**
> As a developer, I want TEMM1E to run both as a cloud-native headless daemon AND on my local machine — so I can develop locally and deploy to the cloud with the same binary.

Acceptance Criteria:
- [ ] `temm1e start --mode cloud` — binds to 0.0.0.0, expects TLS, cloud-optimized defaults
- [ ] `temm1e start --mode local` — binds to 127.0.0.1, no TLS required, local-optimized defaults
- [ ] Auto-detect mode from config `temm1e.mode = "cloud" | "local" | "auto"`
- [ ] `auto` mode: detects environment (checks for cloud metadata endpoints, container runtime)
- [ ] Same binary, same config format, same channels — only defaults change
- [ ] Local mode uses SQLite + filesystem; cloud mode can use PostgreSQL + S3
- [ ] Local mode stores vault key in `~/.temm1e/vault.key`; cloud mode supports KMS

**US-1.2: Configuration System**
> As a developer, I want to configure TEMM1E via a TOML file with environment variable expansion — so I can set up providers, channels, and memory without code changes.

Acceptance Criteria:
- [ ] Reads `config.toml` from workspace root or `~/.temm1e/config.toml`
- [ ] Supports `${ENV_VAR}` expansion for secrets
- [ ] Supports `vault://` URI scheme for vault-backed secrets
- [ ] Validates config on startup with clear error messages
- [ ] Can also read ZeroClaw TOML configs (migration mode)
- [ ] Can also read OpenClaw YAML configs (migration mode)

**US-1.3: CLI Interface**
> As a developer, I want a CLI to start the gateway, run one-shot commands, check status, and manage skills — so I can operate TEMM1E from my terminal.

Acceptance Criteria:
- [ ] `temm1e start` — launch gateway daemon
- [ ] `temm1e chat` — interactive CLI channel
- [ ] `temm1e status` — show running state, connected channels, provider health
- [ ] `temm1e config validate` — check configuration
- [ ] `temm1e skill list/install` — manage skills
- [ ] `temm1e version` — show version info
- [ ] All commands complete in <100ms (except start)

### Epic 2: Messaging Channels (5 channels)

**US-2.1: Telegram Channel**
> As a developer, I want to interact with TEMM1E through a Telegram bot — sending messages, files, and receiving responses with file attachments.

Acceptance Criteria:
- [ ] Bot connects via Telegram Bot API with token from config/vault
- [ ] Receives text messages and routes to agent runtime
- [ ] Receives file uploads (documents, images, archives up to 50 MB)
- [ ] Sends text replies with Markdown formatting
- [ ] Sends file attachments back to user (documents, images, code)
- [ ] For files >50 MB, generates presigned URL to object storage
- [ ] Allowlist-based access control (usernames + user IDs)
- [ ] Graceful reconnection on network interruption

**US-2.2: Discord Channel**
> As a developer, I want to interact with TEMM1E through a Discord bot.

Acceptance Criteria:
- [ ] Bot connects via Discord Gateway with token from config/vault
- [ ] Handles DMs and configured guild channels
- [ ] Receives/sends text with Markdown
- [ ] Receives/sends file attachments (up to 25 MB free, 500 MB Nitro)
- [ ] Presigned URLs for files exceeding limit
- [ ] Allowlist by user ID
- [ ] Slash commands for status/config

**US-2.3: Slack Channel**
> As a developer, I want to interact with TEMM1E through a Slack bot.

Acceptance Criteria:
- [ ] Connects via Slack Bot API (Socket Mode or Events API)
- [ ] Handles DMs and configured workspace channels
- [ ] Receives/sends text with Slack mrkdwn formatting
- [ ] Receives/sends file attachments (up to 1 GB)
- [ ] Allowlist by member ID
- [ ] Thread-aware conversations

**US-2.4: WhatsApp Channel**
> As a developer, I want to interact with TEMM1E through WhatsApp.

Acceptance Criteria:
- [ ] Connects via WhatsApp Business API or Web protocol
- [ ] Receives/sends text messages
- [ ] Receives/sends files (documents, images, media up to 2 GB)
- [ ] QR code pairing flow
- [ ] Allowlist by phone number
- [ ] End-to-end encryption preserved

**US-2.5: CLI Channel**
> As a developer, I want a local CLI channel for development and testing.

Acceptance Criteria:
- [ ] Interactive REPL with readline support
- [ ] File send/receive via file paths
- [ ] Streams responses token by token
- [ ] Colored output with Markdown rendering
- [ ] History persistence

### Epic 3: AI Provider System

**US-3.1: Multi-Provider Support**
> As a developer, I want to use any major AI provider — so I can choose the best model for my use case.

Acceptance Criteria:
- [ ] Anthropic provider (Claude Opus, Sonnet, Haiku family)
- [ ] OpenAI-compatible provider (GPT-4, o-series + any compatible endpoint)
- [ ] Google provider (Gemini models)
- [ ] Mistral provider
- [ ] Groq provider (fast inference)
- [ ] Provider trait allows adding new providers with zero changes to core
- [ ] Streaming responses for all providers
- [ ] Tool/function calling for all providers that support it
- [ ] Automatic retry with exponential backoff
- [ ] Provider health monitoring via Observable trait

### Epic 4: File Transfer Engine

**US-4.1: Bi-directional File Transfer via Chat**
> As a developer, I want to send files to my agent and receive files back through my messaging app — so file I/O is as natural as chatting.

Acceptance Criteria:
- [ ] FileTransfer sub-trait implemented for all 4 messaging channels
- [ ] Receives: .env, .json, .yaml, .toml, .py, .rs, .js, .ts, .md, images, archives, any file
- [ ] Sends: code files, logs, reports, images, archives back to user
- [ ] Small files (<channel limit) sent inline as attachments
- [ ] Large files generate presigned URLs to object storage
- [ ] File metadata preserved (name, MIME type, size)
- [ ] Streaming for large files with progress indication
- [ ] Files stored in tenant workspace on local filesystem
- [ ] Optional S3/R2 backend for overflow storage

**US-4.2: Credential File Parsing**
> As a developer, I want to send a .env or credentials file via chat and have TEMM1E parse and securely store the credentials.

Acceptance Criteria:
- [ ] Detects .env file format and parses key=value pairs
- [ ] Detects JSON credentials (GCP service account, etc.)
- [ ] Encrypts all parsed secrets immediately with ChaCha20-Poly1305
- [ ] Stores encrypted secrets in local vault
- [ ] Deletes plaintext from memory after encryption
- [ ] Confirms what was parsed (key names, not values) back to user
- [ ] Warns user if message contains plaintext API keys

### Epic 4b: Browser & App Automation

**US-4.3: Headless Browser Automation**
> As a developer, I want my agent to browse websites, fill forms, extract data, and interact with web apps — so it can automate web tasks on my behalf.

Acceptance Criteria:
- [ ] Headless Chrome/Chromium browser automation via chromiumoxide or headless-chrome crate
- [ ] Navigate to URLs, click elements, fill forms, extract text/screenshots
- [ ] Support both headless mode (cloud/VPS) and headed mode (local with display)
- [ ] Domain allowlist for controlled web access
- [ ] Screenshot capture and send back to user via chat
- [ ] Cookie/session persistence across browser sessions
- [ ] Browser profiles for different identities/accounts
- [ ] Page snapshots (DOM + screenshot) for agent context

**US-4.4: GUI Mode (Local Desktop)**
> As a developer running TEMM1E locally, I want it to support a GUI mode that can interact with desktop applications and display a browser — so I get full visual automation.

Acceptance Criteria:
- [ ] `temm1e start --gui` enables headed browser and desktop interaction
- [ ] Headed Chrome/Chromium with visible browser window
- [ ] Screen capture for agent context (screenshot tool)
- [ ] Optional: basic desktop automation via accessibility APIs or screen coordinates
- [ ] GUI mode is opt-in; headless is the default
- [ ] Cloud deployments use headless mode exclusively
- [ ] Local deployments can choose GUI or headless

### Epic 5: Memory System

**US-5.1: Multi-Backend Memory with Hybrid Search**
> As a developer, I want persistent memory across sessions with semantic search — so my agent remembers context.

Acceptance Criteria:
- [ ] SQLite backend: local single-file database with vector extension
- [ ] PostgreSQL backend: cloud-hosted shared memory across instances
- [ ] Markdown backend: human-readable files compatible with OpenClaw format
- [ ] Hybrid search: vector similarity (0.7 weight) + keyword matching (0.3 weight)
- [ ] Memory trait allows backend swap via config change
- [ ] Daily log files (append-only) + curated long-term memory
- [ ] Session isolation: separate memory contexts per conversation
- [ ] Memory tools exposed to agent: `memory_search`, `memory_get`, `memory_store`

### Epic 6: Security & Vault

**US-6.1: Chat-Based Secret Management**
> As a developer, I want to send API keys and credentials via chat and have them encrypted and stored securely — so I never touch config files.

Acceptance Criteria:
- [ ] Detects API key patterns in messages (sk-ant-*, sk-*, gsk_*, etc.)
- [ ] Immediately encrypts with ChaCha20-Poly1305 AEAD
- [ ] Local vault stored at `~/.temm1e/vault.enc`
- [ ] Vault key derived from user passphrase or stored locally
- [ ] `vault://` URI scheme in config resolves to vault entries
- [ ] Vault entries can be listed (key names only) and deleted via chat
- [ ] Plaintext secrets never written to disk unencrypted
- [ ] Audit log of all vault access

**US-6.2: Deny-by-Default Security Model**
> As a developer, I want all tools sandboxed and all channels access-controlled by default.

Acceptance Criteria:
- [ ] All channels require explicit allowlist (empty = deny all)
- [ ] Tool execution sandboxed to workspace directory
- [ ] 14 system directories + sensitive dotfiles blocked
- [ ] Null byte injection blocked
- [ ] Symlink escape detection via canonicalization
- [ ] Network egress restricted to configured allowlist
- [ ] All security policies enforced by default, not opt-in

### Epic 7: Skill System

**US-7.1: Local Skill Loading with Ecosystem Compatibility**
> As a developer, I want to load skills from disk in both TEMM1E and OpenClaw formats.

Acceptance Criteria:
- [ ] Loads SKILL.md files with YAML frontmatter from workspace
- [ ] Parses OpenClaw skill format (YAML frontmatter + Markdown instructions)
- [ ] Parses ZeroClaw-style compiled skills (feature-flagged Rust modules)
- [ ] Skill precedence: workspace > user > bundled
- [ ] Skills declare required capabilities (file, network, shell)
- [ ] Runtime enforces declared capabilities
- [ ] CLI: `temm1e skill list`, `temm1e skill info <name>`

### Epic 8: Ecosystem Compatibility

**US-8.1: ZeroClaw Config Compatibility**
> As a ZeroClaw user, I want TEMM1E to read my existing config.toml — so I can migrate easily.

Acceptance Criteria:
- [ ] Parses ZeroClaw config.toml format
- [ ] Maps ZeroClaw provider/channel/memory/tunnel sections to TEMM1E equivalents
- [ ] Warns on unsupported fields
- [ ] Migration guide in docs

**US-8.2: OpenClaw Config & Memory Compatibility**
> As an OpenClaw user, I want TEMM1E to read my YAML config and Markdown memory files.

Acceptance Criteria:
- [ ] Parses OpenClaw YAML configuration
- [ ] Reads OpenClaw Markdown memory files (MEMORY.md, memory/*.md)
- [ ] Imports OpenClaw session history
- [ ] Maps OpenClaw channel configs to TEMM1E equivalents
- [ ] `temm1e migrate --from openclaw <workspace>` CLI command

### Epic 9: Automation

**US-9.1: Heartbeat & Cron System**
> As a developer, I want my agent to check a heartbeat file periodically and run scheduled jobs.

Acceptance Criteria:
- [ ] Reads HEARTBEAT.md from workspace at configurable interval (default 30 min)
- [ ] Cron scheduler persists jobs to configured backend (SQLite or PostgreSQL)
- [ ] Jobs survive gateway restarts
- [ ] Cron output optionally delivered to a chat channel
- [ ] `temm1e cron list/add/remove` CLI commands

### Epic 10: Observability

**US-10.1: Structured Logging & Metrics**
> As a developer, I want structured logs and metrics — so I can debug and monitor my agent.

Acceptance Criteria:
- [ ] Structured JSON logging via tracing
- [ ] Log levels configurable per-module
- [ ] Request tracing with correlation IDs
- [ ] Metrics: messages processed, tool calls, provider latency, memory operations
- [ ] Health endpoint at `/health` for container orchestrators
- [ ] Optional OpenTelemetry export for cloud monitoring

---

## Priority Matrix

| Priority | Epic | Rationale |
|----------|------|-----------|
| **P0 — Must Have** | Epic 1 (Core Runtime) | Nothing works without the agent loop |
| **P0 — Must Have** | Epic 2 (Channels: Telegram + CLI) | Primary interface |
| **P0 — Must Have** | Epic 3 (Providers: Anthropic + OpenAI-compat) | Must talk to AI models |
| **P0 — Must Have** | Epic 6 (Security & Vault) | Deny-by-default is non-negotiable |
| **P1 — Should Have** | Epic 4 (File Transfer) | Primary differentiator |
| **P1 — Should Have** | Epic 5 (Memory: SQLite) | Agent needs memory to be useful |
| **P1 — Should Have** | Epic 2 (Channels: Discord, Slack, WhatsApp) | Full channel coverage |
| **P1 — Should Have** | Epic 3 (Providers: Google, Mistral, Groq) | Full provider coverage |
| **P1 — Should Have** | Epic 4b (Browser/GUI) | Web automation, headless+headed support |
| **P2 — Nice to Have** | Epic 7 (Skills) | Ecosystem compat |
| **P2 — Nice to Have** | Epic 8 (Migration) | Community adoption |
| **P2 — Nice to Have** | Epic 9 (Automation) | Proactive agent |
| **P2 — Nice to Have** | Epic 10 (Observability) | Production monitoring |
| **P2 — Nice to Have** | Epic 5 (Memory: PostgreSQL, Markdown) | Additional backends |

---

## Non-Functional Requirements

| Requirement | Target |
|-------------|--------|
| Binary size | <10 MB (static, musl) |
| Cold start | <50 ms |
| Idle RAM | <20 MB |
| Message latency (excl. model) | <100 ms |
| Concurrent channels | 5+ simultaneous |
| Memory search latency | <50 ms (local SQLite) |
| Encryption | ChaCha20-Poly1305 AEAD |
| Skill signing | Ed25519 |
| Config format | TOML (native) + YAML (compat read) |
| Minimum Rust edition | 2021 |
| CI targets | x86_64-linux-musl, aarch64-linux-musl, x86_64-darwin, aarch64-darwin |

---

## Out of Scope for v0.1

- Multi-tenant deployment (Tenant trait designed but single-tenant only)
- TemHub registry server (local skills only)
- Kubernetes / Fly.io orchestrator backends (Docker only)
- Web UI dashboard
- OAuth redirect flow handling (chat-based keys + file upload only)
- WASM skill sandboxing (file system sandbox only for v0.1)
- Redis memory backend
- Hardware peripherals (GPIO, sensors)
- Full desktop automation (accessibility API integration) — v0.1 provides browser + screenshots only
