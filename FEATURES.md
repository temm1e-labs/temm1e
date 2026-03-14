# TEMM1E Features

> Everything I'm made of. Every module, every capability. 905 tests. 0 clippy warnings. Here's the full inventory.

---

## Phase 0 — The Foundation (because I refuse to be fragile)

| # | Feature | Module | Status |
|---|---------|--------|--------|
| 0.1 | Graceful Shutdown | `src/main.rs` | Done |
| 0.2 | Provider Circuit Breaker | `temm1e-agent/src/circuit_breaker.rs` | Done |
| 0.3 | Channel Reconnection with Backoff | `temm1e-channels/src/telegram.rs` | Done |
| 0.4 | Streaming Responses | `temm1e-agent/src/streaming.rs` | Done |
| 0.5 | Raised max_turns/max_tool_rounds | `temm1e-agent/src/runtime.rs` | Done |

## Phase 1 — My Brain

| # | Feature | Module | Status |
|---|---------|--------|--------|
| 1.1 | Verification Engine | `temm1e-agent/src/runtime.rs` | Done |
| 1.2 | Task Decomposition | `temm1e-agent/src/task_decomposition.rs` | Done |
| 1.3 | Persistent Task Queue with Checkpointing | `temm1e-agent/src/task_queue.rs` | Done |
| 1.4 | Context Manager — Surgical Token Budgeting | `temm1e-agent/src/context.rs` | Done |
| 1.5 | Self-Correction Engine | `temm1e-agent/src/self_correction.rs` | Done |
| 1.6 | DONE Definition Engine | `temm1e-agent/src/done_criteria.rs` | Done |
| 1.7 | Cross-Task Learning | `temm1e-agent/src/learning.rs` | Done |

## Phase 2 — I Keep Myself Alive

| # | Feature | Module | Status |
|---|---------|--------|--------|
| 2.1 | Watchdog | `temm1e-agent/src/watchdog.rs` | Done |
| 2.2 | State Recovery | `temm1e-agent/src/recovery.rs` | Done |
| 2.3 | Health-Aware Heartbeat | `temm1e-automation/src/heartbeat.rs` | Done |
| 2.4 | Memory Backend Failover | `temm1e-memory/src/lib.rs` | Done |

## Phase 3 — Doing More with Less

| # | Feature | Module | Status |
|---|---------|--------|--------|
| 3.1 | Output Compression | `temm1e-agent/src/output_compression.rs` | Done |
| 3.2 | System Prompt Optimization | `temm1e-agent/src/prompt_optimizer.rs` | Done |
| 3.3 | Tiered Model Routing | `temm1e-agent/src/model_router.rs` | Done |
| 3.4 | History Pruning with Semantic Importance | `temm1e-agent/src/history_pruning.rs` | Done |

## Phase 4 — My Reach

| # | Feature | Module | Status |
|---|---------|--------|--------|
| 4.1 | Discord Channel | `temm1e-channels/src/discord.rs` | Done |
| 4.2 | Git Tool | `temm1e-tools/` | Done |
| 4.3 | Skill Registry (TemHub v1) | `temm1e-skills/src/lib.rs` | Done |
| 4.4 | Slack Channel | `temm1e-channels/src/slack.rs` | Done |
| 4.5 | Web Dashboard (Minimal) | `temm1e-gateway/src/dashboard.rs` | Done |

## Phase 5 — Cloud Scale

| # | Feature | Module | Status |
|---|---------|--------|--------|
| 5.1 | S3/R2 FileStore Backend | `temm1e-filestore/src/s3.rs` | Done |
| 5.2 | OpenTelemetry Observability | `temm1e-observable/src/` | Done |
| 5.3 | Multi-Tenancy with Workspace Isolation | `temm1e-core/src/tenant_impl.rs` | Done |
| 5.4 | OAuth Identity Flows | `temm1e-gateway/src/identity.rs` | Done |
| 5.5 | Horizontal Scaling via Orchestrator | `temm1e-core/src/orchestrator_impl.rs` | Done |

## Phase 6 — The Advanced Tem's Mind (where it gets interesting >:3)

| # | Feature | Module | Status |
|---|---------|--------|--------|
| 6.1 | Parallel Tool Execution | `temm1e-agent/src/executor.rs` | Done |
| 6.2 | Agent-to-Agent Delegation | `temm1e-agent/src/delegation.rs` | Done |
| 6.3 | Proactive Task Initiation | `temm1e-agent/src/proactive.rs` | Done |
| 6.4 | Adaptive System Prompt — Self-Tuning | `temm1e-agent/src/prompt_patches.rs` | Done |

## Phase 7 — I Can See

| # | Feature | Module | Status |
|---|---------|--------|--------|
| 7.1 | Vision / Image Understanding | `temm1e-core/src/types/message.rs`, `temm1e-providers/`, `temm1e-agent/src/runtime.rs` | Done |

---

## Feature Details

### 0.1 Graceful Shutdown
I trap SIGTERM/SIGINT, drain my active ChatSlot workers, and flush pending memory writes. Tasks that can't complete within 30s get checkpointed for resume. No silent deaths.

### 0.2 Provider Circuit Breaker
My circuit breaker runs a state machine: Closed, Open (after N failures), Half-Open (after cooldown). I apply exponential backoff with jitter on transient errors (429, 500, 503). When multiple providers are configured, I failover automatically.

### 0.3 Channel Reconnection
I run a supervised retry loop with exponential backoff for Telegram long-poll. I health-check my connection via heartbeat and log every reconnection attempt.

### 0.4 Streaming Responses
`StreamBuffer` + `StreamingConfig` + `StreamingNotifier`. I use `Provider::stream()` for final text responses. On Telegram, I edit messages in-place (throttled at 30 edits/min). During tool rounds, I push status updates so the user knows I'm working.

### 0.5 Raised Limits
`max_turns=200`, `max_tool_rounds=200`, `max_task_duration=1800s`. Configurable via `AgentRuntime::with_limits()`. I don't quit early.

### 1.1 Verification Engine
After every tool execution, I inject a verification hint into the tool result: "Did the action succeed? What evidence confirms this?" Zero API call overhead — prompt injection only. I verify every action.

### 1.2 Task Decomposition
My `TaskGraph` builds `SubTask` nodes with dependency edges. I topologically sort for execution order. Each subtask tracks its own status (Pending/Running/Completed/Failed/Blocked). Cycle detection prevents infinite loops. I break problems into components.

### 1.3 Persistent Task Queue
SQLite-backed `TaskQueue`. My `TaskEntry` stores task_id, chat_id, goal, status, and checkpoint_data (serialized session JSON). After each tool round, I checkpoint my session state. I survive process restarts.

### 1.4 Context Manager
I budget tokens across 7 priority categories: system prompt (always), tool definitions (always), task state (if present), recent 4-8 messages (always), memory search (15% cap), cross-task learnings (5% cap), older history (fill remaining). When I drop messages, I inject summaries so nothing is truly lost.

### 1.5 Self-Correction Engine
My `FailureTracker` counts consecutive failures per tool name. After my threshold (default 2), I inject a strategy rotation prompt: "This approach has failed N times. Try a fundamentally different approach." I don't repeat mistakes.

### 1.6 DONE Definition Engine
I detect compound tasks (multiple verbs, numbered lists, "and"/"then" connectors). I inject DONE criteria for the LLM to articulate verifiable completion conditions. On the final reply, I append a verification reminder. A task isn't done until I can prove it's done.

### 1.7 Cross-Task Learning
My `extract_learnings()` analyzes completed history — tools used, failures, strategy rotations — and produces a `TaskLearning` with task_type, approach, outcome, and lesson. I store these in memory as `LongTerm` entries with `learning:{uuid}` IDs. My context builder searches and injects up to 5 past learnings (5% token budget) into the THINK step of future tasks. Verified working: a shell task produces a learning, and the next session's context allocation shows `learnings=25` tokens injected. I get better over time >:3

### 2.1 Watchdog
I monitor my own subsystems (provider, memory, channel, tools). `WatchdogConfig` sets check intervals and failure thresholds. My `HealthReport` tracks per-subsystem status. When something degrades, I auto-restart it.

### 2.2 State Recovery
My `RecoveryManager` detects corrupted state (broken sessions, orphaned tasks). I generate a `RecoveryPlan` with actions: Restart, Rollback, Skip, Escalate. This integrates with my task queue checkpoints.

### 2.3 Health-Aware Heartbeat
My heartbeat checks subsystem health via the watchdog. I report degraded or failed subsystems and adjust my check interval based on overall system health.

### 2.4 Memory Backend Failover
I automatically fail over from primary to secondary memory backend on failure. The primary/secondary pair is configurable. When my primary recovers, I switch back.

### 3.1 Output Compression
I compress large tool outputs before storing them in context. I extract key information and discard verbose noise. For shell output, I keep the first and last N lines with a summary in between.

### 3.2 System Prompt Optimization
My `SystemPromptBuilder` handles composable prompt construction. I inject workspace path, tool names, file protocol, verification rules, DONE criteria rules, and self-correction rules. Token estimation keeps it tight.

### 3.3 Tiered Model Routing
My `ModelRouter` routes tasks to a `ModelTier` (Fast/Standard/Premium) based on `TaskComplexity` analysis. Simple questions get cheap, fast models. Multi-step tasks get premium models. I spend tokens where they matter.

### 3.4 History Pruning
My `score_message()` assigns `MessageImportance` (Critical/High/Medium/Low) based on role, content, and tool results. `prune_history()` removes the lowest-importance messages first. I preserve conversation coherence.

### 4.1 Discord Channel
Full `Channel` + `FileTransfer` implementation via serenity/poise. I handle slash commands, message splitting, allowlist enforcement, and attachment handling. Behind the `discord` feature flag.

### 4.2 Git Tool
I perform typed git operations: clone, pull, push, commit, branch, diff, log. Safety: I block force-push by default and require explicit confirmation for destructive operations.

### 4.3 Skill Registry
My `SkillRegistry` scans `~/.temm1e/skills/` and the workspace `skills/` directory. I parse YAML frontmatter from Markdown and do keyword-based relevance matching. When a skill is relevant, I inject its instructions into my system prompt.

### 4.4 Slack Channel
`SlackChannel` implementing Channel + FileTransfer. I poll via conversations.list + conversations.history every 2s. I use chat.postMessage and files.upload. Message splitting at 4000 chars, allowlist enforcement, rate limiting. Behind the `slack` feature flag.

### 4.5 Web Dashboard
4 handlers: dashboard_page (HTML), dashboard_health (JSON), dashboard_tasks (JSON), dashboard_config (redacted JSON). HTMX-based, dark theme, under 50KB, polls health every 10s. I serve it at `/dashboard`.

### 5.1 S3/R2 FileStore
My `S3FileStore` uses aws-sdk-s3. I support R2/MinIO (custom endpoint + force_path_style). Multipart upload for `store_stream()`, presigned URLs, paginated listing. Behind the `s3` feature flag.

### 5.2 OpenTelemetry Observability
My `MetricsCollector` uses atomic counters, RwLock gauges, and histograms. `OtelExporter` wraps it with an OTLP endpoint. 6 predefined metrics: provider latency, token usage, tool success rate, and more.

### 5.3 Multi-Tenancy
My `TenantManager` implements the Tenant trait. I enforce per-tenant workspace isolation (workspace/, vault/, memory.db). Rate limiting with day rollover. `ensure_workspace()` creates the isolation directories.

### 5.4 OAuth Identity
My `OAuthIdentityManager` holds an in-memory user store with PKCE support. start_oauth_flow(), complete_oauth_flow(), refresh_token(). I support multiple providers (GitHub, Google, AWS). I send the OAuth URL in chat, the user clicks, the callback hits my gateway, and I store the token.

### 5.5 Horizontal Scaling
My `DockerOrchestrator` uses a DockerClient abstraction. I enforce a max instances safety limit with no privilege escalation. `KubernetesOrchestrator` stub is in place. `create_orchestrator()` factory dispatches the right one.

### 6.1 Parallel Tool Execution
My `execute_tools_parallel()` uses a Semaphore-based concurrency limit (max 5). `detect_dependencies()` uses union-find grouping — read-read operations run independently, write-write and write-read operations are dependent, shell operations are always dependent.

### 6.2 Agent-to-Agent Delegation
My `DelegationManager` uses an `AtomicUsize`-based spawn counter. `plan_delegation()` decomposes tasks via 4 heuristic strategies (numbered lists, semicolons, "then", "and"). Each `SubAgent` gets scoped model/tools/timeout. My sub-agents cannot spawn further sub-agents — no recursion. Max 10 per task, max 3 concurrent.

### 6.3 Proactive Task Initiation
My `ProactiveManager` runs a `TriggerRule` system with 4 trigger types: FileChanged, CronSchedule, Webhook, Threshold. Disabled by default (global opt-in required). I rate-limit to 10 actions/hour with per-rule cooldowns. The `requires_confirmation` flag gates destructive operations.

### 6.4 Adaptive System Prompt
My `PromptPatchManager` handles 5 patch types (ToolUsageHint, ErrorAvoidance, WorkflowPattern, DomainKnowledge, StylePreference). All patches start as Proposed. I only inject Approved patches. Auto-approve kicks in only for low-risk types above 0.8 confidence. Underperforming patches auto-expire. Max 20 patches. I tune myself.

### 7.1 Vision / Image Understanding
`ContentPart::Image` variant with base64 data + media_type. I detect image attachments (JPEG, PNG, GIF, WebP), read from workspace, base64-encode, and include them as image content parts in provider requests. Anthropic format: `{"type": "image", "source": {"type": "base64", ...}}`. OpenAI format: `{"type": "image_url", "image_url": {"url": "data:...;base64,...", "detail": "auto"}}`. My context budgeting estimates ~1000 tokens per image.
