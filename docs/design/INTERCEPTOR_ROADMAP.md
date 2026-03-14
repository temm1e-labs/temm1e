# Interceptor Architecture — Full Roadmap

**TEMM1E v2.4+**

The Interceptor is TEMM1E's real-time task observation and control layer. It sits between
the agent runtime and the messaging channel, giving users and operators visibility into
what the agent is doing and the ability to intervene mid-task.

---

## Phase 1: Status Emission + CancellationToken (v2.4.0 — COMPLETE)

**Status: Shipped**

Two primitives that are prerequisites for everything that follows.

### Delivered

1. **AgentTaskStatus + AgentTaskPhase** (`crates/temm1e-agent/src/agent_task_status.rs`)
   - `AgentTaskPhase` enum: `Preparing`, `Classifying`, `CallingProvider`, `ExecutingTool`,
     `Finishing`, `Done`, `Interrupted`
   - `AgentTaskStatus` struct: phase, started_at, rounds_completed, tools_executed,
     input_tokens, output_tokens, cost_usd
   - Emitted via `tokio::sync::watch` channel at 10 checkpoint locations in the agent loop
   - `send_modify()` is infallible — zero new panic paths

2. **CancellationToken** (`tokio_util::sync::CancellationToken`)
   - Added alongside existing `AtomicBool` interrupt (not replacing)
   - Created per-ChatSlot, cancelled on `/stop` command and heartbeat preemption
   - Phase 1: created and cancelled but not awaited in agent loop
   - Both params are `Option` — `None` = identical to pre-Phase-1 behavior

3. **Prompted Tool Calling Fallback** (bonus — added alongside Phase 1)
   - When native function calling fails (provider 400), switches to prompted JSON mode
   - Tool definitions injected into system prompt, model outputs JSON
   - Lenient JSON parser handles markdown fences, leading text, nested objects
   - Max 1 JSON parse retry with stricter prompt
   - User-friendly error messages replace raw JSON dumps on all error paths

### Files

| File | Status |
|------|--------|
| `crates/temm1e-agent/src/agent_task_status.rs` | NEW |
| `crates/temm1e-agent/src/prompted_tool_calling.rs` | NEW |
| `crates/temm1e-agent/src/runtime.rs` | MODIFIED — status emission, cancel param, prompted fallback |
| `crates/temm1e-agent/src/lib.rs` | MODIFIED — new module exports |
| `crates/temm1e-agent/Cargo.toml` | MODIFIED — tokio-util dependency |
| `src/main.rs` | MODIFIED — ChatSlot, watch channel, format_user_error |
| `crates/temm1e-gateway/src/router.rs` | MODIFIED — updated call signature |

---

## Phase 2: Mid-Stream Cancellation + Progress Streaming

**Status: Planned**

Use the CancellationToken from Phase 1 to abort provider calls and tool executions
mid-flight. Stream status updates to the user in real-time.

### Design

1. **`tokio::select!` on cancellation**
   ```rust
   tokio::select! {
       resp = provider.complete(req) => { /* normal path */ }
       _ = cancel.cancelled() => {
           // Abort mid-stream, return partial response or cancellation notice
       }
   }
   ```
   - Wraps both `provider.complete()` and `execute_tool()` in select
   - On cancellation: session history rolled back, user gets "Task cancelled" message
   - AtomicBool interrupt check removed — CancellationToken replaces it entirely

2. **Status streaming to user**
   - Background task subscribes to `watch::Receiver<AgentTaskStatus>`
   - Sends periodic Telegram/Discord messages: "Round 2/5 — executing shell tool... (3.2s)"
   - Configurable interval (default: 5s for long tasks, disabled for fast ones)
   - Uses `status_rx.changed().await` — zero polling

3. **Progress bar for tool execution**
   - For multi-step tool calls, emit intermediate progress
   - e.g., "Searching... found 12 results, filtering..."

### Migration

- Remove `AtomicBool` interrupt from all call sites
- Replace with `CancellationToken` everywhere
- `ChatSlot.interrupt: Arc<AtomicBool>` → `ChatSlot.cancel: CancellationToken`
- All existing `/stop` behavior preserved, now works mid-stream

### Risk

- **Medium** — changes the interrupt mechanism that's been stable since v1.0
- Mitigation: comprehensive test coverage of cancel paths before migration
- Rollback: keep AtomicBool as dead code for one release cycle

---

## Phase 3: Interceptor Middleware

**Status: Planned**

The full Interceptor — a middleware layer that observes and controls agent execution.

### Design

```
User Message
    ↓
Channel (Telegram/Discord/CLI)
    ↓
Gateway Router
    ↓
┌─────────────────────────┐
│  INTERCEPTOR             │
│                          │
│  ┌──────────────────┐   │
│  │ Status Observer   │   │  ← reads watch::Receiver<AgentTaskStatus>
│  │ Progress Notifier │   │  ← sends "Round 2, executing tool..." to user
│  │ Cancel Handler    │   │  ← cancels on /stop, timeout, budget exceeded
│  │ Budget Guard      │   │  ← aborts if cost exceeds per-task limit
│  │ Timeout Guard     │   │  ← aborts if task exceeds time limit
│  │ Audit Logger      │   │  ← logs every phase transition for compliance
│  └──────────────────┘   │
│                          │
│  Agent Runtime           │
│    ↕ watch channel       │
│    ↕ CancellationToken   │
└─────────────────────────┘
    ↓
Outbound Reply
```

### Components

1. **InterceptorConfig** — per-chat or global configuration
   ```rust
   pub struct InterceptorConfig {
       pub progress_interval: Duration,       // How often to send status updates
       pub max_task_duration: Duration,        // Auto-cancel after this
       pub max_task_cost_usd: f64,            // Auto-cancel if cost exceeds
       pub audit_enabled: bool,               // Log all phase transitions
       pub user_notifications: bool,          // Send progress messages to user
   }
   ```

2. **StatusObserver** — background task per-message
   - Subscribes to `watch::Receiver<AgentTaskStatus>`
   - On each change: logs, checks guards, optionally notifies user
   - Lifetime: created when message processing starts, dropped when done

3. **Guards** — automatic intervention
   - **BudgetGuard**: reads `status.cost_usd`, cancels if over limit
   - **TimeoutGuard**: reads `status.started_at`, cancels if over duration
   - **StallGuard**: detects if phase hasn't changed in N seconds (agent stuck)

4. **AuditLogger** — compliance and debugging
   - Every phase transition logged with timestamp, user_id, chat_id
   - Stored in memory backend as structured entries
   - Queryable via admin API

### New Files

| File | Purpose |
|------|---------|
| `crates/temm1e-agent/src/interceptor.rs` | InterceptorConfig, StatusObserver |
| `crates/temm1e-agent/src/interceptor/guards.rs` | BudgetGuard, TimeoutGuard, StallGuard |
| `crates/temm1e-agent/src/interceptor/notifier.rs` | User-facing progress messages |
| `crates/temm1e-agent/src/interceptor/audit.rs` | Audit logging |

### Risk

- **Low** — purely additive, reads from watch channel (no mutation)
- All guards use CancellationToken (from Phase 2) for intervention
- Interceptor is optional — disabled by default, enabled per-config

---

## Phase 4: Interactive Control

**Status: Future**

Users control the agent mid-task through chat commands.

### Commands

| Command | Effect |
|---------|--------|
| `/stop` | Cancel current task immediately (existing — enhanced) |
| `/status` | Show current AgentTaskStatus in chat |
| `/pause` | Pause after current tool round completes |
| `/resume` | Resume paused task |
| `/slower` | Increase progress notification interval |
| `/faster` | Decrease progress notification interval |
| `/budget` | Show remaining budget for current task |

### Design

- Commands are intercepted by the Interceptor before reaching the agent
- `/pause` uses a child CancellationToken that's separate from full cancel
- `/status` reads latest value from `watch::Receiver` (instant, no provider call)

### Risk

- **Medium** — pause/resume semantics are complex (session state, provider timeouts)
- Phase 4 is speculative — may be simplified based on Phase 2-3 learnings

---

## Phase 5: Multi-Agent Observation

**Status: Future**

When the agent spawns sub-agents (delegation), the Interceptor tracks all of them.

### Design

- Parent CancellationToken creates child tokens for each sub-agent
- Cancelling parent cancels all children
- StatusObserver aggregates status from all sub-agent watch channels
- User sees: "Main task Round 2 — 3 sub-agents running (search, analyze, summarize)"

### Risk

- **High** — depends on delegation system maturity
- May be descoped if sub-agent usage is rare

---

## Dependencies Between Phases

```
Phase 1 (DONE)
    │
    ├── AgentTaskStatus types
    ├── watch channel emission
    ├── CancellationToken infrastructure
    └── Prompted tool calling fallback
         │
Phase 2 (Next)
    │
    ├── tokio::select! on CancellationToken
    ├── Remove AtomicBool interrupt
    └── Status streaming to user
         │
Phase 3
    │
    ├── Interceptor middleware
    ├── Guards (budget, timeout, stall)
    └── Audit logging
         │
Phase 4
    │
    ├── /pause, /resume, /status commands
    └── Interactive control
         │
Phase 5
    │
    └── Multi-agent observation
```

Each phase is independently shippable and valuable. Phase 2 is the next priority
as it enables real mid-stream cancellation (currently /stop only works between tool rounds).

---

## Design Principles

1. **Zero-overhead when unused** — all Interceptor features are behind `Option`.
   `None` = current behavior, zero extra work.

2. **No new panic paths** — `watch::send_modify` is infallible,
   `CancellationToken::cancel()` is infallible. Status observation is read-only.

3. **Per-message lifecycle** — watch channel created when `process_message()` starts,
   dropped when it returns. No persistent state between messages.

4. **Additive, not invasive** — each phase adds capabilities without changing
   existing code paths. Guards intervene via CancellationToken (a clean interface),
   not by reaching into runtime internals.

5. **Observable, not controllable (Phase 1-3)** — the Interceptor reads status
   and can cancel, but doesn't modify agent behavior. Phase 4 adds limited control
   (pause/resume). The agent loop remains the authority on execution.
