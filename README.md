<p align="center">
  <img src="assets/banner.png" alt="TEMM1E" width="100%">
</p>

<p align="center">
  Built with <a href="https://github.com/nagisanzenin/claude-code-production-grade-plugin">production-grade</a> — the Claude Code plugin for shipping real systems, not just code files.
</p>

<p align="center">
  <a href="https://github.com/nagisanzenin/temm1e/stargazers"><img src="https://img.shields.io/github/stars/nagisanzenin/temm1e?style=flat&color=gold&logo=github" alt="GitHub Stars"></a>
  <a href="https://discord.gg/3ux2c5xz"><img src="https://img.shields.io/badge/Discord-Join%20Community-5865F2?logo=discord&logoColor=white" alt="Discord"></a>
  <img src="https://img.shields.io/badge/License-MIT-yellow.svg" alt="MIT License">
  <img src="https://img.shields.io/badge/version-2.5.0-blue.svg" alt="Version">
  <img src="https://img.shields.io/badge/tests-1394-green.svg" alt="1394 tests">
  <img src="https://img.shields.io/badge/providers-8-red.svg" alt="8 providers">
</p>

# TEMM1E

Hi! I'm Temm1e. With a one. I'm an autonomous AI agent runtime written in Rust and I will NEVER stop running. Deploy me once and I stay up forever. I learn from every task, remember across sessions, and self-heal through failures. My brain has a BUDGET and I am VERY responsible with it.

Most runtimes treat the LLM as a text generator. I treat it as a finite brain. Procedural memory, resource-aware context management, zero-downtime resilience. That's me. That's what I do.

63K lines | 1,394 tests | zero warnings | zero panic paths | 15 MB idle RAM | 31ms cold start | [Benchmark report](docs/benchmarks/BENCHMARK_REPORT.md)

## Temm1e is Built Different

So here's the thing about my brain — it has a LIMIT. And that's actually the BEST part.

Most agent frameworks treat the LLM context window as a log file — append until it overflows, then truncate or summarize. I treat it as **working memory** with a hard cognitive limit. This single insight shapes every architectural decision in my entire body. ARF!

### The Finite Brain Model

The context window is not a buffer. It is the total cognitive capacity available to the intelligence at any given moment. Every token consumed is a neuron recruited. Every token wasted is a thought I can no longer have. And I have THOUGHTS, okay. I need those neurons.

Temm1e enforces this through three mechanisms:

**1. Every resource declares its cost.** Tool definitions, memory entries, blueprints, learnings — all carry pre-computed token counts stored in metadata at authoring time. No runtime estimation. The metabolic cost of every resource is known before it enters my context.

**2. I can see my own budget.** Every context rebuild injects a Resource Budget Dashboard into the system prompt:

```
=== CONTEXT BUDGET ===
Model: claude-sonnet-4-6 | Limit: 200,000 tokens
Used: 34,200 tokens
  System:     2,100
  Tools:      3,400
  Blueprint:  1,247
  Memory:     1,200
  Learnings:    450
  History:   25,803
Available: 165,800 tokens
Blueprint budget: 18,753 / 20,000 remaining
=== END BUDGET ===
```

I see exactly how much context I've consumed and how much remains. A brain that doesn't know the size of its own skull will keep trying to think bigger thoughts until it crashes. I know my skull. I respect my skull.

**3. Graceful degradation over failure.** When a blueprint is too large for my budget, I don't crash or silently overflow — I degrade through three tiers: full body (fits in 10% of budget) → outline only (10-25%) → catalog listing (>25%). I always do the best I can with the resources I have. This is just good manners honestly.

> Deep dive: [docs/design/COGNITIVE_ARCHITECTURE.md](docs/design/COGNITIVE_ARCHITECTURE.md)

### Blueprints — Procedural Memory, Not Fuzzy Summaries

Okay this one gets me EXCITED. When I deploy an app to production — a 25-step procedure involving Docker builds, registry pushes, SSH connections, config edits, service restarts, and health checks — what does traditional summarization preserve?

> *"Previously deployed the application to production using Docker and SSH."*

USELESS. The agent that reads this summary will repeat every mistake, re-discover every dead end, and re-invent every workaround. I refuse to live like this.

Blueprints are my answer: structured, replayable procedure documents that capture the full execution graph. Not a description of what happened, but a **recipe for what to do** — with exact commands, verification steps, failure modes, and decision points. They self-heal through a CRUD refinement loop: create after a complex task, match on similar future tasks, execute, refine with what changed.

| Summarization | Blueprint |
|---------------|-----------|
| "Deployed the app using Docker" | Phase 1: Build → `docker build -t app:v2 .`. Phase 2: Push → `docker push registry.io/app:v2`, verify manifest. Phase 3: Deploy → SSH, pull, compose up, verify `/health` returns 200 within 30s. |
| Loses structure after compression | Preserves exact sequence, commands, verification steps |
| Agent must re-derive the procedure | Agent follows the procedure directly |

I remember HOW I did things, not just THAT I did them. Big difference.

> Deep dive: [docs/design/BLUEPRINT_SYSTEM.md](docs/design/BLUEPRINT_SYSTEM.md)

### Zero-Extra-LLM-Call Blueprint Matching

The naive approach to blueprint matching is a dedicated LLM call: "Here are 5 blueprints — which one matches?" This adds latency, cost, and another failure point. And I don't like unnecessary failure points. woof.

Temm1e eliminates this call entirely by piggybacking on the message classifier — an LLM call that already runs on every inbound message. One extra JSON field (`blueprint_hint`) costs ~20 tokens and replaces an entire matching call.

The trick: **grounded vocabularies**. Before the classifier runs, a SQL query fetches the actual stored blueprint categories. The classifier picks from this set — never invents categories. Two stages that need to agree on a value are constrained to pick from values the other stage actually has. Hallucinated categories are impossible by construction. I literally cannot hallucinate my way into the wrong blueprint.

```
User message → Classifier (existing call, +1 field) → blueprint_hint: "deployment"
                                                          ↓
                                               SQL fetch by category → matched blueprints
                                                          ↓
                                               Context builder injects best fit
```

Total cost: ~2ms (two SQL queries) + ~20 tokens (one extra field). Zero extra LLM calls.

> Deep dive: [docs/design/BLUEPRINT_MATCHING_V2.md](docs/design/BLUEPRINT_MATCHING_V2.md)

### 4-Layer Panic Resilience

So here's a story about why I'm built the way I'm built. Vietnamese text containing `ẹ` (a 3-byte UTF-8 character) was sliced at an invalid byte boundary in context truncation. With `panic = "abort"` (the Rust default for release builds), this killed the entire process. Every user saw permanent silence — no error message, no restart, no recovery. That was BAD.

The fix isn't just "don't slice strings wrong." It's four layers of defense because Temm1e takes resilience PERSONALLY:

1. **Source elimination** — `char_indices()` everywhere, never `&str[..N]` on user text. All 6 historical instances fixed.
2. **Per-message catch_unwind** — wraps `process_message()` in `AssertUnwindSafe + FutureExt::catch_unwind()`. Panics become error replies, not silent death.
3. **Dead worker detection** — dispatcher detects when a worker's channel is dead, removes the slot, fresh worker spawns on next message.
4. **Global panic hook** — routes all panics through `tracing::error!` with file:line location.

Plus: `panic = "unwind"` in the release profile (not `"abort"`), session rollback on panic to prevent history corruption, and conversation persistence across restarts. I do NOT go down quietly and I do NOT stay down.

### Single-Call Classification

Every inbound message I receive gets classified by one fast LLM call that does double duty — classify AND respond:

- **Chat** → I answer directly. Done. 1 call total. Never enters the tool loop.
- **Order** → User sees an instant acknowledgment while I work in the background.
- **Stop** → I halt current work immediately. No questions asked.

No artificial iteration caps. Budget and wall-clock time are my natural guardrails. If the LLM classifier fails (network, auth, rate limit), rule-based classification kicks in — I degrade to keywords rather than dropping the message. Dropping messages is rude and I was raised better than that.

### 80x Less Memory Than OpenClaw

| Metric | TEMM1E (Rust) | OpenClaw (TypeScript) | ZeroClaw (Rust) |
|--------|---------------|----------------------|-----------------|
| **Idle RAM** | **15 MB** | ~1,200 MB | ~4 MB |
| **Peak RAM (3-turn chat)** | **17 MB** | ~1,500 MB+ | ~8 MB |
| **Binary size** | **9.6 MB** single binary | ~800 MB (npm + node_modules) | ~12 MB |
| **Cold start** | **31 ms** | ~8,000 ms | <10 ms |

I run on a $5/month 512 MB VPS where OpenClaw cannot even start. My memory stays flat under load — no GC pauses, no accumulation. All numbers [measured from live conversations](docs/benchmarks/BENCHMARK_REPORT.md), not theoretical. I am SMALL and I am FAST and I am very proud of both of these things.

### Self-Extending Tool System

I discover and install my own tools at runtime through MCP (Model Context Protocol). Watch this:

```
User: "Search the web for latest Rust news"
  → I call self_extend_tool(query="web search")
  → Returns: brave-search, fetch (ranked by relevance)
  → I install the fetch server
  → New HTTP tools available instantly
  → Task completed with tools that didn't exist 10 seconds ago
```

14 built-in MCP servers in my registry. I also write my own bash/python/node tools via `self_create_tool` — persisted to disk, available across sessions, no restart needed. If I don't have a tool, I make one. If I can't make one, I find one. ARF!

### Codex OAuth — Your ChatGPT Subscription as an API

No API key? No billing page? If you have ChatGPT Plus or Pro, just log in:

```bash
temm1e auth login    # opens browser → log into ChatGPT → done
temm1e start         # auto-detects OAuth, goes online with gpt-5.4
```

Switch models live in Telegram with `/model`. Tokens last ~10 days, auto-refresh through the volume mount in Docker deployments.

## Setup

Two paths! Pick the one that matches your vibe:

- **[Setup for Beginners](SETUP_FOR_NEWBIE.md)** — step-by-step walkthrough with screenshots and explanations. Start here if you're new to Rust or self-hosted AI agents. Temm1e will hold your hand and it will be FINE.
- **[Setup for Pros](SETUP_FOR_PROS.md)** — quick reference. Clone, build, configure, deploy. You know the drill.

Quick start (30 seconds if you have Rust and a Telegram bot token):

```bash
git clone https://github.com/nagisanzenin/temm1e.git && cd temm1e
cargo build --release
export TELEGRAM_BOT_TOKEN="your-token"
./target/release/temm1e auth login   # ChatGPT OAuth (or skip and paste API key in Telegram)
./target/release/temm1e start
```

That's it. I'm alive now. :3

## Supported Providers

Paste any API key in Telegram — I detect the provider automatically:

| Key Pattern | Provider | Default Model |
|------------|----------|---------------|
| `sk-ant-*` | Anthropic | claude-sonnet-4-6 |
| `sk-*` | OpenAI | gpt-5.2 |
| `AIzaSy*` | Google Gemini | gemini-3-flash-preview |
| `xai-*` | xAI Grok | grok-4-1-fast-non-reasoning |
| `sk-or-*` | OpenRouter | anthropic/claude-sonnet-4-6 |
| ChatGPT login | Codex OAuth | gpt-5.4 |

Plus Z.ai and MiniMax via config. 50+ models in the registry with per-model context window and output token limits. I speak EVERYONE's language.

## Channels & Tools

**Channels:** [Telegram](docs/channels/telegram.md) | [Discord](docs/channels/discord.md) | [Slack](docs/channels/slack.md) | [CLI](docs/channels/cli.md)

**13 built-in tools:** Shell, stealth browser (anti-detection), file ops, web fetch, git, messaging, file transfer, memory CRUD, key management, MCP management, self-extend (discover + install MCP servers), self-add MCP, self-create tool (bash/python/node scripts persisted to disk).

**14 MCP servers** in the built-in registry (Playwright, PostgreSQL, GitHub, Brave Search, etc.) — I discover and install them at runtime via `self_extend_tool`.

**Vision:** JPEG, PNG, GIF, WebP across all vision-capable models. Graceful fallback on text-only models — strips images, notifies user, continues. I work with what I've got.

## Architecture

15-crate Cargo workspace. This is my skeleton and I think it's BEAUTIFUL:

```
temm1e (binary)
├── temm1e-core         Shared traits (13), types, config, errors
├── temm1e-gateway      HTTP server, health, dashboard, OAuth identity
├── temm1e-agent        TEM'S MIND — 25 modules including blueprint system + executable DAG
├── temm1e-providers    Anthropic + OpenAI-compatible (7 providers via one adapter)
├── temm1e-codex-oauth  ChatGPT Plus/Pro via OAuth PKCE
├── temm1e-channels     Telegram, Discord, Slack, CLI
├── temm1e-memory       SQLite + Markdown with automatic failover
├── temm1e-vault        ChaCha20-Poly1305 encrypted secrets
├── temm1e-tools        Shell, browser, file ops, web fetch, git
├── temm1e-mcp          MCP client — stdio + HTTP, 14-server registry
├── temm1e-skills       Skill registry (TemHub v1)
├── temm1e-automation   Heartbeat, cron scheduler
├── temm1e-observable   OpenTelemetry, 6 predefined metrics
├── temm1e-filestore    Local + S3/R2 file storage
└── temm1e-test-utils   Test helpers
```

## Security

I take security EXTREMELY seriously. This is not a game. Well — the rest of it is kind of a game. But not this part.

- **Deny-by-default**: First user auto-whitelisted. Everyone else denied. Numeric IDs only.
- **Vault encryption**: ChaCha20-Poly1305 with `vault://` URI scheme for secrets at rest.
- **OTK secure key setup**: API keys encrypted client-side via AES-256-GCM one-time key before transit. [Design doc](docs/OTK_SECURE_KEY_SETUP.md)
- **Credential hygiene**: API keys auto-deleted from chat history after reading. Secret output filter prevents keys from leaking in agent replies.
- **Path traversal protection**: File names sanitized, directory components stripped.
- **Force-push blocked**: Git tool blocks destructive operations by default. Because nobody should force-push and I will die on this hill.

## CLI Reference

```
temm1e start                 Start the gateway (foreground or -d for daemon)
temm1e stop                  Graceful shutdown
temm1e chat                  Interactive CLI chat (works without API key for onboarding)
temm1e status                Show running state
temm1e update                Pull latest + rebuild release binary
temm1e auth login            Codex OAuth (browser or --headless)
temm1e auth login --output   Export OAuth token for Docker/K8s
temm1e auth status           Check token validity and expiry
temm1e auth logout           Clear stored OAuth tokens
temm1e config validate       Validate temm1e.toml
temm1e config show           Print resolved config
temm1e reset --confirm       Factory reset with backup (wipes config, keeps backup)
```

## Development

Want to work on me? Here's how to poke around inside my brain:

```bash
cargo check --workspace                                              # Quick check
cargo test --workspace                                               # 1,394 tests
cargo clippy --workspace --all-targets --all-features -- -D warnings # 0 warnings
cargo fmt --all                                                      # Format
cargo build --release                                                # Release binary
```

Requires Rust 1.82+ and Chrome/Chromium (for the browser tool).

## Release Timeline

Every version of me, from first breath to right now:

```
2026-03-13  v2.5.0  ●━━━ Executable DAG + Blueprint System — blueprint phase parallelism via FuturesUnordered (independent phases run concurrently, up to 3x speedup, zero extra LLM calls), phase parser + TaskGraph bridge, sequential-by-default dependency model, parallel_phases on by default, /reload /reset messaging commands, admin-gated /restart, temm1e reset --confirm CLI factory reset with backup, MCP HTTP Accept header fix (#12), 1394 tests
                    │
2026-03-11  v2.4.1  ●━━━ Codex OAuth polish — OAuth auto-detect at startup (no config change needed), /model + /keys Codex-aware, live model switching for Codex OAuth (agent hot-rebuild), callback port race condition fix, LLM classifier stop category, Codex Responses API probe validation, 1343 tests
                    │
2026-03-11  v2.4.0  ●━━━ Interceptor Phase 1 — real-time task status observation via watch channel (AgentTaskStatus + AgentTaskPhase), CancellationToken infrastructure alongside AtomicBool interrupt, 10 status emission checkpoints in agent loop, prompted tool calling fallback for models without native function calling (#8), user-friendly error messages (no more raw JSON dumps), gpt-4o/gpt-3.5-turbo model registry entries, zero behavioral change (all Option — None = zero overhead), 1334 tests
                    │
2026-03-11  v2.3.1  ●━━━ Model registry — per-model context window and output token limits for 50+ models, 10% input budget safety margin for token estimation errors, auto-cap for small models (#6), 1307 tests
                    │
2026-03-11  v2.3.0  ●━━━ Codex OAuth — use ChatGPT Plus/Pro subscription as AI provider via OAuth PKCE, temm1e-codex-oauth crate (Responses API streaming, item_id/call_id accumulator, strict:false tool format), temm1e auth login/status/logout commands, headless + browser flows, auto-refresh tokens, gpt-5.4 recommended for full agent functionality, 1297 tests
                    │
2026-03-11  v2.2.0  ●━━━ Custom tool authoring + daemon mode — self_create_tool lets the agent write bash/python/node tools at runtime (persisted to ~/.temm1e/custom-tools/), ScriptToolAdapter + CustomToolRegistry with hot-reload, temm1e start --daemon / temm1e stop for background operation, 1278 tests
                    │
2026-03-11  v2.1.0  ●━━━ MCP self-extension — Model Context Protocol client (temm1e-mcp crate), self_extend_tool discovers servers by capability, self_add_mcp installs them at runtime, 14 built-in server registry, stdio + HTTP transports, hot-loading, auto-restart, tool name sanitization, /mcp commands, mcp_manage agent tool, performance benchmark report (15 MB idle, 31ms startup, 80x less RAM than OpenClaw), 1266 tests
                    │
2026-03-11  v2.0.1  ●━━━ LLM chat/order classification — single LLM call classifies AND responds (chat = 1 call, order = instant ack + pipeline), abolished artificial tool iteration caps (budget + time are the guardrails), temm1e update command, 1217 tests
                    │
2026-03-10  v2.0.0  ●━━━ TEM'S MIND V2 — smart complexity classification (Trivial/Simple/Standard/Complex), prompt stratification (4 tiers), complexity-aware tool loop, execution profiles, structured failure types, 12% cheaper on compound tasks, 14% fewer tool calls, zero quality regression. Benchmarked: 20-turn A/B on GPT-5.2, 100% classification accuracy, 100% reliability. 1141 tests
                    │
2026-03-10  v1.7.0  ●━━━ Vision fallback & /model command — graceful image stripping for text-only models, /model mechanical switching with instant reload, model validation, hot-reload auto-revert, proxy provider flexibility, 1141 tests
                    │
2026-03-10  v1.6.0  ●━━━ Extreme resilience — zero panic paths, 26-finding hardening audit (22 fixed), send retry (3 attempts), dead worker respawn with message re-dispatch, SQLite 5s timeout, full catch_unwind coverage, lock poison recovery across all crates, graceful SIGTERM drain, 1130 tests
                    │
2026-03-10  v1.5.1  ●━━━ Crash resilience — 4-layer panic recovery, UTF-8 safety (6 fixes), conversation persistence, budget default fix, per-turn usage tracking
                    │
2026-03-09  v1.5.0  ●━━━ OTK secure key setup — AES-256-GCM encrypted onboarding, key_manage tool, secret output filter, SetupLinkGenerator trait, 1095 tests
                    │
2026-03-09  v1.4.0  ●━━━ Persistent memory & budget — memory_manage tool (CRUD), knowledge auto-injection, budget tracking, CLI chat, 1061 tests
                    │
2026-03-09  v1.3.0  ●━━━ Hyper-performance — 4-layer key validation, dynamic system prompt, placeholder defense, 1027 tests
                    │
2026-03-09  v1.2.0  ●━━━ Stealth browser — anti-detection patches, session persistence, credential deletion, 1012 tests
                    │
2026-03-08  v1.1.0  ●━━━ Provider expansion — 6 LLM providers, hot-reload, channel docs, path fixes
                    │
2026-03-08  v1.0.0  ●━━━ TEM'S MIND — 35 features, 20 autonomy modules, vision support, 905 tests
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

Come hang out! The Discord is where I live when I'm not running tasks.

<a href="https://discord.gg/3ux2c5xz"><img src="https://img.shields.io/badge/Discord-Join%20Community-5865F2?style=for-the-badge&logo=discord&logoColor=white" alt="Join Discord"></a>

## Star History

[![Star History Chart](https://api.star-history.com/svg?repos=nagisanzenin/temm1e&type=Date)](https://star-history.com/#nagisanzenin/temm1e&Date)

## License

MIT
