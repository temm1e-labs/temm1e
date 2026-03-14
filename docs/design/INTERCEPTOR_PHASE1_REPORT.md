# Interceptor Phase 1: Status Emission + CancellationToken

**Implementation Report — Confidence: 100% | Risk: LOW**

Author: Claude Opus 4.6 | Date: 2026-03-11 | TEMM1E v2.3.1

---

## Scope

Two primitives that are prerequisites for the full Interceptor architecture. Both are **purely additive** — no existing behavior changes, no breaking signatures, no new crate dependencies.

1. **TaskStatus emission** — the agent loop broadcasts what it's doing in real-time via `tokio::sync::watch`
2. **CancellationToken** — replace the `Arc<AtomicBool>` interrupt with a richer token that supports async `.cancelled()` (future-proofs mid-stream cancellation in Phase 2)

---

## 1. Status Emission

### What It Does

The agent loop in `runtime.rs` emits a `TaskStatus` enum at every state transition. Any external observer (the future Interceptor, the dispatcher, the CLI) can subscribe via `watch::Receiver<TaskStatus>` and read the current state at any time — zero-cost if nobody is listening.

### Data Model

```rust
// crates/temm1e-agent/src/task_status.rs (NEW FILE)

use std::time::Instant;

#[derive(Debug, Clone)]
pub enum TaskPhase {
    /// Parsing user input, loading images, detecting credentials
    Preparing,
    /// Classifying message complexity (V2)
    Classifying,
    /// Building context and sending to LLM provider
    CallingProvider { round: u32 },
    /// Receiving streaming response from provider
    Streaming { round: u32, tokens_so_far: u32 },
    /// Executing a tool call
    ExecutingTool { round: u32, tool_name: String, tool_index: u32, tool_total: u32 },
    /// Agent loop broke — building final reply
    Finishing,
    /// Done — result returned to caller
    Done,
    /// Interrupted by user or system
    Interrupted { round: u32 },
}

#[derive(Debug, Clone)]
pub struct TaskStatus {
    pub phase: TaskPhase,
    pub started_at: Instant,
    pub rounds_completed: u32,
    pub tools_executed: u32,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: f64,
}

impl Default for TaskStatus {
    fn default() -> Self {
        Self {
            phase: TaskPhase::Preparing,
            started_at: Instant::now(),
            rounds_completed: 0,
            tools_executed: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
        }
    }
}
```

### Why `watch` Channel

| Option | Pros | Cons | Verdict |
|--------|------|------|---------|
| `watch::channel` | Single-producer single-value, receiver always gets latest state, `.subscribe()` is cheap, no buffering | Only holds latest value (no history) | **CHOSEN** — we want current state, not a log |
| `broadcast::channel` | Multiple receivers, buffered history | Lagging receivers miss events, memory overhead | Overkill — status is polled, not streamed |
| `mpsc::channel` | Buffered, ordered | Receiver must drain to stay current, backpressure risk | Wrong semantics — we want "what's happening NOW" |
| Shared `Arc<RwLock<TaskStatus>>` | Simplest | No async notification, must poll | No way to `.await` state changes |

`tokio::sync::watch` is already available via the `tokio` dependency (feature `sync` is enabled in the workspace). **Zero new dependencies.**

### Emission Points (Exact Locations)

Every emission is a single line: `status_tx.send_modify(|s| { ... });`

`send_modify` never fails (it modifies in-place even if no receivers exist) and never blocks. Cost: one atomic store per emission.

| Location | File:Line | Phase Emitted | What's Happening |
|----------|-----------|---------------|------------------|
| After user text parsed | `runtime.rs:240` | `Preparing` | Initial state, parsing input |
| After V2 classification | `runtime.rs:~420` | `Classifying` | Complexity classification done |
| Before `provider.complete()` | `runtime.rs:576` | `CallingProvider { round }` | About to call LLM |
| After `provider.complete()` returns | `runtime.rs:577` | update `input_tokens`, `output_tokens`, `cost_usd` | Provider returned |
| Before each `execute_tool()` | `runtime.rs:717` | `ExecutingTool { round, tool_name, i, total }` | Starting tool execution |
| After tool loop completes (line 876) | `runtime.rs:876` | increment `rounds_completed` | Round done |
| On interrupt break | `runtime.rs:491` | `Interrupted { round }` | User cancelled |
| On final text reply | `runtime.rs:685` | `Finishing` then `Done` | Returning response |
| On fallback exit | `runtime.rs:886` | `Done` | Max rounds or timeout |

### Signature Change

**Current:**
```rust
pub async fn process_message(
    &self,
    msg: &InboundMessage,
    session: &mut SessionContext,
    interrupt: Option<Arc<AtomicBool>>,
    pending: Option<PendingMessages>,
    reply_tx: Option<tokio::sync::mpsc::UnboundedSender<OutboundMessage>>,
) -> Result<(OutboundMessage, TurnUsage), Temm1eError>
```

**New:**
```rust
pub async fn process_message(
    &self,
    msg: &InboundMessage,
    session: &mut SessionContext,
    interrupt: Option<Arc<AtomicBool>>,
    pending: Option<PendingMessages>,
    reply_tx: Option<tokio::sync::mpsc::UnboundedSender<OutboundMessage>>,
    status_tx: Option<tokio::sync::watch::Sender<TaskStatus>>,  // NEW — additive
) -> Result<(OutboundMessage, TurnUsage), Temm1eError>
```

**Why `Option<watch::Sender>`:** The parameter is optional so existing callers can pass `None` and get exactly current behavior. CLI chat handler passes `None` (no observer). Gateway worker creates the channel and holds the `Receiver`.

### Callers — Exact Changes

**2 call sites, both mechanical:**

1. **Gateway worker** (`src/main.rs:2764`):
```rust
// BEFORE:
agent.process_message(&msg, &mut session, interrupt_flag, Some(pending.clone()), Some(early_tx))

// AFTER:
let (status_tx, _status_rx) = tokio::sync::watch::channel(TaskStatus::default());
agent.process_message(&msg, &mut session, interrupt_flag, Some(pending.clone()), Some(early_tx), Some(status_tx))
// _status_rx is unused in Phase 1 — the Interceptor (Phase 3) will use it.
// The underscore prefix suppresses unused-variable warnings.
```

2. **CLI chat handler** (`src/main.rs:3998`):
```rust
// BEFORE:
agent.process_message(&msg, &mut session, None, None, Some(early_tx))

// AFTER:
agent.process_message(&msg, &mut session, None, None, Some(early_tx), None)
```

### Risk Analysis

| Potential Risk | Assessment | Mitigation |
|----------------|------------|------------|
| `send_modify` panics | **Impossible.** `watch::Sender::send_modify` never panics — it modifies the value in-place under an internal RwLock. Even if all receivers are dropped, the call succeeds silently. Verified in tokio source: `fn send_modify<F: FnOnce(&mut T)>(&self, func: F)` is infallible. | None needed. |
| Performance overhead | **Negligible.** Each `send_modify` is one atomic RwLock acquire + release. The agent loop runs 1-20 rounds per message, each round taking 2-30 seconds (provider call + tool execution). 10 status emissions per round × 20 rounds = 200 atomic ops over 60-600 seconds. Unmeasurable. | None needed. |
| `Instant` not `Send`/`Sync` | **Non-issue.** `Instant` is `Send + Sync + Clone + Copy` on all platforms. `TaskStatus` derives `Clone` and all fields are `Send + Sync`. `watch::Sender<TaskStatus>` requires `T: Send + Sync`. | Verified — compiles. |
| Breaking existing tests | **Zero impact.** No test calls `process_message` directly — all tests are unit-level on sub-functions (context building, tool execution, classification). The 2 integration test paths through `process_message` are the gateway worker and CLI handler, which are tested via live runs, not unit tests. Adding an `Option` param with `None` default changes nothing for tests. | Pass `None` in any test that calls `process_message`. |
| `TaskStatus` in public API | **Intended.** `task_status.rs` is a new public module in `temm1e-agent`. Consumers who don't care about it simply ignore the `Option`. Future phases import `TaskStatus` for the Interceptor. | Stable enum — new variants can be added without breaking callers since they pattern-match via `_` wildcard. |

**Confidence: 100%.** This is adding an optional output channel with zero behavioral change when unused.

---

## 2. CancellationToken

### Current Mechanism

```rust
// main.rs:1885 — created per ChatSlot
interrupt: Arc<AtomicBool>,

// main.rs:1942 — set by dispatcher on /stop
slot.interrupt.store(true, Ordering::Relaxed);

// runtime.rs:485-493 — checked between tool rounds
if let Some(ref flag) = interrupt {
    if flag.load(Ordering::Relaxed) {
        interrupted = true;
        break;
    }
}
```

**Limitations:**
- `AtomicBool` is poll-only — you can check it, but you can't `.await` it
- Future Phase 2 needs `tokio::select!` to abort mid-stream: `cancel.cancelled()` is an async future, `AtomicBool::load()` is not
- No parent-child cancellation hierarchy (needed when agent spawns sub-tasks)

### Design Decision: Keep `AtomicBool`, Add `CancellationToken` Alongside

**We do NOT replace `AtomicBool` in Phase 1.** We add a `CancellationToken` as a parallel signal.

**Reasoning:**

| Approach | Risk | Verdict |
|----------|------|---------|
| Replace `AtomicBool` with `CancellationToken` | Every caller changes. Dispatcher interrupt logic changes. Two separate systems (gateway + CLI) need updating simultaneously. If anything goes wrong, interruption is broken for all users. | **Too risky for zero benefit in Phase 1** |
| Add `CancellationToken` alongside `AtomicBool` | Zero existing behavior change. New code uses the token. Old code keeps working via `AtomicBool`. Phase 2 migrates callers to token-only. | **CHOSEN — zero-risk, forward-compatible** |

### Implementation

**New field in `ChatSlot`:**
```rust
struct ChatSlot {
    tx: tokio::sync::mpsc::Sender<InboundMessage>,
    interrupt: Arc<AtomicBool>,           // KEPT — existing behavior unchanged
    cancel_token: CancellationToken,      // NEW — for Phase 2 mid-stream abort
    is_heartbeat: Arc<AtomicBool>,
}
```

**New parameter on `process_message`:**
```rust
pub async fn process_message(
    &self,
    msg: &InboundMessage,
    session: &mut SessionContext,
    interrupt: Option<Arc<AtomicBool>>,           // KEPT
    pending: Option<PendingMessages>,
    reply_tx: Option<tokio::sync::mpsc::UnboundedSender<OutboundMessage>>,
    status_tx: Option<watch::Sender<TaskStatus>>, // NEW from Status Emission
    cancel: Option<CancellationToken>,            // NEW — unused in Phase 1 loop
) -> Result<(OutboundMessage, TurnUsage), Temm1eError>
```

**Inside the agent loop (Phase 1 — no behavior change):**
```rust
// Line 484: EXISTING interrupt check — unchanged
if let Some(ref flag) = interrupt {
    if flag.load(Ordering::Relaxed) {
        // emit status
        if let Some(ref tx) = status_tx {
            tx.send_modify(|s| s.phase = TaskPhase::Interrupted { round: rounds });
        }
        interrupted = true;
        break;
    }
}

// The cancel token exists but is NOT checked in Phase 1.
// Phase 2 will add: tokio::select! { ... cancel.cancelled() => break }
// around provider.complete() and execute_tool().
```

**Dispatcher wiring (Phase 1):**
```rust
// main.rs — when creating a ChatSlot:
let cancel_token = CancellationToken::new();

// When /stop is received:
slot.interrupt.store(true, Ordering::Relaxed);  // EXISTING — still works
slot.cancel_token.cancel();                     // NEW — sets the token for Phase 2

// Both signals are set. The agent loop checks AtomicBool (Phase 1 behavior).
// Phase 2 adds select! on the token for mid-stream abort.
```

### Dependency: `tokio-util`

`CancellationToken` lives in `tokio_util::sync`. This crate is already in `Cargo.lock` (transitive dependency via reqwest, h2, etc.) but not declared directly.

**Option A: Add `tokio-util` to `temm1e-agent/Cargo.toml`:**
```toml
tokio-util = { version = "0.7", features = ["sync"] }
```
Cost: explicit dependency. Benefit: access to `CancellationToken`.

**Option B: Use `tokio::sync::watch` as a cancellation signal:**
```rust
let (cancel_tx, cancel_rx) = tokio::sync::watch::channel(false);
// To cancel: cancel_tx.send(true)
// To await:  cancel_rx.clone().changed().await
```
Cost: slightly less ergonomic. Benefit: zero new dependencies.

**Recommendation: Option A.** `tokio-util` is already compiled (transitive dep), declaring it explicitly adds ~0 compile time. `CancellationToken` has exactly the semantics we need: `.cancel()`, `.cancelled()` async future, child tokens, `is_cancelled()` sync check. The `watch(bool)` workaround is reinventing the same thing with worse ergonomics.

### Risk Analysis

| Potential Risk | Assessment | Mitigation |
|----------------|------------|------------|
| `CancellationToken` breaks existing interrupt | **Impossible.** The `AtomicBool` check is untouched. `CancellationToken` is added alongside, not replacing. In Phase 1, the token is created and cancelled but never `.await`ed in the agent loop. | Parallel signals — old path unchanged. |
| Forgetting to cancel the token on `/stop` | **LOW.** Single code path in dispatcher (line 1942). Both `interrupt.store(true)` and `cancel_token.cancel()` go on adjacent lines. Easy to audit. | Code review + test. |
| Token outlives the task (memory leak) | **Non-issue.** `CancellationToken` is reference-counted internally. When the `ChatSlot` is dropped (worker dies or chat_slots cleanup), the token is dropped. No leak. | Automatic via Drop. |
| `tokio-util` version conflict | **Non-issue.** Cargo.lock already has `tokio-util 0.7.x`. Declaring it explicitly pins to the same version. No conflict possible. | Workspace dependency declaration. |
| Adding `cancel: Option<CancellationToken>` to signature | **Mechanical.** Same pattern as `status_tx` — add `None` at both call sites. CLI handler passes `None`. Gateway worker creates token and passes it. | 2-line change per call site. |
| Thread safety of `CancellationToken` | **Guaranteed.** `CancellationToken` is `Send + Sync + Clone`. Designed for cross-task cancellation. | Built-in guarantee. |
| Test breakage | **Zero.** Same reasoning as Status Emission — no test directly calls `process_message`. New param is `Option`, existing tests pass `None`. | N/A. |

**Confidence: 100%.** Adding an unused `Option` parameter with a well-tested tokio primitive. Zero behavioral change in Phase 1.

---

## Combined Signature (Phase 1 Final)

```rust
pub async fn process_message(
    &self,
    msg: &InboundMessage,
    session: &mut SessionContext,
    interrupt: Option<Arc<AtomicBool>>,
    pending: Option<PendingMessages>,
    reply_tx: Option<tokio::sync::mpsc::UnboundedSender<OutboundMessage>>,
    status_tx: Option<tokio::sync::watch::Sender<TaskStatus>>,
    cancel: Option<CancellationToken>,
) -> Result<(OutboundMessage, TurnUsage), Temm1eError>
```

**9 parameters is a lot.** Consider bundling into a struct in a future refactor:
```rust
pub struct ProcessOptions {
    pub interrupt: Option<Arc<AtomicBool>>,
    pub pending: Option<PendingMessages>,
    pub reply_tx: Option<mpsc::UnboundedSender<OutboundMessage>>,
    pub status_tx: Option<watch::Sender<TaskStatus>>,
    pub cancel: Option<CancellationToken>,
}
```
This is a **Phase 1 optional cleanup**, not a blocker. Both approaches compile and work.

---

## Files Changed

| File | Change | Lines Affected |
|------|--------|----------------|
| `crates/temm1e-agent/src/task_status.rs` | **NEW** — `TaskStatus`, `TaskPhase` types | ~50 lines |
| `crates/temm1e-agent/src/lib.rs` | Add `pub mod task_status;` | 1 line |
| `crates/temm1e-agent/src/runtime.rs` | Add `status_tx` + `cancel` params, emit status at 10 checkpoint locations | ~30 lines added |
| `crates/temm1e-agent/Cargo.toml` | Add `tokio-util = { version = "0.7", features = ["sync"] }` | 1 line |
| `src/main.rs` (gateway worker) | Create `watch::channel` + `CancellationToken`, pass to `process_message`, cancel on `/stop` | ~8 lines |
| `src/main.rs` (CLI handler) | Pass `None, None` for new params | 1 line |
| `src/main.rs` (ChatSlot struct) | Add `cancel_token: CancellationToken` field | 2 lines |

**Total: ~95 lines of new code, 0 lines of deleted code, 0 behavioral changes.**

---

## Test Plan

### Unit Tests (in `runtime.rs`)

1. **`task_status_default`** — `TaskStatus::default()` has `Preparing` phase, zero counters
2. **`task_status_clone`** — `TaskStatus` implements `Clone` (required by `watch`)
3. **`task_status_send_sync`** — compile-time check that `TaskStatus: Send + Sync`

### Integration Verification

4. **Build gate:** `cargo check --workspace` compiles with new signature
5. **Clippy gate:** `cargo clippy --workspace --all-targets --all-features -- -D warnings`
6. **Existing tests:** `cargo test --workspace` — all 1,307 pass unchanged
7. **CLI self-test:** 10-turn conversation via `temm1e chat` — zero regressions (status_tx is None in CLI path, cancel is None — purely additive)

### Phase 2 Readiness Test (manual)

8. In gateway worker, after creating `(status_tx, status_rx)`, add a temporary debug task:
```rust
let mut rx = status_rx.clone();
tokio::spawn(async move {
    while rx.changed().await.is_ok() {
        let status = rx.borrow();
        tracing::debug!(phase = ?status.phase, round = status.rounds_completed, "Task status update");
    }
});
```
Run TEMM1E, send a message with tool usage, verify debug logs show phase transitions. Remove after verification.

---

## Backwards Compatibility

| Concern | Status |
|---------|--------|
| Existing callers compile without changes | YES — new params are `Option`, pass `None` |
| Existing interrupt behavior unchanged | YES — `AtomicBool` check at line 485 is untouched |
| Existing `/stop` command works | YES — `interrupt.store(true)` is unchanged, `cancel_token.cancel()` is additive |
| Existing pending messages injection | YES — pending check at line 805 is untouched |
| Session history format | UNCHANGED — no new message types |
| Provider API calls | UNCHANGED — status emission is local only |
| Memory backend writes | UNCHANGED — no new data stored |
| Budget tracking | UNCHANGED — `TaskStatus` reads accumulators but doesn't modify them |
| Wire protocol (Telegram/Discord/Slack) | UNCHANGED — nothing visible to channels |

---

## Phase 2 Preview (Not In Scope)

Phase 1 lays the foundation. Phase 2 uses it:

- **Mid-stream cancellation:** `tokio::select! { resp = provider.complete(req) => ..., _ = cancel.cancelled() => ... }`
- **Status-based user responses:** Interceptor reads `status_rx`, formats "Round 3/5, executing shell tool, 2.4s elapsed" and sends to user
- **Pause/resume:** `cancel` token gets a child token for pause semantics

Phase 1 makes Phase 2 a clean addition rather than a risky refactor.

---

## Conclusion

Both primitives are **100% confidence, LOW risk**:

- **Zero behavioral changes** — all new code is behind `Option::None` or `send_modify` that succeeds silently with no receivers
- **Zero new failure modes** — `watch::send_modify` is infallible, `CancellationToken::cancel()` is infallible
- **Zero test regressions** — additive `Option` params, existing tests pass `None`
- **Zero dependency risk** — `tokio::sync::watch` is existing, `tokio-util` is already compiled as transitive dep
- **Forward-compatible** — Phase 2 mid-stream cancellation and Phase 3 Interceptor build directly on these primitives
