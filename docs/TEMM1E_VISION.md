# TEMM1E — Vision & Architecture Spec

> **Cloud-native, Rust-based autonomous AI agent runtime**
> "Higher than the claws that came before — native in the cloud, native in your chats."

---

## 1. What is TEMM1E?

TEMM1E is a **cloud-native Rust AI agent runtime** that combines:
- **ZeroClaw's** performance, trait-based modularity, and security-first design
- **OpenClaw's** rich ecosystem, multi-channel reach, and skill marketplace
- A **cloud-first architecture** where users never need to SSH into a VM

**The core insight**: Users should interact with their AI agent through messaging apps they already use — sending credentials, files, and commands as naturally as chatting with a friend. The agent runs headless in the cloud; the messaging app IS the interface.

---

## 2. Key Differentiators from OpenClaw & ZeroClaw

| Dimension | OpenClaw | ZeroClaw | TEMM1E |
|-----------|----------|----------|---------|
| Deployment | Local-first, SSH for VPS | Local/edge, tunnels | **Cloud-native headless-first** |
| Setup | SSH + install + config | SSH + binary + config | **Send auth via chat → done** |
| Auth/Secrets | Config files, env vars | Config files, env vars | **OAuth flows via messaging, vault-backed** |
| File Transfer | Limited | Limited | **Native bi-directional file I/O via chat** |
| Provisioning | Manual | Manual | **Auto-provisioning cloud VMs/containers** |
| Skill Safety | ClawHub (41.7% vulnerable) | Compiled-in only | **Signed + sandboxed + verified registry** |
| Multi-tenancy | Single operator | Single operator | **Multi-tenant with isolation** |
| Scaling | Single instance | Single instance | **Horizontal auto-scaling** |

---

## 3. Architecture

### 3.1 Cloud-Native Gateway (SkyGate)

```
┌─────────────────────────────────────────────────────────────┐
│                     MESSAGING LAYER                          │
│  Telegram · Discord · Slack · WhatsApp · Signal · iMessage   │
│  Matrix · Teams · LINE · Email · Web · API · Webhook         │
│                                                              │
│  ┌─────────────────────────────────────────────────────┐     │
│  │  Native File I/O: send/receive documents, images,   │     │
│  │  code, archives, credentials — all via chat          │     │
│  └─────────────────────────────────────────────────────┘     │
└──────────────────────┬──────────────────────────────────────┘
                       │ Normalized messages + file streams
                       ▼
┌─────────────────────────────────────────────────────────────┐
│                    SKYGATE (Cloud Gateway)                    │
│                                                              │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────────┐  │
│  │ Channel  │ │ Auth     │ │ Session  │ │ File Transfer │  │
│  │ Router   │ │ Manager  │ │ Manager  │ │ Engine        │  │
│  └──────────┘ └──────────┘ └──────────┘ └───────────────┘  │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────────┐  │
│  │ Cron /   │ │ Tenant   │ │ Health / │ │ Secrets Vault │  │
│  │Heartbeat │ │ Isolator │ │ Metrics  │ │ (cloud KMS)   │  │
│  └──────────┘ └──────────┘ └──────────┘ └───────────────┘  │
└──────────────────────┬──────────────────────────────────────┘
                       │
                       ▼
┌─────────────────────────────────────────────────────────────┐
│                   AGENT RUNTIME (SkyAgent)                    │
│                                                              │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────────┐  │
│  │ Context  │ │ Provider │ │   Tool   │ │   Sandbox     │  │
│  │ Builder  │→│  Trait   │→│  Trait   │→│   (mandatory) │  │
│  └──────────┘ └──────────┘ └──────────┘ └───────────────┘  │
│       ↕              ↕            ↕             ↕            │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────────┐  │
│  │ Memory   │ │ Identity │ │Observable│ │ File Store    │  │
│  │  Trait   │ │  Trait   │ │  Trait   │ │ (S3/R2/GCS)  │  │
│  └──────────┘ └──────────┘ └──────────┘ └───────────────┘  │
└─────────────────────────────────────────────────────────────┘
                       ↕
┌─────────────────────────────────────────────────────────────┐
│                CLOUD INFRASTRUCTURE LAYER                     │
│  ┌──────────┐ ┌──────────┐ ┌──────────┐ ┌───────────────┐  │
│  │Container │ │ Object   │ │ Managed  │ │  Cloud KMS /  │  │
│  │Orchestr. │ │ Storage  │ │   DB     │ │  Vault        │  │
│  │(K8s/Fly) │ │(S3/R2)   │ │(PG/Redis)│ │               │  │
│  └──────────┘ └──────────┘ └──────────┘ └───────────────┘  │
└─────────────────────────────────────────────────────────────┘
```

### 3.2 Core Traits (Extending ZeroClaw's 8)

TEMM1E inherits ZeroClaw's trait-based design and adds cloud-native traits:

| # | Trait | Purpose | TEMM1E Addition |
|---|-------|---------|------------------|
| 1 | **Provider** | AI model backends | Same as ZeroClaw |
| 2 | **Channel** | Messaging adapters | **+ native file transfer protocol per channel** |
| 3 | **Tool** | Agent capabilities | Same, + cloud-native tools |
| 4 | **Memory** | Persistence | **+ distributed backends (Redis, DynamoDB)** |
| 5 | **Tunnel** | External access | **Replaced by cloud-native ingress** |
| 6 | **Identity** | Auth / pairing | **+ OAuth, OIDC, SAML, chat-based auth flows** |
| 7 | **Peripheral** | Hardware | Optional (edge mode) |
| 8 | **Observable** | Monitoring | **+ OpenTelemetry, cloud metrics** |
| 9 | **FileStore** | File transfer & storage | **NEW: S3/R2/GCS + chat file routing** |
| 10 | **Vault** | Secrets management | **NEW: Cloud KMS, HashiCorp Vault, encrypted at rest** |
| 11 | **Orchestrator** | Container/VM lifecycle | **NEW: K8s, Fly.io, Railway, Docker Swarm** |
| 12 | **Tenant** | Multi-tenancy isolation | **NEW: Per-user workspace isolation** |

### 3.3 Native Messaging & File Transfer

This is TEMM1E's **primary differentiator**. The messaging app IS the control plane.

#### User Onboarding Flow (Zero SSH)
```
User sends message to TEMM1E bot on Telegram:
  "Hey, set up my agent"

TEMM1E responds:
  "Welcome! Send me your cloud credentials to get started.
   You can send:
   - A .env file with your API keys
   - OAuth: click this link to authorize [Google] [GitHub] [AWS]
   - Or just type: provider=anthropic key=sk-ant-..."

User sends a .env file attachment → TEMM1E:
  1. Receives file via Telegram Bot API
  2. Parses credentials
  3. Encrypts with ChaCha20-Poly1305 → stores in Vault
  4. Provisions cloud workspace
  5. Agent is live and responding
```

#### File Transfer Engine
Every channel adapter implements a `FileTransfer` sub-trait:

```rust
#[async_trait]
pub trait FileTransfer: Send + Sync {
    /// Receive a file from the user via the messaging platform
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>>;

    /// Send a file to the user via the messaging platform
    async fn send_file(&self, chat_id: &str, file: OutboundFile) -> Result<()>;

    /// Stream a large file with progress
    async fn send_file_stream(
        &self,
        chat_id: &str,
        stream: BoxStream<Bytes>,
        metadata: FileMetadata,
    ) -> Result<()>;

    /// Max file size for this channel
    fn max_file_size(&self) -> usize;
}

pub struct ReceivedFile {
    pub name: String,
    pub mime_type: String,
    pub size: usize,
    pub data: Bytes,          // Small files
    pub stream: Option<BoxStream<Bytes>>,  // Large files
}

pub struct OutboundFile {
    pub name: String,
    pub mime_type: String,
    pub data: FileData,       // Bytes or S3 presigned URL
    pub caption: Option<String>,
}
```

#### Per-Channel File Capabilities
| Channel | Max File | Upload | Download | Formats |
|---------|----------|--------|----------|---------|
| Telegram | 50 MB (bot) / 2 GB (premium) | Yes | Yes | Any |
| Discord | 25 MB (free) / 500 MB (nitro) | Yes | Yes | Any |
| Slack | 1 GB | Yes | Yes | Any |
| WhatsApp | 2 GB | Yes | Yes | Media + docs |
| Email | ~25 MB | Yes | Yes | Attachments |
| Matrix | Configurable | Yes | Yes | Any |
| Web API | Unlimited (streaming) | Yes | Yes | Any |

For files exceeding channel limits, TEMM1E generates **presigned URLs** to cloud object storage.

### 3.4 Cloud Auth via Messaging

Users authenticate and provide secrets **through their messaging app** — no config files, no SSH.

#### Supported Auth Flows
```
1. DIRECT KEY: User sends API key as message → encrypted → stored in Vault
2. FILE UPLOAD: User sends .env / credentials.json → parsed → encrypted → Vault
3. OAUTH LINK: TEMM1E sends OAuth URL → user clicks → callback → token stored
4. QR CODE:    TEMM1E sends QR image in chat → user scans → paired
5. MAGIC LINK: TEMM1E sends one-time link → user clicks → session established
```

#### Supported Integrations (OAuth/API Key)
- **AI Providers**: Anthropic, OpenAI, Google AI, Mistral, Groq, local endpoints
- **Cloud**: AWS, GCP, Azure (IAM roles, service accounts, SAS tokens)
- **Code**: GitHub, GitLab, Bitbucket (OAuth apps)
- **Google**: OAuth (Gmail, Calendar, Drive, Sheets)
- **Social**: Facebook, Twitter/X (OAuth)
- **Comms**: Slack, Discord, Telegram (bot tokens)
- **Custom**: Any OAuth 2.0 / OIDC / API key service

### 3.5 Cloud-Native Memory

Extends ZeroClaw's memory with distributed backends:

```rust
#[async_trait]
pub trait Memory: Send + Sync {
    async fn store(&self, entry: MemoryEntry) -> Result<()>;
    async fn search(&self, query: &str, opts: SearchOpts) -> Result<Vec<MemoryEntry>>;
    async fn get(&self, id: &str) -> Result<Option<MemoryEntry>>;
    async fn delete(&self, id: &str) -> Result<()>;
    async fn list_sessions(&self, tenant: &TenantId) -> Result<Vec<SessionInfo>>;
    async fn sync(&self) -> Result<SyncStatus>;  // For distributed sync
}
```

| Backend | Use Case |
|---------|----------|
| SQLite | Single-instance, edge |
| PostgreSQL | Multi-instance cloud, shared memory |
| Redis | Session cache, fast ephemeral storage |
| S3/R2 + SQLite | Durable file-backed with object storage |
| Markdown (compat) | OpenClaw migration path |

Hybrid search (vector 0.7 + keyword 0.3) preserved from ZeroClaw.

### 3.6 Security Model

**Everything is deny-by-default and mandatory** (not opt-in like OpenClaw).

```
┌─────────────────────────────────────────┐
│            SECURITY LAYERS               │
│                                          │
│  1. Channel Auth    (allowlists, OAuth)  │
│  2. Tenant Isolation (per-user workspace)│
│  3. Tool Sandboxing  (mandatory, always) │
│  4. File Scanning    (AV + policy check) │
│  5. Secrets Vault    (ChaCha20/cloud KMS)│
│  6. Workspace Scope  (fs jail per agent) │
│  7. Network Policy   (egress allowlists) │
│  8. Skill Signing    (ed25519 signatures)│
│  9. Audit Log        (all actions logged)│
│ 10. Rate Limiting    (per-tenant quotas) │
└─────────────────────────────────────────┘
```

### 3.7 Skill System — Safe Marketplace

TEMM1E takes a middle path between OpenClaw's open marketplace and ZeroClaw's compiled-in-only approach:

#### TemHub (Skill Registry)
- **Signed skills**: Every skill must be signed with ed25519 by the author
- **Verified publishers**: GitHub identity verification required
- **Automated scanning**: Static analysis + sandbox execution before publishing
- **Capability declarations**: Skills declare required permissions (file, network, shell)
- **Runtime sandboxing**: Skills execute in isolated WASM or container sandboxes
- **Audit trail**: All skill installs and executions logged

#### Skill Format (OpenClaw-Compatible)
```markdown
---
name: my-skill
version: 1.0.0
author: username
signature: ed25519:base64...
capabilities:
  file: [read]
  network: [api.example.com]
  shell: false
temm1e_min: 0.1.0
openclaw_compat: true
---

# My Skill

Instructions for the agent...
```

#### Three Skill Tiers
1. **Core Skills**: Compiled into binary (ZeroClaw style) — bash, browser, file, git
2. **Verified Skills**: From TemHub with signatures + scanning (safe marketplace)
3. **Custom Skills**: User-provided Markdown (OpenClaw style, sandboxed execution)

### 3.8 Orchestration & Auto-Provisioning

```rust
#[async_trait]
pub trait Orchestrator: Send + Sync {
    async fn provision(&self, spec: AgentSpec) -> Result<AgentInstance>;
    async fn scale(&self, instance: &AgentInstance, replicas: u32) -> Result<()>;
    async fn destroy(&self, instance: &AgentInstance) -> Result<()>;
    async fn health(&self, instance: &AgentInstance) -> Result<HealthStatus>;
    async fn logs(&self, instance: &AgentInstance, tail: usize) -> Result<Vec<LogEntry>>;
}
```

Supported orchestrators:
- Docker (local/single-node)
- Kubernetes (production clusters)
- Fly.io (edge deployment)
- Railway (simple cloud)
- Cloud Run / ECS / Azure Container Instances

---

## 4. Configuration

TOML-based (ZeroClaw compatible), with cloud-native extensions:

```toml
[temm1e]
mode = "cloud"  # "cloud" | "edge" | "hybrid"
tenant_isolation = true

[gateway]
host = "0.0.0.0"  # Cloud-native: bind to all interfaces
port = 8080
tls = true
tls_cert = "/etc/temm1e/cert.pem"
tls_key = "/etc/temm1e/key.pem"

[provider]
name = "anthropic"
# Key loaded from vault, not config
# api_key sourced from vault://temm1e/anthropic/api_key

[memory]
backend = "postgres"
connection_string = "vault://temm1e/db/connection_string"

[memory.search]
vector_weight = 0.7
keyword_weight = 0.3

[filestore]
backend = "s3"
bucket = "temm1e-files"
region = "us-east-1"
# credentials from IAM role or vault

[vault]
backend = "aws-kms"  # or "hashicorp", "local-chacha20"
# For local: key_file = "~/.temm1e/vault.key"

[channel.telegram]
enabled = true
# token from vault://temm1e/telegram/bot_token
allowlist = []
file_transfer = true
max_file_size = "50MB"

[channel.discord]
enabled = true
allowlist = []
file_transfer = true

[channel.web]
enabled = true
cors_origins = ["https://app.temm1e.io"]

[orchestrator]
backend = "kubernetes"
namespace = "temm1e-agents"
auto_scale = true
min_replicas = 1
max_replicas = 10

[security]
sandbox = "mandatory"  # Not opt-in
file_scanning = true
skill_signing = "required"
audit_log = true
rate_limit = { requests_per_minute = 60 }

[heartbeat]
interval = "30m"
checklist = "HEARTBEAT.md"

[cron]
storage = "postgres"  # Persists across restarts and instances
```

---

## 5. Ecosystem Compatibility

### OpenClaw Compatibility
- Reads OpenClaw YAML config (migration mode)
- Reads OpenClaw Markdown memory files
- Supports OpenClaw skill format (SKILL.md with YAML frontmatter)
- ClawHub skills can be installed with safety scanning

### ZeroClaw Compatibility
- Same trait-based architecture (Rust)
- Reads ZeroClaw TOML config
- Same memory backends (SQLite, PostgreSQL, Markdown)
- Skills can be compiled-in (ZeroClaw style)

### TEMM1E Native (TemHub)
- Signed skill registry
- Cloud-native provisioning API
- Multi-tenant management dashboard
- Headless-first deployment

---

## 6. Technology Stack

| Component | Technology |
|-----------|-----------|
| **Language** | Rust |
| **Async Runtime** | Tokio |
| **HTTP/WebSocket** | axum / tungstenite |
| **Serialization** | serde (TOML + JSON + YAML) |
| **Database** | SQLite (sqlx) + PostgreSQL (sqlx) |
| **Object Storage** | aws-sdk-s3 (S3/R2/GCS compatible) |
| **Crypto** | chacha20poly1305, ed25519-dalek |
| **Messaging** | teloxide (Telegram), serenity (Discord), custom traits |
| **Browser** | headless-chrome or chromiumoxide |
| **Containers** | bollard (Docker API) / kube-rs (Kubernetes) |
| **Observability** | tracing + opentelemetry |
| **TLS** | rustls |
| **Vector Search** | qdrant-client or built-in HNSW |
| **WASM Sandbox** | wasmtime (for skill sandboxing) |
| **Config** | config-rs with TOML backend |
| **CLI** | clap |

---

## 7. Summary: Why TEMM1E?

1. **Cloud-native from day one** — not bolted on after
2. **Messaging apps ARE the interface** — no SSH, no web UI required
3. **Native file transfer** — send/receive any file through chat
4. **Auth via chat** — OAuth links, credential files, API keys — all through messaging
5. **Rust performance** — <5 MB RAM, <10 ms boot, single binary
6. **Safe marketplace** — signed skills with mandatory sandboxing
7. **Multi-tenant** — serve many users from one deployment
8. **Auto-scaling** — Kubernetes/Fly.io native orchestration
9. **Ecosystem compatible** — works with OpenClaw skills and ZeroClaw configs
10. **Security-first** — everything deny-by-default, vault-backed secrets, audit logs
