# Executable DAG — Blueprint Phase Parallelism

## Status: Implementation Plan (Pre-Approval)

**Date:** 2026-03-13
**Feature flag:** `[agent] parallel_phases = false` (opt-in, default OFF)

---

## 1. What This Is

Convert blueprint phases from free-form Markdown (LLM reads and follows) into
typed, dependency-aware structs that the runtime can execute as a DAG. Independent
phases run in parallel; dependent phases run sequentially. Zero extra LLM calls.

## 2. What This Is NOT

- NOT LLM-based task decomposition (no `decompose_prompt()` call)
- NOT a replacement for the current blueprint system (additive opt-in)
- NOT changing how blueprints are authored (LLM still writes Markdown)
- NOT changing tool-level parallelism in executor.rs (that stays as-is)

## 3. Why Opt-In, Default Off

- User stability is paramount — existing behavior must not change
- New code path exercised only when explicitly enabled
- If `parallel_phases = false` (default): blueprints work exactly as today
  (injected as context, LLM follows them sequentially)
- If `parallel_phases = true`: runtime parses phases, builds TaskGraph,
  executes independent phases concurrently

## 4. Architecture

### 4.1 Data Flow

```
Blueprint matched
  → parse_blueprint_phases(body) → Vec<BlueprintPhase>
  → phases_to_task_graph(phases) → TaskGraph
  → execute_phase_graph(graph, agent_runtime) → Vec<PhaseResult>
  → aggregate_results(results) → final OutboundMessage
```

### 4.2 New Types

```rust
/// A parsed, typed phase from a blueprint's Markdown body.
pub struct BlueprintPhase {
    pub id: String,           // "phase-1", "phase-2", etc.
    pub name: String,         // "Build", "Deploy", etc.
    pub goal: String,         // From **Goal**: line
    pub steps: String,        // Full steps text (LLM instruction)
    pub depends_on: Vec<String>,  // Phase IDs this depends on
}
```

### 4.3 Dependency Declaration — Conservative Default

Phases are **sequential by default**. Phase N depends on Phase N-1 unless
the blueprint explicitly annotates otherwise.

**Explicit parallelism annotation** (added to authoring prompt):
```markdown
### Phase 3: Run Tests (parallel with Phase 2)
```

The parser detects `(parallel with Phase N)` or `(independent)` annotations.
If no annotation: phase depends on the previous phase (linear chain).

**Why sequential default:** A phase marked parallel when it shouldn't be
causes data corruption. A phase marked sequential when it could be parallel
just runs slower. Slower is safe; corrupted is not.

### 4.4 No Extra API Calls — Proof

| Step | LLM call? | Why |
|------|-----------|-----|
| Blueprint matching | No | Reuses classifier's existing call |
| Phase parsing | No | Regex on Markdown body |
| Dependency resolution | No | Deterministic from annotations |
| TaskGraph construction | No | Pure data structure |
| Phase execution | Same as today | Each phase = one agent turn |
| Result aggregation | No | Concatenation in phase order |

**Total extra LLM calls: 0.** Same number of provider API calls as today.

### 4.5 No Conflict Resolution Needed — Proof

Conflicts between parallel phases are **prevented by design**, not resolved:

1. **Sequential default** — phases are linear unless explicitly annotated
2. **Author responsibility** — the LLM that authored the blueprint decides
   which phases are independent (it saw the full execution)
3. **Isolated contexts** — each parallel phase gets its own SessionContext
4. **No shared mutable state** — parallel phases don't see each other's
   tool outputs or conversation history
5. **Tool-level parallelism** — within each phase, executor.rs handles
   tool dependencies via union-find (already working)

If the author incorrectly marks dependent phases as parallel:
- Worst case: a phase fails because a prerequisite wasn't met
- The DAG marks dependents as blocked (existing TaskGraph behavior)
- Error reported to user: "Phase 3 failed: prerequisite not ready"
- **No silent corruption** — failure is loud and explicit

## 5. Implementation Plan

### Step 1: Config flag (temm1e-core)

Add `parallel_phases: bool` to `AgentConfig` with `default = false`.

**Files:** `crates/temm1e-core/src/types/config.rs`
**Risk:** Zero — additive field with serde default

### Step 2: Phase parser (temm1e-agent/blueprint.rs)

Add `parse_blueprint_phases(body: &str) -> Vec<BlueprintPhase>` function.

**Algorithm:**
1. Split body by `### Phase N:` headers
2. For each section, extract:
   - Phase number/ID from header
   - Name from header text
   - Goal from `**Goal**:` line
   - Steps from `**Steps**:` section
   - Parallel annotation from header `(parallel with Phase N)` or `(independent)`
3. Build dependency list:
   - If `(parallel with N)` → same deps as Phase N (runs alongside it)
   - If `(independent)` → no deps
   - If no annotation → depends on previous phase (linear chain)

**Files:** `crates/temm1e-agent/src/blueprint.rs`
**Risk:** Zero — pure function, no side effects, doesn't change existing code

### Step 3: Phase-to-TaskGraph bridge (temm1e-agent/blueprint.rs)

Add `phases_to_task_graph(phases: Vec<BlueprintPhase>, goal: &str) -> Result<TaskGraph, Temm1eError>`.

**Algorithm:**
- Convert each `BlueprintPhase` to `SubTask` (id, description=steps, dependencies)
- Call `TaskGraph::new(goal, subtasks)` — existing constructor handles validation + cycle detection

**Files:** `crates/temm1e-agent/src/blueprint.rs`
**Risk:** Zero — reuses existing validated TaskGraph constructor

### Step 4: Phase executor (temm1e-agent/runtime.rs)

Add phase execution loop inside `process_message()`, gated by `self.parallel_phases`:

```
if self.parallel_phases && active_blueprint.is_some() {
    let phases = parse_blueprint_phases(&blueprint.body);
    if phases.len() > 1 {
        let graph = phases_to_task_graph(phases, &goal)?;
        return self.execute_phase_graph(graph, msg, session, ...).await;
    }
}
// Otherwise: fall through to existing behavior (unchanged)
```

**`execute_phase_graph()` algorithm:**
1. Loop while graph is not complete and not failed:
   a. Get `ready_tasks()` from graph
   b. If multiple ready → spawn concurrent tokio tasks (with semaphore, max=3)
   c. Each task: create isolated SessionContext, run single-turn agent loop
      with phase steps as the user message
   d. Collect results, mark tasks complete/failed in graph
   e. Continue to next round of ready tasks
2. Aggregate all phase results in order → single OutboundMessage

**Concurrency limit:** 3 (not 5 like tools — phases are heavier, each makes
an LLM call). Configurable later if needed.

**Error handling:**
- Phase fails → mark as Failed in TaskGraph → dependents never become ready
- All phases complete → success
- Any phase fails → report partial results + which phases failed

**Files:** `crates/temm1e-agent/src/runtime.rs`
**Risk:** Low — gated behind opt-in flag, falls through to existing code when off

### Step 5: Authoring prompt update (temm1e-agent/blueprint.rs)

Update `build_authoring_prompt()` to include parallel annotation instructions:

```
## Phases
[Break the procedure into phases. By default, phases run sequentially.
If a phase is genuinely independent of the previous phase, annotate it:]

### Phase N: [Name] (parallel with Phase M)
### Phase N: [Name] (independent)
```

**Files:** `crates/temm1e-agent/src/blueprint.rs`
**Risk:** Zero — only affects new blueprint authoring, doesn't change existing blueprints

### Step 6: Wire flag through runtime (temm1e-agent + main.rs)

Pass `parallel_phases` from config → AgentRuntime → process_message gate.

**Pattern:** Same as `v2_optimizations` — field on AgentRuntime, builder method,
checked at runtime.

**Files:** `crates/temm1e-agent/src/runtime.rs`, `src/main.rs`
**Risk:** Zero — follows existing pattern exactly

### Step 7: Tests

Unit tests in `blueprint.rs`:
- `parse_blueprint_phases` — linear phases, parallel annotation, independent annotation, empty body, single phase
- `phases_to_task_graph` — linear chain, parallel phases, cycle rejection

Unit tests in `runtime.rs` or integration test:
- Phase execution with `parallel_phases = false` → existing behavior
- Phase execution with `parallel_phases = true` → phases execute via TaskGraph

**Files:** `crates/temm1e-agent/src/blueprint.rs`, test modules
**Risk:** Zero — tests only

## 6. Risk Matrix

| Scenario | Mitigation | Residual Risk |
|----------|-----------|---------------|
| Existing users (flag off) | Code path not reached | ZERO |
| Flag on, no blueprint matched | Falls through to existing behavior | ZERO |
| Flag on, blueprint has 1 phase | Falls through (single phase = no parallelism) | ZERO |
| Flag on, linear phases (no annotations) | Sequential execution (same as today) | ZERO |
| Flag on, parallel phases, correct deps | Phases run concurrently, results aggregated | ZERO |
| Flag on, parallel phases, wrong deps | Phase fails, dependents blocked, error reported | LOW (loud failure, no corruption) |
| Blueprint body doesn't parse | Falls through to existing behavior (inject as text) | ZERO |
| Phase execution panics | catch_unwind per phase (existing pattern) | ZERO |

**Overall risk to existing users: ZERO.** The feature is entirely opt-in.
When the flag is off, zero new code paths are reached.

## 7. Files Changed

| File | Change | Lines (est.) |
|------|--------|-------------|
| `crates/temm1e-core/src/types/config.rs` | Add `parallel_phases` field | +5 |
| `crates/temm1e-agent/src/blueprint.rs` | Add phase parser + bridge | +150 |
| `crates/temm1e-agent/src/runtime.rs` | Add phase executor + flag | +120 |
| `src/main.rs` | Pass flag to AgentRuntime | +3 |
| Tests | Phase parser + bridge + executor | +200 |
| **Total** | | **~480** |

## 8. What Stays Unchanged

- Blueprint authoring/refinement flow
- Blueprint matching (classifier + category)
- Context injection (budget tiers)
- Tool-level parallelism (executor.rs)
- All existing tests
- Default user experience (flag is off)
- LLM call count (zero extra calls)
