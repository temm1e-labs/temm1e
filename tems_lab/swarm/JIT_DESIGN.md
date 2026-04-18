# TEMM1E JIT Swarm — Design Document

**Status:** Design complete, amended after harmony sweep. Implementation pending prerequisites.
**Date:** 2026-04-18
**Predecessor:** `tems_lab/swarm/DESIGN.md` (v1 dispatch-time Hive)
**Related:** `docs/design/DISPATCHER_REWORK.md`, `tems_lab/swarm/DISPATCHER_EVOLUTION.md`, `tems_lab/swarm/HARMONY_SWEEP.md`

> **Read `HARMONY_SWEEP.md` first** — it audited this design against the live codebase and identified 9 hidden couplings that amend this document. The amendments are captured in `HARMONY_SWEEP.md` §3-§5 and summarised in the updated prereq list below.

---

## 1. Purpose & Scope

### 1.1 What "JIT Swarm" means

The **v1 Hive** (live in `temm1e-hive/`) only activates at *dispatch time*: the LLM classifier labels a message `Complex` and the runtime routes around the main agent loop via `Err(Temm1eError::HiveRoute(...))` (`runtime.rs:964-972`). The main agent never sees the swarm — the dispatcher makes the call before any tools run.

**Just-In-Time (JIT) Swarm** adds a second entry point: the main agent, *while already in its tool loop*, can itself spawn a swarm when it discovers mid-flight that its work has N independent subtasks. Implementation: swarm becomes a tool the model calls (`spawn_swarm`), not an out-of-band route.

### 1.2 Why add this

Dispatch-time classification answers "is this task obviously parallel?" **before any investigation has happened.** That catches the easy cases ("research these 5 libraries and compare them"). It misses the interesting cases:

- User: *"refactor the auth module"* → main agent reads it → discovers 8 independent files with no shared state → could parallelise from here.
- User: *"debug why tests are failing"* → main agent runs `cargo test` → sees 12 independent failures in 12 unrelated crates → could parallelise from here.
- User: *"update dependencies"* → main agent reads `Cargo.toml` → sees 40 crates to bump-and-verify → could parallelise from here.

In each case, the parallelism is not visible from the raw user message. Only after the main agent does discovery work does the structure become clear. JIT gives the agent the option to exploit it.

### 1.3 Goals

1. **Zero behavioural regression.** If the model never calls `spawn_swarm`, system behaviour is identical to today's single-agent loop.
2. **Upside ceiling bounded by Hive's existing gates.** Swarm only activates when speedup ≥ 1.3× and Queen decomposition cost ≤ 10% of single-agent cost (`hive::config.rs:185-189`). Those gates already exist and already work.
3. **Main-agent output quality parity.** Worker outputs merged via the main agent's own synthesis, not Hive's text-join — so the user-facing voice and reasoning stay consistent.
4. **No new failure modes that aren't bounded by existing safety nets** (budget cap, duration cap, interrupt flag, cancellation token).

### 1.4 Non-goals

- Distributed multi-machine swarm (single-process, as v1).
- Removing the dispatch-time Hive route (see `DISPATCHER_EVOLUTION.md` — it may stay for cost-saving on obvious cases).
- Speculative/competitive execution (N workers trying same task with different strategies). Theoretically interesting, out of scope.

---

## 2. Ground Truth — What Exists Today

All claims here are verified against the current codebase.

### 2.1 Hive API surface

```rust
// crates/temm1e-hive/src/lib.rs:116
pub async fn maybe_decompose<F, Fut>(
    &self,
    message: &str,
    chat_id: &str,
    provider_call: F,
) -> Result<Option<String>, Temm1eError>
where F: Fn(String) -> Fut, Fut: Future<Output = Result<(String, u64), Temm1eError>>;

// crates/temm1e-hive/src/lib.rs:250
pub async fn execute_order<F, Fut>(
    &self,
    order_id: &str,
    cancel: CancellationToken,
    execute_fn: F,
) -> Result<SwarmResult, Temm1eError>
where F: Fn(HiveTask, Vec<(String, String)>) -> Fut + Send + Sync + 'static,
      Fut: Future<Output = Result<TaskResult, Temm1eError>> + Send;
```

Two pure-closure entry points. No trait objects. No references to `AgentRuntime`. **The API is already JIT-ready.**

### 2.2 Dependency shape

`crates/temm1e-hive/Cargo.toml` depends on `temm1e-core` only. `crates/temm1e-agent/Cargo.toml` does not depend on `temm1e-hive`. **Zero circular-dep risk** when the agent crate starts importing Hive.

### 2.3 Current worker execution contract

`main.rs:5318-5358` shows the live dispatch-time worker closure:

```rust
let mini = temm1e_agent::AgentRuntime::with_limits(
    p, m_clone, t, mdl, None,
    10,      // max_calls
    30000,   // max_tokens
    50,      // step_timeout
    300,     // idle_timeout
    0.0,     // cost_override
);
let mut s = SessionContext {
    session_id: format!("hive-{}", task.id),
    history: vec![],   // ← BLANK
    ...
};
```

**Observations:**
- `max_calls = 10` is tight. A worker handling a non-trivial subtask (read two files, make three edits, verify) can run out. For JIT, where the main agent has already used significant budget discovering parallelism, we need a more flexible worker limit.
- `history: vec![]` means workers start blank — no inheritance of what the main agent learned. For dispatch-time this is correct (nothing to inherit). For JIT this is the **largest quality regression risk.**
- Same provider, same tools — tool parity is already good.

### 2.4 Aggregation

`lib.rs:400-432` — `aggregate_results()` plain-string-joins worker outputs. **No synthesis LLM call.** For JIT, this would produce a stitched response in the user's reply stream. That's unacceptable for quality parity → we need a different synthesis strategy.

### 2.5 Hive default config

`hive::config.rs`:
- `max_workers = 3`
- `min_workers = 1`
- `swarm_threshold_speedup = 1.3`
- `queen_cost_ratio_max = 0.10`
- `budget_overhead_max = 1.15`
- `blocker.max_task_duration_secs = 1800` (30 min)
- `blocker.max_retries = 3`

### 2.6 Budget plumbing gap

`SwarmResult.total_tokens` is returned from `execute_order` but **never fed back** into the parent `BudgetTracker`. Swarm currently bypasses the per-message budget cap. This is a latent bug regardless of JIT, but JIT amplifies its blast radius.

### 2.7 Rate-limit handling (not in Hive, in providers)

`anthropic.rs:347`, `openai_compat.rs:677`: on HTTP 429, rotate API key, return `Temm1eError::RateLimited`. **No retry. No `retry-after` header parsing. No per-provider concurrency limit.** Any aggressive parallelism will hammer the API.

---

## 3. Design — The `spawn_swarm` Tool

### 3.1 Why a tool, not an implicit check

Three alternatives considered:

| Option | Decision driver |
|---|---|
| A. **Tool (model-called)** | Model already identifies structure; tool is opt-in; worst case is "never called" = identical to today |
| B. Implicit mid-loop checker | Costs an extra LLM call per K iterations; second-guesses the model; no evidence we'd out-decide the model |
| C. Threshold-triggered (budget % / iter count) | Reactive not anticipatory; layered on top of A is a future enhancement, not a substitute |

**Decision: A.** Revisit C as a *prompt hint* to the model ("you've run 100+ tool calls without converging — consider `spawn_swarm`") if telemetry ever shows the model systematically misses obvious parallel structure.

### 3.2 Tool schema

```json
{
  "name": "spawn_swarm",
  "description": "Spawn parallel worker Tems to handle N independent subtasks in parallel. Use when you have identified multiple units of work that have no sequential dependency on each other. Workers receive the shared context you provide plus their individual task. Returns the aggregated text outputs; you compose the final user-facing reply from them.\n\nOnly call this when (a) you can enumerate ≥2 truly independent subtasks and (b) running them in parallel is meaningfully faster than sequential. For dependent work, run tools yourself.",
  "input_schema": {
    "type": "object",
    "required": ["goal", "shared_context"],
    "properties": {
      "goal": {
        "type": "string",
        "description": "The overall user-facing goal this swarm is serving. Used by the Queen to validate the decomposition."
      },
      "shared_context": {
        "type": "string",
        "description": "Everything workers need to know that you have already discovered. Files you've read, findings, conventions, constraints. Workers start blank — this is their only inheritance."
      },
      "subtasks": {
        "type": "array",
        "description": "Optional: your own decomposition. If provided, the Queen's LLM decomposition step is skipped (1 LLM call saved). Each subtask runs as an independent worker.",
        "items": {
          "type": "object",
          "required": ["description"],
          "properties": {
            "description": {"type": "string"},
            "depends_on": {"type": "array", "items": {"type": "string"}, "description": "IDs of earlier subtasks this one waits for"},
            "writes_files": {"type": "array", "items": {"type": "string"}, "description": "Files this subtask may write — used for writer-exclusion"}
          }
        }
      }
    }
  }
}
```

Two modes in one tool:
1. **Explicit decomposition** — model provides `subtasks`. Hive skips Queen, goes straight to `execute_order`.
2. **Queen decomposition** — model provides only `goal` + `shared_context`. Hive calls Queen first.

The tool description intentionally stresses *independence* — the model's decision bar should be high.

### 3.3 Return value

```rust
struct SwarmToolResult {
    aggregated_text: String,        // Hive's text-join output
    per_task: Vec<TaskOutcome>,     // individual worker outputs for model's inspection
    total_tokens_used: u64,
    total_cost_usd: f64,
    escalated_tasks: Vec<(String, String)>,  // (id, error) for tasks the workers couldn't complete
}
```

The tool returns to the main agent as a normal `ToolResult`. The main agent's next provider call sees the structured result and composes the final user-facing reply in its own voice. **No separate synthesis LLM call — the main agent's loop is the synthesis.**

---

## 4. Critical Guardrails

### 4.1 Recursion block

**Hard invariant:** workers cannot spawn further swarms.

Mechanism: before spawning the per-worker `AgentRuntime`, the closure sets `TEMM1E_IN_SWARM=1` on the runtime's tool filter. The tool-list assembled for the worker omits `spawn_swarm`. The model physically cannot call it.

Alternative rejected: checking `TEMM1E_IN_SWARM` from inside the tool handler. That relies on runtime detection — if detection fails, we get unbounded recursion. Filtering the tool list is fail-safe: a bug in detection just makes the tool absent, never present-when-shouldn't-be.

### 4.2 Budget plumbing

When `spawn_swarm` returns, the tool handler **must** call:
```rust
self.budget.record_usage(
    swarm_result.total_input_tokens,
    swarm_result.total_output_tokens,
    swarm_result.total_cost_usd,
);
```
before returning to the loop. This is a prerequisite fix. See `PREREQUISITES.md`.

### 4.3 Cancellation propagation

The parent agent's `CancellationToken` (or interrupt flag — see §8.2) is cloned into `Hive::execute_order`. When the user sends a Stop (or external interrupt fires), all outstanding workers receive the signal and exit gracefully. Hive's worker loop already checks `cancel.is_cancelled()` (`worker.rs:83`) — wiring is in place.

### 4.4 Per-worker duration + call caps

JIT workers need more room than dispatch-time workers (which hardcode `max_calls=10`). Proposed:
```rust
// JIT worker limits (configurable via HiveConfig)
max_calls:      60,    // today's hardcoded 10 is too tight for JIT
max_tokens:     60000,
step_timeout:   120,
idle_timeout:   300,
```
Still bounded, still safe, but large enough that a worker can do real work. These values become `HiveConfig` fields.

### 4.5 Writer-exclusion (file-write collision prevention)

The danger: two parallel workers both write the same file. Hive's DAG dependency model captures *explicit* dependencies; implicit file conflicts are invisible to it.

**Layer 1 — advisory (ship first):** the `subtasks` schema exposes `writes_files`. When the model provides explicit decomposition, the tool handler pre-validates: if two subtasks declare overlapping `writes_files`, the tool rejects the call with a structured error ("subtask A and subtask B both write `src/main.rs` — sequence them instead"). The model retries with a corrected DAG.

**Layer 2 — Queen prompt amendment (ship first):** when the model uses Queen decomposition (no explicit subtasks), the Queen prompt is amended: *"for each task, list files you'll write. If two tasks target the same file, mark one as dependent on the other."* This relies on Queen quality — acceptable for v1 because Queen is already a single LLM call we trust for decomposition quality.

**Layer 3 — enforced FileLockRegistry (defer):** a runtime-enforced lock per path. Workers that lose a lock wait. Heavy; only ship if Layer 1+2 telemetry shows real collisions.

### 4.6 Shared-context injection

The **largest quality regression risk** in JIT is worker context poverty. The main agent has built up session history, read files, made design choices. Workers starting blank means re-discovery.

Shape of the fix:

```rust
// In spawn_swarm tool handler, building the worker closure:
let initial_user_message = format!(
    "## Context from parent Tem\n{}\n\n\
     ## Your task\n{}\n\n\
     ## Results from dependency tasks\n{}",
    shared_context,
    task.description,
    format_dep_results(&dep_results),
);
```

`shared_context` comes from the tool call itself — the model writes it as a summary of what it has learned. This places the responsibility on the model, not on heuristic extraction. The model's first-hand knowledge of what's relevant is almost always better than a mechanical snapshot.

**Token cost:** ~500-1500 tokens of shared context per worker, paid N times. At scale this matters — but it is what makes swarm output equivalent quality to sequential single-agent work. If we cut it, we regress quality; if we keep it, cost is proportional to work.

### 4.7 Rate-limit handling (prerequisite)

JIT is a rate-limit risk multiplier. A 10-worker swarm issues 10 concurrent streams; on tier-1 Anthropic (50 RPM), a burst of 10 is 20% of the per-minute budget. Today's provider returns `RateLimited` immediately on 429.

Before raising `max_workers` beyond 3, providers must:
1. Detect 429.
2. Parse `retry-after` header (Anthropic sends this; OpenAI sends `x-ratelimit-*` values).
3. Backoff with jitter, retry up to N attempts (N=3).
4. Only return `RateLimited` if all retries exhausted.

This is a prerequisite change. See `PREREQUISITES.md`.

### 4.8 Timeout defence in depth

Three tiers:
1. **Per-worker timeout** (`HiveConfig.blocker.max_task_duration_secs`, default 1800). A stuck worker exits.
2. **Per-swarm timeout** (new field, default 3600). Entire swarm cancels if wall-clock exceeded.
3. **Parent duration cap** (already exists, `AgentConfig.max_task_duration_secs`). Cancellation propagates to swarm.

---

## 5. Failure Mode Matrix

Every known failure path, probability, impact, and mitigation. Anything red or orange must have a mitigation that is already in place or explicitly listed as a prerequisite.

| # | Failure | P | Impact | Mitigation | Status |
|---|---|---|---|---|---|
| 1 | Worker lacks parent's context | **H** | Quality regression | §4.6 shared_context injection | Design |
| 2 | Queen produces malformed decomposition | M | Wasted tokens | Existing 3-retry loop in `maybe_decompose`; None → tool returns "decomposition failed, continuing single-agent" | Exists |
| 3 | Worker hallucinates tool result | M | Bad output | Same as main agent — verification section in system prompt (post-collapse) | After collapse |
| 4 | Two workers mutate same file | **H** | Corruption | §4.5 Layer 1 (advisory rejection) + Layer 2 (Queen prompt) | Design |
| 5 | Rate limit hit mid-swarm | **H** | Partial output + cryptic error | Prereq: §4.7 429 retry in providers | Prerequisite |
| 6 | Nested swarm (worker spawns swarm) | Critical | Cost explosion | §4.1 tool filter removes `spawn_swarm` from worker toolset | Design |
| 7 | Budget exhausted mid-swarm | M | Partial results | Existing cancellation; + §4.2 budget plumbing fix | Prerequisite |
| 8 | Worker crashes (panic) | L | Lost subtask | `panic = "unwind"` + catch_unwind in worker (existing resilience) + Hive retry | Exists |
| 9 | Worker stuck in loop | M | Swarm hangs on one task | §4.4 per-worker timeout; + stagnation detection (prerequisite, see §6 of dispatcher doc) | Partial |
| 10 | Synthesis quality from text-join | **H** | Stitched-sounding reply | §3.3 — main agent re-synthesizes; Hive's text-join is only the tool result, not the user-facing output | Design |
| 11 | Shared context too large | M | Token blow-up per worker | Soft cap (e.g. 2000 tokens on shared_context arg); tool rejects oversized calls with "trim your context" | Design |
| 12 | Model calls swarm for sequential work | M | Wasted LLM budget on failed decomposition | Queen's activation gates (speedup ≥ 1.3, cost ≤ 10%) reject bad calls; tool returns "not beneficial" | Exists |
| 13 | Swarm started, parent cancels mid-way | L | Leaked worker tasks | §4.3 cancellation propagation; workers exit on token | Design |
| 14 | Workers all select the same claim | L | One executes, others wait/reselect | Atomic SQLite claim semantics in `blackboard.rs` — existing behaviour | Exists |
| 15 | Subtask declares `writes_files` but also reads it without `depends_on` | M | Worker sees stale state | Advisory: Queen prompt warns; runtime validation catches on rejection | Partial |

P = probability (L/M/H), bolded = requires explicit design work.

---

## 6. `max_workers` — What's the Right Default?

### 6.1 Hardware is not the constraint

Tokio tasks are ~1KB. 50 concurrent tasks on any modern laptop is noise. Memory: each `AgentRuntime` holds session state (~10-50KB), so 50 workers ≈ 2.5MB. Network: 50 parallel HTTPS streams at ~100 tokens/sec each is well under any home-internet bandwidth.

### 6.2 API rate limits are the constraint

| Provider | Tier 1 | Tier 4+ |
|---|---|---|
| Anthropic | 50 RPM, 50K ITPM | 4K RPM, 400K OTPM, 2M ITPM |
| OpenAI | 500 RPM | 30K RPM (tier 5) |
| Gemini | 360 RPM | 1K+ RPM |
| OpenRouter | Depends on downstream | Depends on downstream |

A 10-worker swarm with 5 calls/worker over 30s = 100 RPM. Fine on tier-2+ everywhere. Problematic on Anthropic tier-1 (50 RPM) — would 429 the other 50%.

### 6.3 Recommendation

| Stage | `max_workers` | Requires |
|---|---|---|
| v0 (today) | **3** | — (existing default) |
| v1 (JIT launch) | **6** | §4.7 429 handling fix |
| v2 (adaptive) | **10+** | §4.7 + runtime tier detection (read rate-limit headers, reduce parallelism on 429) |

Config knob stays `HiveConfig.max_workers`. Documentation updated: tier-1 Anthropic users should explicitly set `max_workers = 3` in `temm1e.toml`.

---

## 7. Dispatch-Time Hive vs JIT Hive — Keep Both?

**Yes, keep both.** They serve different cost profiles:

- **Dispatch-time Hive** (`Err(HiveRoute)`, `runtime.rs:964-972`) fires when the classifier recognises parallelism on the first read. Saves the main-agent turn that JIT would need to "look, discover, decide, spawn". Costs: the classifier's ~1.1k tokens.
- **JIT Hive** (`spawn_swarm` tool) fires when parallelism is only visible after investigation. Catches the cases dispatch-time misses.

If we removed dispatch-time, every parallel case would pay the main-agent discovery cost. If we removed JIT, mid-flight parallelism is invisible. They're complementary.

See `DISPATCHER_EVOLUTION.md` for how the classifier itself should evolve.

---

## 8. Open Design Questions

### 8.1 Should shared_context ever be auto-generated from session history?

Two options:
- (a) Model writes it (current design). Model-chosen relevance, model voice.
- (b) Extract last K turns mechanically. No extra cognitive burden on model, but possibly includes irrelevant detail.

**Resolution**: ship (a). Revisit if telemetry shows models under-filling `shared_context`.

### 8.2 Cancellation: interrupt flag vs CancellationToken?

Runtime currently uses `Arc<AtomicBool>` interrupt flag (`runtime.rs:1125`). Hive uses `CancellationToken` (`lib.rs:253`). They must unify. Cleanest: wrap the atomic in a `CancellationToken` adapter so both paths check the same ground truth.

### 8.3 Should per-worker system prompt be different from parent's?

Workers have a scoped task with full context injected as the first user message. A *Basic* tier prompt (~800 tokens) would save tokens at scale. But we're collapsing to a single prompt (see prompt-caching plan). So: worker and parent share the same system prompt; it's cached on Anthropic anyway. Resolution: same prompt.

### 8.4 Tool filter: dynamic or static?

When `TEMM1E_IN_SWARM=1`, we remove `spawn_swarm` from the tool list. Should other tools also be filtered (e.g., risky ones)? For v1: no, keep worker toolset = parent toolset minus swarm. If we see runaway behaviour, add specific tool exclusions.

### 8.5 How does Eigen-Tune see JIT swarm costs?

Eigen-Tune currently observes the dispatcher. JIT costs flow through the main agent's tool path. Cost attribution to swarm needs explicit tagging in BudgetTracker so Eigen-Tune can report "of N turns, M spawned swarm, avg swarm ratio was X". Minor telemetry work; does not block launch.

---

## 9. Prerequisites

See `PREREQUISITES.md` for detailed scenario matrices. Summary:

| # | Prereq | Why | Risk if skipped |
|---|---|---|---|
| P1 | 429 retry + backoff in providers | Parallelism multiplier → rate-limit pressure | Aggressive swarms produce cryptic `RateLimited` errors |
| P2 | Anthropic `cache_control` on system prompt | Per-worker prompt cost is N× parent | Swarm becomes economically unattractive |
| P3 | Budget plumbing (`SwarmResult` → `BudgetTracker`) | Swarm currently bypasses cap | User with `max_spend_usd` set gets overbilled |
| P4 | Replace 200-call ceiling with stagnation + budget + duration | Workers may legitimately run long | Arbitrary stops mid-swarm; user-facing blank replies |
| P5 | Collapse prompt stratification (post-caching) | Worker + parent share cached prompt | Today's tier mapping gives workers weaker prompts |

P1-P3 are JIT-specific prerequisites. P4-P5 are broader improvements that JIT benefits from. See sequencing in §10.

---

## 10. Implementation Sequencing

Each step is independently shippable and ZERO-RISK after its own scenario matrix. Order chosen so each unblocks the next without coupling.

```
┌─────────────────────────────────────────────────────────────┐
│  1. Fix 429 handling in providers (P1)                     │
│     → standalone value; safe to ship regardless of swarm   │
├─────────────────────────────────────────────────────────────┤
│  2. Wire cache_control on Anthropic system prompt (P2)     │
│     → standalone value; cost optimization                  │
├─────────────────────────────────────────────────────────────┤
│  3. Replace 200-call ceiling with stagnation/budget (P4)   │
│     → standalone value; removes arbitrary limit            │
├─────────────────────────────────────────────────────────────┤
│  4. Fix budget plumbing for existing dispatch-time Hive(P3)│
│     → closes latent bug; required for safe JIT             │
├─────────────────────────────────────────────────────────────┤
│  5. Collapse prompt stratification (P5)                    │
│     → post-caching, cost delta is near-zero                │
│     → simplifies classifier surface (see dispatcher doc)   │
├─────────────────────────────────────────────────────────────┤
│  6. Simplify classifier (see DISPATCHER_EVOLUTION.md)      │
│     → reduced surface → fewer mis-classifications          │
├─────────────────────────────────────────────────────────────┤
│  7. Add spawn_swarm tool + guardrails                      │
│     → JIT launch                                           │
├─────────────────────────────────────────────────────────────┤
│  8. Raise max_workers default 3 → 6                        │
│     → post-429-fix, safe to parallelise more aggressively  │
└─────────────────────────────────────────────────────────────┘
```

**Stopping points:** the system delivers real value at each horizontal line. If we stop after step 3, we've removed the 200 ceiling and fixed billing. If we stop after step 5, we've collapsed the prompt and removed the mis-tier regression. If we stop after step 7, we have JIT. If we stop after step 8, JIT is tuned for throughput.

Every step: write the scenario matrix first, confirm ZERO-RISK, then implement. Per memory policy.

---

## 11. Appendix — Scenario Matrix Template (for future PR)

When any of steps 1-8 moves to implementation, the PR must include a scenario matrix. Template:

```
## Scenario Matrix — <step name>

### Scenarios that MUST remain unchanged

| Scenario | Expected behaviour | Verified by |
|---|---|---|
| User with no swarm config, simple chat | identical to v5.3.5 | manual CLI test + existing tests |
| ... | ... | ... |

### Scenarios that change (intentional)

| Scenario | Before | After | Why |
|---|---|---|---|
| ... | ... | ... | ... |

### Scenarios that COULD regress

| Scenario | Risk | Mitigation | How we'd notice |
|---|---|---|---|
| ... | ... | ... | ... |
```

No step moves to code without its matrix filled in, reviewed, and confirmed ZERO-RISK.

---

## 12. References

- `docs/design/DISPATCHER_REWORK.md` — v5.3.5 Chat-bypass removal.
- `tems_lab/swarm/DESIGN.md` — v1 dispatch-time Hive design.
- `tems_lab/swarm/FINAL_REPORT.md` — v1 implementation report.
- `tems_lab/swarm/PREREQUISITES.md` — prerequisite scenario matrices (TBD as prereqs are scoped).
- `tems_lab/swarm/DISPATCHER_EVOLUTION.md` — what the dispatcher becomes after JIT + prompt collapse.
