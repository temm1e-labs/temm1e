# Engram Memory — Design Document

> *λ-Memory forgets; **Engram** remembers.*
>
> Engram is TEMM1E's **permanent, self-curating long-term memory** — a core tool the
> agent uses in natural language, backed by a deterministic math substrate and an
> optional LLM curator. It is **default-on**; when disabled it degrades to a simple
> capped `MEMORY.md`. Status: **DESIGN + IN PROGRESS** (v5.7.0, branch `570`).
>
> Companion artifacts: `tems_lab/PERMANENT_MEMORY_ARCHITECTURE.pdf` (visual spec),
> `crates/temm1e-agent/src/engram.rs` (the scoring core, implemented + tested).

---

## 1. Problem Statement

λ-Memory scores every entry `importance × exp(−λ·Δt)` with a single global `λ`, so
*every* memory eventually decays below visibility — even one the user explicitly asked
to "remember" (a max-importance 5.0 fact drops below the 0.01 floor by ~30 days).
`explicit_save` only blocks **deletion**, not **decay-visibility**. Other agents use a
`MEMORY.md` the model edits at will, but a plain file grows unbounded (agents hoard,
rarely prune), goes stale, and is unsafe under a multi-channel gateway.

Engram needs to satisfy all of these **at once**:

1. **Skull-bound** — never exceeds the context window; the permanent tier has a hard cap.
2. **Self-sustaining, no repetition** — the agent curates; the user never repeats a fact.
3. **Earned + lost** — facts promote by judged relevance and demote by disuse, automatically.
4. **Core tool, NL-driven** — "hey Tem, remember my birthday" just works; no slash commands.
5. **Immediate control** — "remember / forget / correct" applies instantly and sticks.
6. **Coexists with λ-Memory** (opt-in, conversation-only today) without changing it.

## 2. Relationship to λ-Memory (coexistence, no conflict)

λ-Memory today is **off by default** (gated by `shared_memory_strategy`; opt-in via
`/memory lambda`, `runtime.rs:1409`) and stores **only** `Conversation` turns (the write
path hardcodes `memory_type = Conversation`, `runtime.rs:2062`). Engram **reuses its
storage + decay/FTS code** but is a separate feature:

| | Conversational λ (today) | Engram (new) |
|---|---|---|
| Default | off (Echo), opt-in | **on** (curated + bounded) |
| Written by | auto-capture each turn | the **tool** (agent/user) |
| `memory_type` | `Conversation` | **`Permanent`** (new tag) |
| Gating | `strategy == Lambda` | its **own** flag (not the toggle) |
| Injection | only if Lambda on | **always** (own capped block, scope-filtered) |

Four decoupling rules: (1) distinct `memory_type` ⇒ no collision in the shared table;
(2) separate write path; (3) independent default-on flag ⇒ works in Echo mode;
(4) always-on injection of the small capped block. `/memory echo|lambda` is unchanged.

## 3. Core Model — Two-Component Memory

After the **Two-Component Model** (Bjork & Bjork) operationalized by the **FSRS**
spaced-repetition scheduler. Each fact carries a stored importance `I` and a
`last_accessed` timestamp; everything else is derived lazily at read time (scores are
never stored — the same principle as λ-Memory).

```
I_eff(now) = I · exp( −Δt / τ ),     Δt = max(0, now − last_accessed)   [days]
```

`I_eff` is the **effective importance**: the stored importance annealed by time since
last use. Tier is derived from `I_eff`.

## 4. Lifecycle (the formulas — implemented in `engram.rs`)

```
seed   (first judgment Î∈[0,5]):   I ← clip(Î, 0, 5)
update (re-judged / reinforced):   I ← clip( (1−η)·I + η·Î , 0, 5 ) ; last_accessed ← now
i_eff  (lazy, read-time):          I_eff = I · exp(−Δt/τ)   (user-pin ⇒ I_eff = 5, no anneal)
tier   (hysteresis):               pinned ∨ I_eff ≥ θ↑           ⇒ Permanent
                                   (Permanent ∧ I_eff ≤ θ↓)      ⇒ demote → Active(λ)
                                   I_eff < ε                     ⇒ Archived (hash)
```

- **"Stated once is enough"** — a confidently-judged fact (`Î ≥ θ↑`) is permanent on its
  first write; an unsure one stays in λ and is promoted only if the agent keeps judging it
  relevant. **No *user* repetition either way.**
- **Forgetting is curator-independent** — the lazy anneal drags `I_eff` down with disuse,
  so stale facts demote by pure math even when the Curator is off (**timeproof**).
- **Recency is free** — using a fact resets `last_accessed`, so `I_eff` already encodes
  "recently relevant"; one constant `τ`, no separate recency term.
- **Hysteresis** (`θ↓ < θ↑`) + EMA smoothing prevents turn-to-turn flapping.

Defaults: `η=0.4`, `θ↑=3.5`, `θ↓=2.0`, `τ=60 d`, `ε=0.05`.

## 5. Skull Binding (holds on every model)

```
bone + active + blueprint + permanent(≤P_max) + λ-remainder + reserve + guard  ≤  skull
```

`permanent_render = min(P_max, free_space)`, ranked by `I_eff`. On a small-context model,
the permanent block **degrades fidelity** (full→summary→essence), then renders only the
top-`I_eff` subset **this turn** — it is never demoted for lack of space, and the window
never overflows. Budgeted against `1.15 × estimate_tokens` to absorb the `len/4`
heuristic's error.

## 6. Memory as a Core Tool

Slash commands are redundant; the agent calls a tool in natural language. **Extends the
existing `memory_manage` tool** (`temm1e-tools/src/memory_manage.rs`).

| Action | When the agent calls it |
|---|---|
| `remember{content, type, scope}` | "remember…" or it learns a durable fact (in-loop / end-of-loop) |
| `update{target, content}` | a fact changed ("actually it's RunPod now") → supersede |
| `forget{target}` | "forget that" |
| `recall{query}` | needs a fact not currently in context (FTS over all tiers) |

Two flows: **in-loop, immediate** (the agent writes as you talk; e.g. it normalizes
"2/3/1994" and may ask Feb-3-vs-Mar-2) and **end-of-loop learning** (after a task, a brief
reflection lets it persist durable learnings — sibling of the blueprint-authoring step).

## 7. The Curator (optional safety-net)

The agent normally curates **in-loop** via the tool. The Curator is the same logic run
**automatically** after a *substantive* run, for when the agent didn't think to: one
bounded LLM call reads `run digest + permanent set (capped) + a few stale candidates` and
emits a **structured diff** (`add / rescore / promote / demote / merge / supersede`). It is
gated by the existing "worth remembering" heuristic, throttleable, and **disable-able**.

**The math/apply layer is the trust boundary** — it validates the diff, enforces `P_max`,
and treats **user pins as immutable**. The LLM can never violate an invariant.

## 8. Robustness — Bulletproof (failure mode → guard)

| Failure mode | Guard |
|---|---|
| Curator hallucinates / malformed diff | apply layer validates schema; unknown ids rejected |
| Curator demotes/deletes a user pin | pins **immutable** in apply layer — LLM cannot touch |
| Permanent set exceeds the window | cap `P_max` + fidelity degrade + drop-this-turn |
| Tiny-context model (bone+active large) | render top-`I_eff` subset at lowest fidelity |
| Stale fact never re-judged (curator off) | lazy time-anneal demotes it (no LLM) |
| λ store grows forever on disk | scheduled GC prunes old low-`I_eff` non-pinned |
| Multi-user / multi-chat leakage | scope (global/user/chat) filter at injection |
| Clock skew (now < last_accessed) | `Δt = max(0, ·)` (saturating) |
| Token estimate undercount | budget against `1.15×` + 2% guard band |
| Score thrash | EMA smoothing + hysteresis band `[θ↓, θ↑]` |
| Supersession clobbers wrong fact | match on subject-key + similarity; old kept as provenance |

## 9. Long-term Behavior — Timeproof

`I ∈ [0,5]` clipped (no drift/saturation); context bounded by `P_max`; on-disk storage
bounded by GC; the anneal guarantees forgetting **without** an LLM; no counter grows
unbounded (`u64` seconds). Behavior is identical across models because every budget is
**Skull-relative**. A fact's whole life — promote, persist, anneal, demote, recall — is one
formula evaluated lazily.

## 10. Cost & Cadence

Three buckets; only the Curator is new vs a plain `MEMORY.md`:

| Bucket | When | ~Tokens | New vs MEMORY.md |
|---|---|---|---|
| Read (inject block) | every turn | ≤`P_max` in, cacheable | No |
| Write (tool call) | memory-write turns | +1 round-trip | No |
| Curator (self-heal) | substantive runs, gated | 1 call: ~1–3k in + ~150–300 out | **Yes** |

Heavy user (~30 curator calls/day) ≈ ~1.2M in + 120k out/month ⇒ self-hosted ~$0,
mid-tier ~$0.8/mo, flagship ~$5/mo (prompt caching cuts input further). **Cadence is the
cost dial:** per-substantive-run (default) · every N · session-end · off. **Engram off ⇒
Curator $0.**

## 11. Data Model & Schema

Additive to `LambdaMemoryEntry` (`temm1e-core/src/traits/memory.rs`) and the
`lambda_memories` table (`temm1e-memory/src/sqlite.rs`):

```
+ importance:  f32     // I — seed = first judgment, EMA on update (reuses existing column)
+ pinned_by:   enum { None, Agent, User }      // User ⇒ I_eff=5, locked
+ scope:       enum { Global, User(id), Chat(id) }   // injection filter
+ subject_key: String  // supersession key ("pref:gpu-provider")
+ fact_type:   enum { Identity, Preference, Project, Constraint, Reference }
+ links:       Vec<Hash>
  memory_type = Permanent              // distinct from Conversation (λ)
  derived lazily: I_eff, tier          // never stored
```

Migration: `ALTER TABLE lambda_memories ADD COLUMN …` (idempotent, defaulted) — existing
rows become `pinned_by=None, scope=Global, fact_type=Reference`.

## 12. Config

```toml
[memory.engram]
enabled    = true              # off ⇒ simple capped MEMORY.md fallback
curator    = "substantive"     # | "every:N" | "session-end" | "off"
p_max_frac = 0.10              # permanent block ≤ 10% of the window
max_facts  = 32
eta = 0.4 ; theta_up = 3.5 ; theta_down = 2.0 ; tau_days = 60
```

## 13. Implementation Plan (the needed details)

Scoped to land on branch `570` as checkpoint 2. Each phase builds + tests before the next.

1. **Scoring core** — `crates/temm1e-agent/src/engram.rs` (pure math: seed/EMA/anneal/
   tier/pack). **DONE + 14 unit tests passing.**
2. **Data model** — extend `LambdaMemoryEntry` (core) + `LambdaMemoryType::Permanent` or a
   `permanent` flag; add `EngramConfig` to `temm1e-core/src/types/config.rs`.
3. **Schema migration** — idempotent `ADD COLUMN`s + indexes in `temm1e-memory/src/sqlite.rs`.
4. **Storage methods** — `engram_store / engram_list(scope) / engram_update / engram_forget /
   engram_recall` on the `Memory` trait; implement in **both** `SqliteMemory` and
   `ResilientMemory` (the latter currently no-ops all λ methods — landmine).
5. **Injection** — a capped, scope-filtered Permanent block in `context.rs::build_context`,
   placed before the λ remainder; uses `engram::pack_by_budget` + fidelity degrade.
6. **Tool** — extend `memory_manage` (or a thin `engram` tool) with `remember/update/
   forget/recall` writing `Permanent` entries; NL-driven (no command).
7. **Curator + apply layer** — gated post-run hook in `runtime.rs` (sibling of blueprint
   authoring) → bounded LLM call → structured diff → deterministic apply (trust boundary:
   validate, cap, pins immutable). Cadence from config.
8. **GC** — wire a scheduled/startup prune of old low-`I_eff` non-pinned λ entries.
9. **MEMORY.md fallback** — when `enabled=false`, a single capped markdown doc the agent
   edits via the same tool (no curator/decay/tiers).
10. **Tests** — unit (scoring: done) + integration (store/recall/scope/migration/apply-
    rejects-bad-diff/pin-immutability/packer-cap) + a live curator smoke test.

## 14. Dry Run

| event | memory action | added LLM |
|---|---|---|
| "remember my birthday 2/3/1994" | `remember` → **User-pin** `I_eff=5` (date normalized) | tool round-trip |
| deploy task; "I use Lambda Labs" (clear pref) | `remember` `Î=4` ⇒ `I=4≥θ↑` → **permanent day 0** | + Curator ×1 |
| 60 days, no deploy talk | anneal: `I_eff=4·e^(−60/60)=1.5≤θ↓` → **auto-demote** (no user, no LLM) | none |
| "actually I moved to RunPod" | `update` supersede; new `Î=4.5` → permanent now | tool round-trip |
| new session: "when's my birthday?" | already in Permanent block (scope=user) | none |
| "what was that fix last month?" | `recall` → FTS hit, reheated | tool round-trip |

No repetition anywhere; user pin instant; promotion by judgment; demotion by math.

## 15. References

- R.A. Bjork & E.L. Bjork, *A new theory of disuse and an old theory of stimulus
  fluctuation* (two-component memory: retrievability vs stability).
- FSRS (Free Spaced Repetition Scheduler) — stability grows with spaced retrieval.
- Park et al., *Generative Agents* — memory importance + reflection (our consolidation).
- Packer et al., *MemGPT* / Letta — core (bounded, always-in-context) vs archival memory,
  memory editing as tool calls (our tool surface).
- `tems_lab/LAMBDA_MEMORY.md` — the decay substrate Engram builds on.
