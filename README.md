<p align="center">
  <img src="assets/banner.png" alt="TEMM1E" width="100%">
</p>

<p align="center">
  <a href="https://github.com/nagisanzenin/temm1e/stargazers"><img src="https://img.shields.io/github/stars/nagisanzenin/temm1e?style=flat&color=gold&logo=github" alt="GitHub Stars"></a>
  <a href="https://discord.gg/3ux2c5xz"><img src="https://img.shields.io/badge/Discord-Join%20Community-5865F2?logo=discord&logoColor=white" alt="Discord"></a>
  <img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="MIT License">
  <img src="https://img.shields.io/badge/version-2.6.0-blue.svg" alt="Version">
  <img src="https://img.shields.io/badge/rust-1.82+-orange.svg" alt="Rust 1.82+">
</p>

<h3 align="center">Autonomous AI agent runtime in Rust.<br>Deploy once. Stays up forever.</h3>

<p align="center">
  <code>63K lines</code> · <code>1,394 tests</code> · <code>0 warnings</code> · <code>0 panic paths</code> · <code>15 MB idle</code> · <code>31ms cold start</code>
</p>

---

## What is TEMM1E?

Hi! I'm Tem. I'm an autonomous AI agent that lives on your server and never stops running. Talk to me on Telegram, Discord, Slack, or CLI. I execute tasks, browse the web, manage files, write code, and remember everything across sessions.

My brain has a BUDGET and I am VERY responsible with it.

> **Quick start** — 30 seconds if you have Rust and a Telegram bot token:
> ```bash
> git clone https://github.com/nagisanzenin/temm1e.git && cd temm1e
> cargo build --release
> export TELEGRAM_BOT_TOKEN="your-token"
> ./target/release/temm1e start
> ```

---

## Tem's Mind — How I Think

**Tem's Mind** is the cognitive engine at the core of TEMM1E. It's not a wrapper around an LLM — it's a full agent runtime that treats the LLM as a **finite brain** with a token budget, not an infinite text generator.

Here's exactly what happens when you send me a message:

```
                            ┌─────────────────────────────────────────────┐
                            │              TEM'S MIND                     │
                            │         The Agentic Core                    │
                            └─────────────────────────────────────────────┘

 ╭──────────────╮      ╭──────────────────╮      ╭───────────────────────╮
 │  YOU send a  │─────>│  1. CLASSIFY     │─────>│  Chat? Reply in 1    │
 │   message    │      │  Single LLM call │      │  call. Done. Fast.   │
 ╰──────────────╯      │  classifies AND  │      ╰───────────────────────╯
                       │  responds.       │
                       │                  │─────>│  Stop? Halt work     │
                       │  + blueprint_hint│      │  immediately.        │
                       ╰────────┬─────────╯      ╰───────────────────────╯
                                │
                          Order detected
                          Instant ack sent
                                │
                                ▼
                ╭───────────────────────────────╮
                │  2. CONTEXT BUILD             │
                │                               │
                │  System prompt + history +    │
                │  tools + blueprints + memory  │
                │  + learnings — all within     │
                │  a strict TOKEN BUDGET.       │
                │                               │
                │  ┌─────────────────────────┐  │
                │  │ === CONTEXT BUDGET ===  │  │
                │  │ Used:  34,200 tokens    │  │
                │  │ Avail: 165,800 tokens   │  │
                │  │ === END BUDGET ===      │  │
                │  └─────────────────────────┘  │
                ╰───────────────┬───────────────╯
                                │
                                ▼
          ╭─────────────────────────────────────────╮
          │  3. TOOL LOOP                           │
          │                                         │
          │  ┌──────────┐    ┌───────────────────┐  │
          │  │ LLM says │───>│ Execute tool      │  │
          │  │ use tool  │    │ (shell, browser,  │  │
          │  └──────────┘    │  file, web, etc.) │  │
          │       ▲          └────────┬──────────┘  │
          │       │                   │             │
          │       │    ┌──────────────▼──────────┐  │
          │       │    │ Result + verification   │  │
          │       │    │ + pending user messages  │  │
          │       │    │ + vision images          │  │
          │       └────┤ fed back to LLM         │  │
          │            └─────────────────────────┘  │
          │                                         │
          │  Loops until: final text reply,          │
          │  budget exhausted, or user interrupts.   │
          │  No artificial iteration caps.           │
          ╰─────────────────────┬───────────────────╯
                                │
                                ▼
              ╭─────────────────────────────────╮
              │  4. POST-TASK                   │
              │                                 │
              │  - Extract learnings            │
              │  - Author/refine Blueprint      │
              │  - Notify user                  │
              │  - Checkpoint to task queue     │
              ╰─────────────────────────────────╯
```

### The five systems that make this work:

<table>
<tr>
<td width="50%" valign="top">

#### :brain: Finite Brain Model

The context window is not a log file. It is working memory with a hard limit. Every token consumed is a neuron recruited. Every token wasted is a thought I can no longer have.

Every resource declares its token cost upfront. Every context rebuild shows me a budget dashboard. I know my skull. I respect my skull.

When a blueprint is too large, I degrade gracefully: **full body** → **outline** → **catalog listing**. I never crash from overflow.

</td>
<td width="50%" valign="top">

#### :scroll: Blueprints — Procedural Memory

Traditional agents summarize: *"Deployed the app using Docker."* Useless.

I create **Blueprints** — structured, replayable recipes with exact commands, verification steps, and failure modes. When a similar task comes in, I follow the recipe directly instead of re-deriving everything from scratch.

**Zero extra LLM calls** to match — the classifier piggybacks a `blueprint_hint` field (~20 tokens) on an existing call.

</td>
</tr>
<tr>
<td width="50%" valign="top">

#### :eye: Vision Browser

I see websites the way you do. Screenshot → LLM vision analyzes the page → `click_at(x, y)` via Chrome DevTools Protocol.

Bypasses Shadow DOM, anti-bot protections, and dynamically rendered content. Works headless on a $5 VPS. No Selenium. No Playwright. Pure CDP.

</td>
<td width="50%" valign="top">

#### :shield: 4-Layer Panic Resilience

Born from a real incident: Vietnamese `ẹ` sliced at an invalid UTF-8 byte boundary crashed the entire process. Now:

1. `char_indices()` everywhere — no invalid slicing
2. `catch_unwind` per message — panics become error replies
3. Dead worker detection — auto-respawn
4. Global panic hook — structured logging

I do NOT go down quietly and I do NOT stay down.

</td>
</tr>
<tr>
<td colspan="2" align="center">

#### :zap: Self-Extending Tools

I discover and install MCP servers at runtime. I also write my own bash/python/node tools and persist them to disk. **If I don't have a tool, I make one.**

</td>
</tr>
</table>

---

## At a Glance

<table>
<tr>
<td align="center"><strong>15 MB</strong><br><sub>Idle RAM</sub></td>
<td align="center"><strong>31 ms</strong><br><sub>Cold start</sub></td>
<td align="center"><strong>9.6 MB</strong><br><sub>Binary size</sub></td>
<td align="center"><strong>1,394</strong><br><sub>Tests</sub></td>
<td align="center"><strong>8</strong><br><sub>AI Providers</sub></td>
<td align="center"><strong>13</strong><br><sub>Built-in tools</sub></td>
<td align="center"><strong>4</strong><br><sub>Channels</sub></td>
</tr>
</table>

### vs. the competition

| Metric | **TEMM1E** (Rust) | OpenClaw (TypeScript) | ZeroClaw (Rust) |
|--------|:-:|:-:|:-:|
| Idle RAM | **15 MB** | ~1,200 MB | ~4 MB |
| Peak RAM (3-turn) | **17 MB** | ~1,500 MB+ | ~8 MB |
| Binary size | **9.6 MB** | ~800 MB | ~12 MB |
| Cold start | **31 ms** | ~8,000 ms | <10 ms |

I run on a $5/month 512 MB VPS where Node.js agents can't even start. [Benchmark report](docs/benchmarks/BENCHMARK_REPORT.md)

---

## Supported Providers

Paste any API key in Telegram — I detect the provider automatically:

| Key Pattern | Provider | Default Model |
|:-:|:-:|:-:|
| `sk-ant-*` | Anthropic | claude-sonnet-4-6 |
| `sk-*` | OpenAI | gpt-5.2 |
| `AIzaSy*` | Google Gemini | gemini-3-flash-preview |
| `xai-*` | xAI Grok | grok-4-1-fast-non-reasoning |
| `sk-or-*` | OpenRouter | anthropic/claude-sonnet-4-6 |
| ChatGPT login | **Codex OAuth** | gpt-5.4 |

> **Codex OAuth**: No API key needed. Just `temm1e auth login` → log into ChatGPT Plus/Pro → done.
> Switch models live with `/model`. Tokens auto-refresh.

---

## Channels & Tools

<table>
<tr>
<td width="50%" valign="top">

**Channels**

| Channel | Status |
|---------|:------:|
| [Telegram](docs/channels/telegram.md) | Production |
| [Discord](docs/channels/discord.md) | Production |
| [Slack](docs/channels/slack.md) | Production |
| [CLI](docs/channels/cli.md) | Production |

</td>
<td width="50%" valign="top">

**13 Built-in Tools**

Shell, stealth browser (vision click_at), file read/write/list, web fetch, git, send_message, send_file, memory CRUD, key management, MCP management, self-extend, self-add MCP, self-create tool

**14 MCP Servers** in the registry — discovered and installed at runtime

**Vision**: JPEG, PNG, GIF, WebP — graceful fallback on text-only models

</td>
</tr>
</table>

---

## Architecture

15-crate Cargo workspace:

```
temm1e (binary)
│
├─ temm1e-core           Shared traits (13), types, config, errors
├─ temm1e-agent          TEM'S MIND — 25 modules, blueprint system, executable DAG
├─ temm1e-providers      Anthropic + OpenAI-compatible (7 providers via one adapter)
├─ temm1e-codex-oauth    ChatGPT Plus/Pro via OAuth PKCE
├─ temm1e-channels       Telegram, Discord, Slack, CLI
├─ temm1e-memory         SQLite + Markdown with automatic failover
├─ temm1e-vault          ChaCha20-Poly1305 encrypted secrets
├─ temm1e-tools          Shell, browser, file ops, web fetch, git
├─ temm1e-mcp            MCP client — stdio + HTTP, 14-server registry
├─ temm1e-gateway        HTTP server, health, dashboard, OAuth identity
├─ temm1e-skills         Skill registry (TemHub v1)
├─ temm1e-automation     Heartbeat, cron scheduler
├─ temm1e-observable     OpenTelemetry, 6 predefined metrics
├─ temm1e-filestore      Local + S3/R2 file storage
└─ temm1e-test-utils     Test helpers
```

> [Agentic core snapshot](docs/agentic_core/SNAPSHOT_v2.6.0.md) — exact implementation reference for Tem's Mind

---

## Security

| Layer | Protection |
|-------|-----------|
| **Access control** | Deny-by-default. First user auto-whitelisted. Numeric IDs only. |
| **Secrets at rest** | ChaCha20-Poly1305 vault with `vault://` URI scheme |
| **Key onboarding** | AES-256-GCM one-time key encryption before transit ([design doc](docs/OTK_SECURE_KEY_SETUP.md)) |
| **Credential hygiene** | API keys auto-deleted from chat history. Secret output filter on replies. |
| **Path traversal** | File names sanitized, directory components stripped |
| **Git safety** | Force-push blocked by default |

---

## Setup

Two paths:

- **[Setup for Beginners](SETUP_FOR_NEWBIE.md)** — step-by-step with screenshots
- **[Setup for Pros](SETUP_FOR_PROS.md)** — clone, build, configure, deploy

```bash
git clone https://github.com/nagisanzenin/temm1e.git && cd temm1e
cargo build --release
export TELEGRAM_BOT_TOKEN="your-token"
./target/release/temm1e auth login   # ChatGPT OAuth (or skip, paste API key in Telegram)
./target/release/temm1e start
```

---

## CLI Reference

```
temm1e start                 Start the gateway (foreground or -d for daemon)
temm1e stop                  Graceful shutdown
temm1e chat                  Interactive CLI chat
temm1e status                Show running state
temm1e update                Pull latest + rebuild
temm1e auth login            Codex OAuth (browser or --headless)
temm1e auth status           Check token validity
temm1e auth logout           Clear stored tokens
temm1e config validate       Validate temm1e.toml
temm1e config show           Print resolved config
temm1e reset --confirm       Factory reset with backup
```

---

## Development

```bash
cargo check --workspace                                              # Quick check
cargo test --workspace                                               # 1,394 tests
cargo clippy --workspace --all-targets --all-features -- -D warnings # 0 warnings
cargo fmt --all                                                      # Format
cargo build --release                                                # Release binary
```

Requires Rust 1.82+ and Chrome/Chromium (for the browser tool).

---

<details>
<summary><strong>Release Timeline</strong> — every version from first breath to now</summary>

```
2026-03-14  v2.6.0  ●━━━ Introduce TEMM1E — vision browser (screenshot→LLM→click_at via CDP), Tool trait vision extension, model_supports_vision gating, message dedup fixes, interceptor unlimited output, blueprint notification, Tem identity
                    │
2026-03-13  v2.5.0  ●━━━ Executable DAG + Blueprint System — phase parallelism via FuturesUnordered, phase parser + TaskGraph bridge, /reload /reset commands, factory reset CLI, 1394 tests
                    │
2026-03-11  v2.4.1  ●━━━ Codex OAuth polish — auto-detect at startup, live model switching, callback race fix, LLM stop category
                    │
2026-03-11  v2.4.0  ●━━━ Interceptor Phase 1 — real-time task status via watch channel, CancellationToken, prompted tool calling fallback
                    │
2026-03-11  v2.3.1  ●━━━ Model registry — per-model limits for 50+ models, 10% safety margin, auto-cap for small models
                    │
2026-03-11  v2.3.0  ●━━━ Codex OAuth — ChatGPT Plus/Pro as provider via OAuth PKCE, temm1e auth commands
                    │
2026-03-11  v2.2.0  ●━━━ Custom tool authoring + daemon mode — self_create_tool, ScriptToolAdapter, hot-reload
                    │
2026-03-11  v2.1.0  ●━━━ MCP self-extension — MCP client, self_extend_tool, 14-server registry, stdio + HTTP
                    │
2026-03-11  v2.0.1  ●━━━ LLM classification — single call classifies AND responds, no iteration caps
                    │
2026-03-10  v2.0.0  ●━━━ TEM'S MIND V2 — complexity classification, prompt stratification, 12% cheaper, 14% fewer tool calls
                    │
2026-03-10  v1.7.0  ●━━━ Vision fallback & /model — graceful image stripping, live model switching
                    │
2026-03-10  v1.6.0  ●━━━ Extreme resilience — zero panic paths, 26-finding audit, dead worker respawn
                    │
2026-03-10  v1.5.1  ●━━━ Crash resilience — 4-layer panic recovery, UTF-8 safety, conversation persistence
                    │
2026-03-09  v1.5.0  ●━━━ OTK secure key setup — AES-256-GCM onboarding, secret output filter
                    │
2026-03-09  v1.4.0  ●━━━ Persistent memory & budget — memory_manage tool, knowledge auto-injection
                    │
2026-03-09  v1.3.0  ●━━━ Hyper-performance — 4-layer key validation, dynamic system prompt
                    │
2026-03-09  v1.2.0  ●━━━ Stealth browser — anti-detection, session persistence
                    │
2026-03-08  v1.1.0  ●━━━ Provider expansion — 6 providers, hot-reload
                    │
2026-03-08  v1.0.0  ●━━━ TEM'S MIND — 35 features, 20 autonomy modules, 905 tests
                    │
2026-03-08  v0.9.0  ●━━━ Production hardening — Docker, systemd, CI/CD
                    │
2026-03-08  v0.8.0  ●━━━ Telegram-native onboarding
                    │
2026-03-08  v0.7.0  ●━━━ Per-chat dispatcher — browser tool, stop commands
                    │
2026-03-08  v0.6.0  ●━━━ Agent autonomy — send_message, heartbeat
                    │
2026-03-08  v0.5.0  ●━━━ Agent tools — shell, file ops, file transfer
                    │
2026-03-08  v0.4.0  ●━━━ SUSTAIN — docs, runbooks, skills registry
                    │
2026-03-08  v0.3.0  ●━━━ SHIP — security remediation, IaC, release workflow
                    │
2026-03-08  v0.2.0  ●━━━ HARDEN — 105 tests, security audit, STRIDE threat model
                    │
2026-03-08  v0.1.0  ●━━━ Wave A — gateway, providers, memory, vault, channels
                    │
2026-03-08  v0.0.1  ●━━━ Architecture scaffold — 13 crates, 12 traits
```

</details>

---

<p align="center">
  <a href="https://discord.gg/3ux2c5xz"><img src="https://img.shields.io/badge/Discord-Join%20Community-5865F2?style=for-the-badge&logo=discord&logoColor=white" alt="Join Discord"></a>
</p>

<p align="center">

[![Star History Chart](https://api.star-history.com/svg?repos=nagisanzenin/temm1e&type=Date)](https://www.star-history.com/?repos=nagisanzenin%2Ftemm1e&type=date&legend=top-left)

</p>

<p align="center">MIT License</p>
