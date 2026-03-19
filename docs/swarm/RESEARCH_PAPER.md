# Many Tems: Stigmergic Swarm Intelligence for AI Agent Runtimes

**Authors:** Quan Duong, Claude Opus 4.6
**Date:** March 2026
**Project:** TEMM1E v3.0.0

---

## Abstract

We present Many Tems, a swarm intelligence coordination layer for AI agent runtimes that eliminates quadratic context cost through parallel task execution with scent-based (stigmergic) coordination. Unlike multi-agent chat systems that burn tokens on inter-agent LLM conversations, Many Tems coordinates workers through a time-decaying signal field and a shared SQLite workspace — zero coordination tokens. On 12 independent functions, the pack achieves 5.86x wall-clock speedup with 3.4x lower token cost and identical output quality (12/12 passing tests). On 5 parallel subtasks, 4.54x speedup with 1.01x token ratio. For simple tasks, the system is invisible — zero overhead.

---

## 1. Introduction

Modern AI agent runtimes process complex tasks through multi-turn tool loops: the agent reasons, calls a tool, observes the result, reasons again, calls another tool, and so on. Each turn adds to the conversation context. For a task with *m* subtasks, the context cost grows quadratically:

```
C_single = Σ_{j=1}^{m} [S + T + h̄·j]  =  m·(S+T) + h̄·m(m+1)/2
```

where *S* is the system prompt, *T* is the tool definitions, and *h̄* is the average tokens per subtask added to history. The *h̄·m(m+1)/2* term is quadratic in *m* — this is why complex tasks get expensive fast.

**Many Tems eliminates this quadratic term.** Each worker (Tem) carries only its task description plus results from dependency tasks — not the full conversation history. The cost becomes linear:

```
C_pack = C_alpha + m·(S + R̄)
```

where *C_alpha* is the coordinator's decomposition cost and *R̄* is the bounded dependency context per task.

### 1.1 Key Insight: Stigmergy, Not Chat

Most multi-agent frameworks (AutoGen, CrewAI, LangGraph) coordinate agents through LLM-to-LLM conversations. Every coordination message costs tokens. Many Tems uses **stigmergy** — indirect coordination through environmental signals. Workers observe each other's results in the shared Den (SQLite) and communicate through Scent signals (time-decaying pheromones). Zero LLM calls for coordination.

### 1.2 Terminology

| Term | Meaning |
|------|---------|
| **Many Tems** | The swarm intelligence feature |
| **Pack** | A group of parallel workers |
| **Alpha** | The coordinator that decomposes tasks |
| **Tem** | An individual worker agent |
| **Den** | Shared SQLite workspace |
| **Scent** | Time-decaying coordination signal |

---

## 2. Architecture

```
User Message → Classifier → Order+Complex? → Alpha (decompose) → Task DAG
                                                                      ↓
                                                               Spawn Pack
                                                             ┌────┼────┐
                                                          Tem₁  Tem₂  Tem₃
                                                             └────┼────┘
                                                               Den (SQLite)
                                                                   +
                                                              Scent Field
                                                                   ↓
                                                          Aggregate Results
                                                                   ↓
                                                             Reply to User
```

### 2.1 Activation

The system is invisible for simple tasks. Activation requires:

1. **Classifier says Order+Complex** — the existing LLM classifier (already running on every message) identifies tasks with 3+ independent deliverables
2. **Alpha decomposes into a DAG** — one LLM call produces a JSON task dependency graph
3. **Speedup threshold met** — `S_max ≥ 1.3` (theoretical parallelism benefit worth the coordination cost)
4. **Alpha cost ratio acceptable** — decomposition cost < 10% of estimated single-agent cost

If any condition fails → single-agent mode, zero overhead.

### 2.2 Den (Shared Workspace)

SQLite-backed task state machine:

```
PENDING → READY → ACTIVE → COMPLETE
                    │
                    ├→ BLOCKED → RETRY → READY
                    └→ ESCALATE (max retries exceeded)
```

Task claiming is atomic via `UPDATE ... WHERE status = 'ready'`. SQLite write serialization provides mutual exclusion — no distributed locks needed.

### 2.3 Scent Field (Pheromone Coordination)

Six signal types with exponential decay:

| Signal | Decay | Purpose |
|--------|-------|---------|
| Completion | ~5 min half-life | Task finished |
| Failure | ~6 min half-life | Attempt failed |
| Difficulty | ~2 min half-life | Worker struggling |
| Urgency | Grows over time | Task waiting (prevents starvation) |
| Progress | ~20 sec half-life | Worker heartbeat |
| Help Wanted | ~2 min half-life | Need specialist |

Workers read the scent field to select tasks — no LLM calls, pure arithmetic.

### 2.4 Task Selection Equation

```
S(Tem, task) = A^α · U^β · (1-D)^γ · (1-F)^δ · R^ζ
```

- **A (Affinity):** Jaccard similarity between worker's recent tags and task tags
- **U (Urgency):** Scent field total for urgency on this task
- **D (Difficulty):** Scent field total for difficulty
- **F (Failure):** Scent field total for failure
- **R (Downstream Reward):** `1 + |dependents| / |total_tasks|`

Scores within 5% of the maximum are treated as tied — random selection prevents herding.

---

## 3. Implementation

### 3.1 Crate: `temm1e-hive`

2,490 lines of Rust. 71 unit tests. New leaf crate depending only on `temm1e-core`.

| Module | LOC | Tests | Purpose |
|--------|-----|-------|---------|
| types.rs | 280 | 10 | Core types |
| config.rs | 180 | 4 | Pack configuration |
| dag.rs | 200 | 10 | DAG validation, critical path |
| blackboard.rs | 450 | 10 | Den (SQLite task state machine) |
| pheromone.rs | 350 | 8 | Scent field |
| selection.rs | 200 | 10 | Task selection equation |
| queen.rs | 200 | 8 | Alpha decomposition |
| worker.rs | 350 | 4 | Tem execution loop |
| lib.rs | 280 | 7 | Pack orchestrator |

### 3.2 Parallelism Proof

Two unit tests prove real parallel execution:

**`parallel_workers_actually_parallel`**: 4 independent 200ms tasks, 4 Tems. Peak concurrency ≥ 2. Wall clock < 600ms (not 800ms sequential).

**`parallel_respects_dag_dependencies`**: t1/t2 independent, t3 depends on both. Verifies t3 always executes after t1 AND t2.

### 3.3 Provider: Native Gemini API

The OpenAI-compatible endpoint for Gemini doesn't respect `systemInstruction` — the classifier prompt was ignored, causing the model to solve tasks instead of classifying them. We implemented a native Gemini provider (`generateContent` API) with:

- `systemInstruction` as a first-class field (properly respected)
- `thoughtSignature` capture + echo for Gemini 3 tool calling
- Tool schema sanitization (strips `additionalProperties`)
- `default_api:` prefix stripping from function names

---

## 4. Experiments

All benchmarks use real LLM API calls (Gemini 3.1 Pro Preview and Gemini 3 Flash Preview). Budget: $30, spent: ~$0.04.

### 4.1 Execution Time — 5 Independent Subtasks

Each Tem makes one real LLM call. Single agent processes them serially.

| | Single Agent | Pack (5 Tems) |
|---|---|---|
| Wall clock | 7,882ms | 1,738ms |
| **Speedup** | — | **4.54x** |
| Tokens | 884 | 923 |
| Token ratio | — | 1.01x |
| Cost | $0.000166 | $0.000173 |

**Same work, same tokens, same cost, 4.54x faster.**

### 4.2 Context Degradation — 12 Independent Functions

The key experiment. Single agent accumulates all previous outputs in context (simulating real multi-turn conversation). Pack gives each Tem fresh context.

| | Single Agent | Pack (12 Tems) |
|---|---|---|
| **Quality** | **12/12 PASS** | **12/12 PASS** |
| Wall clock | 102,849ms | 17,551ms |
| **Speedup** | — | **5.86x** |
| **Tokens** | **7,379** | **2,149** |
| **Token savings** | — | **3.4x cheaper** |
| Cost | $0.002435 | $0.000709 |

Single agent context grew from 115 bytes (function 1) to 3,253 bytes (function 12) — 28x growth. Pack stayed flat at ~190 bytes per Tem.

### 4.3 Project Build — Verified Output

Both modes build the same Rust library with 5 integration tests. Verification: `cargo check && cargo test`.

| | Single Agent | Pack |
|---|---|---|
| Wall clock | 19,184ms | 14,203ms |
| Speedup | — | 1.35x |
| cargo check | PASS | PASS |
| cargo test | **5/5 PASS** | **5/5 PASS** |

### 4.4 Simple Tasks — Zero Overhead

| | Single Agent | Pack |
|---|---|---|
| Wall clock | 1,261ms | 1,338ms |
| Swarm activated | — | NO |
| Token overhead | — | 0% |

The activation threshold correctly rejects simple tasks. **Many Tems is invisible when not needed.**

### 4.5 Where Pack Doesn't Help

- **Single-turn tasks**: The LLM handles "do these 7 things" in one response — no history accumulation to eliminate
- **Highly serial DAGs**: Speedup bounded by critical path depth
- **Cargo check (Rust)**: All-or-nothing compilation can't verify files that exist before their siblings — a tooling limitation, not a pack limitation

---

## 5. Comparison with Existing Systems

| System | Coordination | Coordination Cost | Parallelism |
|--------|-------------|-------------------|-------------|
| AutoGen | LLM-to-LLM chat | High (every message) | Sequential turns |
| CrewAI | LLM-to-LLM delegation | High | Sequential by default |
| LangGraph | Graph-based routing | Medium (routing LLM calls) | Node-level parallel |
| **Many Tems** | **Stigmergic (scent signals)** | **Zero tokens** | **Task-level parallel** |

The fundamental difference: other systems use LLM conversations for coordination. Many Tems uses arithmetic on scent signals. The coordination overhead is O(0) tokens.

---

## 6. Resilience

| Threat | Mitigation |
|--------|-----------|
| Tem panic | `panic = "unwind"` + `catch_unwind()`. Task returns to READY. |
| Budget runaway | BudgetTracker cap. Activation threshold. |
| Infinite retry | max_retries=3 → ESCALATE. |
| Cyclic DAG | Kahn's algorithm rejects → single-agent fallback. |
| Starvation | Urgency scent grows over time, capped at 5.0. |
| Scent field bloat | GC every 10 seconds. |

---

## 7. Limitations and Future Work

### Current Limitations
- Pack execution is single-process (multi-tokio-task, not distributed)
- Blueprint learning not yet implemented (Tems don't learn from past tasks)
- Gemini 3's `thoughtSignature` requirement adds complexity to tool calling
- Alpha decomposition quality depends on the LLM model

### Future Work
- **Blueprint Evolution**: Fitness tracking, adaptation recording, variant spawning (Lamarckian inheritance)
- **Colony Tuner**: Periodic optimization of selection exponents and scent decay rates
- **Distributed Pack**: Cross-machine coordination via shared database
- **Resource Conflict Resolution**: Priority-based cooperative backoff for shared resources

---

## 8. Reproducibility

```bash
git clone https://github.com/nagisanzenin/temm1e
cd temm1e && git checkout many-tems

# Unit tests (71 tests including parallelism proofs)
cargo test -p temm1e-hive

# Live benchmarks (requires API key)
export GEMINI_API_KEY="your-key"
cargo test -p temm1e-hive --test context_degradation_bench -- --nocapture
cargo test -p temm1e-hive --test live_ab_bench execution_time_benchmark -- --nocapture
```

---

## 9. Conclusion

Many Tems demonstrates that stigmergic coordination — borrowed from ant colony optimization — is a practical and efficient approach to parallelizing AI agent workloads. The key results:

1. **5.86x speedup** on 12 independent tasks with identical quality
2. **3.4x token savings** from eliminating quadratic context accumulation
3. **Zero coordination token overhead** — scent signals are arithmetic, not LLM calls
4. **Invisible for simple tasks** — activation threshold correctly gates the pack

The system is integrated into the TEMM1E runtime, tested with real API calls across Gemini and GPT providers, and verified with compilable, tested project outputs.

---

## Appendix: Configuration

```toml
[pack]
enabled = true
max_tems = 5
swarm_threshold_speedup = 1.3
alpha_cost_ratio_max = 0.10

[pack.scent]
gc_interval_secs = 10
evaporation_threshold = 0.01
urgency_cap = 5.0

[pack.selection]
alpha = 2.0   # affinity exponent
beta = 1.5    # urgency exponent
gamma = 1.0   # difficulty exponent
delta = 0.8   # failure exponent
zeta = 1.2    # reward exponent
```
