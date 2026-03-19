# λ-Memory — Design Document

> Memory that fades, not disappears. Recall by hash. The skull never overflows.

**Status:** Implemented
**Branch:** `gradient_memory`
**Author:** TEMM1E's Lab
**Date:** 2026-03-15

---

## 1. Problem Statement

Current agentic AI memory systems use binary strategies:

| Strategy | What happens | What's lost |
|----------|-------------|-------------|
| Token window cutoff | Old messages vanish | Everything beyond the window |
| Summarization | Compressed once, irreversible | Nuance, exact details, emotional context |
| RAG retrieval | Keyword-match grab bag | Temporal awareness, no decay model |

All three treat memory as a switch — present or gone. None model the **continuous degradation** that biological memory uses, where resolution decreases over time but the trace remains and can be recalled.

## 2. Core Concept

**Memory does not disappear. It loses resolution. Resolution can be restored on demand.**

A memory is born with full detail. Over time, Tem sees progressively less of it — full text → summary → essence → hash-only. But the full content always exists in the database. If the situation demands it, Tem can recall by hash and the memory becomes vivid again.

This is **λ-Memory**: a continuous decay function over stored memories, with hash-based recall that restores fidelity.

## 3. Design Principles

1. **One LLM call per memory, at creation time.** Decay is pure math — no LLM cost to forget.
2. **Lazy evaluation.** Scores are never stored. They are computed from `(importance, last_accessed, now)` at read time.
3. **Dynamic budget.** Memory's token allocation is derived from the model's skull size minus everything else. Never a flat number.
4. **The skull invariant.** `bone + active + output_reserve + guard + lambda_memory_tokens ≤ skull` — always holds, every turn, every model.
5. **Recall reheats.** Accessing a cold memory resets `last_accessed` to now, making it hot again. Biological reconsolidation.

## 4. Relationship to Existing Architecture

λ-Memory replaces the current Category 5 (memory search results, 15% budget) in `context.rs` with a more sophisticated system. It does **not** replace:

- **Category 1** (system prompt) — bone, fixed
- **Category 2** (tool definitions) — bone, fixed
- **Category 3** (DONE criteria, task state) — bone, fixed
- **Category 3b** (Blueprints, 10% budget) — unchanged
- **Category 4** (recent messages) — active conversation, unchanged
- **Category 5b** (persistent knowledge) — **absorbed into gradient memory** as high-importance, explicit-save memories
- **Category 6** (cross-task learnings) — **absorbed into gradient memory** as learning-tagged memories with elevated importance
- **Category 7** (older conversation history) — unchanged

```
BEFORE (context.rs today):
┌─────────────────────────────────────────┐
│ 1. System prompt         (fixed)        │
│ 2. Tool definitions      (fixed)        │
│ 3. Task state / DONE     (fixed)        │
│ 3b. Blueprints           (10% budget)   │
│ 4. Recent messages       (30-60 msgs)   │
│ 5. Memory search         (15% budget)   │  ← REPLACED
│ 5b. Knowledge entries    (sub-budget)   │  ← ABSORBED
│ 6. Cross-task learnings  (5% budget)    │  ← ABSORBED
│ 7. Older history         (remainder)    │
└─────────────────────────────────────────┘

AFTER (with λ-Memory):
┌─────────────────────────────────────────┐
│ 1. System prompt         (fixed)        │
│ 2. Tool definitions      (fixed)        │
│ 3. Task state / DONE     (fixed)        │
│ 3b. Blueprints           (10% budget)   │
│ 4. Recent messages       (30-60 msgs)   │
│ 5. GRADIENT MEMORY       (dynamic)      │  ← NEW: unified, elastic
│ 6. Older history         (remainder)    │
└─────────────────────────────────────────┘
```

λ-Memory unifies categories 5, 5b, and 6 into a single elastic layer with one packing algorithm.

## 5. Memory Structure

### 5.1 Memory Record

```rust
struct LambdaMemory {
    hash: String,           // blake3(session_id + turn_number + created_at)
    created_at: u64,        // unix timestamp
    last_accessed: u64,     // unix timestamp — updated on recall
    access_count: u32,      // how many times recalled
    importance: f32,        // 1.0–5.0, assigned by LLM at creation
    explicit_save: bool,    // user said "remember this"

    // Three fidelity layers — ALL written at creation time
    full: String,           // verbatim turn content or near-verbatim
    summary: String,        // one sentence, LLM-generated
    essence: String,        // ≤5 words, LLM-generated

    tags: Vec<String>,      // LLM-generated, up to 5
    memory_type: LambdaMemoryType,  // Conversation | Knowledge | Learning
    session_id: String,     // which session created this
}

enum LambdaMemoryType {
    Conversation,   // normal turn memory
    Knowledge,      // persistent knowledge (replaces 5b)
    Learning,       // cross-task learning (replaces 6)
}
```

### 5.2 Storage Schema (SQLite)

Extends the existing `temm1e-memory` SQLite backend. New table alongside `memory_entries`:

```sql
CREATE TABLE lambda_memories (
    hash            TEXT PRIMARY KEY,
    created_at      INTEGER NOT NULL,
    last_accessed   INTEGER NOT NULL,
    access_count    INTEGER NOT NULL DEFAULT 0,
    importance      REAL NOT NULL DEFAULT 1.0,
    explicit_save   INTEGER NOT NULL DEFAULT 0,
    full_text       TEXT NOT NULL,
    summary         TEXT NOT NULL,
    essence         TEXT NOT NULL,
    tags            TEXT NOT NULL DEFAULT '[]',    -- JSON array
    memory_type     TEXT NOT NULL DEFAULT 'conversation',
    session_id      TEXT NOT NULL
);

CREATE INDEX idx_gm_importance ON lambda_memories(importance);
CREATE INDEX idx_gm_last_accessed ON lambda_memories(last_accessed);
CREATE INDEX idx_gm_session ON lambda_memories(session_id);
CREATE INDEX idx_gm_type ON lambda_memories(memory_type);
```

## 6. The Decay Function

### 6.1 Formula

```
score(now) = importance × exp(−λ × hours_since_last_access)
```

Where:
- `importance` ∈ [1.0, 5.0] — assigned by LLM at creation
- `λ` (DECAY_LAMBDA) = 0.01 — tunable decay rate constant
- `hours_since_last_access` = (now − last_accessed) / 3600

### 6.2 Why Exponential Decay

Exponential decay is chosen because:
1. **Mathematically simple** — one multiplication, one exp() call
2. **Never reaches zero** — approaches but never hits 0, so no division-by-zero edge cases
3. **Importance scales linearly** — a 5.0 memory takes 5× longer to reach the same score as a 1.0 memory
4. **Models biological memory** — Ebbinghaus forgetting curve is approximately exponential

### 6.3 Decay Behavior Table

With `λ = 0.01`:

| Importance | 1 hour | 6 hours | 24 hours | 7 days | 30 days | 90 days |
|-----------|--------|---------|----------|--------|---------|---------|
| 1.0 | 0.99 | 0.94 | 0.79 | 0.19 | 0.001 | ~0 |
| 2.0 | 1.98 | 1.88 | 1.57 | 0.37 | 0.001 | ~0 |
| 3.0 | 2.97 | 2.83 | 2.36 | 0.56 | 0.002 | ~0 |
| 5.0 | 4.95 | 4.71 | 3.93 | 0.93 | 0.004 | ~0 |

### 6.4 Lazy Evaluation — Why No Recalculation Loop

**Scores are never stored. They are never recalculated in a batch.**

The score is a pure function of `(importance, last_accessed, now)`. All three values are known at query time. Therefore:

```rust
impl LambdaMemory {
    fn decay_score(&self, now: u64) -> f32 {
        let age_hours = (now - self.last_accessed) as f32 / 3600.0;
        self.importance * (-age_hours * DECAY_LAMBDA).exp()
    }
}
```

This is computed **only when building context** — once per turn, for up to `CANDIDATE_LIMIT` memories. No background process. No cron. No recalculation trigger. The database stores immutable facts; time does the rest.

**Cost per turn:** One SQL query + ~500 float operations = microseconds.

## 7. The Skull Model — Dynamic Token Budget

### 7.1 The Skull Diagram

```
┌──────────────────────────────────────────────────────────┐
│                        SKULL                              │
│               (model.context_window)                      │
│                                                           │
│  ┌────────────────────┐                                   │
│  │ BONE (fixed)       │  system prompt + tool schemas     │
│  │                    │  + DONE criteria + blueprint       │
│  └────────────────────┘                                   │
│  ┌──────────────────────────────┐                         │
│  │ ACTIVE CONVERSATION          │  recent 30-60 messages  │
│  │ (variable, grows each turn)  │                         │
│  └──────────────────────────────┘                         │
│  ┌─────────────┐                                          │
│  │ OUTPUT      │  reserved for Tem's response             │
│  │ RESERVE     │  min(max_output_tokens, skull / 10)      │
│  └─────────────┘                                          │
│  ┌──────────────────────────────┐                         │
│  │ GRADIENT MEMORY (elastic)    │  ← gets what's left     │
│  │ hot | warm | cool | faded    │                         │
│  └──────────────────────────────┘                         │
│  ┌───────┐                                                │
│  │ GUARD │  2% skull — never allocated, safety margin     │
│  └───────┘                                                │
│  ┌────────────────────────────┐                           │
│  │ OLDER HISTORY (remainder)  │  fills after memory       │
│  └────────────────────────────┘                           │
└──────────────────────────────────────────────────────────┘
```

### 7.2 Budget Calculation

```rust
fn lambda_memory_budget(
    model: &ModelLimits,     // from model_registry.rs
    bone_tokens: usize,      // system prompt + tools + DONE + blueprints
    active_tokens: usize,    // recent messages (category 4)
    older_history_tokens: usize,  // category 7
) -> usize {
    let skull = model.context_window;
    let output_reserve = model.max_output_tokens.min(skull / 10);
    let guard = skull / 50;  // 2%

    let occupied = bone_tokens
        + active_tokens
        + output_reserve
        + guard
        + older_history_tokens;

    skull.saturating_sub(occupied)
}
```

**Memory is elastic — it takes what's left after everything with higher priority is placed.** If the conversation grows so large that nothing is left, memory goes dark. Tem keeps working but without memory context. No crash. No overflow.

### 7.3 Cross-Model Behavior

Using actual values from `model_registry.rs`:

```
┌──────────────────────────┬───────────┬──────────┬──────────┬───────────┐
│ Model                    │ Skull     │ Bone+Out │ Memory   │ Memory    │
│                          │           │ +Guard   │ @Turn 1  │ @Turn 100 │
├──────────────────────────┼───────────┼──────────┼──────────┼───────────┤
│ gpt-3.5-turbo            │    16,385 │   ~3,200 │  ~12,000 │   ~2,000  │
│ phi-4                    │    16,384 │   ~3,200 │  ~12,000 │   ~2,000  │
│ qwen-2.5-7b-instruct    │    32,768 │   ~4,500 │  ~26,000 │  ~10,000  │
│ gpt-4o                   │   128,000 │  ~16,000 │ ~108,000 │  ~60,000  │
│ claude-sonnet-4-6        │   200,000 │  ~24,000 │ ~170,000 │ ~100,000  │
│ gpt-5.2                  │   400,000 │  ~44,000 │ ~350,000 │ ~240,000  │
│ gemini-2.5-flash         │ 1,048,576 │  ~70,000 │ ~960,000 │ ~700,000  │
│ grok-4-1-fast            │ 2,000,000 │  ~50,000 │  ~1.9M   │   ~1.5M   │
└──────────────────────────┴───────────┴──────────┴──────────┴───────────┘
```

The same algorithm handles a 16k model and a 2M model. On 16k, memory might only fit hashes and essences. On 2M, it can keep thousands of memories at full fidelity.

## 8. Memory Pressure & Adaptive Thresholds

As the conversation grows, the memory budget shrinks. The thresholds adapt:

```rust
const BASE_HOT:  f32 = 2.0;
const BASE_WARM: f32 = 1.0;
const BASE_COOL: f32 = 0.3;
const BASE_GONE: f32 = 0.01;

fn effective_thresholds(budget: usize, max_budget: usize) -> Thresholds {
    let pressure = 1.0 - (budget as f32 / max_budget as f32).min(1.0);
    // pressure: 0.0 = no pressure, 1.0 = no room

    Thresholds {
        hot:  BASE_HOT  + (pressure * 2.0),
        warm: BASE_WARM + (pressure * 1.0),
        cool: BASE_COOL + (pressure * 0.5),
        gone: BASE_GONE,
    }
}
```

Under pressure, the bar for "hot" rises. Fewer memories get full text. More get compressed to summary or essence. Only the most important memories stay vivid. This mirrors human cognition under load.

## 9. Memory Creation — Inplace LLM Extraction

### 9.1 Gate: Is This Turn Worth Remembering?

Not every turn becomes a memory. A heuristic gate runs **before** any LLM involvement:

```rust
fn worth_remembering(turn: &TurnContent) -> bool {
    let has_decision = contains_decision_language(&turn.user_text);
    let has_action = turn.has_tool_calls;
    let explicit = turn.user_said_remember;
    let emotional = simple_sentiment_check(&turn.user_text);
    let substantive = estimate_tokens(&turn.user_text) > 20;

    explicit || has_decision || (has_action && substantive) || emotional
}
```

Cost: zero. Pure string heuristics.

### 9.2 Inplace Extraction

If the gate passes, append an instruction to the **same** LLM call that generates the response:

```
[system]: For this turn, also emit a <memory> block at the end of your response:
<memory>
summary: (one sentence)
essence: (5 words max)
importance: (1-5, where 1=casual, 3=decision, 5=critical/emotional)
tags: (up to 5, comma-separated)
</memory>
```

The LLM is already processing this turn. The marginal cost is ~50-80 extra output tokens. No separate API call.

After the response comes back, the runtime:
1. Parses the `<memory>` block
2. Strips it from the user-visible response
3. Writes all three layers (full, summary, essence) to the database
4. Hashes the memory: `blake3(session_id + turn_number + created_at)`

### 9.3 What Gets Stored as `full`

The `full` field is **not** the entire conversation turn verbatim. It's the user's message + the core of the assistant's response, with tool call/result noise stripped. This keeps `full` at a reasonable size (typically 200-500 tokens) while preserving the meaningful content.

## 10. Recall — Hash-Based Retrieval

### 10.1 What Tem Sees

In its context window, Tem sees memories at their current fidelity tier:

```
═══ λ-Memory ═══

[hot] User restructured auth module. Was frustrated with the layered
      middleware approach — said it felt "hacky." Rewrote as a single
      tower service with direct DB calls. Took 3 iterations. Final
      version uses axum extractors. User was satisfied.
      (#a7f3b2c | 2026-03-15 14:22 | importance: 4.0 | accessed: 3×)

[warm] Deployed v0.4.2 to staging with new rate limiter.
       (#c91bb3e | 2026-03-14 09:15)

[warm] User prefers explicit error types over anyhow in library code.
       (#f28da41 | 2026-03-12 16:30)

[cool] auth rewrite, user-driven (#d7c4e2f | 2026-03-10)
[cool] DB connection pooling discussion (#91bb3e0 | 2026-03-08)

[faded] #f47ac10 | 2026-02-20 | initial scaffold decisions
[faded] #b23cc91 | 2026-02-18 | dependency selection debate

═══════════════════════
```

### 10.2 Recall Tool

Tem has a tool available:

```
Tool: lambda_recall
Parameters:
  hash: String  — the 7-char hash prefix shown in context

Returns: full memory content + metadata
```

When Tem calls `lambda_recall(hash="f47ac10")`:

1. Database lookup by hash prefix
2. `last_accessed` ← now
3. `access_count` += 1
4. Full content returned into the current context
5. **Next turn**, this memory's `decay_score()` will be high (just accessed) — it naturally appears as hot

### 10.3 Recall Strengthens Memory

This is biological reconsolidation. A cold memory that gets recalled becomes hot:

```
Before recall:
  hash: f47ac10, last_accessed: 2026-02-20, importance: 3.0
  → score at 2026-03-15 = 3.0 × exp(-552 × 0.01) = 0.012 → FADED

After recall:
  hash: f47ac10, last_accessed: 2026-03-15 (NOW), importance: 3.0
  → score at 2026-03-15 = 3.0 × exp(0) = 3.0 → HOT

1 hour after recall:
  → score = 3.0 × exp(-1 × 0.01) = 2.97 → still HOT
```

## 11. Context Assembly — The Packing Algorithm

Every turn, `build_context()` runs the packing algorithm to fill the gradient memory section:

```rust
fn assemble_lambda_context(
    db: &Database,
    budget: usize,       // dynamic, from §7.2
    now: u64,
    max_budget: usize,   // maximum possible budget for this model
) -> String {
    let thresholds = effective_thresholds(budget, max_budget);

    // Phase 1: Query candidates — SQL does the heavy lifting
    let candidates: Vec<LambdaMemory> = db.query(
        "SELECT * FROM lambda_memories ORDER BY importance DESC LIMIT ?",
        CANDIDATE_LIMIT
    );

    // Phase 2: Score each — pure arithmetic
    let mut scored: Vec<(f32, &LambdaMemory)> = candidates
        .iter()
        .map(|m| (m.decay_score(now), m))
        .collect();
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap());

    // Phase 3: Pack by tier, respecting budget
    let mut output = String::from("═══ λ-Memory ═══\n\n");
    let mut remaining = budget - estimate_tokens(&output);

    // 3a: Explicit saves always included (at minimum essence level)
    for (score, mem) in scored.iter().filter(|(_, m)| m.explicit_save) {
        let (text, cost) = best_fit(mem, remaining, *score, &thresholds);
        if cost == 0 { continue; }
        output.push_str(&text);
        remaining -= cost;
    }

    // 3b: Pack remaining by score
    for (score, mem) in &scored {
        if mem.explicit_save { continue; }
        if remaining < MIN_ENTRY_TOKENS { break; }

        let (text, cost) = best_fit(mem, remaining, *score, &thresholds);
        if cost == 0 { continue; }
        output.push_str(&text);
        remaining -= cost;
    }

    output.push_str("\n═══════════════════════\n");
    output
}

fn best_fit(
    mem: &LambdaMemory,
    remaining: usize,
    score: f32,
    t: &Thresholds,
) -> (String, usize) {
    // Try highest fidelity first, fall back to cheaper representations
    if score > t.hot {
        let text = format_hot(mem);
        let cost = estimate_tokens(&text);
        if cost <= remaining { return (text, cost); }
    }
    if score > t.warm {
        let text = format_warm(mem);
        let cost = estimate_tokens(&text);
        if cost <= remaining { return (text, cost); }
    }
    if score > t.cool {
        let text = format_cool(mem);
        let cost = estimate_tokens(&text);
        if cost <= remaining { return (text, cost); }
    }
    // Last resort: faded hash line (~10 tokens)
    if score > t.gone {
        let text = format_faded(mem);
        let cost = estimate_tokens(&text);
        if cost <= remaining { return (text, cost); }
    }
    (String::new(), 0)
}
```

## 12. Garbage Collection

Periodic, not per-turn. Runs on startup or once per day:

```rust
fn gc_lambda_memories(db: &Database, now: u64) {
    // Memories that:
    //   1. Score below GONE threshold
    //   2. Haven't been accessed in 90+ days
    //   3. Are NOT explicit saves
    db.execute(
        "DELETE FROM lambda_memories
         WHERE explicit_save = 0
         AND (? - last_accessed) > 7776000
         AND importance * exp(-((? - last_accessed) / 3600.0) * 0.01) < 0.001",
        [now, now]
    );
}
```

Explicit saves are never garbage collected.

## 13. Tuning Constants

```rust
const DECAY_LAMBDA: f32 = 0.01;       // decay rate
const BASE_HOT: f32 = 2.0;            // threshold for full text
const BASE_WARM: f32 = 1.0;           // threshold for summary
const BASE_COOL: f32 = 0.3;           // threshold for essence
const BASE_GONE: f32 = 0.01;          // below = invisible
const CANDIDATE_LIMIT: usize = 500;   // max memories to score per turn
const MIN_ENTRY_TOKENS: usize = 10;   // minimum to fit a faded entry
```

These can be exposed in `temm1e.toml` for per-deployment tuning:

```toml
[memory.gradient]
decay_lambda = 0.01
hot_threshold = 2.0
warm_threshold = 1.0
cool_threshold = 0.3
candidate_limit = 500
```

---

## 14. Dry Run — Complete Step-by-Step Walkthrough

This dry run demonstrates λ-Memory across 7 turns of a conversation using `claude-sonnet-4-6` (200,000 token skull, 64,000 max output).

### Setup

```
Model:          claude-sonnet-4-6
Skull:          200,000 tokens
Max output:     64,000 tokens
Output reserve: min(64,000, 200,000/10) = 20,000 tokens
Guard (2%):     4,000 tokens
Bone:           ~3,500 tokens (system prompt + tools + DONE)
DECAY_LAMBDA:   0.01
```

### Turn 1 — User asks Tem to refactor the auth module

```
Time: 2026-03-15 10:00:00 (T=0)
```

**Incoming:** "Refactor the auth middleware to use axum extractors instead of the layered approach"

**Memory state:** Empty — no gradient memories exist yet.

**Budget calculation:**
```
skull:           200,000
bone:              3,500
active (1 msg):      150
output_reserve:   20,000
guard:             4,000
older_history:         0
─────────────────────────
occupied:         27,650
memory_budget:   172,350  (but nothing to show)
```

**Gate check:** `worth_remembering()`?
- `contains_decision_language("refactor")` → YES
- Turn passes gate

**LLM processes turn.** Response includes:

```xml
<memory>
summary: User requested auth middleware refactor from layered approach to axum extractors.
essence: auth refactor to extractors
importance: 3
tags: auth, refactor, axum, middleware
</memory>
```

**Memory #1 created:**
```
hash:           a7f3b2c (blake3 of session+turn+timestamp)
created_at:     1742036400
last_accessed:  1742036400
access_count:   0
importance:     3.0
explicit_save:  false
full:           "User: Refactor the auth middleware to use axum extractors
                 instead of the layered approach. Assistant: [refactoring
                 plan and execution details, ~300 tokens]"
summary:        "User requested auth middleware refactor from layered
                 approach to axum extractors."
essence:        "auth refactor to extractors"
tags:           ["auth", "refactor", "axum", "middleware"]
memory_type:    Conversation
```

**What Tem sees in memory section:** Nothing yet (memory was just created; it will appear next turn).

---

### Turn 2 — User says "remember: always use explicit error types"

```
Time: 2026-03-15 10:30:00 (T=+30min)
```

**Incoming:** "Also, remember: always use explicit error types in library crates, not anyhow"

**Budget calculation:**
```
skull:           200,000
bone:              3,500
active (3 msgs):   1,200
output_reserve:   20,000
guard:             4,000
older_history:         0
─────────────────────────
occupied:         28,700
memory_budget:   171,300
```

**Score memory #1 (a7f3b2c):**
```
age_hours = 0.5
score = 3.0 × exp(-0.5 × 0.01) = 3.0 × 0.995 = 2.985
```
Threshold check: 2.985 > HOT (2.0) → **show full text**

**What Tem sees:**
```
═══ λ-Memory ═══

[hot] User: Refactor the auth middleware to use axum extractors instead
      of the layered approach. [execution details...]
      (#a7f3b2c | 2026-03-15 10:00 | importance: 3.0)

═══════════════════════
```

**Gate check for Turn 2:** `user_said_remember` → true → YES

**Memory #2 created:**
```
hash:           f28da41
importance:     4.0    (LLM assigns high — explicit user preference)
explicit_save:  true   (user said "remember")
full:           "User: always use explicit error types in library crates,
                 not anyhow. This is a coding preference for all library
                 code in the workspace."
summary:        "User prefers explicit error types over anyhow in library code."
essence:        "explicit errors, not anyhow"
tags:           ["preference", "error-handling", "library", "coding-style"]
memory_type:    Knowledge
```

---

### Turn 3 — Routine follow-up, 2 hours later

```
Time: 2026-03-15 12:00:00 (T=+2h from Turn 1)
```

**Incoming:** "Looks good, ship it to staging"

**Score all memories:**
```
#a7f3b2c: age=2.0h,  imp=3.0, score = 3.0 × exp(-2.0×0.01)  = 2.94  → HOT
#f28da41: age=1.5h,  imp=4.0, score = 4.0 × exp(-1.5×0.01)  = 3.94  → HOT
```

**Budget calculation:**
```
skull:           200,000
bone:              3,500
active (5 msgs):   2,800
output_reserve:   20,000
guard:             4,000
older_history:         0
─────────────────────────
occupied:         30,300
memory_budget:   169,700
```

**What Tem sees:**
```
═══ λ-Memory ═══

[hot] User prefers explicit error types over anyhow in library code.
      (#f28da41 | 2026-03-15 10:30 | importance: 4.0 | explicit save)

[hot] User: Refactor the auth middleware to use axum extractors instead
      of the layered approach. [execution details...]
      (#a7f3b2c | 2026-03-15 10:00 | importance: 3.0)

═══════════════════════
```

**Gate check:** "Looks good, ship it" — `has_decision=true` (ship), `has_action=true` (deploy tool)

**Memory #3 created:**
```
hash:           c91bb3e
importance:     2.0
summary:        "Deployed auth refactor to staging, user approved."
essence:        "auth deployed to staging"
tags:           ["deploy", "staging", "auth"]
```

---

### Turn 4 — Next day, new session, different topic

```
Time: 2026-03-16 14:00:00 (T=+28h from Turn 1)
```

New session. Tem is asked about database connection pooling.

**Score all memories:**
```
#f28da41: age=27.5h, imp=4.0, score = 4.0 × exp(-27.5×0.01) = 3.04  → HOT
#a7f3b2c: age=28.0h, imp=3.0, score = 3.0 × exp(-28.0×0.01) = 2.27  → HOT
#c91bb3e: age=26.0h, imp=2.0, score = 2.0 × exp(-26.0×0.01) = 1.54  → WARM
```

**What Tem sees:**
```
═══ λ-Memory ═══

[hot] User prefers explicit error types over anyhow in library code.
      (#f28da41 | 2026-03-15 10:30 | importance: 4.0 | explicit save)

[hot] User: Refactor the auth middleware to use axum extractors instead
      of the layered approach. [execution details...]
      (#a7f3b2c | 2026-03-15 10:00 | importance: 3.0)

[warm] Deployed auth refactor to staging, user approved.
       (#c91bb3e | 2026-03-15 12:00)

═══════════════════════
```

Notice: the explicit-save preference (#f28da41, importance 4.0) stays hot longer than the routine deploy (#c91bb3e, importance 2.0) which has already decayed to warm. **Importance directly controls how long memories stay vivid.**

---

### Turn 5 — One week later

```
Time: 2026-03-22 10:00:00 (T=+7 days from Turn 1)
```

**Score all memories:**
```
#f28da41: age=167.5h, imp=4.0, score = 4.0 × exp(-167.5×0.01) = 0.75  → COOL
          (explicit_save=true, so still included even at cool)
#a7f3b2c: age=168.0h, imp=3.0, score = 3.0 × exp(-168.0×0.01) = 0.56  → COOL
#c91bb3e: age=166.0h, imp=2.0, score = 2.0 × exp(-166.0×0.01) = 0.38  → COOL
```

Plus 4 more memories from the week's conversations (let's say importance 2.0-3.0, aged 1-6 days, various scores).

**What Tem sees:**
```
═══ λ-Memory ═══

[hot] Fixed the rate limiter edge case in gateway — intermittent 429s
      under concurrent load were caused by...
      (#e5a91c2 | 2026-03-21 16:00 | importance: 3.0)

[warm] Set up DB connection pooling with max 20 connections.
       (#d7c4e2f | 2026-03-19 11:00)

[cool] explicit errors, not anyhow (#f28da41 | 2026-03-15 | explicit save)
[cool] auth refactor to extractors (#a7f3b2c | 2026-03-15)
[cool] auth deployed to staging (#c91bb3e | 2026-03-15)
[cool] discussed TUI color scheme (#72fd1a3 | 2026-03-17)

═══════════════════════
```

The original auth work has faded to essence-only. But Tem **knows** it happened. The explicit save (#f28da41) persists at cool rather than disappearing — `explicit_save` entries are always packed.

---

### Turn 6 — Recall! Auth comes back

```
Time: 2026-03-22 10:05:00
```

**Incoming:** "We need to revisit the auth refactor — there's a bug in the extractor"

Tem sees `[cool] auth refactor to extractors (#a7f3b2c)` in its context. It decides it needs the full detail.

**Tem calls:** `lambda_recall(hash="a7f3b2c")`

**What happens:**
```
1. DB lookup: SELECT * FROM lambda_memories WHERE hash LIKE 'a7f3b2c%'
2. Found! Update: last_accessed = NOW, access_count = 0 → 1
3. Return full text to Tem's context
```

**Tem now sees the full memory injected into this turn's context:**
```
[RECALLED] User: Refactor the auth middleware to use axum extractors
instead of the layered approach. Assistant: Replaced the 3-layer
middleware stack (AuthLayer → SessionLayer → PermLayer) with a single
AuthExtractor that pulls the session from request extensions. Used
tower::ServiceBuilder for the remaining rate-limit layer...
(#a7f3b2c | originally 2026-03-15 | recalled 2026-03-22 | accessed: 1×)
```

**Next turn's scoring:**
```
#a7f3b2c: age=0h (just accessed!), imp=3.0, score = 3.0 × exp(0) = 3.0 → HOT
```

The memory is hot again. It will naturally appear as full text in subsequent turns until it decays again.

---

### Turn 7 — Small model scenario (same time, different model)

Same moment, but imagine Tem is running on `qwen-2.5-7b-instruct` (32,768 skull):

**Budget calculation:**
```
skull:            32,768
bone:              3,500
active (15 msgs):  8,000
output_reserve:    3,276  (skull/10)
guard:               655  (2%)
older_history:     5,000
─────────────────────────
occupied:         20,431
memory_budget:    12,337
```

**Same 7 memories. Same scores. But tighter budget.**

Adaptive thresholds with pressure:
```
max_budget (qwen at Turn 1): ~27,000
current budget: 12,337
pressure = 1.0 - (12,337 / 27,000) = 0.543

hot  = 2.0 + (0.543 × 2.0) = 3.09   (raised! fewer things are "hot")
warm = 1.0 + (0.543 × 1.0) = 1.54
cool = 0.3 + (0.543 × 0.5) = 0.57
```

With these adjusted thresholds:
```
#a7f3b2c: score 3.0 → 3.0 < 3.09 → WARM (would be HOT on Claude!)
#f28da41: score 0.75 → COOL (essence only)
#e5a91c2: score 2.97 → WARM
Others: COOL or FADED
```

**What Tem sees on the small model:**
```
═══ λ-Memory ═══

[warm] User requested auth middleware refactor from layered approach
       to axum extractors.
       (#a7f3b2c | 2026-03-15)

[warm] Fixed rate limiter edge case in gateway.
       (#e5a91c2 | 2026-03-21)

[cool] explicit errors, not anyhow (#f28da41 | explicit save)
[cool] auth deployed to staging (#c91bb3e)
[cool] DB pooling setup (#d7c4e2f)

[faded] #72fd1a3 | 2026-03-17 | TUI color scheme

═══════════════════════
```

Same memories, same algorithm. But the small skull **compresses more aggressively**. Nothing overflows. Tem still has awareness of everything via hashes/essences.

---

## 15. Cost Summary

| Operation | LLM Calls | Compute | Frequency |
|-----------|-----------|---------|-----------|
| Create memory | ~0 (inplace, same call, ~50 extra output tokens) | DB insert | Per memorable turn |
| Decay | 0 | One `exp()` per candidate | Per turn (lazy) |
| Score 500 memories | 0 | ~500 float ops | Per turn |
| Assemble context | 0 | String concat | Per turn |
| Recall by hash | 0 | DB read + update | On demand |
| Garbage collection | 0 | DB delete sweep | Daily / startup |

**Net new cost vs. current system:** Near zero. The inplace extraction adds ~50 output tokens to turns that pass the gate. Everything else is local compute.

## 16. Integration Points

| Component | How it connects |
|-----------|----------------|
| `context.rs` | `build_context()` calls `assemble_lambda_context()` instead of separate Category 5/5b/6 logic |
| `model_registry.rs` | `ModelLimits.context_window` drives skull size |
| `temm1e-memory/sqlite.rs` | New `lambda_memories` table alongside existing `memory_entries` |
| `runtime.rs` | Parses `<memory>` block from LLM responses, writes to gradient store |
| `learning.rs` | Learnings written as `LambdaMemoryType::Learning` with importance 3.0 |
| Tools | New `lambda_recall` tool registered in tool definitions |
| `temm1e.toml` | `[memory.gradient]` section for tuning constants |

## 17. Open Questions

1. **Should recall boost importance?** Currently recall only resets `last_accessed`. Should repeated recalls also increase `importance` (e.g., +0.5 per recall, capped at 5.0)? This would make frequently-recalled memories permanently more persistent.

2. **Cross-session memory merging.** If two sessions create similar memories about the same topic, should they be merged? Or keep separate with the algorithm naturally surfacing the more recent one?

3. **Decay rate per memory type.** Should `Knowledge` type memories have a lower λ (slower decay) than `Conversation` type? The user's explicit preferences should probably fade slower than routine turn logs.

4. **Blueprint interaction.** Blueprints currently have their own 10% budget. Should blueprint-related memories get an importance boost when a matching blueprint is loaded?

5. **Migration.** Existing `memory_entries` (Category 5b knowledge) need a migration path into `lambda_memories`. One-time migration script with default importance = 3.0 for knowledge entries.
