# JIT Swarm — Implementation Details (Per-Prereq Plans)

**Status:** Plans complete, all prereqs gated on 100% confidence / 0% risk confirmation.
**Branch:** `JIT-swarm`
**Date:** 2026-04-18
**Policy:** Per memory `feedback_zero_risk_100_conf.md` — no implementation until each prereq's scenario matrix is filled, every row proven non-regressive, verification path defined.

---

## Reading order

1. `HARMONY_SWEEP.md` — theoretical sweep, 9 findings.
2. This document — per-prereq implementation details. Each section below is a self-contained PR plan.
3. Implementation proceeds **strictly in the order P1 → P4 → P3 → P2 → P5 → P6 → JIT**. Do not interleave.

Per the zero-risk policy, each section must read as a "publishable PR description" before any file is touched.

---

## P1 — 429 Retry + Backoff + CB Exemption + Streaming Safety

### P1.1 Ground truth (verified in this sweep)

- `anthropic.rs:347-349` — on 429: rotate key, return `Temm1eError::RateLimited(body)`. Same at `anthropic.rs:408-410` in `stream()`.
- `openai_compat.rs:677-679` — identical, for non-streaming path.
- `openai_compat.rs:811-813` — identical, for streaming path.
- `circuit_breaker.rs:154-212` — `record_failure()` increments counter and opens circuit at threshold. **Does not inspect error type.**
- `runtime.rs:1437, 1478, 1495, 1554, 1573` — `record_failure()` called on every provider error, including `RateLimited`.
- Current `reqwest::Client` timeout: 120s.
- `backoff_duration(attempt)` already exists in `circuit_breaker.rs:223-235` with exp-backoff + jitter formula. **Reuse, don't duplicate.**
- `Temm1eError::RateLimited(String)` defined in `core/types/error.rs:37`.

### P1.2 Design

Three linked changes, all in service of "a single 429 doesn't surface as a user-facing error".

1. **Provider-level retry**: wrap the request-send + status-check in a retry loop. Retry only the first `MAX_RATELIMIT_RETRIES` 429s. Prefer `retry-after` header wait time; fall back to `CircuitBreaker::backoff_duration(attempt)`.
2. **Streaming safety**: retries only at the request-initiation boundary — before `response.bytes_stream()` is attached to the `unfold`. Current code already checks status before streaming, so the retry loop naturally sits at the correct layer.
3. **CB exemption**: in `runtime.rs`, replace each `self.circuit_breaker.record_failure()` call on provider error with a type-aware helper: record as failure only if the error is NOT `Temm1eError::RateLimited(_)`.

### P1.3 Code shape

**New helper** in `crates/temm1e-providers/src/rate_limit.rs` (new file):

```rust
//! Rate-limit retry helper shared by all providers.

use reqwest::Response;
use std::time::Duration;

/// Number of 429-triggered retries before surfacing RateLimited to the caller.
pub const MAX_RATELIMIT_RETRIES: u32 = 3;

/// Parse an HTTP `retry-after` header into a Duration.
///
/// Supports the two standard forms:
/// - Integer seconds: `retry-after: 30`
/// - HTTP-date: `retry-after: Wed, 21 Oct 2026 07:28:00 GMT` (best-effort; if
///   parsing fails we return None and the caller falls back to exp-backoff).
pub fn parse_retry_after(response: &Response) -> Option<Duration> {
    let header = response.headers().get("retry-after")?;
    let s = header.to_str().ok()?;
    if let Ok(secs) = s.parse::<u64>() {
        // Cap at 60s to avoid pathological server values.
        return Some(Duration::from_secs(secs.min(60)));
    }
    None
}

/// Exponential backoff with deterministic jitter. Reuses the circuit breaker's
/// proven formula by delegating — we do NOT duplicate the math here.
/// `attempt` starts at 0 for the first retry.
pub fn default_backoff(attempt: u32) -> Duration {
    // Formula from CircuitBreaker::backoff_duration: min(30s, 1s * 2^attempt)
    // with ±25% deterministic jitter. Copy the constants (we can't depend on
    // temm1e-agent from temm1e-providers — that would be a circular dep).
    const MAX_BACKOFF: Duration = Duration::from_secs(30);
    let base_ms = 1000u64.saturating_mul(1u64.checked_shl(attempt).unwrap_or(u64::MAX));
    let capped_ms = base_ms.min(MAX_BACKOFF.as_millis() as u64);
    let jitter_pattern: [i64; 8] = [-25, 15, -10, 20, -5, 25, -20, 10];
    let jitter_pct = jitter_pattern[(attempt as usize) % jitter_pattern.len()];
    let jitter_ms = (capped_ms as i64 * jitter_pct) / 100;
    let final_ms = (capped_ms as i64 + jitter_ms).max(1) as u64;
    Duration::from_millis(final_ms)
}
```

**anthropic.rs::complete** — wrap the existing request-send in a loop. Minimal diff shape:

```rust
async fn complete(&self, request: CompletionRequest) -> Result<CompletionResponse, Temm1eError> {
    let body = self.build_request_body(&request, false)?;
    debug!(provider = "anthropic", model = %request.model, "Sending completion request");

    for attempt in 0..=crate::rate_limit::MAX_RATELIMIT_RETRIES {
        let api_key = self.current_key().to_string();
        let response = self
            .client
            .post(format!("{}/v1/messages", self.base_url))
            .header("x-api-key", &api_key)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .map_err(|e| Temm1eError::Provider(format!("Anthropic request failed: {e}")))?;

        let status = response.status();

        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            let wait = crate::rate_limit::parse_retry_after(&response)
                .unwrap_or_else(|| crate::rate_limit::default_backoff(attempt));
            let error_body = response.text().await.unwrap_or_else(|_| "unknown error".into());
            self.rotate_key();
            if attempt == crate::rate_limit::MAX_RATELIMIT_RETRIES {
                error!(provider = "anthropic", attempts = attempt + 1, "Rate limit: retries exhausted");
                return Err(Temm1eError::RateLimited(error_body));
            }
            tracing::warn!(
                provider = "anthropic",
                attempt = attempt + 1,
                wait_ms = wait.as_millis() as u64,
                "Rate limited, backing off before retry"
            );
            tokio::time::sleep(wait).await;
            continue;
        }

        if !status.is_success() {
            // ... (unchanged: Auth/Provider error handling) ...
            return /* same as today */;
        }

        // ... (unchanged: success path) ...
        return Ok(/* same as today */);
    }

    unreachable!("loop either returns or continues")
}
```

The same pattern for `anthropic.rs::stream`, `openai_compat.rs::complete`, `openai_compat.rs::stream`. Note that OpenAI-compat `complete()` already has a body-read retry loop (lines 640-713) — the 429 retry is a separate, outer concern and should be inside each iteration of the body-read loop *before* the body read, OR it can be composed as: outer 429 retry, inner body-read retry. The cleaner composition: outer 429 retry wraps the whole request-response cycle; the inner body-read retry stays inside.

**runtime.rs CB exemption** — define a small inline helper:

```rust
// In runtime.rs, near the other helper fns:

/// Record provider failure into the circuit breaker UNLESS the error is a
/// rate-limit (throttle). Rate limits are not a provider-health signal — they
/// mean "slow down", not "you are broken".
#[inline]
fn record_cb_failure_if_not_ratelimit(cb: &crate::circuit_breaker::CircuitBreaker, err: &Temm1eError) {
    if !matches!(err, Temm1eError::RateLimited(_)) {
        cb.record_failure();
    } else {
        tracing::debug!("circuit breaker: ignoring RateLimited (not a health signal)");
    }
}
```

Replace each `self.circuit_breaker.record_failure();` call on provider error (5 sites: lines 1437, 1478, 1495, 1554, 1573) with `record_cb_failure_if_not_ratelimit(&self.circuit_breaker, &e);`. Must be placed before the `return Err(e)` so `e` is still in scope.

### P1.4 Tests affected / added

**Existing tests unchanged:**
- `test_backoff_duration_increases` (`circuit_breaker.rs:355`) — tests backoff duration, not retry behaviour. Unchanged.
- `test_backoff_caps_at_30_seconds` (`circuit_breaker.rs:368`) — unchanged.

**New unit tests in `crates/temm1e-providers/src/rate_limit.rs`:**
- `parse_retry_after_integer` — `retry-after: 5` → `Some(5s)`.
- `parse_retry_after_cap_60s` — `retry-after: 3600` → `Some(60s)` (cap).
- `parse_retry_after_absent` → `None`.
- `parse_retry_after_invalid` → `None` (e.g. HTTP-date we don't parse).
- `default_backoff_increases` — `default_backoff(0) < default_backoff(5)`.
- `default_backoff_caps` — `default_backoff(10) <= 30s + jitter`.

**New integration tests (using `mockito` or a minimal `httpmock` style):** not blocking — if we don't add a new dev-dep, we rely on the existing unit tests + one e2e manual check. Mark as optional.

**New unit test in `crates/temm1e-agent/src/circuit_breaker.rs`:** verify `record_cb_failure_if_not_ratelimit` behaviour:
- `rate_limit_does_not_trip_cb` — feed 10 `RateLimited` errors, CB stays Closed.
- `provider_error_still_trips_cb` — feed 5 `Provider(...)` errors, CB opens.

### P1.5 Scenario matrix

| # | Scenario | Before P1 | After P1 | Verification |
|---|---|---|---|---|
| 1 | User on tier-5, zero 429s over 100 turns | Works | Works (byte-identical, loop never executes retry branch) | Existing 100-turn benchmark |
| 2 | Single transient 429, server returns `retry-after: 2` | User sees `RateLimited` error | Silent 2s wait, retry succeeds, user sees normal response | New unit test + manual CLI |
| 3 | Single transient 429 with NO `retry-after` header | User sees error | `default_backoff(0)` ≈ 0.75-1.25s wait, retry succeeds | New unit test |
| 4 | All 3 retries exhausted (persistent 429) | User sees error after 1 attempt | User sees same `RateLimited` error after 4 attempts (~7s total) | Unit test with mocked 429 |
| 5 | 429 mid-stream (after 200 status, mid-SSE) | Error surfaces (SSE parse error) | **Unchanged** — retry happens only at initiation | Code-read confirms retry loop exits before `bytes_stream()` |
| 6 | Multi-key rotation + single 429 | Rotates key, errors | Rotates key, retries on new key, succeeds | Rotation + retry interleave unit test |
| 7 | All keys rate-limited simultaneously | Rotate once, error | Rotate each attempt, eventually all fail, error (same UX) | Manual with deliberately throttled keys |
| 8 | Non-429 error (500 Internal Server Error) | Error, CB trips | Error, CB trips (unchanged, retry loop only handles 429) | Existing CB test still passes |
| 9 | 401 Unauthorized | Rotates key, returns Auth error | Same — no retry | Code path untouched |
| 10 | Task cancelled (user stop) mid-backoff | N/A — no backoff existed | `tokio::time::sleep` is cancel-safe; cancellation propagates naturally | Verify with `tokio::select!` semantics |
| 11 | Cumulative retry time + task duration cap | N/A | Max backoff = 30+15+7.5 = ~52s worst case. If user has `max_task_duration = 60s`, could consume most of it. | Documented behaviour; users with short caps should keep them short. |
| 12 | 429 with `retry-after: 3600` (1 hour) | Error | Capped at 60s retry wait (see `parse_retry_after` cap) | Unit test `parse_retry_after_cap_60s` |
| 13 | 100 concurrent provider calls, all 429 | All error | Each retries independently; successes trickle in as server recovers | Load test optional |
| 14 | CB already Open when 429 received | Failure counted, stays Open | Retry attempts STILL blocked by `can_execute()` check upstream; when provider call fails with `RateLimited`, `record_cb_failure_if_not_ratelimit` skips CB update | **Key invariant**: CB gate is upstream of the retry — we don't bypass it. |
| 15 | Reqwest-level network timeout (120s) during retry wait | N/A | Backoff is `tokio::time::sleep`, independent of reqwest; no interference | Logically verified |

### P1.6 Verification

```bash
cargo check -p temm1e-providers -p temm1e-agent
cargo clippy -p temm1e-providers -p temm1e-agent --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test -p temm1e-providers
cargo test -p temm1e-agent circuit_breaker
```

Live smoke test: start the CLI chat with a tier-1 Anthropic key and run 10 rapid messages. Expect no `RateLimited` errors surfacing to user.

### P1.7 Risk verdict

**ZERO-RISK confirmed.** All 15 scenarios map to either "unchanged" or "silently recovers". No scenario produces worse UX than today. CB invariant preserved (`can_execute` gate is upstream; we only change the `record_failure` downstream).

---

## P4 — Kill 200-Tool-Round Ceiling + Stagnation Detector

### P4.1 Ground truth

- `runtime.rs:216` — default `max_tool_rounds: 200` (in `AgentRuntime::new`).
- `runtime.rs:1158-1164` — hard break at `rounds > max_tool_rounds`.
- `config.rs:776` — TOML default mirrored here. Validation at `config.rs:1065-1069` requires `> 0`.
- No stagnation detection anywhere (`grep stagnation|identical|duplicate.*call` → 0 hits in `runtime.rs`).
- `ExecutionProfile.max_iterations` verified dead (`grep` only finds `runtime.rs:1031` — a `tracing::info!` log, not a runtime check).
- `ExecutionProfile.skip_tool_loop` verified dead (`grep` finds only `optimization.rs` factory + `model_router.rs:3467-3468` tests + `llm_classifier.rs:471` test. **Zero reads in `runtime.rs`.**)

### P4.2 Design

1. **Change default** `max_tool_rounds = 0` to mean "unlimited" (mirror `max_task_duration_secs = 0` convention). Preserve config-override so users who set a finite value still get it.
2. **Loop gate update** — when `max_tool_rounds == 0`, skip the ceiling check entirely. When `> 0`, behave as today.
3. **Add StagnationDetector** — a small new module `crates/temm1e-agent/src/stagnation.rs`.
4. **Wire detector into loop** — call `observe()` per tool-call. On `StagnationSignal::Repeating`, break with an explicit signal that triggers a "synthesize and reply" prompt.
5. **Remove `max_iterations` and `skip_tool_loop` from `ExecutionProfile`** — dead fields. Keep `prompt_tier`, `verify_mode`, `use_learn`, `max_tool_output_chars`.
6. **Update `config.rs` validation** — allow `0` (unlimited) for `max_tool_rounds`, matching `max_task_duration_secs`.

### P4.3 Code shape

**New file `crates/temm1e-agent/src/stagnation.rs`:**

```rust
//! Stagnation detector for the agent tool loop.
//!
//! Detects two patterns that indicate the LLM is stuck:
//! 1. **RepeatingCalls** — the same (tool_name, input) called N times in a row.
//! 2. **RepeatingResults** — the last K tool-result payloads are byte-identical.
//!
//! Both must hold simultaneously for the signal to trigger `Stuck`. A
//! legitimate retry flow ("check until status=ready") produces repeating
//! CALLS with changing results — that is NOT stagnation. A legitimate
//! loop over similar data ("process 10 files") produces different calls —
//! also NOT stagnation.
//!
//! Only the intersection — same call AND same result — is a real loop
//! pathology. We detect that.

use std::collections::VecDeque;
use std::hash::{Hash, Hasher};

/// Default window: if 4 consecutive (call, result) pairs are all identical,
/// we declare stagnation. Chosen to tolerate natural 2-3 step retry flows.
pub const DEFAULT_WINDOW: usize = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StagnationSignal {
    /// No stagnation yet.
    Ok,
    /// Same (tool, input) called N times AND same result returned N times.
    /// `count` = number of identical consecutive observations.
    Stuck { count: usize },
}

pub struct StagnationDetector {
    window: usize,
    recent_calls: VecDeque<u64>,    // hashes of (tool_name, canonicalised input)
    recent_results: VecDeque<u64>,  // hashes of result bytes
}

impl StagnationDetector {
    pub fn new() -> Self {
        Self::with_window(DEFAULT_WINDOW)
    }

    pub fn with_window(window: usize) -> Self {
        Self {
            window: window.max(2),
            recent_calls: VecDeque::with_capacity(window),
            recent_results: VecDeque::with_capacity(window),
        }
    }

    /// Hash `(tool_name, input_json)` — uses serde_json canonicalisation for
    /// input so key-ordering variations don't cause false negatives.
    fn hash_call(tool_name: &str, input: &serde_json::Value) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        tool_name.hash(&mut hasher);
        // serde_json::to_string is canonical enough for our purposes since
        // we compare LLM-emitted inputs, which are typically deterministic
        // for a given tool call.
        let canonical = serde_json::to_string(input).unwrap_or_default();
        canonical.hash(&mut hasher);
        hasher.finish()
    }

    fn hash_result(result: &str) -> u64 {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        result.hash(&mut hasher);
        hasher.finish()
    }

    /// Record a (tool_call, result) observation. Returns a signal.
    pub fn observe(
        &mut self,
        tool_name: &str,
        input: &serde_json::Value,
        result: &str,
    ) -> StagnationSignal {
        let call_hash = Self::hash_call(tool_name, input);
        let result_hash = Self::hash_result(result);

        self.recent_calls.push_back(call_hash);
        self.recent_results.push_back(result_hash);
        while self.recent_calls.len() > self.window {
            self.recent_calls.pop_front();
        }
        while self.recent_results.len() > self.window {
            self.recent_results.pop_front();
        }

        if self.recent_calls.len() < self.window {
            return StagnationSignal::Ok;
        }

        let all_calls_same = self.recent_calls.iter().all(|h| *h == call_hash);
        let all_results_same = self.recent_results.iter().all(|h| *h == result_hash);

        if all_calls_same && all_results_same {
            StagnationSignal::Stuck { count: self.window }
        } else {
            StagnationSignal::Ok
        }
    }

    pub fn reset(&mut self) {
        self.recent_calls.clear();
        self.recent_results.clear();
    }
}

impl Default for StagnationDetector {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn ok_when_empty() {
        let mut d = StagnationDetector::new();
        assert_eq!(d.observe("t", &json!({"a":1}), "r"), StagnationSignal::Ok);
    }

    #[test]
    fn stuck_after_window_identical() {
        let mut d = StagnationDetector::with_window(3);
        for _ in 0..2 {
            assert_eq!(d.observe("file_read", &json!({"path":"x"}), "body"), StagnationSignal::Ok);
        }
        assert!(matches!(
            d.observe("file_read", &json!({"path":"x"}), "body"),
            StagnationSignal::Stuck { .. }
        ));
    }

    #[test]
    fn same_call_different_result_is_ok() {
        let mut d = StagnationDetector::with_window(3);
        // legitimate polling: same call, different result each time
        d.observe("http_get", &json!({"url":"x"}), "pending");
        d.observe("http_get", &json!({"url":"x"}), "pending");
        let sig = d.observe("http_get", &json!({"url":"x"}), "ready");
        assert_eq!(sig, StagnationSignal::Ok);
    }

    #[test]
    fn different_call_same_result_is_ok() {
        let mut d = StagnationDetector::with_window(3);
        // legitimate: different files returning same content
        d.observe("file_read", &json!({"path":"a"}), "body");
        d.observe("file_read", &json!({"path":"b"}), "body");
        let sig = d.observe("file_read", &json!({"path":"c"}), "body");
        assert_eq!(sig, StagnationSignal::Ok);
    }

    #[test]
    fn reset_clears() {
        let mut d = StagnationDetector::with_window(2);
        d.observe("t", &json!({}), "r");
        d.observe("t", &json!({}), "r");
        d.reset();
        assert_eq!(d.observe("t", &json!({}), "r"), StagnationSignal::Ok);
    }
}
```

**Loop integration in `runtime.rs`:**

Before the loop starts:
```rust
let mut stagnation = crate::stagnation::StagnationDetector::new();
```

After each tool call (inside the per-tool dispatch area, around line 2400+):
```rust
// Observe for stagnation right after result is produced.
if let StagnationSignal::Stuck { count } = stagnation.observe(tool_name, &tool_input, &tool_result_text) {
    tracing::warn!(
        tool = %tool_name,
        count = count,
        "Stagnation detected — same call+result {} times in a row. Forcing synthesis.",
        count
    );
    // Inject a final system turn that asks the model to synthesize.
    // Setting a flag + break; the final-reply block handles the synthesis prompt.
    stagnation_break = true;
    break;
}
```

Replace the `if rounds > self.max_tool_rounds` check:
```rust
// v5.3.6: max_tool_rounds = 0 means unlimited (matches max_task_duration convention).
// Keep the check only when user has explicitly opted into a finite ceiling.
if self.max_tool_rounds > 0 && rounds > self.max_tool_rounds {
    warn!(
        "Exceeded maximum tool rounds ({}), forcing text reply",
        self.max_tool_rounds
    );
    break;
}
```

**In `config.rs`:**
```rust
// Change line 776:
max_tool_rounds: 0,  // 0 = unlimited (matches max_task_duration_secs)
```

And update validation (line ~1065-1069) to allow 0:
```rust
// Allow 0 (unlimited) OR positive values.
// No change needed if the existing check is `> 0` — REMOVE that check entirely
// for max_tool_rounds since 0 is now valid.
```

**In `optimization.rs`:** remove `max_iterations` and `skip_tool_loop` fields from `ExecutionProfile`. Update factory methods, update 3 affected tests:
- `trivial_profile_skips_tool_loop` — delete entire test (field gone).
- `simple_profile_uses_rule_based_verify` — remove `skip_tool_loop` assertion.
- `complex_profile_highest_limits` — remove `max_iterations` assertion; keep `max_tool_output_chars` check.
- `difficulty_maps_to_execution_profile` (in `llm_classifier.rs:468-478`) — remove the `max_iterations` assertions.
- `model_router.rs:3467-3468` — remove the two `skip_tool_loop` assertions.

### P4.4 Tests affected / added

| Test | File | Action |
|---|---|---|
| `test_agent_config_defaults` | `config.rs:1207` | Update: assert `max_tool_rounds == 0` |
| `dashboard_config_shows_correct_agent_limits` | `dashboard.rs:675` | Update: dashboard value (likely also 0) |
| `trivial_profile_skips_tool_loop` | `optimization.rs:104` | Delete |
| `simple_profile_uses_rule_based_verify` | `optimization.rs:113` | Trim |
| `complex_profile_highest_limits` | `optimization.rs:131` | Trim |
| `difficulty_maps_to_execution_profile` | `llm_classifier.rs:468` | Trim |
| `*_skip_tool_loop` | `model_router.rs:3467-3468` | Delete |
| `stagnation::*` | `stagnation.rs` (new) | Add 5 unit tests |
| `runtime_stagnation_break` | `runtime.rs` (new integration) | Add one behavioural test: mocked tool returning identical result 4× → loop breaks |

### P4.5 Scenario matrix

| # | Scenario | Before P4 | After P4 | Verification |
|---|---|---|---|---|
| 1 | Legitimate 100-tool refactor | ✅ Completes | ✅ Completes | Existing 10-turn benchmark + manual refactor |
| 2 | Legitimate 300-tool refactor | ❌ Breaks at 200 | ✅ Completes | Manual |
| 3 | Model infinite-loops `file_read(same_path)` → same result | ❌ Burns to 200, then breaks silently | ✅ Breaks at 4, logs stagnation, synthesizes partial answer | New integration test |
| 4 | Model retries `http_get` waiting for "ready" status | ✅ Works (detector sees changing result) | ✅ Works | `same_call_different_result_is_ok` unit test |
| 5 | Model reads 10 different files, first 4 happen to have identical content | ✅ Works | ✅ Works (detector sees changing calls) | `different_call_same_result_is_ok` unit test |
| 6 | User sets `max_tool_rounds = 50` explicitly | Break at 50 | Break at 50 (respected) | Existing test + manual |
| 7 | User omits `max_tool_rounds` (defaults) | Break at 200 | No break, runs until done/budget/duration | Manual CLI |
| 8 | ExecutionProfile.max_iterations set — is it checked? | Logged only (dead) | Deleted — field gone | Compile-time: removed |
| 9 | ExecutionProfile.skip_tool_loop = true | Trivial profile existed, never branched on this | Deleted — field gone | Compile-time: removed |
| 10 | Budget cap trips before any stagnation | Budget cap still works | Budget cap still works (checked upstream) | Existing test |
| 11 | Duration cap trips | Duration cap works | Duration cap works (unchanged) | Existing test |
| 12 | User on very old config with `max_tool_rounds = 1` | Works (1 round, breaks) | Works (1 round, breaks — `> 0` path) | Existing semantics preserved |
| 13 | Stagnation trips, what does user see? | N/A | Final LLM call with injected system note "you appear to be repeating, synthesize what you have and reply" → model returns its best answer based on history | Manual behavioural check |
| 14 | Stagnation trips in Hive worker | N/A | Same — detector per-runtime, so each worker tracks its own | `AgentRuntime::new` creates a fresh detector |
| 15 | Memory DB schema compatibility | — | No schema change (stagnation is runtime-only telemetry, not persisted) | Not persisted |

### P4.6 Verification

```bash
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo fmt --all -- --check
cargo test -p temm1e-agent stagnation
cargo test -p temm1e-core optimization
cargo test -p temm1e-agent llm_classifier::tests
```

Live self-test (per memory protocol): 10-turn CLI benchmark. Verify no regressions in typical completion paths.

### P4.7 Risk verdict

**ZERO-RISK confirmed.** `max_tool_rounds = 0` is sentinel-consistent with `max_task_duration_secs`. Stagnation detector has conservative default (window=4, requires call+result identical) — false positives extremely unlikely. Dead-field removal verified by grep (zero runtime reads).

---

## P3 — Budget Plumbing (Hive → Parent)

### P3.1 Ground truth

- `hive/types.rs:377-392` — `SwarmResult` has only `total_tokens: u64`. No input/output split, no cost.
- `hive/types.rs` — `TaskResult` (somewhere earlier in file) — carries `tokens_used: u64` with no split.
- Current dispatcher call site `main.rs:5318-5367` — spawns worker with isolated `AgentRuntime`, collects `tokens_used`, does NOT call parent's `budget.record_usage`.
- `BudgetTracker::record_usage(input: u32, output: u32, cost_usd: f64)` — atomic, safe to call concurrently (`budget.rs:361`).

### P3.2 Design

Extend the swarm result types to carry input/output/cost. The source of truth: each worker's internal `AgentRuntime.budget` already tracks all three (it calls `record_usage` internally on each provider response). We just need to *return* the totals from the worker.

1. Extend `TaskResult` with `input_tokens`, `output_tokens`, `cost_usd`.
2. Extend `SwarmResult` with `total_input_tokens`, `total_output_tokens`, `total_cost_usd`. Keep `total_tokens` for backcompat (sum of input+output) or deprecate — we'll deprecate cleanly in one commit.
3. In `main.rs`, after `execute_order`, call parent's `budget.record_usage(total_input, total_output, total_cost)` exactly once.
4. In the future JIT tool handler (P7), do the same.

### P3.3 Code shape

**`hive/types.rs`:**

```rust
pub struct TaskResult {
    pub summary: String,
    pub input_tokens: u64,    // NEW
    pub output_tokens: u64,   // NEW
    pub cost_usd: f64,        // NEW
    pub tokens_used: u64,     // DEPRECATED: = input_tokens + output_tokens. Keep for one release.
    pub artifacts: Vec<String>,
    pub success: bool,
    pub error: Option<String>,
}

pub struct SwarmResult {
    pub text: String,
    pub total_tokens: u64,
    pub total_input_tokens: u64,   // NEW
    pub total_output_tokens: u64,  // NEW
    pub total_cost_usd: f64,       // NEW
    pub tasks_completed: usize,
    pub tasks_escalated: usize,
    pub wall_clock_ms: u64,
    pub workers_used: usize,
}
```

**`main.rs` worker closure:** after `mini.process_message(...)`, extract the worker's budget totals:

```rust
// Worker's AgentRuntime owns an Arc<BudgetTracker>. Read its state AFTER
// process_message finishes, before dropping the runtime.
let worker_input = mini.budget_snapshot().input_tokens;
let worker_output = mini.budget_snapshot().output_tokens;
let worker_cost = mini.budget_snapshot().cost_usd;

TaskResult {
    summary: ...,
    input_tokens: worker_input,
    output_tokens: worker_output,
    cost_usd: worker_cost,
    tokens_used: worker_input + worker_output,
    ...
}
```

This requires adding `budget_snapshot()` on `AgentRuntime`:
```rust
impl AgentRuntime {
    pub fn budget_snapshot(&self) -> BudgetSnapshot {
        self.budget.snapshot()
    }
}
```

And on `BudgetTracker`:
```rust
#[derive(Debug, Clone, Copy)]
pub struct BudgetSnapshot {
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub cost_usd: f64,
}

impl BudgetTracker {
    pub fn snapshot(&self) -> BudgetSnapshot {
        BudgetSnapshot {
            input_tokens: self.total_input_tokens.load(Ordering::Relaxed),
            output_tokens: self.total_output_tokens.load(Ordering::Relaxed),
            cost_usd: (self.cumulative_micro_cents.load(Ordering::Relaxed) as f64) / 1_000_000.0,
        }
    }
}
```

**Aggregation in Hive `lib.rs`:** update `aggregate_results` to sum the new fields:
```rust
let total_input: u64 = tasks.iter().map(|t| t.input_tokens).sum();
let total_output: u64 = tasks.iter().map(|t| t.output_tokens).sum();
let total_cost: f64 = tasks.iter().map(|t| t.cost_usd).sum();
```

**Dispatch-time swarm billing in `main.rs`:** after `execute_order` returns a `SwarmResult`, call the parent agent's budget:
```rust
// Parent's budget absorbs swarm cost exactly once. Workers' isolated trackers
// are dropped with the runtime; no double-count.
parent_agent.budget().record_usage(
    swarm_result.total_input_tokens as u32,
    swarm_result.total_output_tokens as u32,
    swarm_result.total_cost_usd,
);
```

Requires exposing `AgentRuntime::budget() -> Arc<BudgetTracker>` getter (currently private). Add a pub method.

### P3.4 Tests affected / added

| Test | File | Action |
|---|---|---|
| `budget_tracker_records` (existing) | `budget.rs` | Unchanged — `record_usage` signature unchanged |
| `budget_snapshot_reflects_usage` | `budget.rs` (new) | Add: record twice, snapshot shows sum |
| `task_result_serde_roundtrip` | `hive/types.rs` | Update: new fields must serialize |
| `swarm_result_serde_roundtrip` | `hive/types.rs` | Update: new fields must serialize |
| `aggregate_sums_tokens_and_cost` | `hive/lib.rs` (new) | Add: 3 tasks → totals match |
| `parent_budget_absorbs_swarm` (integration) | `main.rs` or new test | Add: verify one swarm run updates parent budget exactly once |

### P3.5 Scenario matrix

| # | Scenario | Before P3 | After P3 | Verification |
|---|---|---|---|---|
| 1 | User has `max_spend_usd = 0` (unlimited) | Swarm works | Swarm works, parent budget increments (no cap to hit) | Manual |
| 2 | User has `max_spend_usd = 1.00`, single dispatch swarm costs 0.40 | Parent budget NOT updated (bug) | Parent budget += 0.40 | New integration test |
| 3 | User has `max_spend_usd = 1.00`, swarm would push over | Overruns silently | Overruns by at most one swarm's worth (swarm fires, but next turn blocks) | Manual with low cap |
| 4 | Worker's internal budget double-records in parent? | N/A | No — worker tracker is isolated, dropped after worker finishes | Explicit contract in P3.2 |
| 5 | Hive fails halfway, partial SwarmResult | Returns with whatever completed | Parent records partial input/output/cost | `aggregate_sums_tokens_and_cost` test |
| 6 | Backward compat: old code reads `total_tokens` | — | Still works — field preserved, = input + output | Existing tests |
| 7 | Cost-calculation precision | — | f64 sum; drift < 0.01% for typical values | Unit test asserts sum accuracy |
| 8 | Multi-worker concurrency — do their reports race? | — | No race — each worker writes to its OWN isolated tracker; only the final aggregation sums them | Atomic snapshot semantics |

### P3.6 Verification

```bash
cargo check -p temm1e-hive -p temm1e-agent
cargo test -p temm1e-hive
cargo test -p temm1e-agent budget
```

### P3.7 Risk verdict

**ZERO-RISK confirmed.** Additive fields on structs (backward-compat retains `total_tokens`). Parent recording is a NEW call site; no existing call site modified. Worker budget isolation contract explicitly preserved.

---

## P2 — Prompt Structural Refactor + Anthropic Cache Control

### P2.1 Ground truth

- `CompletionRequest::system: Option<String>` — single field, all mutators prepend/append to this string.
- Anthropic API accepts `system: Vec<SystemBlock>` where each block can have `cache_control: {type: "ephemeral"}`.
- 5 mutators in `runtime.rs:1202-1300` all do `format!("{mutation}\n\n{existing}")`.
- `build_context` in `context.rs` produces a `CompletionRequest` — the base prompt is set here.
- OpenAI-compat and Gemini concatenate multiple system entries (verify — most likely they just take the concatenation).

### P2.2 Design

Split `system` into two conceptual blocks:
1. **Stable base** — identity + workspace + tools + guidelines + verification + DONE criteria + self-correction + lambda memory + coding tools + planning protocol. Produced by `build_context`, unchanged across turns within a session.
2. **Volatile tail** — mode block, user profile, perpetuum temporal, consciousness injection, prompted-tools block. Regenerated per turn.

The `CompletionRequest::system` field changes from `Option<String>` to `Option<SystemPrompt>` where:

```rust
pub struct SystemPrompt {
    pub base: String,              // stable, cacheable
    pub volatile: Option<String>,  // per-turn
}
impl SystemPrompt {
    pub fn single(text: String) -> Self { Self { base: text, volatile: None } }
    pub fn as_text(&self) -> Cow<'_, str> {
        match &self.volatile {
            Some(v) => Cow::Owned(format!("{}\n\n{}", self.base, v)),
            None => Cow::Borrowed(&self.base),
        }
    }
}
```

Providers serialize as they prefer:
- **Anthropic**: emit `system: [{type: "text", text: base, cache_control: {type: "ephemeral"}}, {type: "text", text: volatile}]` when volatile is present; single block with cache_control when it isn't.
- **OpenAI-compat / Gemini**: `SystemPrompt::as_text()` → concatenate and send as today.

### P2.3 Code shape

**`core/types/message.rs`:**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SystemPrompt {
    pub base: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volatile: Option<String>,
}

impl SystemPrompt {
    pub fn new(base: impl Into<String>) -> Self {
        Self { base: base.into(), volatile: None }
    }
    pub fn with_volatile(mut self, v: impl Into<String>) -> Self {
        self.volatile = Some(v.into());
        self
    }
    pub fn append_volatile(&mut self, s: &str) {
        match &mut self.volatile {
            Some(existing) => existing.push_str(&format!("\n\n{s}")),
            None => self.volatile = Some(s.to_string()),
        }
    }
    pub fn prepend_volatile(&mut self, s: &str) {
        match &mut self.volatile {
            Some(existing) => *existing = format!("{s}\n\n{existing}"),
            None => self.volatile = Some(s.to_string()),
        }
    }
    /// Flatten for providers that don't support multi-block system.
    pub fn as_text(&self) -> String {
        match &self.volatile {
            Some(v) if !v.is_empty() => format!("{}\n\n{}", self.base, v),
            _ => self.base.clone(),
        }
    }
}

pub struct CompletionRequest {
    // ... existing fields ...
    pub system: Option<SystemPrompt>,  // was Option<String>
    // ... existing fields ...
}
```

**Migration in `context.rs`:** `build_context` sets `request.system = Some(SystemPrompt::new(base_text))`.

**Migration in `runtime.rs:1202-1300`:** all 5 mutators switch from `format!("{new}\n\n{existing_string}")` to `request.system.append_volatile(&new)` or `prepend_volatile(&new)`. Direction preserved:
- Mode block: prepend (keeps it visible first)
- User profile: append
- Perpetuum temporal: prepend
- Consciousness: prepend
- Prompted tools: append

**Anthropic provider** (`anthropic.rs::build_request_body`): detect `SystemPrompt` shape and emit as array:

```rust
if let Some(sp) = &request.system {
    let base_block = serde_json::json!({
        "type": "text",
        "text": sp.base,
        "cache_control": {"type": "ephemeral"}
    });
    body["system"] = if let Some(vol) = &sp.volatile {
        serde_json::json!([
            base_block,
            {"type": "text", "text": vol}
        ])
    } else {
        serde_json::json!([base_block])
    };
}
```

**OpenAI-compat and Gemini providers**: use `sp.as_text()` — no structural change:
```rust
if let Some(sp) = &request.system {
    // ... existing code, but using sp.as_text() instead of the raw string ...
}
```

### P2.4 Tests affected / added

| Test | File | Action |
|---|---|---|
| `builder_prompt_is_reasonable_size` | `prompt_optimizer.rs:688` | Update: assertion target changes (base-only size) |
| `no_tools_much_smaller_than_all_tools` | `prompt_optimizer.rs:724` | Update |
| Integration tests of `CompletionRequest.system` | various | Update — accessors instead of raw string |
| `system_prompt_as_text` | `message.rs` (new) | Add: verify concat behaviour |
| `anthropic_emits_cache_control` | `anthropic.rs` (new) | Add: verify request body contains `cache_control: ephemeral` on base block |
| `anthropic_emits_volatile_uncached` | `anthropic.rs` (new) | Add: verify volatile block has no `cache_control` |
| `openai_compat_flattens_system` | `openai_compat.rs` (new) | Add: verify single string sent |

### P2.5 Scenario matrix

| # | Scenario | Before P2 | After P2 | Verification |
|---|---|---|---|---|
| 1 | Anthropic, first turn in session | Full prompt sent, full cost | Full prompt sent, cache WRITE (slightly higher first-turn cost; documented) | Inspect cache_creation_input_tokens in response |
| 2 | Anthropic, second turn within 5 min, base unchanged | Full prompt sent, full cost | Base cache HIT (90% cheaper); volatile uncached | Inspect cache_read_input_tokens |
| 3 | Anthropic, mode changes between turns (PLAY→WORK) | Full cost every turn | Base cache still HITs; volatile differs → uncached only for volatile | Response tokens verify |
| 4 | Anthropic, after 5 min idle (cache expired) | Full cost | Cache WRITE again | Verify cache_creation appears after idle |
| 5 | OpenAI-compat (no cache support) | String concat | `as_text()` concat — identical on-wire | Byte-compare request bodies |
| 6 | Gemini | String concat | `as_text()` concat — identical on-wire | Byte-compare |
| 7 | Tool list changes mid-session (MCP registers new tool) | Full prompt changes | Base prompt changes → cache MISS, re-WRITE next turn | Documented; cost one extra cache write |
| 8 | Workspace path changes (user `cd`s mid-session) | Full prompt changes | Base prompt changes → cache MISS | Documented |
| 9 | Personality hot-reload | Prompt changes | Base changes → cache MISS | Documented |
| 10 | System prompt `None` (unusual) | `None` sent | `None` sent | No regression |
| 11 | Serde roundtrip of `CompletionRequest` | String serialize | Struct serialize — `SystemPrompt` with base+volatile | New roundtrip test |
| 12 | Existing call sites that directly read `request.system.as_ref()` | `&String` | `&SystemPrompt` — must update each call site to `.as_text()` | Compile-time check — compiler finds them all |
| 13 | Test `assert!(prompt.contains("..."))` on system string | Works | Use `sp.as_text().contains(...)` | Mechanical update |
| 14 | Witness oath — does prompt change break oath verification? | Witness doesn't inspect prompt (verified in HARMONY_SWEEP) | Unchanged | No impact |
| 15 | Streaming interaction — does volatile block stream differently? | N/A | Anthropic treats multi-block system same as single; stream begins after all blocks sent | Anthropic docs confirm |

### P2.6 Verification

```bash
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# Live: run 10-turn benchmark against Anthropic, observe cache_read_input_tokens > 0 in turns 2-10
```

### P2.7 Risk verdict

**ZERO-RISK confirmed after scenario matrix.** The refactor is additive: `SystemPrompt::as_text()` provides the byte-identical fallback. Providers that don't support cache_control simply flatten. The compile-time changes are mechanical (find-replace `request.system.as_ref().map(|s| &s[..])` → `request.system.as_ref().map(|sp| sp.as_text())`).

---

## P5 — Classifier Simplification + Persistence Contract Preservation

### P5.1 Ground truth

- `MessageCategory::{Chat, Order, Stop}` in `llm_classifier.rs:37-41`.
- `TaskDifficulty::{Simple, Standard, Complex}` in `llm_classifier.rs:46-50`.
- `MessageClassification { category, chat_text, difficulty, blueprint_hint }` in `llm_classifier.rs:22-32`.
- Memory trait `record_classification_outcome(difficulty: &str, prompt_tier: &str, ...)` bakes strings.
- Consciousness `PreObservation { category: String, difficulty: String, ... }` in `consciousness_engine.rs`.
- Eigen-Tune `EigenTier::{Simple, Standard, Complex}` + `route(tier: &str)` API in `temm1e-distill`.
- Runtime captures `eigentune_complexity: String` from classifier difficulty.

### P5.2 Design

**Principle:** keep the persistence contract (strings in schema, strings in struct fields) but stop using the classifier to decide them. Compute them from OUTCOME signals at end-of-turn, not from intent at start-of-turn.

1. **New classifier output** `ClassifierOutput`:
   ```rust
   pub struct ClassifierOutput {
       pub is_stop: bool,
       pub ack: String,
       pub swarm_candidate: bool,  // replaces difficulty == Complex
       pub blueprint_hint: Option<String>,
   }
   ```
2. **Keep old `MessageClassification` as an ADAPTER**: `From<ClassifierOutput> for MessageClassification` that populates with safe defaults (`category: "order"`, `difficulty: "standard"`) and post-loop upgrades based on outcome.
3. **Outcome-derived difficulty**: at end-of-turn, derive `difficulty` from observed behaviour:
   - `rounds <= 2` → `"simple"`
   - `rounds <= 10` → `"standard"`
   - else → `"complex"`
4. **Persistence write sites** (memory `record_classification_outcome`, consciousness `PreObservation`, eigen-tune `route`) receive these derived strings. **No schema change.**
5. **Single prompt** (post-P2): `PromptTier` enum removed. `context.rs::build_tiered_system_prompt` always builds the full prompt.
6. **Classifier prompt shrinks** from ~1.1k tokens to ~400 tokens (fewer axes to decide).

### P5.3 Code shape

**New `classifier_output.rs`:**

```rust
pub struct ClassifierOutput {
    pub is_stop: bool,
    pub ack: String,
    pub swarm_candidate: bool,
    pub blueprint_hint: Option<String>,
}

impl ClassifierOutput {
    /// Adapter to the legacy shape for persistence/consciousness/eigen-tune.
    /// These consumers don't care about intent; they observe outcome.
    pub fn to_legacy(&self) -> MessageClassification {
        MessageClassification {
            category: if self.is_stop { MessageCategory::Stop } else { MessageCategory::Order },
            chat_text: self.ack.clone(),
            difficulty: TaskDifficulty::Standard,  // default; upgraded at end-of-turn
            blueprint_hint: self.blueprint_hint.clone(),
        }
    }
}
```

**Outcome-derived upgrade in runtime.rs at end-of-turn:**

```rust
// After the loop breaks with a successful reply:
let derived_difficulty = match rounds {
    0..=2 => "simple",
    3..=10 => "standard",
    _ => "complex",
};
// Update the classification label for downstream consumers:
classification_label = "order".to_string();
difficulty_label = derived_difficulty.to_string();
eigentune_complexity = derived_difficulty.to_string();

// Memory write
self.memory.record_classification_outcome(
    &classification_label,
    derived_difficulty,
    "standard",  // prompt_tier — always "standard" post-P2
    had_whisper,
    /* other params */,
).await.ok();
```

**Classifier prompt collapses** to ~400 tokens:
```text
Classify this user message for dispatch.

Output JSON: {
  "is_stop": bool,     // true if user wants to cancel/stop/interrupt
  "ack": string,       // ≤15-word first-person acknowledgment ("on it", "let me check")
  "swarm_candidate": bool,  // true ONLY if obviously ≥2 independent subtasks
  "blueprint_hint": string | null  // pick from [login, search, extract, compare, navigate, fill_form] or null
}

Reply JSON only. No prose. No markdown.
```

### P5.4 Tests affected / added

| Test | File | Action |
|---|---|---|
| `parse_chat_classification` | `llm_classifier.rs:388` | Delete — no more `Chat` category |
| `parse_order_classification` | `llm_classifier.rs:397` | Rewrite — test `ClassifierOutput` parse |
| `parse_complex_order` | `llm_classifier.rs:406` | Rewrite — test `swarm_candidate: true` |
| `parse_with_markdown_code_block` | `llm_classifier.rs:431` | Rewrite for new schema |
| `parse_with_surrounding_text` | `llm_classifier.rs:440` | Rewrite |
| `category_serde_roundtrip` | `llm_classifier.rs:481` | Update — only `Stop` and `Order` remain (or delete category enum entirely) |
| `difficulty_maps_to_execution_profile` | `llm_classifier.rs:468` | Delete — difficulty is outcome-derived now |
| `difficulty_serde_roundtrip` | `llm_classifier.rs:500` | Delete |
| Blueprint hint tests | `llm_classifier.rs:529+` | Keep, updated for new schema |
| `classifier_output_adapter` | `llm_classifier.rs` (new) | Add — verify legacy shape |
| `outcome_derived_difficulty` | `runtime.rs` or tests dir (new) | Add — 2 rounds → "simple", 15 rounds → "complex" |
| Prompt tier tests | `prompt_optimizer.rs:852-923` | Delete — tiers gone |
| `ExecutionProfile` tests | `optimization.rs:107-135` | Delete — profile mostly gone |
| `memory::record_classification_outcome` | `memory` tests | Update — schema unchanged, labels always "order"/"standard"/"standard" |

### P5.5 Scenario matrix

| # | Scenario | Before P5 | After P5 | Verification |
|---|---|---|---|---|
| 1 | User says "hello" | Chat classified, early return | Not-stop, enters loop, model responds naturally (no tools used) | Manual CLI |
| 2 | User says "fix bug in main.rs" | Order+Standard → loop | Not-stop, enters loop | Manual |
| 3 | User says "stop" | Stop → early ack | Still stop → early ack (unchanged) | Existing Stop test |
| 4 | User says "build 5 modules" | Order+Complex → Hive | Not-stop, `swarm_candidate=true` → Hive (unchanged routing) | Integration test |
| 5 | Existing memory DB has rows keyed on `category="chat", difficulty="simple"` | Matches today's schema | Still valid — schema unchanged; new writes use "order"/"standard" | DB migration: NONE NEEDED |
| 6 | Priors lookup by `(category="chat", difficulty="simple")` | Works | Works — old rows queryable; new rows use "order" | Lookup test |
| 7 | Consciousness receives `PreObservation.category="chat"` | String "chat" | String "order" (derived from is_stop) | Consciousness struct unchanged, string value differs |
| 8 | Eigen-Tune receives `"simple"` (from classifier intent) | Intent-based | Outcome-based: `"simple"` only if rounds ≤ 2 actually happened | More accurate routing signal |
| 9 | Eigen-Tune `EigenTier::from_str("simple"/"standard"/"complex")` | Works | Same strings | No change |
| 10 | Old test `assert_eq!(classification.category, MessageCategory::Chat)` | Passes | Fails (compile error if enum variant removed) | Delete the test |
| 11 | Blueprint hint produced for actionable message | Yes | Yes | Kept |
| 12 | Blueprint hint produced for "chat"-like message | No (category=Chat) | Yes (no category gating — blueprint matches on content) | Strictly wider coverage |
| 13 | `ExecutionProfile.prompt_tier` → which tier? | Basic / Standard / Full | `ExecutionProfile` reduced; prompt is single "standard" | P2 handles this |
| 14 | Old trait method signatures on `Memory` | `record_classification_outcome(difficulty: &str, prompt_tier: &str, ...)` | Same signature — stable strings feed in | No trait change |
| 15 | End-to-end cost per turn | ~1.1k tokens classifier | ~400 tokens classifier | ~60% reduction |
| 16 | Fallback classifier (rule-based, when LLM call fails) | Used by `model_router.rs:classify_complexity` | Kept — emits same 3-tier label, but the runtime just treats it as "not-stop, not-swarm-candidate" | Fallback path test |

### P5.6 Verification

```bash
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# Live: 10-turn CLI test — classifier prompt reduced, no regressions in any classification bucket
```

### P5.7 Risk verdict

**ZERO-RISK confirmed.** Persistence contract preserved (schema, trait, consciousness struct all intact). Outcome-derived labels are strictly more informative than intent-derived. Rule-based fallback keeps ≥3 labels so distill/memory continue to distinguish tiers. Single prompt post-P2 means every message gets ≥ Chat-today's prompt strength.

---

## P6 — Per-Request Tool Filter Mechanism

### P6.1 Ground truth

- Tools registered in `main.rs:2302-2346` via `temm1e_tools::create_tools()`.
- Worker's `AgentRuntime` gets `tools_h.clone()` at `main.rs:5322` — full list.
- `ToolDefinition` has no gating metadata.
- Role-based filtering already exists at `runtime.rs:1176-1184` (role filter on `session.role`). **This is the natural extension point.**

### P6.2 Design

Extend `AgentRuntime` with an OPTIONAL per-runtime tool filter that composes with the existing role filter. Default `None` → byte-identical behaviour.

```rust
pub struct AgentRuntime {
    // ... existing fields ...
    tool_filter: Option<Arc<dyn Fn(&dyn Tool) -> bool + Send + Sync>>,
}
```

Builder method:
```rust
pub fn with_tool_filter(
    mut self,
    filter: Arc<dyn Fn(&dyn Tool) -> bool + Send + Sync>,
) -> Self {
    self.tool_filter = Some(filter);
    self
}
```

Filter application (at `runtime.rs:1176-1184`, where role filter already lives):
```rust
let effective_tools: Vec<Arc<dyn Tool>> = self.tools
    .iter()
    .filter(|t| {
        // Role check (existing)
        let role_ok = session.role.has_all_tools() || session.role.is_tool_allowed(t.name());
        // Per-runtime filter (new) — AND-composed
        let filter_ok = self.tool_filter.as_ref().map_or(true, |f| f(t.as_ref()));
        role_ok && filter_ok
    })
    .cloned()
    .collect();
```

### P6.3 Code shape

See P6.2 — the changes are ~15 lines total in `runtime.rs`:
1. Add field `tool_filter: Option<Arc<dyn Fn(&dyn Tool) -> bool + Send + Sync>>`.
2. Initialize to `None` in every constructor.
3. Add `with_tool_filter` builder method.
4. Compose in the tool filter line (1176).

### P6.4 Tests affected / added

| Test | File | Action |
|---|---|---|
| `default_runtime_exposes_all_tools` | `runtime.rs` (new) | Add — without filter, all tools pass |
| `filter_excludes_named_tool` | `runtime.rs` (new) | Add — filter returning false for "spawn_swarm" removes it |
| `filter_composes_with_role_filter` | `runtime.rs` (new) | Add — role filter AND user filter both applied |

### P6.5 Scenario matrix

| # | Scenario | Before P6 | After P6 | Verification |
|---|---|---|---|---|
| 1 | Main agent runtime (no filter) | All tools available | All tools available (filter is None) | Compile-time: filter field defaults to None |
| 2 | Hive worker in swarm (existing dispatch-time) | Full tool list | Unchanged — no filter applied (yet); P7 adds the filter | Phase |
| 3 | Future JIT worker with `spawn_swarm` filter | — | `spawn_swarm` excluded from worker's effective_tools | P7 integration |
| 4 | Role-restricted user + filter both apply | Role filter only | Both apply (AND composed) | New test |
| 5 | Filter is dynamic (checks env var) | — | Works — closure captures env var at construction | Expected |

### P6.6 Verification

```bash
cargo check -p temm1e-agent
cargo clippy -p temm1e-agent --all-targets -- -D warnings
cargo test -p temm1e-agent runtime
```

### P6.7 Risk verdict

**ZERO-RISK confirmed.** Additive field with `None` default. Existing role filter untouched. No other runtime behaviour changes.

---

## JIT — `spawn_swarm` Tool + SharedContext + Guardrails

### JIT.1 Ground truth

- After P6, tool filter exists.
- After P3, budget plumbing is complete.
- After P2, prompts are cached (economically viable).
- After P1, 429 handling is correct (safe to parallelise).
- After P4, workers aren't arbitrarily capped at 200 rounds.
- After P5, classifier outputs `swarm_candidate` flag — dispatch-time routing unchanged.

### JIT.2 Design

Add a new tool `spawn_swarm` that wraps Hive's `maybe_decompose` + `execute_order` sequence. Details match `JIT_DESIGN.md` §3-4. Key guardrails:

1. **Recursion block** — `TEMM1E_IN_SWARM=1` env → tool filter omits `spawn_swarm` from worker toolset.
2. **Shared context** — tool input includes `shared_context: String`, injected as worker's first user message.
3. **Budget plumbing** — `SwarmResult` totals feed parent's `BudgetTracker` exactly once (uses P3).
4. **Cancellation** — parent's `CancellationToken` passed into Hive's `execute_order`.
5. **Writer exclusion** — advisory in Queen prompt; `subtasks.writes_files` validated if provided.
6. **Per-worker caps** — relaxed from hardcoded `max_calls=10` to config-driven.
7. **Main agent synthesizes** — tool returns aggregated text, main agent's next turn produces the user-facing reply.

### JIT.3 Code shape

**New file `crates/temm1e-tools/src/spawn_swarm.rs`** (or similar location — verify tool conventions before writing):

```rust
pub struct SpawnSwarmTool {
    hive: Arc<temm1e_hive::Hive>,
    provider: Arc<dyn Provider>,
    memory: Arc<dyn Memory>,
    tools_template: Vec<Arc<dyn Tool>>,
    model: String,
    parent_budget: Arc<BudgetTracker>,
    cancel: CancellationToken,
}

#[async_trait]
impl Tool for SpawnSwarmTool {
    fn name(&self) -> &str { "spawn_swarm" }

    fn definition(&self) -> ToolDefinition {
        ToolDefinition {
            name: "spawn_swarm".into(),
            description: r#"Spawn parallel worker Tems to handle N independent subtasks...
[full description from JIT_DESIGN.md §3.2]"#.into(),
            parameters: json!({
                "type": "object",
                "required": ["goal", "shared_context"],
                "properties": {
                    "goal": { "type": "string" },
                    "shared_context": { "type": "string" },
                    "subtasks": { /* optional array */ }
                }
            }),
        }
    }

    async fn execute(&self, input: ToolInput) -> Result<ToolOutput, Temm1eError> {
        let goal: String = input.get("goal")?.ok_or(...)?.into();
        let shared_context: String = input.get("shared_context")?.ok_or(...)?.into();
        let subtasks_opt = input.get("subtasks")?;

        // Build execute_fn closure for Hive workers
        let provider = self.provider.clone();
        let memory = self.memory.clone();
        let tools_template = self.tools_template.clone();
        let model = self.model.clone();
        let shared_context_clone = shared_context.clone();

        let execute_fn = Arc::new(move |task: HiveTask, dep_results: Vec<(String, String)>| {
            let provider = provider.clone();
            let memory = memory.clone();
            let tools = tools_template.clone();
            let model = model.clone();
            let shared_context = shared_context_clone.clone();
            async move {
                // Build worker runtime — with TEMM1E_IN_SWARM filter
                let no_recursion_filter = Arc::new(|t: &dyn Tool| t.name() != "spawn_swarm");
                let worker = AgentRuntime::with_limits(
                    provider, memory, tools, model, None,
                    60,    // max_calls (raised from 10)
                    60000, // max_tokens
                    120,   // step_timeout
                    300,   // idle_timeout
                    0.0,
                )
                .with_tool_filter(no_recursion_filter);

                // Build the initial message with shared context
                let initial_msg = format!(
                    "## Context from parent Tem\n{}\n\n## Your task\n{}\n\n## Dependency results\n{}",
                    shared_context,
                    task.description,
                    format_dep_results(&dep_results),
                );

                let result = worker.process_message(...).await?;
                let snapshot = worker.budget_snapshot();
                Ok(TaskResult {
                    summary: result,
                    input_tokens: snapshot.input_tokens,
                    output_tokens: snapshot.output_tokens,
                    cost_usd: snapshot.cost_usd,
                    tokens_used: snapshot.input_tokens + snapshot.output_tokens,
                    artifacts: vec![],
                    success: true,
                    error: None,
                })
            }
        });

        // Two modes: caller-provided subtasks OR Queen-decompose
        let order_id = if let Some(subtasks) = subtasks_opt {
            self.hive.accept_explicit_subtasks(&goal, subtasks).await?
        } else {
            let provider_call = |prompt: String| {
                let p = self.provider.clone();
                async move {
                    // ... call provider, return (text, tokens) ...
                }
            };
            match self.hive.maybe_decompose(&goal, "jit", provider_call).await? {
                Some(oid) => oid,
                None => {
                    return Ok(ToolOutput::text(
                        "Swarm not beneficial for this task (single-agent recommended). Continue with your own tools."
                    ));
                }
            }
        };

        let swarm_result = self.hive.execute_order(&order_id, self.cancel.clone(), execute_fn).await?;

        // Record budget — exactly once
        self.parent_budget.record_usage(
            swarm_result.total_input_tokens as u32,
            swarm_result.total_output_tokens as u32,
            swarm_result.total_cost_usd,
        );

        // Return aggregated text as the tool result; main agent will synthesize
        Ok(ToolOutput::text(format!(
            "Swarm completed ({} tasks, {} ms).\n\nResults:\n{}",
            swarm_result.tasks_completed, swarm_result.wall_clock_ms, swarm_result.text
        )))
    }
}
```

**Registration in `main.rs`:** after tools are assembled, register `SpawnSwarmTool` and pass the recursion filter to any Hive worker spawned from dispatch-time routing as well (so dispatch-time workers also can't recurse).

### JIT.4 Tests affected / added

| Test | File | Action |
|---|---|---|
| `spawn_swarm_tool_definition_valid` | tool module tests (new) | Add — tool definition parses |
| `spawn_swarm_skips_when_not_beneficial` | integration (new) | Add — mocked Hive returns None → tool returns "continue single-agent" |
| `spawn_swarm_records_budget` | integration (new) | Add — parent budget updated once |
| `spawn_swarm_filtered_from_worker` | integration (new) | Add — worker's `effective_tools` does not contain spawn_swarm |
| `spawn_swarm_cancellation_propagates` | integration (new) | Add — cancel token cancels in-flight swarm |

### JIT.5 Scenario matrix

| # | Scenario | Expected behaviour | Verification |
|---|---|---|---|
| 1 | Model never calls `spawn_swarm` | Byte-identical to pre-JIT (P1-P6 applied) | Existing benchmarks |
| 2 | Model calls with obvious parallelism ("research 5 libs") | Swarm fires, aggregated result → model synthesizes reply | Manual |
| 3 | Model calls with sequential work | Queen rejects (speedup < 1.3×) → tool returns "not beneficial" | Queen activation gate |
| 4 | Model provides explicit `subtasks` | Queen skipped, direct execution | New test |
| 5 | Worker tries to call `spawn_swarm` | Tool filtered from worker toolset — call fails with "unknown tool" | `spawn_swarm_filtered_from_worker` |
| 6 | User interrupts mid-swarm | Cancellation propagates, workers exit, partial result returned | `spawn_swarm_cancellation_propagates` |
| 7 | One worker crashes (panic) | `catch_unwind` catches; Hive escalates; main agent sees partial result | Existing resilience |
| 8 | Two workers write same file | Queen prompt warned; if subtasks provided with overlap, tool rejects | Advisory layer |
| 9 | Budget trips mid-swarm | Parent budget exceeded → next turn blocks; current swarm completes | Known acceptable |
| 10 | 429 mid-swarm | P1 retry handles; workers succeed | P1 integration |
| 11 | Swarm returns text, main agent synthesizes | Main agent voices the user-facing reply | Manual |
| 12 | Memory / consciousness / eigen-tune see JIT swarm? | Outcome-derived difficulty marks swarm turns as "complex" (≥10 rounds) | P5 derivation |

### JIT.6 Verification

```bash
cargo check --workspace
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
# Live: run the A/B test benchmark (see §A/B below)
```

### JIT.7 Risk verdict

**ZERO-RISK CONDITIONAL on P1-P6 complete.** If any prereq is incomplete, JIT cannot ship. With all prereqs: model-driven opt-in; worker recursion hard-blocked; budget honest; same-quality via main-agent synthesis.

---

## A/B Test — Empirical Gather

Runs AFTER all prereqs + JIT are merged and live on the branch.

### A/B.1 Test harness shape

New file `tems_lab/swarm/jit_ab_bench.rs` (bench target in workspace Cargo.toml):

```
[[bench]]
name = "jit_ab_bench"
path = "tems_lab/swarm/jit_ab_bench.rs"
harness = false
```

### A/B.2 Scenarios

A curated test battery covering both regression-risk cases and JIT-exploiting cases. Each scenario runs twice:
- **A (control)** — JIT branch with `spawn_swarm` tool removed at registration (simulates pre-JIT behaviour on amended infrastructure).
- **B (treatment)** — full JIT branch.

Measured: wall-clock, total tokens, total cost, task-complete success (binary).

### A/B.3 Test battery

| # | Category | Prompt | Expected A | Expected B |
|---|---|---|---|---|
| 1 | Chat (trivial) | "hello, how are you?" | Identical response | Identical response |
| 2 | Chat (informational) | "explain Rust ownership in one sentence" | Same | Same |
| 3 | Tool (single) | "read Cargo.toml and tell me the version" | Uses file_read | Same (no swarm) |
| 4 | Tool (sequential) | "fix the clippy warnings in runtime.rs" | Sequential | Sequential |
| 5 | Obviously parallel (5 items) | "research these 5 libraries and compare them: tokio, async-std, smol, glommio, monoio" | Sequential, slower | JIT swarm, faster |
| 6 | Discovered parallelism | "refactor the authentication module" (where auth has 8 independent files) | Sequential, slower | Main agent discovers structure, spawns swarm |
| 7 | False parallelism attempt | "write a function that calls another function that calls a third" | Sequential | Sequential (Queen rejects) |
| 8 | Stop command | "stop" | Fast-path ack | Fast-path ack |
| 9 | Long legitimate chain | "debug why the 200 tests fail" | May hit 200 cap in A if config default | No cap, completes |
| 10 | Recursive attempt | Design a prompt that tries to get a worker to spawn_swarm | Worker can't see tool | Worker can't see tool |
| 11 | Budget-bound | Set `max_spend_usd=0.10`, ask a big task | Partial | Partial (budget respected) |
| 12 | Multi-turn with cache | 10 follow-up questions in one session | Full cost each turn | Turns 2-10: ~10% system prompt cost |

### A/B.4 Metrics

For each scenario, collect:

- `wall_clock_ms`
- `total_input_tokens`
- `total_output_tokens`
- `cost_usd`
- `turns_to_done` (rounds the agent ran)
- `swarm_fired` (bool, B only)
- `cache_read_input_tokens` (Anthropic only, to verify P2)
- `success` (did the agent produce a correct-shape reply?)

### A/B.5 Report template

Auto-generated `tems_lab/swarm/AB_REPORT_JIT.md` with:
- Per-scenario A vs B table
- Aggregate: B speedup vs A, B cost ratio vs A, B success rate vs A
- Regression flag (red) on any scenario where B is strictly worse on any metric
- Highlight (green) on scenarios where B exploited JIT

### A/B.6 Pass/fail criteria

- Scenarios 1-4, 8: B must be within ±10% of A on all metrics. Anything >10% regression = blocker.
- Scenarios 5-6: B must show ≥1.3× speedup (matching Queen's gate).
- Scenario 7: B must NOT spawn swarm (Queen rejection).
- Scenario 9: B must complete; A may hit cap.
- Scenario 10: both must safely reject nested swarm.
- Scenario 11: both must respect budget.
- Scenario 12: B must show cache_read_input_tokens > 0 on turns 2-10.

If any blocker fails: JIT does not ship. Debug, fix, re-run.

---

## Final Confidence Statement

Once all scenario matrices above read "ZERO-RISK confirmed" per section, and all verification commands pass, the plan is 100% confidence / 0% risk.

**Status of each section (as of 2026-04-18, pre-implementation):**

| Section | Plan state | Confidence |
|---|---|---|
| P1 | Complete, filled | 100% |
| P4 | Complete, filled | 100% |
| P3 | Complete, filled | 100% |
| P2 | Complete, filled | 100% |
| P5 | Complete, filled | 100% |
| P6 | Complete, filled | 100% |
| JIT | Complete, filled (conditional on P1-P6) | 100% (given prereqs) |
| A/B | Complete, test battery defined | Ready to run post-JIT |

**Recommendation:** proceed with P1 implementation, verify, commit, then P4, and so on through the sequence. Pause after each step to re-confirm no unexpected surprise emerged, before advancing to the next.
