# Self-Audit Cost Attribution — Design Note

**TEMM1E v5.7+ candidate**

The v5.6.0 Self-Audit Pass adds one extra LLM round-trip per text-only turn (when the audit fires) and v5.7+ is expected to flip it default-on. Audit cost IS already counted in aggregate spend, but it is NOT separately attributed — users cannot tell what fraction of their bill is audit overhead. This doc captures the gap, the proposed fix, and the empirical motivation.

---

## Why this matters

Empirically observed during the v5.6.1 A/B (see commit `9f4ee59` / journal branch):

- V0 (Self-Audit OFF) on `A_enum` prompt: 2 API calls per trial
- V1 (Self-Audit ON) on `A_enum` prompt: 3 API calls per trial
- **+50% API calls on every text-only turn that fires the audit**

At gpt-5.4 prices (~$5/M input + $25/M output) this is real money. If we flip Self-Audit default-on in v5.7, users deserve to see "X% of your last month's bill was Self-Audit overhead" so they can decide if it's worth it. Aggregate spend tracking alone hides this.

---

## Current state (verified in code)

### What IS tracked

1. **Every audit LLM call is cost-counted** — the audit fires by pushing a synthetic user message into `session.history` and `continue`-ing the main loop. The next iteration calls `self.provider.complete()` through the normal path, which goes through `record_call_cost` in `crates/temm1e-agent/src/budget.rs:333+` and accumulates into `total_spend_usd`. No audit call is ever "free" or unbilled — it counts against `max_spend_usd`.

2. **A cost-cap gate exists at audit fire site** — `crates/temm1e-agent/src/runtime.rs:2083-2085`:

   ```rust
   && (self.budget.max_spend_usd() == 0.0
       || self.budget.total_spend_usd() + (turn_cost_usd * 0.2)
           < self.budget.max_spend_usd())
   ```

   The audit is skipped if total + estimate (audit at ~20% of turn cost so far) would exceed the budget. So default-on cannot blow past the cap.

3. **Audit outcomes are persisted** via `record_audit_outcome` into the `model_discipline` SQLite table (counters per `provider+model`: `text_only_exits`, `audit_done_responses`, `audit_tool_call_responses`, `audit_failed_responses`, `audit_skipped`).

### What is NOT tracked — the gap

1. **No per-call-kind label.** `provider.complete()` doesn't know whether it's being invoked for the main turn, the classifier (`temm1e_agent::llm_classifier`), the consciousness pre-pass (`temm1e_agent::consciousness_engine`), or the audit. So when budget accumulates a cost line, it can't be attributed.

2. **No "audit overhead %" telemetry.** Users see `total_spend_usd` but not `audit_spend_usd / total_spend_usd`.

3. **The 20% cost estimate at the fire site is a static guess.** Real audit cost varies (5–80% of main turn cost depending on output length and audit response). For real-time gating that's fine; for retrospective accounting it's not informative.

4. **`/usage` shows no breakdown** — only the total.

---

## Why it wasn't built in v5.6.0

v5.6.0 prioritized landing the audit machinery + outcome telemetry + cost-cap gate. Per-call-kind attribution would touch the `Provider` trait signature across **every** provider implementation (Anthropic, OpenAI-compat, Gemini, Codex OAuth) plus call sites in `runtime.rs`, `llm_classifier.rs`, `consciousness_engine.rs`, and `self_audit.rs`. Larger blast radius than v5.6.0's scope. Deferred deliberately.

---

## Proposed design

### 1. `CallKind` enum threaded through provider trait

New enum in `crates/temm1e-core/src/types/`:

```rust
/// What kind of LLM call this is. Label-only — doesn't affect the API
/// request body or behavior. Used for cost attribution, telemetry, and
/// budget breakdowns.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum CallKind {
    /// Main user-facing turn — what the user is paying for primarily.
    Main,
    /// LLM classifier (task complexity / intent / difficulty).
    Classifier,
    /// Consciousness pre-pass or post-pass observer.
    Consciousness,
    /// Self-Audit Pass (GH-62 verification round).
    SelfAudit,
    /// Constraint-Audit Pass (future — see A/B writeup).
    ConstraintAudit,
    /// Cambium skill-growth or self-work LLM call.
    Cambium,
    /// TemDOS core invocation (specialist sub-agent).
    TemDosCore,
    /// Witness verification (predicate Tier 1/2).
    Witness,
    /// Other / unlabeled.
    Other,
}
```

### 2. Provider trait change

Add an optional `kind` parameter to `Provider::complete()`. Default impl forwards as `Other` for backwards compatibility — every provider implementation overrides explicitly. Body is unchanged:

```rust
async fn complete(
    &self,
    request: CompletionRequest,
    kind: CallKind,  // new — label only
) -> Result<CompletionResponse, Temm1eError>;
```

The `kind` is passed through to whoever records cost (`budget.rs`'s `record_call_cost`) so it can update per-kind counters.

### 3. Budget tracker per-kind counters

Extend `BudgetTracker` in `crates/temm1e-agent/src/budget.rs`:

```rust
pub struct BudgetTracker {
    // ... existing fields ...
    /// Per-call-kind spend breakdown.
    spend_by_kind: Arc<RwLock<HashMap<CallKind, f64>>>,
}

impl BudgetTracker {
    pub fn record_call_cost(&self, cost_usd: f64, tokens_in: u32, tokens_out: u32, kind: CallKind) {
        // existing: accumulate total_spend_usd
        // new: spend_by_kind.entry(kind).and_modify(|v| *v += cost_usd).or_insert(cost_usd);
    }

    pub fn spend_by_kind(&self) -> HashMap<CallKind, f64> { /* clone snapshot */ }
}
```

### 4. `/usage` command shows breakdown

In `src/main.rs`'s `/usage` handler:

```text
Total spend: $0.4382 (limit: $5.00)

By call kind:
  Main:           $0.3120  (71.2%)
  SelfAudit:      $0.0710  (16.2%)
  Classifier:     $0.0420  ( 9.6%)
  Consciousness:  $0.0132  ( 3.0%)
```

### 5. Telemetry table extension

Add per-kind aggregate to the `model_discipline` table OR a new `cost_by_kind` table:

```sql
CREATE TABLE cost_by_kind (
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    kind TEXT NOT NULL,
    total_cost_usd REAL NOT NULL DEFAULT 0,
    total_calls INTEGER NOT NULL DEFAULT 0,
    last_updated INTEGER NOT NULL,
    PRIMARY KEY (provider, model, kind)
);
```

Lets us compute `audit_overhead_pct = sum(cost_by_kind WHERE kind='SelfAudit') / sum(cost_by_kind)` over arbitrary windows.

---

## Implementation sketch

| File | Change |
|---|---|
| `crates/temm1e-core/src/types/call_kind.rs` | New file: `CallKind` enum |
| `crates/temm1e-core/src/lib.rs` | Re-export `CallKind` |
| `crates/temm1e-core/src/traits/provider.rs` | Add `kind: CallKind` parameter to `complete()` and `stream()` |
| `crates/temm1e-providers/src/anthropic.rs` | Accept `kind`, pass through to cost recording. No API behavior change. |
| `crates/temm1e-providers/src/openai_compat.rs` | Same. |
| `crates/temm1e-providers/src/gemini.rs` | Same. |
| `crates/temm1e-codex-oauth/src/responses_provider.rs` | Same. |
| `crates/temm1e-agent/src/budget.rs` | Add `spend_by_kind` HashMap, extend `record_call_cost`. |
| `crates/temm1e-agent/src/runtime.rs` | Audit fire site passes `CallKind::SelfAudit`. Main calls pass `CallKind::Main`. |
| `crates/temm1e-agent/src/llm_classifier.rs` | Pass `CallKind::Classifier`. |
| `crates/temm1e-agent/src/consciousness_engine.rs` | Pass `CallKind::Consciousness`. |
| `src/main.rs` | `/usage` command renders breakdown. |
| `crates/temm1e-memory/src/sqlite.rs` | New `cost_by_kind` table + Memory trait methods. |

Rough size: ~200 lines of new code across ~12 files. No new crates. Provider-agnostic by design (it's a label, not an API change).

---

## Test plan

1. **Unit**: BudgetTracker accumulates per-kind correctly when `record_call_cost` is called with different `CallKind` values.
2. **Integration** (`crates/temm1e-agent/tests/self_audit_integration.rs`): existing `QueuedMockProvider`-based test that already validates the audit fires; extend to assert `budget.spend_by_kind()[CallKind::SelfAudit] > 0` after the audit round, and `spend_by_kind()[CallKind::Main] > spend_by_kind()[CallKind::SelfAudit]`.
3. **Backwards-compat**: existing tests must keep passing — the trait change defaults to `Other` for any provider that doesn't override.
4. **Telemetry**: a SQLite test for the new `cost_by_kind` table — counter increments, multiple kinds for one provider+model, concurrent-write safe.
5. **`/usage` smoke**: end-to-end test that runs a chat turn with Self-Audit ON and verifies the `/usage` output includes a `SelfAudit:` line with non-zero spend.

---

## Risks

| Risk | Severity | Mitigation |
|---|---|---|
| Provider trait change breaks downstream provider impls in the wild | Low | The change is additive (new parameter, default `Other`). Existing code that ignores `kind` keeps working. |
| Per-kind HashMap adds contention to hot path | Low | Atomic ops or a striped RwLock. Cost is one f64 add per call — negligible. |
| Telemetry table grows unbounded | Low | `cost_by_kind` is keyed `(provider, model, kind)` — bounded by number of providers × models × kinds (~50 rows max). |
| Misattribution if a call kind isn't labeled explicitly | Medium | Default `Other` makes the gap visible in `/usage` — users see "Other: $X" and can ask why. Audit during v5.7 release to label every call site explicitly. |

---

## Empirical baseline (v5.6.1 A/B)

From the 100-trial A/B run (`/tmp/temm1e_ab/`, see `9f4ee59` commit context):

| Prompt | V0 (audit OFF) calls/trial | V1 (audit ON) calls/trial | Δ |
|---|---|---|---|
| `A_enum` (release-note) | 2 | 3 | +50% |
| `B_chained_1` (math) | 2 | 2 | 0% (audit didn't fire) |
| `B_chained_2` (Python bug) | invalid | invalid | n/a — CLI line-buffer bug, fixed in v5.6.1 |
| `C_implicit_1` (validate_phone) | 2 | 2 | 0% (audit didn't fire) |
| `C_implicit_2` (haiku) | 2 | 2 | 0% (audit didn't fire) |

Audit fires on text-only exits with tools available. On pure-text turns (release-note generation) it fires every time. On turns the model already terminates cleanly with a tool call (or no tools available), it doesn't fire. The 50% overhead is the worst-case, not the average — but it's the worst case that affects users running CLI chat for non-tool-using tasks.

---

## Open questions

1. **Should the `kind` parameter be required or `Option<CallKind>`?** Required forces every call site to think; Optional preserves zero-cost migration. Lean: required, with `CallKind::Other` available as an explicit "I don't know."
2. **Where does Codex OAuth fit?** Subscription-based; cost-per-call is $0 in dollar terms but consumes quota. The `kind` label is still useful for quota attribution even if `cost_usd = 0`.
3. **Streaming calls** — same treatment? Yes, `Provider::stream()` gets the same `kind` parameter.
4. **Should `record_audit_outcome` also carry the audit's actual cost?** Currently records only outcome. Could extend to include cost for fine-grained telemetry.

---

## Status

- **Proposed**: 2026-05-15 (this doc)
- **Target release**: v5.7.0 (alongside Self-Audit default-on flip — see release-notes for v5.6.0 in `README.md` Release Timeline)
- **Empirical basis**: v5.6.1 A/B (100 trials on gpt-5.4)
- **Owner**: TBD
- **Dependencies**: none (Self-Audit Pass already shipped in v5.6.0; this is observability-only)
