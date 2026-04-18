# JIT Swarm — Prerequisites

**Status:** Living document. Each prerequisite gets its own scenario matrix before implementation.
**Date:** 2026-04-18
**Related:** `JIT_DESIGN.md` §9-10

---

## Summary table

| # | Prereq | Blocks | Standalone value |
|---|---|---|---|
| P1 | 429 retry + backoff in providers | JIT, raising `max_workers` | **High** — fixes rate-limit UX today |
| P2 | Anthropic `cache_control` on system prompt | Prompt collapse, JIT worker cost | **High** — ~90% system-prompt cost reduction |
| P3 | Budget plumbing (`SwarmResult` → `BudgetTracker`) | JIT (safety-critical) | Medium — closes latent bug in dispatch-time Hive |
| P4 | Replace 200-call ceiling with stagnation + budget + duration | JIT worker legitimacy | **High** — removes arbitrary limit the user asked to kill |
| P5 | Collapse prompt stratification | JIT worker prompt parity | Medium — simplifies prompt-builder surface |

Each P has three gates before implementation:

1. **Scenario matrix filled in** — known-safe and known-changing scenarios, plus regression-possible scenarios with mitigations.
2. **ZERO-RISK confirmation** — per memory policy, no low/medium/high risk allowed unless explicitly approved.
3. **Self-test plan** — how we'd verify behaviour before shipping.

---

## P1 — 429 retry + backoff in providers

**Files touched:** `crates/temm1e-providers/src/anthropic.rs`, `crates/temm1e-providers/src/openai_compat.rs`

### Current state

`anthropic.rs:347`: detect 429, rotate key, return `Temm1eError::RateLimited(body)`. No retry. No header parsing. Comment: *"Non-success status codes return immediately — no retry."*

`openai_compat.rs:677`: identical shape.

### Proposed state

```rust
// Pseudocode, final shape TBD during scenario-matrix review
async fn send_with_backoff(req: Request) -> Result<Response, Temm1eError> {
    for attempt in 0..MAX_RATELIMIT_RETRIES {
        let resp = send(req.clone()).await?;
        if resp.status() == 429 {
            let wait = parse_retry_after(&resp).unwrap_or(backoff(attempt));
            tokio::time::sleep(wait).await;
            continue;
        }
        return Ok(resp);
    }
    Err(Temm1eError::RateLimited("exhausted retries".into()))
}
```

Knobs:
- `MAX_RATELIMIT_RETRIES` = 3 (config: `[provider] max_ratelimit_retries`)
- `backoff(attempt)` = 2^attempt * 500ms + jitter
- `parse_retry_after`: read `retry-after` header (Anthropic) or `x-ratelimit-reset-requests` (OpenAI), prefer explicit wait

### Scenarios to verify ZERO-RISK

| Scenario | Expected | Status |
|---|---|---|
| User on tier-5, no 429s ever | identical behaviour | TBD |
| User gets one 429, retry succeeds | silent retry, completes | TBD |
| User gets N 429s, all retries exhausted | same `RateLimited` error as today | TBD |
| User with API key rotation configured | rotation still happens on 429 **before** retry | TBD |
| Streaming request hits 429 mid-stream | error surfaces, no retry (semantically wrong to retry) | TBD |
| Parallel JIT workers all hit 429 | each retries independently; no pile-on | TBD |

### Self-test plan

- Unit test: mock HTTP 429 response, verify retry count and backoff timing.
- Integration test: configure invalid-then-valid key rotation, verify rotation + retry compose.
- Live test: tier-1 account, run a 10-parallel swarm, verify no cryptic errors.

---

## P2 — Anthropic `cache_control` on system prompt

**Files touched:** `crates/temm1e-providers/src/anthropic.rs`

### Current state

`anthropic.rs:107-108`: system prompt placed as plain string in the request body. No `cache_control` annotation.

### Proposed state

Mark the system prompt as `ephemeral` cacheable:

```rust
// Pseudocode
body["system"] = json!([{
    "type": "text",
    "text": system_prompt,
    "cache_control": {"type": "ephemeral"}
}]);
```

Per Anthropic docs: cached tokens cost 10% of base input rate. TTL 5 minutes, refreshed on each hit. First turn pays full rate; subsequent turns within 5 min pay 10%.

### Scenarios to verify ZERO-RISK

| Scenario | Expected | Status |
|---|---|---|
| First message in session | same behaviour, cache write | TBD |
| Second message within 5 min | cache hit, ~90% cheaper system prompt | TBD |
| Second message after 5 min | cache miss, identical to first | TBD |
| System prompt changes between turns (e.g., different tier today, collapsed tomorrow) | cache invalidates, rebuilt next turn | TBD |
| Non-Anthropic provider (OpenAI-compat, Gemini) | no-op, unchanged behaviour | TBD |
| Anthropic API returns cache-related error | graceful fallback to uncached request | TBD |

### Self-test plan

- Add assertion in test harness: second consecutive turn's `cache_read_input_tokens` > 0.
- Manual CLI test: run 10-turn benchmark from memory, observe cost drop.
- Cost telemetry: log `cache_creation_input_tokens` and `cache_read_input_tokens` separately in BudgetTracker.

### Dependency note

Step 5 of the sequencing plan (collapse prompt stratification) only makes economic sense **after** P2 ships. Without caching, collapsing increases token cost ~2× for Chat-classified messages. With caching, cost delta is near zero.

---

## P3 — Budget plumbing

**Files touched:** `crates/temm1e-hive/src/lib.rs`, `crates/temm1e-hive/src/types.rs`, call sites in `main.rs` and (new) `runtime.rs` tool handler.

### Current state

`SwarmResult` (`types.rs:379-392`) returns `total_tokens` and `wall_clock_ms` but no input/output split and no cost. Parent `BudgetTracker` never sees swarm usage.

### Proposed state

Extend `SwarmResult`:

```rust
struct SwarmResult {
    text: String,
    total_input_tokens: u64,
    total_output_tokens: u64,
    total_cost_usd: f64,
    wall_clock_ms: u64,
    task_count: usize,
    completed_count: usize,
    escalated: Vec<(String, String)>,
}
```

Dispatch-time call site (`main.rs:5318+`) adds:
```rust
self.budget.record_usage(
    swarm_result.total_input_tokens,
    swarm_result.total_output_tokens,
    swarm_result.total_cost_usd,
);
```

JIT tool handler (new) does the same.

### Scenarios to verify ZERO-RISK

| Scenario | Expected | Status |
|---|---|---|
| User with `max_spend_usd = 0` (unlimited) | identical behaviour, no cap | TBD |
| User with `max_spend_usd = 1.00`, single dispatch-time swarm, total 0.40 | spend tracked, well under cap | TBD |
| User with `max_spend_usd = 1.00`, swarm pushes over cap mid-run | cancellation fires, partial result returned | TBD |
| Per-worker cost summed vs actual provider pricing | match to ±1% (rounding) | TBD |

### Self-test plan

- Unit test: mock provider returning known usage, verify `BudgetTracker` total.
- Integration: run a 3-task swarm with `max_spend_usd = 0.01`, verify cancellation trips.

---

## P4 — Replace 200-call ceiling with stagnation + budget + duration

**Files touched:** `crates/temm1e-agent/src/runtime.rs`, new `crates/temm1e-agent/src/stagnation.rs`, `crates/temm1e-core/src/types/config.rs`.

### Current state

`runtime.rs:1158`: `if rounds > self.max_tool_rounds { break; }` with `max_tool_rounds = 200` default. Configurable but present. No stagnation detection (grep for `stagnation|identical|same.*tool|duplicate.*call` → 0 hits).

### Proposed state

1. Change default `max_tool_rounds = 0` (unlimited sentinel, matching `max_task_duration_secs`).
2. Add `StagnationDetector`:
   ```rust
   struct StagnationDetector {
       recent_hashes: VecDeque<u64>,     // tool_name + canonical_json(input)
       recent_results: VecDeque<u64>,    // hash of tool result bytes
       window: usize,                    // 4-8
   }
   impl StagnationDetector {
       fn observe(&mut self, call: &ToolCall, result: &ToolResult) -> StagnationSignal;
   }
   enum StagnationSignal {
       None,
       RepeatingCalls { count: usize },      // same (tool, input) N times
       RepeatingResults { count: usize },    // same output N times (different inputs)
   }
   ```
3. On non-None signal, break the loop with an explicit final prompt to the model: *"You appear to be repeating. Synthesize what you have and reply."*

### Scenarios to verify ZERO-RISK

| Scenario | Expected | Status |
|---|---|---|
| User runs a legitimate 300-tool refactor | completes, no arbitrary stop | TBD |
| Model stuck calling `file_read` on same file forever | stagnation detector breaks at N=4 | TBD |
| Model stuck getting identical tool results (different tool choices) | stagnation detector breaks at N=3 | TBD |
| User has `max_tool_rounds = 50` in config (explicit limit) | limit honoured, behaviour as today | TBD |
| Model calls same tool with same args as part of legitimate verification loop | risk: false positive. Mitigation: window size + result diversity requirement | **OPEN** |
| Very long legitimate session with budget cap | budget fires first, graceful stop | TBD |

### Risk flag

Scenario 5 is the open question. Legitimate verification flows (e.g., "check until deploy succeeds") may call same tool + args multiple times. The detector must distinguish "same call, expecting different result" (legitimate) from "same call, getting same result" (pathological). Current proposal: break only when `recent_hashes` AND `recent_results` both show repetition. A retry loop with changing results is fine; a retry loop with identical results is not.

### Self-test plan

- Unit tests for each StagnationSignal variant.
- Integration: deliberate infinite-loop tool mock, verify break within N iterations.
- Regression: run existing 10-turn benchmark (per memory protocol), verify no false positives.

---

## P5 — Collapse prompt stratification

**Files touched:** `crates/temm1e-agent/src/prompt_optimizer.rs`, `crates/temm1e-agent/src/context.rs`, `crates/temm1e-core/src/types/optimization.rs`.

### Current state

Four tiers (Minimal/Basic/Standard/Full) mapped from classifier `difficulty` via `ExecutionProfile`. Tier determines which sections go in the system prompt. Real cost delta (Basic ~800 tokens, Full ~2500) because prompt caching is off.

### Proposed state

After P2 ships, collapse to a single prompt that includes all sections except Minimal-only identity (always on). Delete `PromptTier` enum, `ExecutionProfile.prompt_tier`, `build_sections` per-tier branches. All messages get the same prompt.

### Scenarios to verify ZERO-RISK

| Scenario | Expected | Status |
|---|---|---|
| Chat-classified message, post-collapse | sees full prompt incl. Verification + Self-correction (fixes today's subtle regression) | TBD |
| Complex-classified message, post-collapse | sees same prompt as Chat (was Full anyway) | TBD |
| Multi-turn cost | same or lower (cache pays off after turn 1) | TBD |
| Model misses planning guidance on complex tasks | risk: Planning section was Full-only. Keep it in collapsed prompt. | TBD |
| First turn cost | higher than today's Chat (paid once), same as today's Complex | TBD |

### Dependency

**Must ship after P2.** Without caching, every Chat-classified turn pays an extra ~1200 tokens.

### Self-test plan

- Verify tier test expectations removed (`tier_token_ordering` etc.).
- 10-turn CLI benchmark: first turn slightly more expensive, turns 2-10 cheaper (cache hits).
- Manual: run each classifier bucket post-collapse, verify behaviour matches pre-collapse for corresponding tier.

---

## Tracking

Each prereq moves through: `Proposed` → `Scenario matrix complete` → `ZERO-RISK confirmed` → `Implementation PR` → `Shipped`.

Status as of 2026-04-18: all five at `Proposed`.
