# TEMM1E Hive: Stigmergic Swarm Intelligence Runtime

## Zero-Risk Design Document v1.0

**Date:** 2026-03-17
**Branch:** `many-tems`
**Status:** Design Complete → Implementation Ready

---

## 0. Design Philosophy

This document adapts the TEMM1E Hive Swarm Spec into a zero-risk implementation plan grounded in the actual TEMM1E codebase (v2.8.1, 16 crates, 1312 tests).

**Core principle:** The Hive is a new leaf crate (`temm1e-hive`) that depends only on `temm1e-core`. It does not modify any existing crate. Integration with the agent runtime and dispatcher happens through feature-gated, opt-in code paths. When `[hive] enabled = false` (the default), the system is byte-identical to pre-Hive TEMM1E.

**What we're building:** A coordination layer that lets multiple TEMM1E agent workers process subtasks of a complex order in parallel, communicating through a shared SQLite blackboard and a pheromone signal field — not through LLM-to-LLM chat.

**What we're NOT building (yet):**
- Distributed multi-machine swarm (single-process, multi-task for v1)
- Cross-network agent communication
- Blueprint evolution Loops 3-4 (deferred to v2)

---

## 1. System Axioms (Adapted for TEMM1E)

These five invariants are non-negotiable. Every mechanism must preserve all five.

**A1 — Goal Completion.**
Every accepted order reaches SUCCESS or ESCALATE. No ABANDONED state. If the swarm cannot solve a task after bounded retries, it produces a failure report and falls back to single-agent mode (not silence).

**A2 — Budget Boundedness.**
Total token expenditure: `Σ C(wᵢ, tⱼ) ≤ Ω`. The Hive inherits TEMM1E's existing `BudgetTracker` (atomic USD tracking, per-model pricing). No new budget mechanism needed — the Hive respects the same `max_spend_usd` cap.

**A3 — Monotonic Progress.**
`P(t) = |completed tasks| / |total tasks|` is non-decreasing. Completed tasks are never reverted. If downstream work invalidates an earlier result, a NEW correction task is created.

**A4 — Graceful Degradation.**
All state lives on the Blackboard (SQLite). If N-1 workers panic, the surviving worker reads state from SQLite and continues. This leverages TEMM1E's existing resilience: `panic = "unwind"`, `catch_unwind()` wrappers, dead worker detection.

**A5 — Cost Dominance.**
`C_swarm(order) ≤ 1.15 × C_single(order)`. The swarm activates only when parallelism is worth it. Simple tasks use single-agent mode with zero overhead.

---

## 2. Architecture: How Hive Fits Into TEMM1E

### 2.1 Current Architecture (unchanged)

```
Channel → mpsc → Dispatcher → ChatSlot(per-chat worker) → AgentRuntime → Provider
```

Each chat gets one serial worker. Messages within a chat are processed sequentially.

### 2.2 Hive Architecture (additive)

```
Channel → mpsc → Dispatcher → ChatSlot → HiveOrchestrator (NEW)
                                              │
                                    ┌─────────┼─────────┐
                                    │         │         │
                               Worker₁   Worker₂   Worker₃
                               (task₁)   (task₂)   (task₃)
                                    │         │         │
                                    └────┬────┘────┬────┘
                                         │         │
                                    Blackboard  Pheromone
                                    (SQLite)    Field
```

**Key difference:** The HiveOrchestrator sits between the ChatSlot and AgentRuntime. For simple messages, it passes through directly (zero overhead). For complex orders that decompose into parallelizable subtasks, it spawns multiple workers.

### 2.3 Integration Points (exactly 3 files touched)

| File | Change | Risk |
|------|--------|------|
| `Cargo.toml` (workspace) | Add `temm1e-hive` to members | ZERO — additive |
| `crates/temm1e-core/src/types/config.rs` | Add `HiveConfig` struct (serde default) | ZERO — new field with Default, existing configs parse unchanged |
| `src/main.rs` | Feature-gated Hive initialization in dispatcher | LOW — behind `if config.hive.enabled` |

### 2.4 Dependency Graph

```
temm1e-hive
├── temm1e-core (traits, types, errors)
├── sqlx (SQLite — already a workspace dep)
├── serde + serde_json (already workspace deps)
├── tokio (already workspace dep)
├── tracing (already workspace dep)
├── uuid (already workspace dep)
└── chrono (already workspace dep)
```

No new external dependencies. Every dep is already in the workspace.

---

## 3. The Blackboard (Task DAG)

### 3.1 Task State Machine

```
PENDING → READY → ACTIVE → COMPLETE
                     │
                     ├→ BLOCKED → RETRY → (back to READY)
                     │              └→ ESCALATE (max retries)
                     └→ ESCALATE (budget exceeded)
```

Terminal states: COMPLETE, ESCALATE.

### 3.2 SQLite Schema

```sql
-- Core task table
CREATE TABLE IF NOT EXISTS hive_tasks (
    id              TEXT PRIMARY KEY,
    order_id        TEXT NOT NULL,
    description     TEXT NOT NULL,
    status          TEXT NOT NULL DEFAULT 'pending',
    claimed_by      TEXT,                    -- worker ID
    dependencies    TEXT DEFAULT '[]',       -- JSON array of task IDs
    context_tags    TEXT DEFAULT '[]',       -- JSON array of string tags
    estimated_tokens INTEGER DEFAULT 0,
    actual_tokens   INTEGER DEFAULT 0,
    result_summary  TEXT,                    -- compact output for dependents
    artifacts       TEXT DEFAULT '[]',       -- JSON array of artifact paths
    retry_count     INTEGER DEFAULT 0,
    max_retries     INTEGER DEFAULT 3,
    error_log       TEXT,                    -- last error message
    created_at      INTEGER NOT NULL,        -- unix epoch ms
    started_at      INTEGER,
    completed_at    INTEGER
);

CREATE INDEX IF NOT EXISTS idx_hive_tasks_status ON hive_tasks(status, order_id);
CREATE INDEX IF NOT EXISTS idx_hive_tasks_order ON hive_tasks(order_id);

-- Order tracking
CREATE TABLE IF NOT EXISTS hive_orders (
    id              TEXT PRIMARY KEY,
    chat_id         TEXT NOT NULL,
    original_message TEXT NOT NULL,
    task_count      INTEGER NOT NULL,
    completed_count INTEGER DEFAULT 0,
    status          TEXT NOT NULL DEFAULT 'active',  -- active, completed, failed
    total_tokens    INTEGER DEFAULT 0,
    queen_tokens    INTEGER DEFAULT 0,
    created_at      INTEGER NOT NULL,
    completed_at    INTEGER
);
```

### 3.3 Atomic Task Claiming

```sql
BEGIN;
UPDATE hive_tasks SET status = 'active', claimed_by = ?1
WHERE id = ?2 AND status = 'ready';
-- rows_affected = 0 → another worker claimed it, re-select
COMMIT;
```

SQLite's write serialization provides mutual exclusion. No distributed locks needed.

### 3.4 Dependency Resolution

When a task completes, check all tasks with it as a dependency:

```sql
-- Find tasks whose dependencies are now all met
SELECT id FROM hive_tasks
WHERE order_id = ?1 AND status = 'pending'
AND id NOT IN (
    SELECT ht.id FROM hive_tasks ht
    JOIN json_each(ht.dependencies) dep ON dep.value IN (
        SELECT id FROM hive_tasks WHERE status != 'complete'
    )
    WHERE ht.order_id = ?1
);
-- Transition these to 'ready'
```

Cycle detection uses Kahn's algorithm at decomposition time — reject any DAG with cycles before execution begins.

---

## 4. Pheromone Field

### 4.1 Data Model

A pheromone is a time-decaying signal: `Φ(t) = I₀ · e^(-ρ · (t - t₀))`

```sql
CREATE TABLE IF NOT EXISTS hive_pheromones (
    id          INTEGER PRIMARY KEY AUTOINCREMENT,
    signal_type TEXT NOT NULL,        -- completion, failure, difficulty, urgency, etc.
    target      TEXT NOT NULL,        -- task ID or resource path
    intensity   REAL NOT NULL,
    decay_rate  REAL NOT NULL,
    emitter     TEXT,                 -- worker ID
    metadata    TEXT,                 -- JSON blob
    created_at  INTEGER NOT NULL      -- unix epoch ms
);

CREATE INDEX IF NOT EXISTS idx_pheromones_lookup
ON hive_pheromones(signal_type, target);
```

### 4.2 Signal Types

| Signal | Decay (per sec) | Default I₀ | Purpose |
|--------|----------------|-----------|---------|
| `completion` | 0.003 | 1.0 | Task finished |
| `failure` | 0.002 | 1.0 | Attempt failed |
| `difficulty` | 0.006 | 0.5–1.0 | Worker struggling |
| `urgency` | -0.001 (grows) | 0.1 | Task waiting (capped at 5.0) |
| `progress` | 0.035 | 0.5 | Worker heartbeat |
| `help_wanted` | 0.006 | 1.0 | Need specialist |

### 4.3 Reading the Field

Intensity at time `now`:

```rust
fn intensity_at(&self, now_ms: i64) -> f64 {
    let dt = (now_ms - self.created_at) as f64 / 1000.0;
    let value = self.intensity * (-self.decay_rate * dt).exp();
    if self.decay_rate < 0.0 {
        value.min(5.0)  // urgency cap
    } else {
        value
    }
}
```

Total for a (signal_type, target) pair: sum all matching pheromones' current intensities (linear superposition).

### 4.4 Garbage Collection

Every 10 seconds, delete pheromones where `Φ(now) < 0.01`.

---

## 5. Queen Decomposition

### 5.1 When to Decompose

The Queen is NOT an always-on entity. It's a single LLM call that runs when a message arrives and the Hive is enabled.

**Activation check (pure arithmetic, no LLM):**
1. Is `[hive] enabled = true`?
2. Does the message look complex enough? (heuristic: length > 200 chars, or contains structural markers like numbered lists, "and then", "also", semicolons)

If yes → one LLM call to decompose into a task DAG.
If no → pass through to single-agent mode.

### 5.2 Decomposition Prompt

```
You are a task decomposer. Given a user's request, break it into atomic subtasks
that can be executed independently or in parallel where possible.

Return a JSON object:
{
  "tasks": [
    {
      "id": "t1",
      "description": "...",
      "dependencies": [],
      "context_tags": ["rust", "api"],
      "estimated_tokens": 3000
    },
    ...
  ],
  "single_agent_recommended": false,
  "reasoning": "..."
}

Rules:
- Each task must be completable by one agent in one tool loop
- Minimize dependencies — maximize parallelism
- If the request is simple (1-2 tasks), set single_agent_recommended: true
- Estimated tokens should be conservative (overestimate by 20%)
```

### 5.3 Swarm Activation Threshold

After decomposition, compute:

```
S_max = total_estimated_time / critical_path_time
```

Activate swarm if:
- `S_max ≥ 1.3` (meaningful speedup possible)
- `queen_tokens < 0.10 × estimated_single_cost` (decomposition wasn't too expensive)
- `single_agent_recommended == false`

If any condition fails → single-agent mode, zero overhead.

---

## 6. Worker Task Selection

### 6.1 Score Function (Pure Arithmetic — No LLM)

```
S(worker, task) = A^α · U^β · (1-D)^γ · (1-F)^δ · R^ζ
```

Where:
- **A (Affinity):** Jaccard similarity of worker's recent context_tags vs task's context_tags. Floor = 0.1.
- **U (Urgency):** Pheromone field total for `urgency` on this task. Grows over time.
- **D (Difficulty):** Pheromone field total for `difficulty`. High = others struggled.
- **F (Failure):** Pheromone field total for `failure`. High = others failed.
- **R (Downstream Reward):** `1 + |dependents| / |total_tasks|`. High = unblocks more work.

### 6.2 Exponents

```
α = 2.0  (strong expertise preference)
β = 1.5  (moderate urgency pressure)
γ = 1.0  (linear difficulty avoidance)
δ = 0.8  (mild failure avoidance)
ζ = 1.2  (moderate downstream reward)
```

### 6.3 Tie-Breaking

Scores within 5% of each other → random selection (prevents herding).

---

## 7. Worker Execution Model

### 7.1 Scoped Context (The Cost Savings)

Each worker gets a **task-scoped** AgentRuntime context:
- System prompt (standard TEMM1E system prompt)
- Task description
- Results from dependency tasks (NOT full conversation history)
- Tools relevant to the task (subset of all tools)
- Blueprint if matched

**This is where the quadratic→linear savings come from.** A single agent carrying full history pays `h̄ · m(m+1)/2`. Workers carrying only dependency results pay `m · R̄` where R̄ is bounded by dependency count (typically 1-3), not total subtask count.

### 7.2 Worker Lifecycle

```
1. Select READY task (§6 score function)
2. Claim task (atomic SQLite transaction)
3. Build scoped context (task desc + dependency results + tools)
4. Run AgentRuntime.process_message() with scoped context
5. On completion:
   a. Write result_summary to Blackboard
   b. Emit `completion` pheromone
   c. Transition dependents from PENDING → READY
6. On failure:
   a. Emit `failure` + `difficulty` pheromones
   b. Increment retry_count
   c. If retry_count < max_retries → BLOCKED → RETRY → READY
   d. If retry_count >= max_retries → ESCALATE
7. Look for next READY task (loop back to 1)
```

### 7.3 Result Aggregation

When all tasks in an order are COMPLETE, the Hive assembles the final response:
- Collect all `result_summary` values in topological order
- Concatenate with task descriptions as headers
- Return as a single OutboundMessage to the user

If any task ESCALATED, include the escalation report in the final message.

---

## 8. Blocker Resolution (Simplified for v1)

| Blocker | Detection | Resolution |
|---------|-----------|------------|
| Tool failure | Tool returns error | Retry with backoff (max 3) |
| Budget exceeded | BudgetTracker limit hit | BLOCKED → another worker with fresh context |
| Dependency stall | Upstream task > 5 min | Emit `urgency` pheromone |
| All retries failed | retry_count >= max_retries | ESCALATE → fall back to single-agent for this subtask |

**v1 simplification:** No resource conflict resolution (workers operate on independent subtasks). No sub-decomposition on ESCALATE (fall back to single-agent instead). These are v2 features.

---

## 9. Blueprint System (v1: Read-Only)

### 9.1 Blueprint Matching

Workers match tasks against existing blueprints in memory using context_tags:

```rust
fn blueprint_affinity(blueprint_tags: &HashSet<String>, task_tags: &HashSet<String>) -> f64 {
    if blueprint_tags.is_empty() || task_tags.is_empty() {
        return 0.1; // floor
    }
    let intersection = blueprint_tags.intersection(task_tags).count();
    let union = blueprint_tags.union(task_tags).count();
    (intersection as f64 / union as f64).max(0.1)
}
```

### 9.2 Blueprint Creation

On task completion, extract a blueprint:
- Task description → blueprint title
- Tools used → step list
- Context tags → blueprint tags
- Token usage → avg_tokens

Store as `MemoryEntryType::Blueprint` (already in the enum).

### 9.3 Deferred to v2

- Fitness tracking (success_count / times_used)
- Adaptation recording
- Variant spawning
- Blueprint pruning

---

## 10. Configuration

```toml
[hive]
enabled = false                    # MUST be explicitly enabled
min_workers = 1
max_workers = 3                    # conservative default
swarm_threshold_speedup = 1.3
queen_cost_ratio_max = 0.10
budget_overhead_max = 1.15

[hive.pheromone]
gc_interval_secs = 10
evaporation_threshold = 0.01
urgency_cap = 5.0

[hive.selection]
alpha = 2.0
beta = 1.5
gamma = 1.0
delta = 0.8
zeta = 1.2
tie_threshold = 0.05

[hive.blocker]
max_retries = 3
max_task_duration_secs = 300
```

---

## 11. Resilience Guarantees

| Threat | Mitigation |
|--------|-----------|
| Worker panic | `catch_unwind()` wrapper (inherits TEMM1E resilience). Dead worker → task returns to READY. |
| SQLite corruption | WAL mode + connection pooling (inherits from TaskQueue pattern) |
| Budget runaway | BudgetTracker cap enforced per-worker. Hive total = sum of worker costs. |
| Infinite retry loop | Hard cap: max_retries=3, max ESCALATE depth=1 (v1) |
| Queen produces bad DAG | Cycle detection (Kahn's algorithm). Reject + single-agent fallback. |
| All workers stuck | Urgency pheromone grows → forces any idle worker to take waiting tasks |
| UTF-8 panic | All string truncation uses `char_indices()` (existing TEMM1E rule) |
| Pheromone field bloat | GC every 10s. Bounded: max 50MB at N=10 workers (§3.3 of spec) |

---

## 12. Cost Model Verification

### 12.1 A/B Test Design

**Model:** Gemini 3.1 Flash Lite (`gemini-3.1-flash-lite-preview`)
**Budget:** $30 maximum
**Provider:** Gemini (OpenAI-compatible endpoint)

| Benchmark | Single Agent | Swarm | Measured |
|-----------|-------------|-------|----------|
| Simple chat (1 turn) | baseline | should NOT activate swarm | overhead < 3% |
| 3-step task | baseline | marginal speedup expected | tokens, latency, quality |
| 7-step task | baseline | main benefit zone | tokens, latency, quality |
| 10-step task | baseline | maximum speedup zone | tokens, latency, quality |

Each benchmark runs 3 times. Metrics recorded:
- `C_single`: total tokens (input + output) for single agent
- `C_swarm`: total tokens for swarm
- `L_single`: wall-clock seconds, single agent
- `L_swarm`: wall-clock seconds, swarm
- `Q`: quality score (0-1, rubric-based self-eval)
- `tasks_total`, `tasks_parallel`, `critical_path_len`

### 12.2 Budget Estimation

Gemini 3.1 Flash Lite pricing (estimated):
- Input: $0.075 / 1M tokens
- Output: $0.30 / 1M tokens

At $30 budget: ~100M output tokens or ~400M input tokens.
Each benchmark run uses ~50K-200K tokens total.
12 benchmark scenarios × 3 runs × 200K = ~7.2M tokens ≈ $2.16

**We have massive headroom.** Can run 50+ iterations if needed.

---

## 13. What This Design Deliberately Omits (v2+)

1. **Distributed swarm** — v1 is single-process, multi-tokio-task
2. **Sub-decomposition on ESCALATE** — v1 falls back to single-agent
3. **Blueprint evolution** (fitness, adaptation, variant spawning) — v1 is read-only
4. **Colony tuner** (parameter auto-tuning) — needs weeks of data
5. **Resource conflict resolution** — v1 workers have independent subtasks
6. **Decomposition ledger** — v1 doesn't learn from past decompositions

These omissions are safe because:
- v1 is strictly better than or equal to single-agent mode
- The activation threshold prevents swarm when it can't help
- Fallback to single-agent is always available
- No existing behavior changes when `[hive] enabled = false`

---

## 14. Risk Assessment

| Component | Risk Level | Justification |
|-----------|-----------|---------------|
| New crate `temm1e-hive` | ZERO | Leaf crate, no existing code touched |
| `HiveConfig` in config.rs | ZERO | New field with `#[serde(default)]`, existing TOML files parse unchanged |
| Hive integration in main.rs | LOW | Behind `if config.hive.enabled`, feature-gated |
| Pheromone GC thread | ZERO | Runs only when hive enabled, no shared state with non-hive paths |
| Worker spawning | LOW | Uses same `tokio::spawn` pattern as existing ChatSlot workers |
| Budget tracking | ZERO | Reuses existing BudgetTracker, no new mechanism |
| SQLite schema | ZERO | New tables (`hive_*`), doesn't touch existing tables |

**Overall: ZERO RISK to existing single-agent behavior.**
