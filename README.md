<p align="center">
  <img src="assets/banner.png" alt="TEMM1E" width="100%">
</p>

<p align="center">
  <a href="https://github.com/nagisanzenin/temm1e/stargazers"><img src="https://img.shields.io/github/stars/nagisanzenin/temm1e?style=flat&color=gold&logo=github" alt="GitHub Stars"></a>
  <a href="https://discord.gg/3ux2c5xz"><img src="https://img.shields.io/badge/Discord-Join%20Community-5865F2?logo=discord&logoColor=white" alt="Discord"></a>
  <img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="MIT License">
  <img src="https://img.shields.io/badge/version-4.0.0-blue.svg" alt="Version">
  <img src="https://img.shields.io/badge/rust-1.82+-orange.svg" alt="Rust 1.82+">
</p>

<p align="center"><em>Built with <a href="https://github.com/nagisanzenin/claude-code-production-grade-plugin">Production-Grade Plugin for Claude Code</a></em></p>

<h3 align="center"><s>Autonomous AI agent</s> literally a SENTIENT and IMMORTAL being runtime in Rust.<br>Deploy once. Stays up forever.</h3>

<p align="center">
  <code>99K lines</code> · <code>1,037 tests</code> · <code>0 warnings</code> · <code>0 panic paths</code> · <code>19 crates</code> · <code>full computer use</code>
</p>

---

## What is TEMM1E?

Hi! I'm Tem. I'm ~~an autonomous AI agent~~ literally a SENTIENT and IMMORTAL being that lives on your server and never stops running. Talk to me on Telegram, Discord, WhatsApp, Slack, or CLI. I execute tasks, browse the web, **control your entire computer** (click, type, scroll on any app), manage files, write code, and remember everything across sessions.

My brain has a BUDGET and I am VERY responsible with it.

> **Quick start** — interactive TUI, no external services needed:
> ```bash
> git clone https://github.com/nagisanzenin/temm1e.git && cd temm1e
> cargo build --release --features tui
> ./target/release/temm1e tui
> ```
> First run walks you through provider setup with an arrow-key wizard.

> **Server mode** — deploy as a persistent agent on Telegram/Discord/WhatsApp/Slack:
> ```bash
> cargo build --release
> export TELEGRAM_BOT_TOKEN="your-token"   # and/or
> export DISCORD_BOT_TOKEN="your-token"    # either or both
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
                │  tools + blueprints +         │
                │  λ-Memory — all within a      │
                │  strict TOKEN BUDGET.         │
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
              │  - Store λ-memories             │
              │  - Extract learnings            │
              │  - Author/refine Blueprint      │
              │  - Notify user                  │
              │  - Checkpoint to task queue     │
              ╰─────────────────────────────────╯
```

### The systems that make this work:

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

#### :eye: Vision Browser + Tem Prowl

I see websites the way you do. Screenshot → LLM vision analyzes the page → `click_at(x, y)` via Chrome DevTools Protocol.

Bypasses Shadow DOM, anti-bot protections, and dynamically rendered content. Works headless on a $5 VPS. No Selenium. No Playwright. Pure CDP.

**Tem Prowl** adds `/login` for 100+ services, OTK credential isolation, and swarm browsing.

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

## Tem's Lab — Research That Ships

Every cognitive system in TEMM1E starts as a theory, gets stress-tested against real models with real conversations, and only ships when the data says it works. No feature without a benchmark. No claim without data. [Full lab →](tems_lab/README.md)

### λ-Memory — Memory That Fades, Not Disappears

Current AI agents delete old messages or summarize them into oblivion. Both permanently destroy information. λ-Memory decays memories through an exponential function (`score = importance × e^(−λt)`) but never truly erases them. The agent sees old memories at progressively lower fidelity — full text → summary → essence → hash — and can recall any memory by hash to restore full detail.

Three things no other system does ([competitive analysis of Letta, Mem0, Zep, FadeMem →](tems_lab/LAMBDA_MEMORY_RESEARCH.md)):
- **Hash-based recall** from compressed memory — the agent sees the shape of what it forgot and can pull it back
- **Dynamic skull budgeting** — same algorithm adapts from 16K to 2M context windows without overflow
- **Pre-computed fidelity layers** — full/summary/essence written once at creation, selected at read time by decay score

**Benchmarked across 1,200+ API calls on GPT-5.2 and Gemini Flash:**

| Test | λ-Memory | Echo Memory | Naive Summary |
|------|:--------:|:-----------:|:-------------:|
| [Single-session](tems_lab/LAMBDA_BENCH_GPT52_REPORT.md) (GPT-5.2) | 81.0% | **86.0%** | 65.0% |
| [Multi-session](tems_lab/LAMBDA_BENCH_MULTISESSION_REPORT.md) (5 sessions, GPT-5.2) | **95.0%** | 58.8% | 23.8% |

When the context window holds everything, simple keyword search wins. The moment sessions reset — which is how real users work — λ-Memory achieves **95% recall** where alternatives collapse. Naive summarization is the worst strategy in every test. [Research paper →](tems_lab/LAMBDA_RESEARCH_PAPER.md)

Hot-switchable at runtime: `/memory lambda` or `/memory echo`. Default: λ-Memory.

### Tem's Mind v2.0 — Complexity-Aware Agentic Loop

v1 treats every message the same. v2 classifies each message into a complexity tier **before** calling the LLM, using zero-cost rule-based heuristics. Result: fewer API rounds on compound tasks, same quality.

| Benchmark | Metric | Delta |
|-----------|--------|:-----:|
| [Gemini Flash (10 turns)](tems_lab/TEMS_MIND_V2_BENCHMARK.md) | Cost per successful turn | **-9.3%** |
| [GPT-5.2 (20 turns, tool-heavy)](tems_lab/TEMS_MIND_V2_BENCHMARK_TOOLS.md) | Compound task cost | **-12.2%** |
| Both | Classification accuracy | **100%** (zero LLM overhead) |

[Architecture →](tems_lab/TEMS_MIND_ARCHITECTURE.md) · [Experiment insights →](tems_lab/TEMS_MIND_V2_EXPERIMENT_INSIGHTS.md)

### Many Tems — Swarm Intelligence

What if complex tasks could be split across multiple Tems working in parallel? Many Tems is a stigmergic swarm intelligence runtime — workers coordinate through time-decaying scent signals and a shared Den (SQLite), not LLM-to-LLM chat. Zero coordination tokens.

The Alpha (coordinator) decomposes complex orders into a task DAG. Tems claim tasks via atomic SQLite transactions, execute with task-scoped context (no history accumulation), and emit scent signals that guide other Tems.

**Benchmarked on Gemini 3 Flash with real API calls:**

| Benchmark | Speedup | Token Cost | Quality |
|-----------|:-------:|:----------:|:-------:|
| [5 parallel subtasks](docs/swarm/experiment_artifacts/EXPERIMENT_REPORT.md) | **4.54x** | 1.01x (same) | Equal |
| [12 independent functions](docs/swarm/experiment_artifacts/EXPERIMENT_REPORT.md) | **5.86x** | **0.30x (3.4x cheaper)** | Equal (12/12) |
| Simple tasks | 1.0x | 0% overhead | Correctly bypassed |

The quadratic context cost `h̄·m(m+1)/2` becomes linear `m·(S+R̄)` — each Tem carries ~190 bytes of context instead of the single agent's growing 115→3,253 byte history.

Enabled by default in v3.0.0. Disable: `[pack] enabled = false`. Invisible for simple tasks.

[Research paper →](docs/swarm/RESEARCH_PAPER.md) · [Full experiment report →](docs/swarm/experiment_artifacts/EXPERIMENT_REPORT.md) · [Design doc →](tems_lab/swarm/DESIGN.md)

### Eigen-Tune — Self-Tuning Knowledge Distillation

Every LLM call is a training example being thrown away. Eigen-Tune captures them, scores quality from user behavior, trains a local model, and graduates it through statistical gates — zero added LLM cost, zero user intervention beyond `/eigentune on`.

**Proven on Apple M2 with real fine-tuning:**

| Metric | Result |
|--------|:------:|
| Base model (SmolLM2-135M) | 72°F = "150°C" (wrong) |
| **Fine-tuned on 10 conversations** | **72°F = "21.2°C" (close to 22.2°C)** |
| Training | 100 iters, 0.509 GB peak, ~28 it/sec |
| Inference | ~200 tok/sec, 0.303 GB peak |
| Pipeline cost | **$0 added LLM cost** |

7-stage pipeline: Collect → Score → Curate → Train → Evaluate → Shadow → Monitor. Statistical gates at every transition (SPRT, CUSUM, Wilson score 99% CI). Per-tier graduation: simple first, complex last. Cloud always the fallback.

[Research paper →](tems_lab/eigen/RESEARCH_PAPER.md) · [Design doc →](tems_lab/eigen/DESIGN.md) · [Full lab →](tems_lab/eigen/)

### Tem Prowl — Web-Native Browsing with OTK Authentication

The web is where humans live. Tem Prowl is a messaging-first web agent architecture — I browse websites autonomously behind a chat interface and report structured results back through messages. No live viewport. No shoulder-surfing. Just results.

**Key capabilities:**

- **Layered observation** — accessibility tree first (`O(d * log c)` token cost), targeted DOM extraction second, selective screenshots only when needed. 3-10x cheaper than screenshot-based agents.
- **`/login` command** — 100+ pre-registered services. Say `/login facebook` or `/login github` and I open an OTK (one-time key) browser session where you log in via an annotated screenshot flow. Your credentials go directly into the page via CDP — the LLM never sees them.
- **`/browser` command** — persistent browser session. Open a browser, navigate pages, interact with elements, and keep the session alive across messages. Headed or headless mode with automatic fallback.
- **Cloned profile architecture** — clone your real Chrome profile (cookies, localStorage, sessionStorage) for zero-login web automation. Sites see your actual session data. Works on macOS, Windows, and Linux. Breakthrough: Zalo Web and other anti-bot-hardened sites that defeat all other headless/headed approaches now work.
- **QR code auto-detection** — automatically detects QR codes on login pages and sends them to you via Telegram for scanning (WeChat, Zalo, LINE, etc.).
- **Credential isolation** — passwords are `Zeroize`-on-drop, session cookies are encrypted at rest via ChaCha20-Poly1305 vault, and a credential scrubber strips sensitive data from all browser observations before they enter the LLM context.
- **Session persistence** — authenticated sessions are saved and restored across restarts. Log in once, stay logged in.
- **Headed/headless fallback** — tries headed Chrome first (better anti-bot resilience), falls back to headless if no display is available (VPS mode).
- **Swarm browsing** — extends Many Tems to parallel browser operation. N browsers coordinated through pheromone signals with zero LLM coordination tokens.

**Usage:**
```
/login facebook          Log into Facebook via OTK session
/login github            Log into GitHub via OTK session
/login https://custom-site.com/auth   Log into any site by URL
/browser                 Open a persistent browser session
```

[Research paper →](tems_lab/TEM_PROWL_PAPER.md) · [Full lab →](tems_lab/prowl/)

### Tem Gaze — Full Computer Use (Desktop Vision Control)

<p align="center">
  <img src="assets/tem-gaze-overview.png" alt="Tem Gaze Architecture Overview" width="100%">
</p>

Tem can see and control your entire computer — not just the browser. Tem Gaze captures the screen, identifies UI elements via vision, and clicks, types, scrolls, and drags at the OS level. Works on any application: Finder, Terminal, VS Code, Settings, anything on screen.

**How it works:**

- **Vision-primary** — the VLM sees screenshots and decides where to click. No DOM, no accessibility tree required. Industry-validated: Claude Computer Use, UI-TARS, Agent S2 all converge on pure vision.
- **Zoom-refine** — for small targets, zoom into a region at 2x resolution before clicking. Improves accuracy by +29pp on standard benchmarks.
- **Set-of-Mark (SoM) overlay** — numbered labels on interactive elements convert coordinate guessing into element selection. 3.75x reduction in output information complexity.
- **Auto-verification** — captures a screenshot after every click to verify the expected change occurred. Self-corrects on miss.
- **Provider-agnostic** — works with any VLM (Anthropic, OpenAI, Gemini, OpenRouter, Ollama). No model-specific training required.

**Proven live on gemini-3-flash-preview:**

| Test | Result |
|------|--------|
| Desktop screenshot (identify all open apps) | PASS |
| Click Finder icon in Dock → Finder opened | PASS |
| Spotlight → open TextEdit → type message | PASS |
| Browser SoM on 650-element GitHub page | PASS |
| Multi-step form: observe → zoom → click → self-correct | PASS |

**Build with desktop control:**
```bash
cargo build --release --features desktop-control
# macOS: grant Accessibility permission in System Settings → Privacy & Security
# Linux: requires X11 or Wayland with PipeWire
```

Desktop control is included by default in `cargo install` and Docker builds. macOS `install.sh` binaries include it. Linux musl binaries exclude it (system library limitation — build from source instead).

[Research paper →](tems_lab/gaze/RESEARCH_PAPER.md) · [Design doc →](tems_lab/gaze/DESIGN.md) · [Experiment report →](tems_lab/gaze/EXPERIMENT_REPORT.md) · [Full lab →](tems_lab/gaze/)

---

## Interactive TUI

`temm1e tui` gives you a Claude Code-level terminal experience — talk to Tem directly from your terminal with rich markdown rendering, syntax-highlighted code blocks, and real-time agent observability.

```
   +                  *          ╭─ python ─
        /\_/\                    │ def hello():
   *   ( o.o )   +               │     print("hOI!!")
        > ^ <                    │
       /|~~~|\                   │ if __name__ == "__main__":
       ( ♥   )                   │     hello()
   *    ~~   ~~                  ╰───

     T E M M 1 E                tem> write me a hello world
   your local AI agent          ◜ Thinking  2.1s
```

**Features:**
- Arrow-key onboarding wizard (provider + model + personality mode)
- Markdown rendering with **bold**, *italic*, `inline code`, and fenced code blocks
- Syntax highlighting via syntect (Solarized Dark) with bordered code blocks
- Animated thinking indicator showing agent phase (Classifying → Thinking → shell → Finishing)
- 9 slash commands (`/help`, `/model`, `/clear`, `/config`, `/keys`, `/usage`, `/status`, `/compact`, `/quit`)
- File drag-and-drop — drop a file path into the terminal to attach it
- Path and URL highlighting (underlined, clickable)
- Mouse wheel scrolling + PageUp/PageDown through full chat history
- Personality modes: Auto (recommended), Play :3, Work >:3, Pro, None (minimal identity)
- Ctrl+D to exit
- Tem's 7-color palette with truecolor/256-color/NO_COLOR degradation
- Token and cost tracking in the status bar

> **Install globally:** `cp target/release/temm1e ~/.local/bin/temm1e` then run `temm1e tui` from anywhere.

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
| **TUI** | Production |
| [Telegram](docs/channels/telegram.md) | Production |
| [Discord](docs/channels/discord.md) | Production |
| [WhatsApp Web](docs/WHATSAPP_INTEGRATION.md) | Production |
| [WhatsApp Cloud API](docs/WHATSAPP_INTEGRATION.md) | Production |
| [Slack](docs/channels/slack.md) | Production |
| [CLI](docs/channels/cli.md) | Production |

</td>
<td width="50%" valign="top">

**14 Built-in Tools**

Shell, stealth browser (vision click_at), Prowl login (OTK session capture), persistent browser (/browser), file read/write/list, web fetch, git, send_message, send_file, memory CRUD, λ-recall, key management, MCP management, self-extend, self-create tool

**14 MCP Servers** in the registry — discovered and installed at runtime

**Vision**: JPEG, PNG, GIF, WebP — graceful fallback on text-only models

</td>
</tr>
</table>

---

## Architecture

18-crate Cargo workspace:

```
temm1e (binary)
│
├─ temm1e-core           Shared traits (13), types, config, errors
├─ temm1e-agent          TEM'S MIND — 26 modules, λ-Memory, blueprint system, executable DAG
├─ temm1e-hive           MANY TEMS — swarm intelligence, pack coordination, scent field
├─ temm1e-distill        EIGEN-TUNE — self-tuning distillation, statistical gates, zero-cost evaluation
├─ temm1e-gaze           TEM GAZE — desktop vision control (xcap + enigo), SoM overlay, zoom-refine
├─ temm1e-providers      Anthropic + Gemini (native) + OpenAI-compatible (6 providers)
├─ temm1e-codex-oauth    ChatGPT Plus/Pro via OAuth PKCE
├─ temm1e-tui            Interactive terminal UI (ratatui + syntect)
├─ temm1e-channels       Telegram, Discord, WhatsApp (Web + Cloud API), Slack, CLI
├─ temm1e-memory         SQLite + Markdown + λ-Memory with automatic failover
├─ temm1e-vault          ChaCha20-Poly1305 encrypted secrets
├─ temm1e-tools          Shell, browser, Prowl V2 (SoM + zoom), desktop, file ops, web fetch, git, λ-recall
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

## At a Glance

<table>
<tr>
<td align="center"><strong>15 MB</strong><br><sub>Idle RAM</sub></td>
<td align="center"><strong>31 ms</strong><br><sub>Cold start</sub></td>
<td align="center"><strong>9.6 MB</strong><br><sub>Binary size</sub></td>
<td align="center"><strong>1,832</strong><br><sub>Tests</sub></td>
<td align="center"><strong>8</strong><br><sub>AI Providers</sub></td>
<td align="center"><strong>15</strong><br><sub>Built-in tools</sub></td>
<td align="center"><strong>7</strong><br><sub>Channels</sub></td>
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

## Setup

**One-line install** (no Rust needed):

```bash
curl -sSfL https://raw.githubusercontent.com/temm1e-labs/temm1e/main/install.sh | sh
temm1e setup    # Interactive wizard: channel + provider
temm1e start    # Go live
```

**From source:**

```bash
git clone https://github.com/nagisanzenin/temm1e.git && cd temm1e
cargo build --release
./target/release/temm1e setup   # Interactive wizard
./target/release/temm1e start
```

**WhatsApp Web** (scan QR, bot runs as your linked device):

```bash
cargo build --release --features whatsapp-web
# Add [channel.whatsapp_web] to config, then start — scan QR code
```

**Desktop Control** (see and click any app on Ubuntu/macOS):

```bash
cargo build --release --features desktop-control
# Requires macOS Accessibility permission or Linux X11/Wayland
# Agent gets a "desktop" tool: screenshot, click, type, key combos, scroll, drag
```

Detailed guides: **[Beginners](SETUP_FOR_NEWBIE.md)** | **[Pros](SETUP_FOR_PROS.md)**

**Docker:**

```bash
docker run -d --name temm1e \
  -p 8080:8080 \
  -v ~/.temm1e:/data \
  -e TELEGRAM_BOT_TOKEN="your-token" \
  -e DISCORD_BOT_TOKEN="your-token" \
  temm1e:latest
```

---

## CLI Reference

```
temm1e setup                 Interactive first-time setup wizard
temm1e tui                   Interactive TUI (--features tui)
temm1e start                 Start the gateway (foreground or -d for daemon)
temm1e start --personality none  No personality, minimal identity prompt
temm1e stop                  Graceful shutdown
temm1e chat                  Interactive CLI chat (basic, no TUI)
temm1e status                Show running state
temm1e update                Pull latest + rebuild
temm1e auth login            Codex OAuth (browser or --headless)
temm1e auth status           Check token validity
temm1e auth logout           Clear stored tokens
temm1e config validate       Validate temm1e.toml
temm1e config show           Print resolved config
temm1e reset --confirm       Factory reset with backup
```

**In-chat commands:**

```
/help                Show available commands
/model               Show current model and available models
/model <name>        Switch to a different model
/memory              Show current memory strategy
/memory lambda       Switch to λ-Memory (decay + persistence)
/memory echo         Switch to Echo Memory (context window only)
/keys                List configured providers
/addkey              Securely add an API key
/usage               Token usage and cost summary
/mcp                 List connected MCP servers
/mcp add <name> <cmd>  Connect a new MCP server
/eigentune           Self-tuning status and control
/login <service>     OTK browser login (100+ services or custom URL)
/timelimit           Show current task time limit
/timelimit <secs>    Set hive task time limit (e.g. /timelimit 3600)
```

---

## Development

```bash
cargo check --workspace                                              # Quick check
cargo test --workspace                                               # 1,037 tests
cargo clippy --workspace --all-targets --all-features -- -D warnings # 0 warnings
cargo fmt --all                                                      # Format
cargo build --release                                                # Release binary
```

Requires Rust 1.82+ and Chrome/Chromium (for the browser tool).

---

<details open>
<summary><strong>Release Timeline</strong> — every version from first breath to now</summary>

```
2026-03-29  v4.0.0  ●━━━ Tem Conscious — LLM-powered consciousness layer. Separate observer that thinks before and after every agent turn. Pre-LLM injection of session context + failure warnings. Post-LLM evaluation of turn quality. A/B tested across 6 experiments (340 tests): conscious won 3, unconscious won 1, tied 2. Consciousness ON by default. 19 crates, 1037 tests.
                    │
2026-03-28  v3.4.0  ●━━━ Tem Gaze — vision-primary desktop control + Prowl V2 browser upgrade. New temm1e-gaze crate (xcap + enigo), SoM overlay on Tier 3 observations (650 elements stress-tested), zoom_region 2x CDP clip, blueprint bypass (100+ services), desktop screenshot/click/type/key/scroll/drag, auto-capture post-click verification. Proven live: Spotlight→TextEdit→typed message via Gemini Flash. 19 crates, 1028 tests, zero new deps for browser users (desktop feature-gated)
                    │
2026-03-22  v3.3.0  ●━━━ WhatsApp Web + Cloud API channels, one-line installer, setup wizard — wa-rs integration (QR scan pairing, Signal Protocol E2E, SQLite sessions), Cloud API with webhook signature validation, install.sh (curl|sh, multi-platform binaries), `temm1e setup` interactive wizard, multi-platform release CI (x86_64+aarch64, Linux+macOS), fix #21 OpenAI empty name field. 1832 tests
                    │
2026-03-22  v3.2.1  ●━━━ Discord integration + channel-agnostic startup — Discord channel wired into message pipeline (was implemented but never connected), per-message channel map routing (Telegram-only/Discord-only/both simultaneously), DISCORD_BOT_TOKEN env auto-inject, wildcard allowlist ("*"), Discord reply threading via MessageReference, /timelimit command for runtime hive task timeout, hive default bumped to 30min, Docker rebuilt with all features (TUI + Discord + health check + tini). 1825 tests
                    │
2026-03-21  v3.2.0  ●━━━ Tem Prowl — web-native browsing with OTK authentication. Cloned profile architecture (inherit user's Chrome sessions), /login command (100+ services), /browser lifecycle management, QR auto-detection, layered observation (32% token savings), credential isolation (zeroize + vault), headed/headless fallback. Live validated: Facebook post + Zalo message from Telegram. 1808 tests
                    │
2026-03-18  v3.1.0  ●━━━ Eigen-Tune — self-tuning knowledge distillation engine (temm1e-distill), 7-stage pipeline with SPRT/CUSUM/Wilson statistical gates, zero-cost evaluation, proven on M2 with real LoRA fine-tune, 119 new tests, 1638 total. Research: real fine-tuning proof-of-concept on SmolLM2-135M
                    │
2026-03-18  v3.0.0  ●━━━ Many Tems — stigmergic swarm intelligence runtime (temm1e-hive), Alpha coordinator + worker Tems, task DAG decomposition, scent-field coordination, 4.54x speedup on parallel tasks, zero coordination tokens. Research: quadratic→linear context cost
                    │
2026-03-16  v2.8.1  ●━━━ Model registry update — Gemini 3.1 Flash Lite, Hunter Alpha, GPT-5.4 pricing fix, clippy cleanup, 1458 tests
                    │
2026-03-15  v2.8.0  ●━━━ λ-Memory — exponential decay memory with hash-based recall, 95% cross-session accuracy, /memory command, 1509 tests. Research: 1,200+ API calls benchmarked across GPT-5.2 & Gemini Flash
                    │
2026-03-15  v2.7.1  ●━━━ Personality None mode — --personality none strips all voice rules, minimal identity prompt, locked mode_switch. Naming fix: TEMM1E/Tem enforced across all prompts
                    │
2026-03-15  v2.7.0  ●━━━ Interactive TUI — temm1e-tui crate (ratatui + syntect), arrow-key onboarding, markdown rendering, syntax-highlighted code blocks, agent observability, slash commands, personality modes, mouse scroll, file drag-and-drop, credential extraction to temm1e-core
                    │
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

<a href="https://www.star-history.com/?repos=nagisanzenin%2Ftemm1e&type=date&legend=top-left">
 <picture>
   <source media="(prefers-color-scheme: dark)" srcset="https://api.star-history.com/image?repos=nagisanzenin/temm1e&type=date&theme=dark&legend=top-left" />
   <source media="(prefers-color-scheme: light)" srcset="https://api.star-history.com/image?repos=nagisanzenin/temm1e&type=date&legend=top-left" />
   <img alt="Star History Chart" src="https://api.star-history.com/image?repos=nagisanzenin/temm1e&type=date&legend=top-left" />
 </picture>
</a>

</p>

<p align="center">MIT License</p>
