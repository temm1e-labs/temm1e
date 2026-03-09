<p align="center">
  <img src="assets/banner.png" alt="SkyClaw" width="100%">
</p>

<p align="center">
  Built with <a href="https://github.com/nagisanzenin/claude-code-production-grade-plugin">production-grade</a> — the Claude Code plugin for shipping real systems, not just code files.
</p>

<p align="center">
  <a href="https://discord.gg/3ux2c5xz"><img src="https://img.shields.io/badge/Discord-Join%20Community-5865F2?logo=discord&logoColor=white" alt="Discord"></a>
  <img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="MIT License">
  <img src="https://img.shields.io/badge/version-1.3.0-blue.svg" alt="Version">
  <img src="https://img.shields.io/badge/tests-1022-green.svg" alt="1022 tests">
  <img src="https://img.shields.io/badge/providers-6-red.svg" alt="6 providers">
</p>

# SkyClaw

Hyper-performance (Rust) & self-sustaining claw that lives indefinitely in Cloud. 40K lines, 1022 tests, zero warnings.

## What It Does

SkyClaw is an autonomous AI agent that lives on your server and talks to you through messaging apps. It runs shell commands, browses the web, reads/writes files, fetches URLs, understands images, delegates sub-tasks, self-heals, and learns from its own mistakes — all controlled through natural conversation.

No web dashboards. No config files to edit. Deploy, paste your API key in Telegram, and go.

## Key Metrics

| Metric | Value |
|--------|-------|
| **Lines of Rust** | 40,810 across 96 source files |
| **Tests** | 1,022 passing, 0 failures |
| **Clippy warnings** | 0 (CI gate: `-D warnings`) |
| **Workspace crates** | 13 + 1 binary |
| **Implemented features** | 43 across 8 phases |
| **AGENTIC CORE modules** | 20 |
| **Traits (core)** | 13 shared trait definitions |
| **AI providers** | 6 (Anthropic, OpenAI, Gemini, Grok, OpenRouter, MiniMax) |
| **Messaging channels** | 4 ([Telegram](docs/channels/telegram.md), [Discord](docs/channels/discord.md), [Slack](docs/channels/slack.md), [CLI](docs/channels/cli.md)) |
| **Agent tools** | 7 (shell, browser, file ops, web fetch, git, messaging, file transfer) |
| **Encryption** | ChaCha20-Poly1305 + Ed25519 |
| **Memory backends** | 3 (SQLite, Markdown, failover) |
| **File storage** | 2 (local, S3/R2) |
| **Observability** | OpenTelemetry, 6 predefined metrics |
| **Security features** | Auto-whitelist, vault encryption, path traversal protection, force-push block, credential message deletion, 4-layer key validation |
| **Vision support** | JPEG, PNG, GIF, WebP (Anthropic + OpenAI formats) |
| **Release profile** | `opt-level=z`, LTO, 1 codegen unit, stripped, `panic=abort` |
| **Minimum Rust version** | 1.82 (Edition 2021) |
| **Binary size** | 7.1 MB (release, stripped, LTO) |
| **Memory (idle)** | 14 MB RSS |
| **Startup time** | < 1 second |

## Performance

SkyClaw is built for hyper-performance. Rust's zero-cost abstractions, async runtime, and aggressive release optimizations deliver server-grade capability at embedded-system resource usage.

| Metric | SkyClaw (Rust) | OpenClaw (TypeScript) |
|--------|---------------|----------------------|
| **Idle RAM** | 14 MB | 800 MB – 3 GB |
| **Binary / Install** | 7.1 MB single binary | 75 MB+ (npm + node_modules) |
| **Startup** | < 1 second | 5 – 15 minutes |
| **Runtime** | Native arm64/x86_64 | Node.js + Electron + VS Code |
| **Threads (idle)** | 13 | 50+ (Electron + Node workers) |
| **Dependencies** | 0 runtime deps (static binary) | npm ecosystem |

50-200x lower memory. Instant startup. Single binary deployment. No runtime dependencies.

## 3-Step Setup

### Step 1: Get a Telegram Bot Token

1. Open Telegram and search for [@BotFather](https://t.me/BotFather)
2. Send `/newbot`
3. Choose a name and a username (must end in `bot`)
4. BotFather replies with your bot token
5. Copy it

### Step 2: Deploy

```bash
git clone https://github.com/nagisanzenin/skyclaw.git
cd skyclaw
cargo build --release
export TELEGRAM_BOT_TOKEN="your-token-here"
./target/release/skyclaw start
```

### Step 3: Activate

1. Open your bot in Telegram
2. Send any message — SkyClaw asks for your API key
3. Paste your API key (Anthropic, OpenAI, Gemini, Grok, or OpenRouter)
4. SkyClaw validates it against the real API and goes online

## Supported Providers

Paste any of these API keys in Telegram — SkyClaw detects the provider automatically:

| Key Pattern | Provider | Default Model |
|------------|----------|---------------|
| `sk-ant-*` | Anthropic | claude-sonnet-4-6 |
| `sk-*` | OpenAI | gpt-5.2 |
| `AIzaSy*` | Google Gemini | gemini-2.5-flash |
| `xai-*` | xAI Grok | grok-4-1-fast-non-reasoning |
| `sk-or-*` | OpenRouter | anthropic/claude-sonnet-4-6 |
| *(config only)* | MiniMax | MiniMax-M2.5 |

## Channels

| Channel | Status | Feature Flag | Setup Guide |
|---------|--------|-------------|-------------|
| **Telegram** | Production | `telegram` | [docs/channels/telegram.md](docs/channels/telegram.md) |
| **Discord** | Production | `discord` | [docs/channels/discord.md](docs/channels/discord.md) |
| **Slack** | Production | `slack` | [docs/channels/slack.md](docs/channels/slack.md) |
| **CLI** | Built-in | — | [docs/channels/cli.md](docs/channels/cli.md) |

## Tools

| Tool | Description |
|------|-------------|
| **Shell** | Run any command on your server |
| **Browser** | Stealth headless Chrome — anti-detection patches, session persistence, navigate, click, type, screenshot |
| **File ops** | Read, write, list files on the server |
| **Web fetch** | HTTP GET with token-budgeted response extraction |
| **Git** | Clone, pull, push, commit, branch, diff, log |
| **Messaging** | Send real-time updates during multi-step tasks |
| **File transfer** | Send/receive files through messaging channels |

## AGENTIC CORE

SkyClaw's intelligence layer — 20 modules that make it autonomous:

| Category | Modules |
|----------|---------|
| **Resilience** | Circuit breaker, channel reconnection, graceful shutdown, streaming responses |
| **Intelligence** | Task decomposition, self-correction, DONE criteria, cross-task learning |
| **Self-Healing** | Watchdog, state recovery, health-aware heartbeat, memory failover |
| **Efficiency** | Output compression, system prompt optimization, tiered model routing, history pruning |
| **Autonomy** | Parallel tool execution, agent-to-agent delegation, proactive task initiation, adaptive system prompt |
| **Multimodal** | Vision / image understanding (JPEG, PNG, GIF, WebP) |

## Vision Support

SkyClaw can see and understand images. Send a photo through any channel — the runtime automatically:

1. Downloads the image to workspace
2. Base64-encodes it
3. Includes it as an image content part in the provider request
4. The LLM sees and analyzes the image

Supports Anthropic and OpenAI vision formats natively.

## Architecture

13-crate Cargo workspace:

```
skyclaw (binary)
├── skyclaw-core         Traits (13), types, config, errors
├── skyclaw-gateway      HTTP server, health, dashboard, OAuth identity
├── skyclaw-agent        AGENTIC CORE (20 modules)
├── skyclaw-providers    Anthropic, OpenAI-compatible
├── skyclaw-channels     Telegram, Discord, Slack, CLI
├── skyclaw-memory       SQLite + Markdown with failover
├── skyclaw-vault        ChaCha20-Poly1305 encrypted secrets
├── skyclaw-tools        Shell, browser, file ops, web fetch, git
├── skyclaw-skills       Skill registry (SkyHub v1)
├── skyclaw-automation   Heartbeat, cron scheduler
├── skyclaw-observable   OpenTelemetry, 6 predefined metrics
├── skyclaw-filestore    Local + S3/R2 file storage
└── skyclaw-test-utils   Test helpers
```

## Security

- **Auto-whitelist**: First user to message gets whitelisted. Everyone else denied.
- **Numeric ID only**: Allowlist matches on Telegram user IDs, not usernames.
- **Vault encryption**: ChaCha20-Poly1305 with vault:// URI scheme for secrets.
- **Path traversal protection**: File names sanitized, directory components stripped.
- **Force-push blocked**: Git tool blocks destructive operations by default.
- **Credential message deletion**: API keys and passwords are auto-deleted from chat history after reading.

## Self-Configuration

Tell SkyClaw to change its own settings through natural language:

- "Change model to claude-opus-4-6"
- "Switch to GPT-5.2"

Config lives at `~/.skyclaw/credentials.toml` — SkyClaw reads and edits this file itself.

## CLI Reference

```
skyclaw start              Start the gateway daemon
skyclaw chat               Interactive CLI chat
skyclaw status             Show running state
skyclaw config validate    Validate configuration
skyclaw config show        Print resolved config
skyclaw version            Show version info
```

## Development

```bash
cargo check --workspace                                    # Quick compilation check
cargo build --workspace                                    # Debug build
cargo test --workspace                                     # Run all 1022 tests
cargo clippy --workspace --all-targets --all-features -- -D warnings  # Lint (0 warnings)
cargo fmt --all                                            # Format
cargo build --release                                      # Release build
```

## Requirements

- Rust 1.82+
- Chrome/Chromium (for browser tool)
- A Telegram bot token

## Release Timeline

```
2026-03-09  v1.3.0  ●━━━ Hyper-performance — 4-layer key validation, dynamic system prompt, placeholder defense, 1022 tests
                    │
2026-03-09  v1.2.0  ●━━━ Stealth browser — anti-detection patches, session persistence, credential deletion, 1012 tests
                    │
2026-03-08  v1.1.0  ●━━━ Provider expansion — 6 LLM providers, hot-reload, channel docs, path fixes
                    │
2026-03-08  v1.0.0  ●━━━ AGENTIC CORE — 35 features, 20 autonomy modules, vision support, 905 tests
                    │
2026-03-08  v0.9.0  ●━━━ Production hardening — Dockerfile, systemd, CI/CD, multi-user support
                    │
2026-03-08  v0.8.0  ●━━━ Telegram-native onboarding — API key validation, headless browser, self-config
                    │
2026-03-08  v0.7.0  ●━━━ Per-chat dispatcher — browser tool, stop commands, pending message injection
                    │
2026-03-08  v0.6.0  ●━━━ Agent autonomy — send_message tool, heartbeat system, configurable tool rounds
                    │
2026-03-08  v0.5.0  ●━━━ Agent tools — shell, file ops, file transfer, context management
                    │
2026-03-08  v0.4.0  ●━━━ SUSTAIN — docs, runbooks, skills registry, incident response
                    │
2026-03-08  v0.3.0  ●━━━ SHIP — security remediation (2C/6H/2M fixed), IaC, release workflow
                    │
2026-03-08  v0.2.0  ●━━━ HARDEN — 105 new tests, security audit, STRIDE threat model, deep code review
                    │
2026-03-08  v0.1.0  ●━━━ Wave A — gateway, providers, memory, vault, channels, full type system
                    │
2026-03-08  v0.0.1  ●━━━ Architecture scaffold — 13 crates, 12 traits, research documentation
```

## Community

Join the Discord to discuss, share feedback, and get help.

<a href="https://discord.gg/3ux2c5xz"><img src="https://img.shields.io/badge/Discord-Join%20Community-5865F2?style=for-the-badge&logo=discord&logoColor=white" alt="Join Discord"></a>

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=nagisanzenin/skyclaw&type=Date)](https://star-history.com/#nagisanzenin/skyclaw&Date)

## License

MIT
