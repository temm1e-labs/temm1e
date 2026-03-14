# TEMM1E Roadmap

> Every item is measured against the [Five Pillars](./VISION.md). Nothing ships without clear user value.

**Rating scales:**
- **User Value**: Who benefits and how much. `CRITICAL` → blocks core use. `HIGH` → daily impact. `MEDIUM` → notable improvement. `LOW` → nice-to-have.
- **Innovation Score**: 1–10. How novel is this relative to existing agent runtimes (OpenClaw, ZeroClaw, OpenHands, etc.)?
- **Risk**: `LOW` → well-understood, isolated change. `MEDIUM` → cross-cutting or requires design decisions. `HIGH` → architectural, may need iteration. `CRITICAL` → could destabilize existing functionality.

**Current state:** TEMM1E is a production-ready single-instance Telegram agent with Anthropic/OpenAI providers, 8 tools, SQLite/Markdown memory, heartbeat, and local vault. The Tem's Mind runs ORDER → THINK → ACTION but lacks explicit VERIFY, task persistence, and self-correction. ~10.6K LOC Rust.

---

## Phase 0 — Harden the Foundation ✓ COMPLETE (2026-03-08)

*Pillar: Robustness. Fix what exists before building what doesn't.*

### 0.1 Graceful Shutdown with In-Flight Task Completion ✓ DONE (2026-03-08)

The process currently dies hard on SIGTERM. Active agent tasks are abandoned mid-execution.

- Trap SIGTERM/SIGINT, drain active `ChatSlot` workers, flush pending memory writes, then exit.
- Tasks that cannot complete within a timeout (30s) are checkpointed to disk for Phase 1.3 resume.

| Metric | Rating |
|--------|--------|
| **User Value** | `CRITICAL` — Users lose in-progress work on every deploy or restart. Deployed via systemd, so restarts happen regularly. |
| **Innovation** | 2/10 — Standard practice, but essential. |
| **Risk** | `LOW` — Isolated to `main.rs` signal handling and `ChatSlot` drain logic. No architectural change. |
| **Pillar** | Robustness |

### 0.2 Provider Circuit Breaker and Failover ✓ DONE (2026-03-08)

When Anthropic is down, the agent loops on errors until `max_tool_rounds` is exhausted. No fallback, no backoff.

- Implement circuit breaker (closed → open after N failures → half-open after cooldown).
- If multiple providers are configured, fail over to the next. If none available, notify user and pause.
- Exponential backoff with jitter on transient errors (429, 500, 503).

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Provider outages currently produce a stream of error messages. Users think the bot is broken. |
| **Innovation** | 3/10 — Standard resilience pattern, but rare in single-binary agent runtimes. |
| **Risk** | `MEDIUM` — Touches provider layer and agent runtime. Needs careful state management for the circuit. |
| **Pillar** | Robustness, Autonomy |

### 0.3 Channel Reconnection with Backoff ✓ DONE (2026-03-08)

If the Telegram long-poll connection drops (network blip, API throttle), the channel dies silently. The process stays alive but the bot is deaf.

- Wrap `teloxide::repl` in a supervised retry loop with exponential backoff.
- Health-check the connection periodically (piggyback on heartbeat).
- Log reconnection attempts. Alert user on persistent failure via alternative channel if configured.

| Metric | Rating |
|--------|--------|
| **User Value** | `CRITICAL` — A network blip silently kills the bot. User has no idea until they notice it's not responding. |
| **Innovation** | 2/10 — Basic reliability. |
| **Risk** | `LOW` — Scoped to `telegram.rs`. The retry wrapper is straightforward. |
| **Pillar** | Robustness |

### 0.4 Streaming Responses to User ✓ DONE (2026-03-08)

The agent currently buffers the entire response and sends it at the end. For long-running tasks with many tool rounds, the user sees nothing for minutes.

- Use `Provider::stream()` (already implemented but unused) for the final text response.
- Send incremental Telegram messages (edit-in-place or chunked sends) as tokens arrive.
- For tool-use rounds, send brief status updates: "Running shell command..." → "Reading file..." → "Analyzing output..."

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Users currently stare at a blank chat for 30-120 seconds. Perceived responsiveness is a trust signal. |
| **Innovation** | 4/10 — Streaming is common in web UIs but rare in messaging bot agents. Edit-in-place on Telegram is a nice touch. |
| **Risk** | `MEDIUM` — Telegram's edit API has rate limits (30 edits/min). Needs throttling logic. |
| **Pillar** | Elegance |

### 0.5 Raise `max_tool_rounds` and `max_turns` Defaults ✓ DONE (2026-03-08)

Current limits: `max_turns=6`, `max_tool_rounds=50`. The Vision demands no task too long.

- Raise `max_turns` default to 200. Raise `max_tool_rounds` to 200.
- Add a configurable `max_task_duration` (default: 30 minutes) as the real safety valve instead.
- The agent should hit time limits, not arbitrary round limits.

| Metric | Rating |
|--------|--------|
| **User Value** | `CRITICAL` — A 6-turn limit means the agent gives up on any multi-step task. Directly violates Pillar I (Autonomy). |
| **Innovation** | 5/10 — Most agent runtimes cap at 10-25 turns. 200 with time-based cutoff is a design statement. |
| **Risk** | `LOW` — Config change. The real risk is token cost, but that's the user's decision, not the agent's. |
| **Pillar** | Autonomy, Brutal Efficiency |

---

## Phase 1 — The Tem's Mind

*Pillar: Tem's Mind, Autonomy. This is the heart of TEMM1E. Everything else serves it.*

### 1.1 Verification Engine — Explicit Post-Action Verification ✓ DONE (2026-03-08)

The current loop is ORDER → THINK → ACTION → THINK → ACTION. There is no explicit VERIFY step. The agent assumes tool outputs are correct and moves on.

- After each tool execution, inject a structured verification prompt: "The tool returned [output]. Does this confirm the action succeeded? What evidence supports success or failure?"
- Verification should be lightweight — a single reasoning step, not a full tool round.
- On verification failure, the agent must re-plan, not retry blindly.

| Metric | Rating |
|--------|--------|
| **User Value** | `CRITICAL` — Without verification, the agent builds on failed steps. A failed `git push` followed by "deployment complete" is worse than no automation. |
| **Innovation** | 8/10 — Most agent runtimes trust tool output implicitly. Structured verification is a significant differentiator. Closest analogy: formal proof assistants, but applied to LLM agents. |
| **Risk** | `MEDIUM` — Adds latency (one extra reasoning step per action). Must be tuned to avoid verification of trivial actions (e.g., reading a file). Token cost increases ~20-30%. |
| **Pillar** | Tem's Mind |

### 1.2 Task Decomposition — Compound Orders to Task Graphs ✓ DONE (2026-03-08)

Currently every user message is one task. "Deploy the app, run migrations, verify health, and send me the logs" is treated as a single prompt, handled in a single agent loop.

- Parse compound orders into a directed acyclic graph (DAG) of sub-tasks.
- Each sub-task has: goal, success criteria, dependencies, status.
- Sub-tasks execute in dependency order. Independent sub-tasks can run concurrently (future: Phase 1.6).
- The user can query task status: "What's the progress?"

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Users naturally give compound orders. Decomposition makes the agent reliable on complex work. |
| **Innovation** | 7/10 — Few runtimes decompose into DAGs. Most use flat chains. DAG decomposition with LLM-driven planning is cutting edge. |
| **Risk** | `HIGH` — Task graph management is complex. DAG cycles, failed dependencies, partial completion — all need handling. Start simple: linear decomposition first, DAG later. |
| **Pillar** | Tem's Mind, Autonomy |

### 1.3 Persistent Task Queue with Checkpointing ✓ DONE (2026-03-08)

Tasks currently live in memory. Process restart = all tasks lost. The Vision demands tasks survive crashes.

- Persist task queue to SQLite: task ID, goal, status, checkpoint data, created_at, updated_at.
- On restart, load incomplete tasks and resume from last checkpoint.
- Checkpoint after each successful tool round: save conversation state, task progress, and pending sub-tasks.
- Users can list tasks: `/tasks` → shows active, queued, completed.

| Metric | Rating |
|--------|--------|
| **User Value** | `CRITICAL` — A deploy that restarts TEMM1E currently kills all running tasks. Users must re-issue orders. Directly violates Pillar II (Robustness). |
| **Innovation** | 6/10 — Process-level task persistence is uncommon in agent runtimes. Most assume ephemeral sessions. |
| **Risk** | `HIGH` — Serializing/deserializing agent state (conversation history, tool context, pending messages) is non-trivial. Schema must handle version migration. |
| **Pillar** | Robustness, Tem's Mind |

### 1.4 Context Manager — Surgical Token Budgeting ✓ DONE (2026-03-08)

The current context builder loads full conversation history up to `max_context_tokens` (30K). No prioritization — oldest messages first, truncate at limit.

- Implement token budgeting: allocate tokens by category (system prompt, task state, relevant history, tool declarations, recent conversation).
- Prioritize: current task state > recent messages > relevant memory search results > older history.
- Summarize dropped context: instead of truncating, generate a 1-paragraph summary of what was pruned.
- Track actual token usage per request and report in logs.

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Better context management means the agent remembers what matters and forgets what doesn't. Directly improves task success rate on long sessions. |
| **Innovation** | 7/10 — Most runtimes use simple sliding window. Priority-based budgeting with summarization is advanced. |
| **Risk** | `MEDIUM` — Summarization costs tokens itself. Need a fast/cheap model for summaries or a heuristic approach. Token counting must match the provider's tokenizer. |
| **Pillar** | Brutal Efficiency, Tem's Mind |

### 1.5 Self-Correction — Retry with Alternative Strategy ✓ DONE (2026-03-08)

When a tool fails, the agent currently retries the same approach or gives up. There is no explicit "try a different way" logic.

- After N failures (configurable, default: 2) on the same approach, the agent must generate alternative strategies.
- Prompt pattern: "Approach X failed N times. List 3 alternative approaches, then execute the most promising one."
- Track failed approaches in task state to prevent infinite loops.
- Ultimate fallback: ask the user for guidance (but only after exhausting alternatives).

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Users currently see the agent bang its head against the same wall. Self-correction turns failures into solved problems. |
| **Innovation** | 8/10 — Explicit strategy rotation with failure memory is rare. Most runtimes rely on the LLM's implicit reasoning, which often loops. |
| **Risk** | `MEDIUM` — Risk of the agent trying worse strategies. Needs a "strategy quality" heuristic. Token cost increases on failure paths. |
| **Pillar** | Autonomy, Tem's Mind |

### 1.6 DONE Definition Engine ✓ DONE (2026-03-08)

The Vision states: "DONE is not a feeling. It is a measurable state." Currently, the agent decides it's done when it runs out of tool calls or produces a text response.

- Before executing a compound task, the agent must articulate DONE criteria: "This task is complete when: [list of verifiable conditions]."
- After the agent declares DONE, run a verification pass against each criterion.
- If any criterion fails, loop back to THINK.
- Report DONE to user with evidence: "Task complete. Verified: [criterion 1] ✓, [criterion 2] ✓, ..."

| Metric | Rating |
|--------|--------|
| **User Value** | `CRITICAL` — Users need to trust that "done" means done. Currently, the agent sometimes declares success on partially completed work. |
| **Innovation** | 9/10 — Explicit, verifiable DONE criteria with automated checking is genuinely novel. No mainstream agent runtime does this. |
| **Risk** | `MEDIUM` — DONE criteria generation is LLM-dependent. Poorly defined criteria lead to false positives or infinite loops. Needs prompt engineering iteration. |
| **Pillar** | Tem's Mind |

### 1.7 Cross-Task Learning ✓ DONE (2026-03-08)

The agent currently has no memory across tasks beyond raw conversation history. It doesn't learn from past successes or failures.

- After each completed task, extract and persist: what worked, what failed, what tools were useful, what approaches to avoid.
- On new tasks, query memory for similar past tasks and inject relevant learnings into context.
- Learning entries are structured: `{task_type, approach, outcome, lesson, timestamp}`.
- Decay old learnings that haven't been useful.

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — The agent gets smarter over time. The 10th deployment is faster than the 1st. Users feel the agent "knows their setup." |
| **Innovation** | 8/10 — Structured learning from execution history is research-grade. Most agents start fresh every conversation. |
| **Risk** | `MEDIUM` — Stale or wrong learnings can degrade performance. Needs a relevance scoring mechanism and a way for users to clear bad learnings. |
| **Pillar** | Tem's Mind, Elegance |

---

## Phase 2 — Self-Healing Infrastructure

*Pillar: Robustness. The system must run indefinitely without human intervention.*

### 2.1 Process Supervisor / Watchdog ✓ DONE (2026-03-08)

TEMM1E currently relies on systemd for restart-on-crash. There is no internal self-monitoring.

- Implement an internal watchdog thread that monitors:
  - Agent loop liveness (has it produced output in the last N minutes?)
  - Memory backend connectivity (can we write/read?)
  - Provider reachability (last successful API call timestamp)
  - Channel health (is the Telegram poller alive?)
- On anomaly: attempt internal recovery first (reconnect, restart subsystem). If recovery fails, trigger clean shutdown for systemd to restart.
- Log all health transitions to memory for post-incident review.

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Silent failures are the worst kind. A zombie process that's alive but not working is harder to debug than a crash. Watchdog catches these. |
| **Innovation** | 5/10 — Watchdog patterns are well-established. Applying them inside a single-binary agent is a solid engineering choice, not novel. |
| **Risk** | `LOW` — Watchdog is a separate thread/task. Minimal coupling. Main risk: false positives triggering unnecessary restarts. |
| **Pillar** | Robustness |

### 2.2 State Recovery from Durable Storage ✓ DONE (2026-03-08)

If the process dies mid-task, all in-memory state is lost: active tasks, pending messages, conversation context.

- Depends on Phase 1.3 (Persistent Task Queue).
- On startup: check for incomplete tasks in SQLite, load their checkpoints, notify the user: "I restarted. Resuming task: [description]."
- Recover `ChatSlot` worker state: which chats were active, what was the last message.
- Idempotency: ensure resumed tasks don't re-execute completed steps (use step IDs in checkpoints).

| Metric | Rating |
|--------|--------|
| **User Value** | `CRITICAL` — Combined with 0.1 and 1.3, this completes the "crash and recover" promise. Users issue an order and it gets done regardless of process lifecycle. |
| **Innovation** | 7/10 — Agent state recovery across process boundaries is rare. Most runtimes treat sessions as ephemeral. |
| **Risk** | `HIGH` — State deserialization across code versions is fragile. Checkpoint format needs versioning and migration. Edge cases: what if the external world changed during downtime? |
| **Pillar** | Robustness, Autonomy |

### 2.3 Heartbeat Evolution — Health-Aware Heartbeat ✓ DONE (2026-03-08)

The current heartbeat reads `HEARTBEAT.md` and sends it to the agent. It doesn't check system health or adapt its behavior.

- Before running the heartbeat prompt, check subsystem health (provider, memory, channel, tools).
- If unhealthy: run recovery procedures instead of the heartbeat task.
- Heartbeat prompt should include system health summary so the agent can self-diagnose.
- Add heartbeat metrics: last run, last success, failure count, recovery actions taken.

| Metric | Rating |
|--------|--------|
| **User Value** | `MEDIUM` — Users don't see the heartbeat directly, but a health-aware heartbeat prevents silent degradation. |
| **Innovation** | 6/10 — Self-diagnosing heartbeats are an elegant extension of the existing system. |
| **Risk** | `LOW` — Additive to existing heartbeat. No breaking changes. |
| **Pillar** | Robustness, Tem's Mind |

### 2.4 Memory Backend Failover ✓ DONE (2026-03-08)

If SQLite is corrupted or the file is locked, memory operations fail and the agent loses context.

- Detect memory backend failures (write errors, read timeouts, corruption signals).
- Fall back to in-memory cache for immediate operations.
- Attempt repair (SQLite: `PRAGMA integrity_check`, rebuild index).
- If repair fails: create a new database, notify user, preserve what can be salvaged.

| Metric | Rating |
|--------|--------|
| **User Value** | `MEDIUM` — SQLite corruption is rare but catastrophic when it happens. Recovery prevents total memory loss. |
| **Innovation** | 4/10 — Database failover is standard. The single-binary context makes it slightly novel. |
| **Risk** | `MEDIUM` — Silently switching to in-memory cache could cause data loss if the user doesn't notice. Must be loud about it. |
| **Pillar** | Robustness |

---

## Phase 3 — Brutal Efficiency ✓ COMPLETE (2026-03-08)

*Pillar: Brutal Efficiency. Maximum quality at minimum resource cost.*

### 3.1 Adaptive Tool Output Compression ✓ DONE (2026-03-08)

Tool outputs are currently truncated at 30KB. A 29KB log dump is sent raw to the provider, wasting context.

- After tool execution, assess output relevance: is the full output needed or can it be summarized?
- For large outputs (>2KB): extract key information (error lines, status codes, relevant sections) and compress to a summary.
- Use heuristics first (regex for errors, exit codes, structured data extraction). Fall back to LLM summarization only for unstructured text.
- Preserve raw output in memory for the agent to request if needed: "Show me the full output of the last command."

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Compressed outputs mean the agent can handle more tool rounds before hitting context limits. More rounds = more complex tasks completed. |
| **Innovation** | 7/10 — Most runtimes do naive truncation. Intelligent compression with retrievable raw output is a meaningful advance. |
| **Risk** | `MEDIUM` — Over-compression loses critical information. Under-compression wastes tokens. The heuristic needs tuning per tool type. |
| **Pillar** | Brutal Efficiency, Tem's Mind |

### 3.2 System Prompt Optimization ✓ DONE (2026-03-08)

The system prompt is currently a static string. It may contain redundant instructions, unused tool descriptions, or verbose formatting.

- Audit current system prompt for token waste. Measure baseline token count.
- Compress without quality loss: remove redundant phrasing, use terse instruction format, conditional tool descriptions (only include tools that are enabled).
- A/B test compressed vs verbose prompts on a task suite. Measure: task success rate, token usage, response quality.
- Target: 40% token reduction in system prompt with zero quality loss.

| Metric | Rating |
|--------|--------|
| **User Value** | `MEDIUM` — Users don't see the system prompt, but fewer system tokens = more context for their tasks. Directly reduces API cost. |
| **Innovation** | 5/10 — Prompt compression is a known technique but rarely applied systematically with measurement. |
| **Risk** | `LOW` — System prompt changes are easy to A/B test and revert. |
| **Pillar** | Brutal Efficiency |

### 3.3 Tiered Model Routing ✓ DONE (2026-03-08)

Every request — from "read this file" to "architect a distributed system" — goes to the same model (e.g., Claude Sonnet 4.6). Wasteful.

- Classify tasks by complexity: simple (file reads, status checks) → fast/cheap model; complex (architecture, debugging, multi-step) → capable model.
- Classification is done by the context manager, not an LLM call (rule-based: number of tools, task description length, history depth).
- User can override: "use the best model for this."
- Verification steps (Phase 1.1) always use the primary model.

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — 60-80% of agent operations are simple tool dispatches. Using a cheaper model for those saves significant API cost without quality loss. |
| **Innovation** | 6/10 — Multi-model routing exists in some platforms but is rarely done at the agent runtime level with task-aware classification. |
| **Risk** | `MEDIUM` — Wrong classification sends complex tasks to a weak model. Needs a conservative default (if in doubt, use the good model). |
| **Pillar** | Brutal Efficiency |

### 3.4 Conversation History Pruning with Semantic Importance ✓ DONE (2026-03-08)

History is currently kept on a sliding window (oldest dropped first). Important decisions made early in a session are lost.

- Score each message in history by semantic importance: decisions > errors > tool results > status updates > greetings.
- When pruning, drop low-importance messages first regardless of age.
- Summarize pruned segments into a "session context" block that persists.
- Keep all user messages (never prune the user's words without summarization).

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — The agent remembers important decisions even in long sessions. Users don't need to repeat themselves. |
| **Innovation** | 7/10 — Importance-based pruning is discussed in research but rarely implemented in production agents. |
| **Risk** | `MEDIUM` — Importance scoring is subjective. A bad scorer drops critical context. Needs conservative defaults. |
| **Pillar** | Brutal Efficiency, Tem's Mind |

### 3.5 Binary and Startup Optimization ✓ DONE (2026-03-08)

Current release binary: ~6.9 MB. Startup time is fast but can be measured and improved.

- Profile binary size by crate. Identify heavy dependencies that can be feature-gated or replaced.
- LTO (Link-Time Optimization) and `codegen-units = 1` for release builds.
- Lazy initialization for expensive resources (already done for browser; extend to providers, memory).
- Target: <5 MB binary, <100 ms to first health check response.

| Metric | Rating |
|--------|--------|
| **User Value** | `LOW` — Users rarely notice binary size or startup time. Matters for containerized deployments (image pull time). |
| **Innovation** | 2/10 — Standard Rust optimization. |
| **Risk** | `LOW` — Build config changes. No runtime impact. |
| **Pillar** | Brutal Efficiency |

---

## Phase 4 — Ecosystem Expansion ✓ COMPLETE (2026-03-08)

*Pillar: Autonomy, Elegance. More channels, more tools, more reach.*

### 4.1 Discord Channel ✓ DONE (2026-03-08)

Full Discord bot integration via `serenity` crate. Mirror Telegram capabilities: allowlist, file transfer, slash commands.

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Discord is the second most requested channel. Opens TEMM1E to developer communities, gaming, and team use cases. |
| **Innovation** | 3/10 — Standard channel integration. The trait system makes this straightforward. |
| **Risk** | `LOW` — The `Channel` trait is well-defined. Implementation is isolated to `temm1e-channels`. |
| **Pillar** | Autonomy |

### 4.2 Git Tool ✓ DONE (2026-03-08)

Native git operations: clone, pull, push, commit, branch, diff, log. Currently users rely on shell commands for git.

- Typed parameters: `{action: "commit", message: "...", files: ["..."]}`.
- Safety: block force-push by default, require explicit confirmation for destructive operations.
- Integrate with GitHub/GitLab APIs for PR creation, issue management.

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Git is the most common developer workflow. A dedicated tool with safety guards is better than raw shell. |
| **Innovation** | 4/10 — Most agent runtimes have git tools. Safety guards (block force-push) add some value. |
| **Risk** | `LOW` — Well-scoped tool. The `Tool` trait makes this plug-and-play. |
| **Pillar** | Autonomy, Elegance |

### 4.3 Skill Registry (TemHub v1) ✓ DONE (2026-03-08)

Load and execute user-defined skills from Markdown files with YAML frontmatter. No signing yet — local skills only.

- Scan `~/.temm1e/skills/` and workspace `skills/` directories.
- Parse skill format: name, description, capabilities, instructions.
- Inject skill instructions into system prompt when the agent detects a relevant task.
- `/skills` command: list available skills.

| Metric | Rating |
|--------|--------|
| **User Value** | `MEDIUM` — Power users can extend the agent with domain-specific knowledge. Not needed for basic use. |
| **Innovation** | 5/10 — OpenClaw has a skill system. TEMM1E's version is simpler but safer (sandbox enforcement). |
| **Risk** | `MEDIUM` — Skill injection into prompts can conflict with system instructions. Needs priority/override rules. |
| **Pillar** | Elegance, Tem's Mind |

### 4.4 Slack Channel ✓ DONE (2026-03-08)

Slack Bot integration via Slack API (Events API + Web API). Workspace-level deployment.

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Slack is dominant in enterprise. Opens TEMM1E to professional team use. |
| **Innovation** | 3/10 — Standard integration. |
| **Risk** | `MEDIUM` — Slack's API is more complex than Telegram's (OAuth app installation, event subscriptions, workspace permissions). |
| **Pillar** | Autonomy |

### 4.5 Web Dashboard (Minimal) ✓ DONE (2026-03-08)

A lightweight web UI for monitoring TEMM1E: active tasks, health status, configuration, logs. Not a chat interface — the messaging app remains primary.

- Serve from the existing gateway on `/dashboard`.
- Static HTML + HTMX (no JS framework). Total <50KB.
- Views: health, active tasks, recent conversations (redacted), config overview.
- Read-only. No configuration changes from the dashboard.

| Metric | Rating |
|--------|--------|
| **User Value** | `MEDIUM` — Useful for operators monitoring a deployed instance. Not needed for single-user setups. |
| **Innovation** | 3/10 — Standard ops dashboard. HTMX-only approach is a nice minimalism statement. |
| **Risk** | `LOW` — Additive. No impact on core functionality. |
| **Pillar** | Elegance |

---

## Phase 5 — Cloud Scale ✓ COMPLETE (2026-03-08)

*Pillar: Robustness, Elegance. Scale beyond a single instance.*

### 5.1 S3/R2 FileStore Backend ✓ DONE (2026-03-08)

Implement the `FileStore` trait with S3-compatible object storage. For files that exceed channel limits (50MB on Telegram), generate presigned URLs.

| Metric | Rating |
|--------|--------|
| **User Value** | `MEDIUM` — Enables large file exchange. Currently capped at Telegram's 50MB limit. |
| **Innovation** | 3/10 — Standard cloud integration. |
| **Risk** | `LOW` — Isolated crate (`temm1e-filestore`). Trait already defined. |
| **Pillar** | Autonomy |

### 5.2 OpenTelemetry Observability ✓ DONE (2026-03-08)

Implement the `Observable` trait with OpenTelemetry tracing and metrics. Export to Jaeger, Prometheus, or any OTLP-compatible backend.

- Trace every agent loop iteration: provider calls, tool executions, memory lookups.
- Metrics: request latency, token usage, tool success rate, task completion rate.
- Structured logging with trace context propagation.

| Metric | Rating |
|--------|--------|
| **User Value** | `MEDIUM` — Essential for production operators. Not needed for personal use. |
| **Innovation** | 3/10 — Standard observability. |
| **Risk** | `LOW` — Additive. The `tracing` crate already provides the foundation. |
| **Pillar** | Elegance, Robustness |

### 5.3 Multi-Tenancy with Workspace Isolation ✓ DONE (2026-03-08)

The `Tenant` trait: per-user workspaces, isolated file systems, separate memory databases, individual provider quotas.

- Each tenant gets: own workspace directory, own SQLite database, own vault namespace.
- Tenant creation via messaging: admin sends `/adduser @username`.
- Resource quotas: max tasks, max storage, max API calls per day.

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Enables shared TEMM1E instances for teams. Currently single-user only. |
| **Innovation** | 6/10 — Multi-tenant agent runtimes are rare. Most are single-user or require separate deployments. |
| **Risk** | `HIGH` — Cross-tenant isolation is a security-critical feature. File system jails, memory isolation, and quota enforcement all need careful implementation. A bug here is a security vulnerability. |
| **Pillar** | Robustness, Elegance |

### 5.4 OAuth Identity Flows ✓ DONE (2026-03-08)

Implement the `Identity` trait with OAuth 2.0 / OIDC flows initiated through messaging.

- Agent sends an OAuth URL in chat → user clicks → callback to gateway → token stored in vault.
- Supported: GitHub, Google, AWS (via OIDC federation).
- Token refresh handled automatically. Expired tokens trigger re-auth prompt.

| Metric | Rating |
|--------|--------|
| **User Value** | `MEDIUM` — Replaces manual API key management for services that support OAuth. Improves security (short-lived tokens vs long-lived keys). |
| **Innovation** | 7/10 — OAuth flows via messaging bot is genuinely novel UX. Most agents require web UI for auth. |
| **Risk** | `HIGH` — OAuth is notoriously tricky (state management, PKCE, token refresh, callback routing). Security-sensitive: a bug leaks user tokens. |
| **Pillar** | Elegance, Autonomy |

### 5.5 Horizontal Scaling via Orchestrator ✓ DONE (2026-03-08)

Implement the `Orchestrator` trait for Kubernetes and Docker. Auto-provision agent instances per tenant.

| Metric | Rating |
|--------|--------|
| **User Value** | `LOW` — Only relevant for large-scale multi-tenant deployments. Single-user instances don't need orchestration. |
| **Innovation** | 5/10 — Container orchestration is standard, but self-provisioning agent runtimes are rare. |
| **Risk** | `CRITICAL` — Orchestration bugs can spawn unlimited containers, cause billing runaway, or leave zombie instances. Needs strict resource limits and kill switches. |
| **Pillar** | Robustness |

---

## Phase 6 — Advanced Tem's Mind ✓ COMPLETE (2026-03-08)

*Pillar: Tem's Mind, Innovation. Push the frontier of what autonomous agents can do.*

### 6.1 Parallel Tool Execution ✓ DONE (2026-03-08)

Currently tools execute sequentially within a round. Independent tools (e.g., read 3 files) wait for each other.

- Detect independent tool calls (no data dependencies between them).
- Execute in parallel with `tokio::join!`.
- Merge results into a single response for the provider.
- Respect concurrency limits (max 5 parallel tool calls) to avoid resource exhaustion.

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Multi-file reads, parallel shell commands, concurrent API checks — all become faster. Reduces wall-clock time for complex tasks by 2-5x. |
| **Innovation** | 6/10 — Some runtimes support this but most don't. Clean implementation in a single-binary agent is notable. |
| **Risk** | `MEDIUM` — Race conditions if tools share state (e.g., two shell commands writing to the same file). Needs dependency analysis. |
| **Pillar** | Brutal Efficiency, Tem's Mind |

### 6.2 Agent-to-Agent Delegation ✓ DONE (2026-03-08)

For complex tasks, the primary agent spawns a sub-agent with a scoped objective. The sub-agent has its own context, tools, and verification loop.

- Primary agent decomposes task and delegates sub-tasks to specialized sub-agents.
- Sub-agents report back with structured results.
- Primary agent aggregates results and verifies overall task completion.
- Sub-agents can use cheaper/faster models for scoped work.

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Complex tasks (e.g., "refactor the codebase and update all tests") benefit from divide-and-conquer. |
| **Innovation** | 9/10 — Multi-agent orchestration within a single runtime is frontier territory. Most multi-agent systems are framework-level (LangGraph, CrewAI). TEMM1E doing this natively in Rust is novel. |
| **Risk** | `HIGH` — Agent coordination is hard. Sub-agents can conflict, loop, or produce inconsistent results. Needs strict scoping and a clear aggregation protocol. |
| **Pillar** | Tem's Mind, Autonomy |

### 6.3 Proactive Task Initiation ✓ DONE (2026-03-08)

The agent currently only acts on user messages and heartbeats. It never initiates action on its own based on observed conditions.

- Monitor triggers: file changes in workspace, cron schedules, external webhook events.
- When a trigger fires, the agent evaluates whether action is needed and what to do.
- User must opt-in to proactive behavior (not enabled by default — sovereignty requires consent).
- Examples: "A deployment failed (webhook) → agent investigates and reports." "A new file appeared in /inbox → agent processes it."

| Metric | Rating |
|--------|--------|
| **User Value** | `HIGH` — Transforms TEMM1E from reactive (waits for orders) to proactive (anticipates needs). This is the difference between a tool and an assistant. |
| **Innovation** | 8/10 — Event-driven agent initiation is rare in current runtimes. Most are purely conversational. |
| **Risk** | `HIGH` — Proactive agents that act without user input can cause damage. Needs strict guardrails: action requires user confirmation for destructive operations, rate limits on proactive actions. |
| **Pillar** | Tem's Mind, Autonomy |

### 6.4 Adaptive System Prompt — Self-Tuning Agent ✓ DONE (2026-03-08)

The system prompt is currently static. The agent can't modify its own instructions based on experience.

- After task completion, the agent evaluates: "What instruction would have made this task easier?"
- Proposed prompt modifications are stored as "prompt patches" in memory.
- On next session, relevant patches are injected into the system prompt.
- User can review and approve/reject patches: `/patches` → list, approve, reject.

| Metric | Rating |
|--------|--------|
| **User Value** | `MEDIUM` — The agent improves its own performance over time. But users may not notice the subtle improvements. |
| **Innovation** | 9/10 — Self-modifying agent instructions based on execution experience is research-grade. Very few systems attempt this. |
| **Risk** | `CRITICAL` — Self-modification can degrade performance or introduce unsafe behaviors. Requires user approval gate and easy rollback. Prompt drift is a real concern. |
| **Pillar** | Tem's Mind, Brutal Efficiency |

---

## Dependency Graph

```
Phase 0 (Foundation)           Phase 1 (Tem's Mind)         Phase 2 (Self-Healing)
┌─────────────────┐            ┌─────────────────┐            ┌─────────────────┐
│ 0.1 Graceful    │───────────▶│ 1.3 Task Queue  │───────────▶│ 2.2 State       │
│     Shutdown    │            │     + Checkpoint │            │     Recovery    │
└─────────────────┘            └─────────────────┘            └─────────────────┘
┌─────────────────┐            ┌─────────────────┐            ┌─────────────────┐
│ 0.2 Circuit     │            │ 1.1 Verification│            │ 2.1 Watchdog    │
│     Breaker     │            │     Engine      │            │                 │
└─────────────────┘            └────────┬────────┘            └─────────────────┘
┌─────────────────┐                     │                     ┌─────────────────┐
│ 0.3 Channel     │            ┌────────▼────────┐            │ 2.3 Health-Aware│
│     Reconnect   │            │ 1.6 DONE        │            │     Heartbeat   │
└─────────────────┘            │     Definition  │            └─────────────────┘
┌─────────────────┐            └─────────────────┘
│ 0.4 Streaming   │            ┌─────────────────┐
│     Responses   │            │ 1.2 Task         │
└─────────────────┘            │     Decomposition│
┌─────────────────┐            └────────┬────────┘
│ 0.5 Raise       │                     │
│     Limits      │            ┌────────▼────────┐
└─────────────────┘            │ 1.4 Context     │            Phase 6 (Advanced)
                               │     Manager     │            ┌─────────────────┐
                               └─────────────────┘     ┌─────▶│ 6.2 Agent-to-   │
                               ┌─────────────────┐     │      │     Agent       │
                               │ 1.5 Self-       │─────┘      └─────────────────┘
                               │     Correction  │            ┌─────────────────┐
                               └─────────────────┘            │ 6.1 Parallel    │
                               ┌─────────────────┐            │     Tools       │
                               │ 1.7 Cross-Task  │            └─────────────────┘
                               │     Learning    │            ┌─────────────────┐
                               └─────────────────┘            │ 6.3 Proactive   │
                                                              │     Initiation  │
Phase 3 (Efficiency)           Phase 4 (Ecosystem)            └─────────────────┘
┌─────────────────┐            ┌─────────────────┐            ┌─────────────────┐
│ 3.1 Tool Output │            │ 4.1 Discord     │            │ 6.4 Self-Tuning │
│     Compression │            ├─────────────────┤            │     Prompt      │
├─────────────────┤            │ 4.2 Git Tool    │            └─────────────────┘
│ 3.2 Prompt      │            ├─────────────────┤
│     Optimization│            │ 4.3 Skill       │
├─────────────────┤            │     Registry    │
│ 3.3 Tiered      │            ├─────────────────┤
│     Model Route │            │ 4.4 Slack       │
├─────────────────┤            ├─────────────────┤
│ 3.4 History     │            │ 4.5 Web Dash    │
│     Pruning     │            └─────────────────┘
├─────────────────┤
│ 3.5 Binary      │            Phase 5 (Cloud Scale)
│     Optimize    │            ┌─────────────────┐
└─────────────────┘            │ 5.3 Multi-      │
                               │     Tenancy     │
                               ├─────────────────┤
                               │ 5.4 OAuth       │
                               │     Identity    │
                               ├─────────────────┤
                               │ 5.5 Orchestrator│
                               └─────────────────┘
```

Key dependencies:
- `0.1` → `1.3` → `2.2` (shutdown → persistence → recovery)
- `1.1` → `1.6` (verification → DONE definition)
- `1.5` → `6.2` (self-correction → agent delegation)

Independent tracks that can proceed in parallel:
- Phase 0 (all items)
- Phase 3 (all items)
- Phase 4 (all items except 4.3 which benefits from 1.4)

---

## Scorecard Summary

| # | Item | User Value | Innovation | Risk | Pillar |
|---|------|-----------|------------|------|--------|
| 0.1 | Graceful Shutdown | CRITICAL | 2 | LOW | Robustness | ✓ DONE |
| 0.2 | Circuit Breaker | HIGH | 3 | MEDIUM | Robustness | ✓ DONE |
| 0.3 | Channel Reconnect | CRITICAL | 2 | LOW | Robustness | ✓ DONE |
| 0.4 | Streaming Responses | HIGH | 4 | MEDIUM | Elegance | ✓ DONE |
| 0.5 | Raise Limits | CRITICAL | 5 | LOW | Autonomy | ✓ DONE |
| 1.1 | Verification Engine | CRITICAL | 8 | MEDIUM | Tem's Mind | ✓ DONE |
| 1.2 | Task Decomposition | HIGH | 7 | HIGH | Tem's Mind | ✓ DONE |
| 1.3 | Persistent Task Queue | CRITICAL | 6 | HIGH | Robustness | ✓ DONE |
| 1.4 | Context Manager | HIGH | 7 | MEDIUM | Brutal Efficiency | ✓ DONE |
| 1.5 | Self-Correction | HIGH | 8 | MEDIUM | Autonomy | ✓ DONE |
| 1.6 | DONE Definition | CRITICAL | 9 | MEDIUM | Tem's Mind | ✓ DONE |
| 1.7 | Cross-Task Learning | HIGH | 8 | MEDIUM | Tem's Mind | ✓ DONE |
| 2.1 | Watchdog | HIGH | 5 | LOW | Robustness | ✓ DONE |
| 2.2 | State Recovery | CRITICAL | 7 | HIGH | Robustness | ✓ DONE |
| 2.3 | Health-Aware Heartbeat | MEDIUM | 6 | LOW | Robustness | ✓ DONE |
| 2.4 | Memory Failover | MEDIUM | 4 | MEDIUM | Robustness | ✓ DONE |
| 3.1 | Tool Output Compression | HIGH | 7 | MEDIUM | Brutal Efficiency | ✓ DONE |
| 3.2 | Prompt Optimization | MEDIUM | 5 | LOW | Brutal Efficiency | ✓ DONE |
| 3.3 | Tiered Model Routing | HIGH | 6 | MEDIUM | Brutal Efficiency | ✓ DONE |
| 3.4 | History Pruning | HIGH | 7 | MEDIUM | Brutal Efficiency | ✓ DONE |
| 3.5 | Binary Optimization | LOW | 2 | LOW | Brutal Efficiency | ✓ DONE |
| 4.1 | Discord | HIGH | 3 | LOW | Autonomy | ✓ DONE |
| 4.2 | Git Tool | HIGH | 4 | LOW | Autonomy | ✓ DONE |
| 4.3 | Skill Registry | MEDIUM | 5 | MEDIUM | Elegance | ✓ DONE |
| 4.4 | Slack | HIGH | 3 | MEDIUM | Autonomy | ✓ DONE |
| 4.5 | Web Dashboard | MEDIUM | 3 | LOW | Elegance | ✓ DONE |
| 5.1 | S3 FileStore | MEDIUM | 3 | LOW | Autonomy | ✓ DONE |
| 5.2 | OpenTelemetry | MEDIUM | 3 | LOW | Elegance | ✓ DONE |
| 5.3 | Multi-Tenancy | HIGH | 6 | HIGH | Robustness | ✓ DONE |
| 5.4 | OAuth Identity | MEDIUM | 7 | HIGH | Elegance | ✓ DONE |
| 5.5 | Orchestrator | LOW | 5 | CRITICAL | Robustness | ✓ DONE |
| 6.1 | Parallel Tools | HIGH | 6 | MEDIUM | Brutal Efficiency | ✓ DONE |
| 6.2 | Agent Delegation | HIGH | 9 | HIGH | Tem's Mind |
| 6.3 | Proactive Initiation | HIGH | 8 | HIGH | Tem's Mind |
| 6.4 | Self-Tuning Prompt | MEDIUM | 9 | CRITICAL | Tem's Mind |

---

## Recommended Execution Order

**Immediate** (unblocks everything else):
`0.5` → `0.3` → `0.1` → `0.2`

**Next** (the Tem's Mind — TEMM1E's differentiator):
`1.1` → `1.6` → `1.5` → `1.3` → `1.4` → `1.7`

**Parallel track** (efficiency + ecosystem, no dependencies on Phase 1):
`3.1` + `3.2` + `0.4` + `4.2` + `4.1`

**Then** (self-healing, builds on Phase 1):
`2.1` → `2.2` → `2.3`

**Scale when needed:**
`5.1` → `5.3` → `5.4`

**Frontier (when the core is solid):**
`6.1` → `6.2` → `6.3`
