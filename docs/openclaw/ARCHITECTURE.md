# OpenClaw — Full Architecture Documentation

> **Project**: OpenClaw (formerly Clawdbot, Moltbot)
> **Author**: Peter Steinberger (now at OpenAI; project moving to open-source foundation)
> **Language**: TypeScript / Node.js
> **License**: MIT
> **GitHub**: github.com/openclaw/openclaw (~247k stars, ~47.7k forks, 900+ contributors as of March 2026)

---

## 1. Overview

OpenClaw is a free, open-source, self-hosted autonomous AI agent that connects AI models and tools to messaging apps you already use. It runs on your own infrastructure and acts as a personal AI assistant — reading messages, executing tasks, managing memory, scheduling jobs, and controlling a browser.

OpenClaw treats AI as an **infrastructure problem**: sessions, memory, tool sandboxing, access control, and orchestration are first-class concerns. The AI model provides the intelligence; OpenClaw provides the execution environment.

---

## 2. High-Level Architecture (Hub-and-Spoke)

```
┌──────────────────────────────────────────────────────┐
│                    User Interfaces                    │
│  WhatsApp · Telegram · Discord · Slack · iMessage     │
│  Signal · Matrix · IRC · Web UI · CLI · macOS App     │
└──────────────────────┬───────────────────────────────┘
                       │ Messages (normalized)
                       ▼
┌──────────────────────────────────────────────────────┐
│                      GATEWAY                          │
│  WebSocket server (port 18789, loopback by default)   │
│  ┌─────────┐ ┌──────────┐ ┌───────────┐ ┌─────────┐ │
│  │ Channel  │ │ Session  │ │  Cron /   │ │ Tunnel  │ │
│  │ Monitors │ │ Router   │ │ Heartbeat │ │ Manager │ │
│  └─────────┘ └──────────┘ └───────────┘ └─────────┘ │
└──────────────────────┬───────────────────────────────┘
                       │ InboundContext
                       ▼
┌──────────────────────────────────────────────────────┐
│                   AGENT RUNTIME                       │
│  ┌────────┐ ┌────────────┐ ┌────────┐ ┌───────────┐ │
│  │Context │ │  LLM Call  │ │  Tool  │ │  Reply    │ │
│  │Assembly│→│  (model)   │→│ Exec   │→│ Streaming │ │
│  └────────┘ └────────────┘ └────────┘ └───────────┘ │
│       ↕              ↕            ↕                   │
│  ┌────────┐   ┌──────────┐  ┌────────────┐           │
│  │ Memory │   │ Provider │  │ Sandboxing │           │
│  │ System │   │ (OpenAI/ │  │  (Docker)  │           │
│  │        │   │Anthropic)│  │            │           │
│  └────────┘   └──────────┘  └────────────┘           │
└──────────────────────────────────────────────────────┘
                       ↕
┌──────────────────────────────────────────────────────┐
│                   SKILLS / TOOLS                      │
│  Built-in: bash, browser, file, canvas, cron, git    │
│  ClawHub:  3,286+ community skills (Markdown-based)  │
│  Plugins:  Custom extensions via openclaw.plugin.json │
└──────────────────────────────────────────────────────┘
```

---

## 3. Core Components — Detailed

### 3.1 Gateway

The Gateway is the **single always-on process** — the central control plane.

| Aspect | Details |
|--------|---------|
| **Protocol** | WebSocket on port 18789 |
| **Binding** | 127.0.0.1 (loopback) by default — refuses 0.0.0.0 unless tunnel configured or `allow_public_bind` set |
| **Daemon** | systemd (Linux), LaunchAgent (macOS) |
| **Heartbeat** | Configurable interval (default 30 min, 60 min with Anthropic OAuth) |
| **Config** | YAML-based configuration files |

**Responsibilities:**
- Owns every channel integration
- Routes incoming messages to the correct session
- Dispatches agent runtime execution
- Manages cron scheduler persistence under `~/.openclaw/cron/`
- Orchestrates tunnel connections for remote access

**Security concern**: The WebSocket on localhost has no built-in cross-origin protection. Any website visited can open a WebSocket connection to localhost, potentially allowing JavaScript on arbitrary webpages to connect to the Gateway.

### 3.2 Agent Runtime

The runtime executes the **agent loop**:

```
receive → route → context + LLM + tools → stream → persist
```

**Step-by-step:**
1. Message hits a channel → Gateway routes it to a session
2. Agent loads context (session history + memory + skills) for that session
3. Assembled context is sent to the LLM (via configured provider)
4. Model returns tool calls → runtime executes them against sandboxed environment
5. Reply is streamed back through the channel
6. Conversation + memory updates are persisted to workspace

### 3.3 Memory System

All memory is stored as **plain Markdown files** on the local filesystem.

#### File Structure
```
workspace/
├── MEMORY.md              # Long-term: decisions, preferences, durable facts
├── SOUL.md                # Immutable personality / operating instructions
├── AGENTS.md              # Agent configuration and instructions
├── HEARTBEAT.md           # Periodic check checklist
└── memory/
    ├── 2026-03-01.md      # Daily log (append-only)
    ├── 2026-03-02.md
    └── ...
```

#### Memory Layers
| Layer | File | Loaded When | Purpose |
|-------|------|-------------|---------|
| **Daily log** | `memory/YYYY-MM-DD.md` | Session start (today + yesterday) | Running context, day-to-day notes |
| **Long-term** | `MEMORY.md` | Main private session only | Curated facts, preferences, decisions |
| **Soul** | `SOUL.md` | Always | Immutable operating instructions |

#### Memory Tools (Agent-Facing)
- **`memory_search`**: Semantic recall over indexed snippets (vector search)
- **`memory_get`**: Targeted read of a specific Markdown file/line range (paths outside `MEMORY.md` / `memory/` rejected)

#### Vector Indexing
- SQLite-based per-agent index at `~/.openclaw/memory/<agentId>.sqlite`
- Configurable via `agents.defaults.memorySearch.store.path` (supports `{agentId}` token)
- File watcher on `MEMORY.md` + `memory/` marks index dirty (1.5s debounce)
- Sync runs asynchronously: on session start, on search, or on interval

### 3.4 Channel System

Each messaging platform has a dedicated **channel monitor/adapter** that:
1. Receives inbound messages from the platform
2. Normalizes them into a common `InboundContext` payload
3. Delivers to the Gateway's agent runtime
4. On outbound, delivers agent reply back through the channel

#### Supported Channels (25+)
WhatsApp, Telegram, Discord, Slack, Signal, iMessage, BlueBubbles, IRC, Microsoft Teams, Matrix, Feishu, LINE, Mattermost, Nextcloud Talk, Nostr, Synology Chat, Tlon, Twitch, Zalo, WebChat, Google Chat, and more.

#### Authentication Methods
| Channel | Auth Method |
|---------|------------|
| WhatsApp | QR code pairing via Baileys (WhatsApp Web WebSocket protocol). Credentials stored in `~/.openclaw/credentials` |
| Telegram | Bot token via `TELEGRAM_BOT_TOKEN` env var. Uses grammY TypeScript framework |
| Discord | Bot token via `DISCORD_BOT_TOKEN`. Guild/channel-based architecture |
| iMessage | Direct macOS integration |

#### Multi-Channel Session Continuity
One long-running Gateway process receives messages from different platforms and routes them into the **same session store**. A conversation started on WhatsApp can continue on Telegram because context is shared.

### 3.5 Tool / Skill System

#### Built-in Tools
- **Bash/Shell**: Command execution with approval workflows
- **Browser**: Chrome/Chromium automation (snapshots, actions, uploads, profiles)
- **File I/O**: Read, write, search within workspace
- **Canvas**: Visual/document creation
- **Cron**: Scheduled task management
- **Git**: Version control operations
- **Camera**: Snap/clip, screen record
- **Location, Notifications**: Device integration
- **Gmail Pub/Sub**: Email automation
- **Webhooks**: HTTP event handling

#### Exec Tool & Approvals
- Commands can run on host or in Docker sandbox via `SandboxContext`
- If `host=sandbox` is requested but sandbox runtime unavailable → **fails closed** (no silent fallback to host)
- Explicit approval workflows for dangerous operations

#### Skill Architecture (Markdown-Based)
A skill is a **directory containing a `SKILL.md`** file with YAML frontmatter + instructions:

```markdown
---
name: my-skill
version: 1.0.0
description: Does something useful
tags: [productivity, automation]
tools: [bash, browser]
---

# My Skill

Instructions for the agent to follow when this skill is activated...
```

**Skill Precedence** (highest to lowest):
1. Workspace skills (project-level)
2. User-managed / local skills
3. Bundled skills (shipped with OpenClaw)

**Plugin Skills**: Plugins can ship their own skills by listing skill directories in `openclaw.plugin.json`. Plugin skills load when the plugin is enabled.

### 3.6 ClawHub — Skill Marketplace

ClawHub is the **public skill registry** — equivalent to npm for Node.js or pip for Python.

| Aspect | Details |
|--------|---------|
| **URL** | https://clawhub.ai |
| **GitHub** | github.com/openclaw/clawhub |
| **Skills Count** | 3,286+ (as of March 2026) |
| **Search** | Embedding-based vector search (natural language queries) |
| **Publishing** | Requires GitHub account ≥1 week old |
| **Pricing** | Free — all skills are public and open |

#### CLI Commands
```bash
# Search for skills
clawhub search "your query"

# Install a skill
clawhub install <skill-slug>

# Update skills
clawhub update

# Publish a skill
clawhub publish ./my-skill

# Backup installed skills
clawhub backup
```

#### Configuration
- `CLAWHUB_SITE`: Override site URL
- `CLAWHUB_REGISTRY`: Override registry API URL
- `CLAWHUB_CONFIG_PATH`: Override token/config storage location
- `CLAWHUB_WORKDIR`: Override default workdir
- Installed skills tracked in `.clawhub/lock.json`

#### Security Concerns
- Security researchers found **41.7% of published ClawHub skills contained vulnerabilities**
- Hundreds were outright malicious (typosquatting, credential theft)
- 280+ skills found leaking API keys and PII (Snyk research)
- This is a major ecosystem risk that ZeroClaw's compiled-in approach addresses

### 3.7 Automation: Heartbeat & Cron

#### Heartbeat
- Periodic check running inside the agent's main session
- Reads `HEARTBEAT.md` from workspace to decide if anything needs attention
- Default interval: 30 minutes
- Handles routine monitoring: inbox, calendar, notifications
- Keep `HEARTBEAT.md` small to minimize token overhead

#### Cron Jobs
- Gateway's built-in scheduler
- Persists jobs under `~/.openclaw/cron/` (survives restarts)
- Precise time-based scheduling (daily reports, weekly reviews, one-shot reminders)
- Can run in isolated sessions without affecting main context
- Output optionally delivered back to a chat

### 3.8 Tunnel / Remote Access

OpenClaw supports multiple methods for secure remote access:

| Method | Command / Config |
|--------|-----------------|
| **ngrok** | `ngrok http 18789` — instant public URL |
| **Cloudflare Tunnel** | `cloudflared tunnel --url http://localhost:18789` |
| **SSH Tunnel** | SSH port forwarding (universal fallback) |
| **Moltworker** | Cloudflare Sandbox container — fully managed deployment |

Security best practice: Separate routes by trust level — Telegram webhook ingress stays narrow, Gateway UI access remains private-first and operator-gated.

---

## 4. Configuration

OpenClaw uses **YAML-based configuration** with scope rules:

```yaml
# Example agent configuration
agents:
  defaults:
    provider: anthropic
    model: claude-sonnet-4-20250514
    memorySearch:
      store:
        path: ~/.openclaw/memory/{agentId}.sqlite

channels:
  telegram:
    enabled: true
    token: ${TELEGRAM_BOT_TOKEN}
    allowlist:
      - username1
      - 123456789  # numeric user ID

  discord:
    enabled: true
    token: ${DISCORD_BOT_TOKEN}
    allowlist:
      - 987654321  # user ID

gateway:
  port: 18789
  host: 127.0.0.1
  heartbeat:
    interval: 30m

sandbox:
  enabled: true
  runtime: docker
```

---

## 5. Technology Stack

| Component | Technology |
|-----------|-----------|
| **Runtime** | Node.js / TypeScript |
| **Gateway** | WebSocket server |
| **Browser** | Puppeteer / Playwright (Chrome/Chromium) |
| **WhatsApp** | Baileys (TS library for WA Web protocol) |
| **Telegram** | grammY (TS framework) |
| **Memory Index** | SQLite + vector embeddings |
| **Sandbox** | Docker containers |
| **Package Manager** | npm |
| **Config** | YAML |
| **Skills** | Markdown + YAML frontmatter |

---

## 6. Strengths & Weaknesses

### Strengths
- Massive ecosystem (247k stars, 3,286+ skills)
- 25+ channel integrations out of the box
- Rich memory system with semantic search
- Proven multi-channel session continuity
- Extensive community and documentation
- Heartbeat + Cron for proactive automation
- ClawHub marketplace for skill discovery

### Weaknesses
- **Resource-heavy**: Node.js runtime requires 1GB+ RAM
- **Security gaps**: WebSocket localhost cross-origin issue, ClawHub supply chain risks (41.7% vulnerable skills)
- **Not cloud-native**: Designed as personal, single-operator tool — assumes one trusted user
- **No native headless cloud support**: Requires SSH setup for VPS deployment
- **JavaScript ecosystem risks**: Dependency bloat, npm supply chain
- **Sandbox is opt-in**: Not enforced by default

---

## 7. Key Takeaways for TEMM1E

1. **Hub-and-spoke gateway pattern works well** — adopt it but make it cloud-native
2. **Markdown-based skills are powerful and portable** — maintain compatibility
3. **Memory system needs improvement** — vector + keyword hybrid search is better (ZeroClaw does this)
4. **ClawHub marketplace has critical security issues** — TEMM1E needs a safer skill distribution model
5. **Channel normalization pattern** is essential for multi-platform support
6. **Headless/cloud deployment is an afterthought** in OpenClaw — this is TEMM1E's primary differentiator
7. **Auth via messaging apps** (QR codes, bot tokens) works but needs cloud-native OAuth/secret management

---

## Sources

- [OpenClaw GitHub](https://github.com/openclaw/openclaw)
- [OpenClaw Architecture Explained (Substack)](https://ppaolo.substack.com/p/openclaw-system-architecture-overview)
- [OpenClaw Wikipedia](https://en.wikipedia.org/wiki/OpenClaw)
- [How OpenClaw Works (Medium)](https://bibek-poudel.medium.com/how-openclaw-works-understanding-ai-agents-through-a-real-architecture-5d59cc7a4764)
- [OpenClaw Complete Tutorial 2026 (Towards AI)](https://pub.towardsai.net/openclaw-complete-guide-setup-tutorial-2026-14dd1ae6d1c2)
- [OpenClaw Docs: Memory](https://docs.openclaw.ai/concepts/memory)
- [OpenClaw Docs: Skills](https://docs.openclaw.ai/tools/skills)
- [OpenClaw Docs: ClawHub](https://docs.openclaw.ai/tools/clawhub)
- [OpenClaw Docs: Security](https://docs.openclaw.ai/gateway/security)
- [OpenClaw Docs: Cron vs Heartbeat](https://docs.openclaw.ai/automation/cron-vs-heartbeat)
- [OpenClaw Security (Nebius)](https://nebius.com/blog/posts/openclaw-security)
- [OpenClaw Skill Security (Lakera)](https://www.lakera.ai/blog/the-agent-skill-ecosystem-when-ai-extensions-become-a-malware-delivery-channel)
- [ClawHub Credential Leaks (Snyk)](https://snyk.io/blog/openclaw-skills-credential-leaks-research/)
- [OpenClaw Channel Comparison](https://zenvanriel.com/ai-engineer-blog/openclaw-channel-comparison-telegram-whatsapp-signal/)
- [Deep Dive into OpenClaw (Medium)](https://medium.com/@dingzhanjun/deep-dive-into-openclaw-architecture-code-ecosystem-e6180f34bd07)
- [What is OpenClaw (DigitalOcean)](https://www.digitalocean.com/resources/articles/what-is-openclaw)
- [What is OpenClaw (Milvus)](https://milvus.io/blog/openclaw-formerly-clawdbot-moltbot-explained-a-complete-guide-to-the-autonomous-ai-agent.md)
