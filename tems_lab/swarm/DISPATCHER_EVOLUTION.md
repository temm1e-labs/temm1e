# Dispatcher Evolution — Post-v5.3.5, Post-Prompt-Collapse, Post-JIT

**Status:** Design note
**Date:** 2026-04-18
**Related:** `JIT_DESIGN.md`, `docs/design/DISPATCHER_REWORK.md`

---

## 1. The question

After v5.3.5 made `Chat` inert and after the planned changes:

- Prompt stratification is collapsed (single Standard+Planning prompt with Anthropic cache)
- JIT `spawn_swarm` tool lets the main agent decide to parallelise mid-loop
- Budget / stagnation / duration / interrupt replace the 200-call ceiling

**What is the classifier still doing?** And is it worth the ~1.1k tokens per turn it costs?

---

## 2. What the classifier does today

`crates/temm1e-agent/src/llm_classifier.rs` returns a `MessageClassification`:

```rust
struct MessageClassification {
    category: MessageCategory,       // Chat | Order | Stop
    chat_text: String,               // ack text for UX
    difficulty: TaskDifficulty,      // Simple | Standard | Complex
    blueprint_hint: Option<String>,  // semantic tag for blueprint matching
}
```

Behaviours driven by each field:

| Field | Drives | Post-changes status |
|---|---|---|
| `category = Chat` | Falls through to agent loop (same as Order after v5.3.5) | **Obsolete** — no unique behaviour |
| `category = Order` | Falls through to agent loop | **Obsolete** — default path |
| `category = Stop` | Early return, cancels active task | **Still unique** — fast-path cancellation |
| `chat_text` | Early ack shown to user before tool loop starts | **Still valuable** — UX nicety |
| `difficulty = Simple/Standard/Complex` | Maps to `PromptTier::Basic/Standard/Full` | **Obsolete after prompt collapse** |
| `difficulty = Complex + hive_enabled` | Routes to dispatch-time Hive | **Still unique** — cheaper than JIT for obvious cases |
| `blueprint_hint` | V2 blueprint matcher uses this tag for zero-cost retrieval | **Still valuable** — blueprint system relies on it |

After all planned changes, three fields earn their keep: Stop-detection, ack text, blueprint hint, and the Complex→Hive route. The rest is dead.

---

## 3. Options

### Option D1 — Keep classifier slimmed (recommended)

Reduce the classifier's output to what's actually used:

```rust
struct MessageClassification {
    is_stop: bool,                    // fast-path cancellation
    ack_text: String,                 // early UX reply
    swarm_candidate: bool,            // obvious-parallelism flag (replaces difficulty=Complex)
    blueprint_hint: Option<String>,
}
```

Classifier prompt collapses from ~1.1k tokens (5-axis judgement) to ~400 tokens (4 cleaner axes). Dispatch logic becomes:

```
if classification.is_stop → cancel active task, return ack
else if classification.swarm_candidate && hive_enabled → dispatch-time swarm
else → enter agent loop with ack, blueprint hint, full prompt
```

**Pros:** preserves UX ack, preserves cost-saving dispatch-time swarm, preserves blueprint hint. Eliminates every obsolete axis.
**Cons:** still pays ~400 tokens per message for classification.
**Risk:** low. Behaviour change is schema-only; end-to-end paths identical.

### Option D2 — Remove classifier entirely

Main agent handles everything:
- Stop: detected in main loop (cheap regex + LLM check in first tool decision)
- Ack: main agent streams its first token (streaming already exists)
- Swarm: JIT tool only — no dispatch-time route
- Blueprint: main agent does blueprint matching via a tool

**Pros:** simplest architecture, no extra LLM call per message, one place to reason about message handling.
**Cons:** (a) blueprint matching moves from pre-loop to in-loop — agent pays extra tool-use round for work currently done "free" in parallel with classification; (b) no early ack until first streamed token (small UX delta); (c) every parallel case now pays a discovery turn (lost dispatch-time swarm optimisation).

**Risk:** medium. UX and cost regressions are real. Blueprint integration becomes more invasive.

### Option D3 — Minimal classifier (Stop-only) + ack folded into agent

Tiny classifier that only answers "is this Stop?". Main agent generates its own first-token ack. Blueprint matching stays pre-loop via a separate cheap match. Dispatch-time Hive dropped.

**Pros:** ~100-token classifier prompt, minimal cost.
**Cons:** blueprint still needs its own pre-match step, so we've split one LLM call into two cheap calls. No clean win.
**Risk:** low but probably not worth the complexity.

---

## 4. Recommendation

**D1 — slim the classifier, keep the dispatcher shape.**

Rationale:
- The dispatcher's three remaining jobs (Stop, ack, dispatch-time swarm routing) are all worth doing pre-loop.
- A 400-token classifier prompt is cheap, especially with `cache_control` on the prompt base.
- Blueprint hint is a known-valuable output of the classifier that we'd otherwise re-compute.
- The `swarm_candidate` flag gives the Queen-bypass dispatch-time route — it catches obvious parallelism without paying for main-agent discovery, and the JIT tool catches everything else. They compose.

Post-change pipeline:

```
User message
    │
    ▼
┌────────────────────────┐
│ Slim classifier        │ ~400 tokens, cached base
│ (is_stop, ack,         │
│  swarm_candidate,      │
│  blueprint_hint)       │
└────────────────────────┘
    │
    ├─ is_stop ─────────────► cancel active task, return ack
    │
    ├─ swarm_candidate ─────► dispatch-time Hive (Err(HiveRoute))
    │
    └─ else ────────────────► main agent loop
                              (with collapsed system prompt + cache,
                               no iteration ceiling,
                               spawn_swarm tool available)
```

Any classification can be wrong. The key invariant from v5.3.5 holds: **misclassification never blocks tool access.** `is_stop=false` by default; `swarm_candidate=false` by default. Worst case of a mis-classified message is "goes to main agent loop", which is the default correct behaviour.

---

## 5. Classifier prompt (draft)

Target ~400 tokens after collapse. Fields to populate:

```
Classify this user message for dispatch:

1. is_stop: true if the user is telling you to stop, cancel, or interrupt
   an ongoing task. false for everything else including new requests.

2. ack: a short (≤15 words) first-person acknowledgement you would say
   to the user right now. "on it" / "reading the file" / "let me check".

3. swarm_candidate: true ONLY if the message obviously describes
   2+ independent units of work with no sequential dependency between
   them. "research X, Y, Z and compare" → true. "refactor the auth
   module" → false (one unit, may parallelise internally, that's for
   the main agent to decide).

4. blueprint_hint: one of {login, search, extract, compare, navigate,
   fill_form, unknown} or null.

Return JSON: {"is_stop": bool, "ack": string, "swarm_candidate": bool,
              "blueprint_hint": string | null}
```

Prompt is static-cacheable. Only the user message varies.

---

## 6. Migration path

The slim is schema-only. Steps:

1. Add new fields on `MessageClassification` (keep old fields as deprecated, unused).
2. Update classifier prompt to populate both old (for observability) and new fields.
3. Flip runtime to read new fields.
4. Remove old fields.
5. Drop tier mapping logic from `ExecutionProfile`.

Each step zero-risk on its own. No single-commit rewrite.

---

## 7. Observability

What we lose by slimming:

- Difficulty labels (Simple/Standard/Complex) were useful for Eigen-Tune routing (per memory: `eigentune_complexity` tag). After collapse, replace with: `spawned_swarm: bool`, `total_tool_rounds: u32`, `actual_cost_usd: f64`. Eigen-Tune gets *outcome* telemetry instead of *intent* telemetry — strictly better for routing decisions.

What we keep:
- `is_stop` counts (how often users interrupt)
- `swarm_candidate` counts (vs `spawned_swarm` actual) → classifier calibration metric
- `blueprint_hint` counts → blueprint match-rate metric

---

## 8. Summary

The dispatcher does not die — it slims down. After all planned changes, its job is:

1. **Stop fast-path** (cancellation)
2. **Ack generation** (UX)
3. **Dispatch-time swarm for obvious cases** (cost optimisation over JIT)
4. **Blueprint hint production** (blueprint match dependency)

The 5-axis classifier collapses to 4 cleaner axes. Tokens drop ~60%. Misclassification risk drops because each axis is a simpler question. The main agent is the arbiter of everything else — its access to tools is never gated by classifier output.

This is the natural conclusion of v5.3.5: the classifier stops pretending to *decide*, and starts just *advising*.
