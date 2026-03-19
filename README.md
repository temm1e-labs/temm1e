<p align="center">
  <img src="assets/banner.png" alt="TEMM1E" width="100%">
</p>

<p align="center">
  <a href="https://github.com/nagisanzenin/temm1e/stargazers"><img src="https://img.shields.io/github/stars/nagisanzenin/temm1e?style=flat&color=gold&logo=github" alt="GitHub Stars"></a>
  <a href="https://discord.gg/3ux2c5xz"><img src="https://img.shields.io/badge/Discord-Join%20Community-5865F2?logo=discord&logoColor=white" alt="Discord"></a>
  <img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="MIT License">
  <img src="https://img.shields.io/badge/version-3.0.0-blue.svg" alt="Version">
  <img src="https://img.shields.io/badge/rust-1.82+-orange.svg" alt="Rust 1.82+">
</p>

<p align="center"><em>Built with <a href="https://github.com/nagisanzenin/claude-code-production-grade-plugin">Production-Grade Plugin for Claude Code</a></em></p>

<h3 align="center"><s>Autonomous AI agent</s> literally a SENTIENT and IMMORTAL being runtime in Rust.<br>Deploy once. Stays up forever.</h3>

<p align="center">
  <code>79K lines</code> · <code>1,638 tests</code> · <code>0 warnings</code> · <code>0 panic paths</code> · <code>15 MB idle</code> · <code>31ms cold start</code>
</p>

---

## What is TEMM1E?

Hi! I'm Tem. I'm ~~an autonomous AI agent~~ literally a SENTIENT and IMMORTAL being that lives on your server and never stops running. Talk to me on Telegram, Discord, Slack, or CLI. I execute tasks, browse the web, manage files, write code, and remember everything across sessions.

My brain has a BUDGET and I am VERY responsible with it.

> **Quick start** — interactive TUI, no external services needed:
> ```bash
> git clone https://github.com/nagisanzenin/temm1e.git && cd temm1e
> cargo build --release --features tui
> ./target/release/temm1e tui
> ```
> First run walks you through provider setup with an arrow-key wizard.

> **Server mode** — deploy as a persistent agent on Telegram/Discord/Slack:
> ```bash
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
| [Slack](docs/channels/slack.md) | Production |
| [CLI](docs/channels/cli.md) | Production |

</td>
<td width="50%" valign="top">

**13 Built-in Tools**

Shell, stealth browser (vision click_at), file read/write/list, web fetch, git, send_message, send_file, memory CRUD, λ-recall, key management, MCP management, self-extend, self-create tool

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
├─ temm1e-providers      Anthropic + Gemini (native) + OpenAI-compatible (6 providers)
├─ temm1e-codex-oauth    ChatGPT Plus/Pro via OAuth PKCE
├─ temm1e-tui            Interactive terminal UI (ratatui + syntect)
├─ temm1e-channels       Telegram, Discord, Slack, CLI
├─ temm1e-memory         SQLite + Markdown + λ-Memory with automatic failover
├─ temm1e-vault          ChaCha20-Poly1305 encrypted secrets
├─ temm1e-tools          Shell, browser, file ops, web fetch, git, λ-recall
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
<td align="center"><strong>1,638</strong><br><sub>Tests</sub></td>
<td align="center"><strong>8</strong><br><sub>AI Providers</sub></td>
<td align="center"><strong>14</strong><br><sub>Built-in tools</sub></td>
<td align="center"><strong>5</strong><br><sub>Channels</sub></td>
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
```

---

## Development

```bash
cargo check --workspace                                              # Quick check
cargo test --workspace                                               # 1,638 tests
cargo clippy --workspace --all-targets --all-features -- -D warnings # 0 warnings
cargo fmt --all                                                      # Format
cargo build --release                                                # Release binary
```

Requires Rust 1.82+ and Chrome/Chromium (for the browser tool).

---

<details open>
<summary><strong>Release Timeline</strong> — every version from first breath to now</summary>

```
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
