# TEMM1E Full Sweep Protocol

**Periodic deep scan of the entire Tem system to find and fix inefficiencies, bugs, unintended behavior, and LLM handling issues.**

**Goal: EXTREME RESILIENCE — Tem stays up forever, does all jobs reliably, never fails a user.**

A failed task, a silent crash, a truncated response — every one of these is a user staring at their phone wondering why their bot died. Full Sweep exists to find these before users do.

---

## Zero-Risk Implementation Rule

**Only fixes at 100% confidence and 0% risk of regression are implemented.** Everything else is deferred.

1. Every fix must have a research artifact written BEFORE any code changes.
2. The artifact must include: full code path analysis, exact files/lines to change, behavioral diff (before vs after), and E2E test scenarios.
3. If confidence is not 100% after research — the fix goes on the **Deferred List** and is revisited later.
4. Deferred items are tracked in the sweep's `fix-plan.md` with the reason for deferral and what's needed to reach 100% confidence.
5. There is no pressure to fix everything in one sweep. A fix that introduces a regression is worse than the original bug.

---

## Sweep Execution Workflow

The sweep follows a strict pipeline. Every finding goes through this flow:

```
SCAN → CLASSIFY → RESEARCH → DECIDE → IMPLEMENT → TEST → COMMIT
                                ↓
                          DEFER or BIN
```

### Step 1: Scan (Phases 1-10)
Run all 10 phases. Collect raw findings with the 15-dimension risk matrix.

### Step 2: Triage into Priority Waves
- **Wave 1 (P0):** Emergency fixes — implement first, each with own commit.
- **Wave 2 (P1):** Critical fixes — before next release.
- **Wave 3 (P2):** High — within 1 week.
- **Wave 4 (P3):** Medium — within 1 sprint.
- **Wave 5 (P4):** Low — opportunistic.

### Step 3: Execute Waves (TRIVIAL items)
Within each wave, implement **TRIVIAL fixes first** (1-5 lines, single file, no behavioral change for normal operation). These can be implemented immediately after reading the code — no full research artifact needed.

After each fix: `cargo check && cargo clippy && cargo fmt --check && cargo test`.

### Step 4: Research (MODERATE/COMPLEX items)
For MODERATE and COMPLEX fixes:
1. Launch deep research agents (parallel, one per fix).
2. Each agent reads the ACTUAL code — not just grep patterns.
3. Agent reports: exact code paths, all callers, all edge cases, proposed edit, risk assessment.
4. Write research artifacts to `docs/full-sweep-<N>/`.

### Step 5: Decide — IMPLEMENT, DEFER, or BIN

**IMPLEMENT** (100% confidence, 0% risk):
- Full code path understood.
- Exact edit known (old_string → new_string with file:line).
- All callers mapped and verified unaffected.
- No behavioral change for existing correct usage.
- Apply the fix, run all 4 compilation gates.

**DEFER** (high confidence but not 100%):
- Research is complete but the fix touches too many sites, needs E2E testing with live services, or requires a coordinated multi-file change that can't be verified in a code-only session.
- Record in `fix-plan.md` with confidence % and specific blocker.
- Revisit in next sweep or when blocker is resolved.

**BIN** (proven impossible to reach 100/0):
- Research proves the fix is architecturally impossible, would cause behavioral regression, provides negligible value, or has zero-risk alternatives that were already applied.
- Record in `fix-plan.md` with full rationale.
- A binned item can be **rescued** by re-research — ask "can this be done differently at 100/0?"

### Step 6: Process ALL deferred items
After initial waves complete, revisit every deferred item:
1. Launch focused research agents for each deferred item.
2. Determine: has the blocker been resolved? Is there a simpler approach?
3. If 100/0 is reached → implement.
4. If still not → remains deferred for next sweep.

### Step 7: Final effort on binned items
Before closing the sweep, re-examine every binned item one more time:
- Ask: "Is there a DIFFERENT approach that reaches 100/0?"
- Often, the original approach was impossible but a simpler alternative exists.
- Examples from Sweep 1: chmod 600 instead of vault encryption, provenance text instead of role change, cache eviction instead of full ResilientMemory wiring.

### Step 8: Exhaustive self-test
After all fixes land:
1. Run `cargo test --workspace` — full unit test suite.
2. Build release binary.
3. Run 10-turn CLI self-test (provider connectivity, memory recall, budget tracking).
4. Verify sweep-specific items from logs (WAL mode, error counts, etc.).
5. Write test report to `docs/full-sweep-<N>/test-report.md`.

### Step 9: Create PR and merge
1. Verify clean git status.
2. Create PR with full summary of findings, fixes, deferred, and binned items.
3. Wait for CI to pass.
4. Merge.

---

## When to Run

| Trigger | Scope |
|---------|-------|
| After every major release (vX.0.0) | Full sweep — all 10 phases |
| After adding/modifying a provider or channel | Targeted sweep — phases 1, 3, 4, 6, 7 |
| After touching agent runtime or core traits | Full sweep — all 10 phases |
| After any production incident | Targeted sweep — phases matching the incident area + phase 10 |
| Monthly maintenance | Full sweep — all 10 phases |
| After adding a new crate | Full sweep — all 10 phases |

---

## Pre-Sweep Setup

```bash
# 1. Clean build to catch stale artifacts
cargo clean

# 2. Full compilation gates (baseline — all must pass before sweep begins)
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo test --workspace

# 3. Record baseline metrics
echo "=== SWEEP BASELINE ==="
cargo test --workspace 2>&1 | grep 'test result' | awk '{sum += $4} END {print "Tests:", sum}'
find . -name '*.rs' -not -path './target/*' | wc -l | xargs echo "Files:"
find . -name '*.rs' -not -path './target/*' | xargs wc -l | tail -1 | awk '{print "Lines:", $1}'
ls crates/ | wc -l | xargs echo "Crates:"
```

If any compilation gate fails, **stop and fix it first**. The sweep assumes a green baseline.

---

## Finding Risk Matrix (MANDATORY for every finding)

Every finding from the sweep MUST include this risk matrix. No code changes are authorized until the matrix is filled and reviewed. This is the decision-making instrument — it tells us what to fix, what to leave alone, and what order to work in.

### Matrix Dimensions

| # | Dimension | Scale | Description |
|---|-----------|-------|-------------|
| 1 | **RISK if Changed (Rchange)** | 0–100% | Probability that fixing this code introduces a regression to the existing system. High = the fix itself is dangerous (touches hot paths, complex state, subtle invariants). |
| 2 | **RISK if Unchanged (Runchanged)** | 0–100% | Probability that leaving this as-is causes a production incident within 90 days under normal load. High = ticking time bomb. |
| 3 | **Risk Coefficient (RC)** | Calculated | `RC = Runchanged / (Rchange + 1)`. Higher = more urgent to fix. RC > 5 = fix immediately. RC 1–5 = schedule. RC < 1 = leave alone unless free. |
| 4 | **Agentic Core Impact** | NONE / INDIRECT / DIRECT | Does the fix touch `temm1e-agent/`, `temm1e-core/src/traits/`, `temm1e-core/src/types/`, or `src/main.rs`? DIRECT = requires extreme review, founder approval, and full regression test. INDIRECT = fix is outside core but changes behavior that core depends on. NONE = isolated leaf crate. |
| 5 | **Blast Radius** | ISOLATED / CHANNEL / GLOBAL / SYSTEM | ISOLATED = affects 1 user in 1 session. CHANNEL = affects all users on one channel. GLOBAL = affects all users on all channels. SYSTEM = can crash the process (every user loses their bot). |
| 6 | **Reversibility** | INSTANT / DEPLOY / IRREVERSIBLE | INSTANT = config change, feature flag toggle. DEPLOY = requires new binary deploy but no data impact. IRREVERSIBLE = data loss, credential exposure, corrupted state that persists after fix. |
| 7 | **Data Safety** | NONE / READ / WRITE / CREDENTIAL | NONE = no user data involved. READ = can expose user messages or metadata. WRITE = can corrupt conversation history, memory, or session state. CREDENTIAL = can expose API keys, tokens, or vault contents. |
| 8 | **Concurrency Exposure** | NONE / LOW / HIGH | NONE = single-threaded path or startup-only. LOW = hit by <10 req/min. HIGH = hit by every incoming message, every tool call, or every provider request. Race conditions here affect every user. |
| 9 | **Provider Coupling** | AGNOSTIC / SINGLE / MULTI | AGNOSTIC = fix respects provider-agnostic principle. SINGLE = affects only one provider adapter. MULTI = fix changes shared provider interface — every provider must be re-verified. |
| 10 | **Test Coverage** | COVERED / PARTIAL / NONE | COVERED = existing tests exercise this exact code path and would catch a regression. PARTIAL = tests exist but don't cover the specific edge case. NONE = no test for this path — blind change. |
| 11 | **User Visibility** | SILENT / DEGRADED / ERROR / FATAL | SILENT = user never notices (internal log noise). DEGRADED = slower response, worse quality, extra retries. ERROR = user sees an error message but can continue. FATAL = user sends a message and gets nothing back — ever. Permanent silence. |
| 12 | **Fix Complexity** | TRIVIAL / MODERATE / COMPLEX / ARCHITECTURAL | TRIVIAL = 1–5 lines, single file. MODERATE = 10–50 lines, single file. COMPLEX = multi-file, requires understanding cross-crate interactions. ARCHITECTURAL = cross-crate refactor, trait changes, or new abstraction needed. |
| 13 | **Cross-Platform** | UNIVERSAL / UNIX_ONLY / NEEDS_VERIFY | UNIVERSAL = fix works on Windows, macOS, Linux. UNIX_ONLY = fix uses Unix-specific APIs. NEEDS_VERIFY = fix might behave differently on other platforms. |
| 14 | **Incident History** | NEVER / THEORETICAL / HAS_OCCURRED | NEVER = no evidence this has ever triggered. THEORETICAL = plausible scenario but never observed. HAS_OCCURRED = this exact bug has caused a production incident (reference the incident). |
| 15 | **Recovery Path** | SELF_HEALING / RESTART / MANUAL / NONE | SELF_HEALING = system auto-recovers (circuit breaker, failover). RESTART = process restart fixes it. MANUAL = requires human intervention (DB repair, key rotation). NONE = permanent damage (data loss, credential leak). |

### Priority Score Calculation

```
Priority = (Runchanged × Blast_W × Visibility_W × Recovery_W) / (Rchange × Complexity_W + 1)
```

**Weight mappings:**

| Dimension | Values → Weights |
|-----------|-----------------|
| Blast Radius | ISOLATED=1, CHANNEL=3, GLOBAL=7, SYSTEM=10 |
| User Visibility | SILENT=1, DEGRADED=3, ERROR=5, FATAL=10 |
| Recovery Path | SELF_HEALING=1, RESTART=2, MANUAL=5, NONE=10 |
| Fix Complexity | TRIVIAL=1, MODERATE=2, COMPLEX=4, ARCHITECTURAL=8 |

**Priority bands:**

| Score | Band | Action |
|-------|------|--------|
| > 100 | **P0 — EMERGENCY** | Fix before anything else. This is a live production risk. |
| 25–100 | **P1 — CRITICAL** | Fix before next release. No exceptions. |
| 5–25 | **P2 — HIGH** | Fix within 1 week. |
| 1–5 | **P3 — MEDIUM** | Fix within 1 sprint. |
| < 1 | **P4 — LOW** | Fix opportunistically. Rchange exceeds Runchanged — changing it is riskier than leaving it. |

### Example Finding with Matrix

```markdown
### [CRIT-001] unwrap() on user-controlled JSON parse in openai_compat.rs:247

**Phase:** 1.1 — Unwrap/Expect Scan

| Dimension | Value | Rationale |
|-----------|-------|-----------|
| Rchange | 5% | Single-line change: `.unwrap()` → `?`. Well-understood transformation. |
| Runchanged | 40% | Any malformed provider response triggers panic. Happens ~monthly with API changes. |
| **RC** | **8.0** | 40 / (5 + 1) = 6.67. High urgency. |
| Agentic Core | INDIRECT | File is in temm1e-providers but return value feeds into agent runtime. |
| Blast Radius | GLOBAL | All users on all channels use this provider path. |
| Reversibility | DEPLOY | Requires new binary. No data impact. |
| Data Safety | NONE | Parse failure doesn't expose data. |
| Concurrency | HIGH | Every LLM response passes through this path. |
| Provider Coupling | SINGLE | Only affects OpenAI-compatible provider. |
| Test Coverage | PARTIAL | Provider tests exist but don't test malformed JSON. |
| User Visibility | FATAL | Panic kills the worker → user gets permanent silence. |
| Fix Complexity | TRIVIAL | 1 line change. |
| Cross-Platform | UNIVERSAL | No platform-specific code. |
| Incident History | HAS_OCCURRED | Vietnamese text incident 2026-03-09 (similar class). |
| Recovery Path | RESTART | Dead worker detection respawns, but user's message is lost. |

**Priority Score:** (40 × 7 × 10 × 2) / (5 × 1 + 1) = 5600 / 6 = **933** → **P0 EMERGENCY**

**Proposed fix:** Replace `.unwrap()` with `?` and add `#[test] fn test_malformed_provider_json()`.
```

### Agentic Core Review Gate

Any finding where **Agentic Core Impact = DIRECT** triggers a mandatory review gate:

1. **Full code path trace** — Map every caller and callee of the affected function
2. **Behavioral diff** — Document exact before/after behavior for every possible input
3. **Regression test** — Write a test that proves existing behavior is preserved
4. **10-turn CLI self-test** — Must pass after the change
5. **Founder sign-off** — DIRECT changes to Agentic Core require explicit approval

Findings with **Agentic Core Impact = INDIRECT** require steps 1–4 but not step 5.

Findings with **Agentic Core Impact = NONE** can proceed with normal fix + test.

---

## Phase 1: Panic Path Audit

**What we're hunting:** Any code path that can kill the process or a worker.

### 1.1 — Unwrap / Expect Scan

```bash
# Find all unwrap() and expect() in production code (exclude tests and test-utils)
rg '\.unwrap\(\)' --type rust -g '!**/tests/**' -g '!*test*' -g '!*mock*' --stats
rg '\.expect\(' --type rust -g '!**/tests/**' -g '!*test*' -g '!*mock*' --stats
```

**For each hit, classify:**
- `[SAFE]` — Infallible (e.g., `Regex::new()` on a literal, `Mutex::lock()` when poisoning is impossible)
- `[GUARDED]` — Preceded by a conditional check (e.g., `if option.is_some() { option.unwrap() }`)
- `[RISK]` — Can actually fail on user input, network data, or file I/O → **must fix**

**Action:** Every `[RISK]` hit gets converted to `?`, `.unwrap_or_default()`, or proper error handling. Zero tolerance.

### 1.2 — Index Slicing on User Text

```bash
# String slicing that could panic on multi-byte UTF-8
rg '\[\.\.[\w]+\]|\[[\w]+\.\.\]|\[[\w]+\.\.[\w]+\]' --type rust -g '!**/tests/**'
```

**Rule:** NEVER use `&text[..N]` on any string that could contain user input. Use `char_indices()` to find safe boundaries. This is what caused the Vietnamese text crash (2026-03-09).

### 1.3 — Catch-Unwind Coverage

Verify these critical paths are wrapped in `catch_unwind()`:

| Path | File | Must Be Wrapped |
|------|------|-----------------|
| Gateway message dispatch | `src/main.rs` | YES |
| CLI chat handler | `src/main.rs` | YES |
| Agent `process_message()` | `crates/temm1e-agent/src/runtime.rs` | YES (via gateway) |
| Perpetuum pulse | `crates/temm1e-perpetuum/src/lib.rs` | YES |
| Perpetuum concern dispatch | `crates/temm1e-perpetuum/src/cortex.rs` | YES |
| Tool execution | `crates/temm1e-tools/src/` | YES (via agent) |
| MCP bridge calls | `crates/temm1e-mcp/src/bridge.rs` | CHECK |
| Browser session operations | `crates/temm1e-tools/src/browser_session.rs` | CHECK |
| Hive swarm dispatch | `crates/temm1e-hive/src/` | CHECK |
| Cambium deploy pipeline | `crates/temm1e-cambium/src/deploy.rs` | CHECK |

**Action:** Any `CHECK` that isn't wrapped → wrap it. A panic in any of these kills user-facing functionality.

### 1.4 — Panic Profile Verification

```bash
# Cargo.toml must have panic = "unwind" for release
grep -A5 '\[profile.release\]' Cargo.toml
```

**MUST read:** `panic = "unwind"`. If someone changed it to `"abort"`, the entire 4-layer defense collapses.

---

## Phase 2: Error Handling Integrity

**What we're hunting:** Swallowed errors, incorrect error variants, missing error context.

### 2.1 — Swallowed Errors

```bash
# Find all places where errors are silently ignored
rg 'let _ =' --type rust -g '!**/tests/**' -g '!*test*'
rg '\.ok\(\);' --type rust -g '!**/tests/**' -g '!*test*'
rg 'if let Err\(_\)' --type rust -g '!**/tests/**' -g '!*test*'
rg 'Err\(_\) =>' --type rust -g '!**/tests/**' -g '!*test*'
```

**For each hit, classify:**
- `[INTENTIONAL]` — Documented why the error is safe to ignore (e.g., best-effort logging)
- `[LOGGING]` — Error is logged but not propagated (acceptable if non-critical path)
- `[SWALLOWED]` — Error is silently dropped with no log, no fallback → **must fix**

**Action:** Every `[SWALLOWED]` error must either be propagated (`?`), logged (`tracing::warn!`), or have a comment explaining why it's safe to ignore.

### 2.2 — Error Variant Correctness

For each `Temm1eError` usage, verify the variant matches the actual failure:
- Provider errors use `Temm1eError::Provider`
- Channel errors use `Temm1eError::Channel`
- Config parse errors use `Temm1eError::Config`
- Auth/credential errors use `Temm1eError::Auth`

```bash
# Find all Temm1eError constructions
rg 'Temm1eError::' --type rust -g '!**/tests/**' --stats
```

**Action:** Mismatched variants confuse error recovery logic. Fix any that don't match their origin.

### 2.3 — Error Context Completeness

```bash
# Find bare error returns without context
rg 'return Err\(Temm1eError::\w+\("' --type rust -g '!**/tests/**'
```

**Rule:** Error messages must include enough context to diagnose without reading the source code. Bad: `"Failed to send"`. Good: `"Failed to send message to Telegram chat {chat_id}: {e}"`.

---

## Phase 3: LLM Handling Audit

**What we're hunting:** Token waste, context overflow, malformed prompts, cost leaks, rate limit mishandling.

### 3.1 — Context Window Safety

Check that every provider path respects context limits:

```bash
# Find all max_tokens / max_context_tokens references
rg 'max_tokens|max_context|context_window' --type rust
```

**Verify:**
- [ ] `estimate_prompt_tokens()` is called before every provider request
- [ ] History pruning triggers before context exceeds model limit
- [ ] Provider-specific limits are respected (each model has different max context)
- [ ] The 10% input budget safety margin is applied
- [ ] No hardcoded `max_tokens` on LLM output (must be `None` — Skull manages input)

### 3.2 — Token Estimation Accuracy

```bash
# The ~4 chars per token heuristic
rg 'estimate.*token|token.*estimate|chars.*token' --type rust
```

**Verify:**
- [ ] Heuristic accounts for system prompt, tool declarations, and conversation history
- [ ] Heuristic works for non-Latin scripts (CJK, Arabic, Devanagari — these tokenize differently)
- [ ] Buffer exists for token count drift (estimate vs actual)

### 3.3 — Prompt Injection Surface

For every place where user input is injected into LLM context:

```bash
# Find all format!/format string constructions that include user data
rg 'format!\(.*message|format!\(.*content|format!\(.*input|format!\(.*query' --type rust -g '!**/tests/**'
```

**Verify:**
- [ ] User messages are placed in `user` role, never `system` role
- [ ] Tool outputs are placed in `tool` role, never `system` role
- [ ] No user input is interpolated into system prompts without sanitization
- [ ] Credential scrubber (`credential_scrub.rs`) runs before injecting tool output into context

### 3.4 — Rate Limit & Retry Behavior

```bash
rg 'rate.?limit|429|retry|backoff' --type rust
```

**Verify:**
- [ ] HTTP 429 triggers key rotation (Anthropic provider)
- [ ] Exponential backoff with jitter (not fixed intervals)
- [ ] Maximum retry count exists (no infinite retry loops)
- [ ] Circuit breaker opens after N consecutive failures
- [ ] Rate limit errors surface to user as a message ("I'm being rate limited, retrying..."), not silence

### 3.5 — Cost Tracking Integrity

```bash
rg 'BudgetTracker|ModelPricing|cost|spend|usage' --type rust -g '!**/tests/**'
```

**Verify:**
- [ ] Every provider response updates the budget tracker
- [ ] Streaming responses accumulate tokens correctly (not just final count)
- [ ] Budget limit (`max_spend_usd`) actually stops the agent (not just logs a warning)
- [ ] Cost per model is accurate and up to date with current pricing
- [ ] Budget display in CLI/TUI shows accurate running total

### 3.6 — Tool Call Loop Protection

```bash
rg 'max_tool_rounds|tool_round|max_turns' --type rust
```

**Verify:**
- [ ] `max_tool_rounds` (200) is enforced — agent cannot loop forever calling tools
- [ ] Tool output truncation at `MAX_TOOL_OUTPUT_CHARS` (30K) works correctly
- [ ] Recursive tool calls (tool A triggers tool B triggers tool A) are detected or bounded
- [ ] Empty/null tool results are handled gracefully (not sent to LLM as empty string)

---

## Phase 4: Channel Resilience

**What we're hunting:** Disconnections that go undetected, message loss, auth failures that kill the channel.

### 4.1 — Per-Channel Health Matrix

Run this check for each active channel:

| Check | Telegram | Discord | WhatsApp Web | WhatsApp Cloud | Slack | CLI |
|-------|----------|---------|--------------|----------------|-------|-----|
| Reconnection on disconnect | | | | | | N/A |
| Backoff on repeated failures | | | | | | N/A |
| Auth token refresh/rotation | | | | | | N/A |
| Message delivery confirmation | | | | | | |
| Large message handling (>4096 chars) | | | | | | |
| Media/file transfer error handling | | | | | | |
| Rate limit compliance | | | | | | N/A |
| Graceful shutdown (no message loss) | | | | | | |
| Allowlist enforcement | | | | | | |

**For each empty cell:** Read the implementation and fill in YES/NO/PARTIAL. Every NO is a finding.

### 4.2 — Message Ordering Guarantees

```bash
rg 'mpsc::channel|mpsc::unbounded|broadcast::channel' --type rust
```

**Verify:**
- [ ] Messages are processed in order per-user (no interleaving of concurrent messages from same user)
- [ ] Long-running agent tasks don't block messages from other users
- [ ] Channel-full backpressure doesn't silently drop messages

### 4.3 — Allowlist Edge Cases

```bash
rg 'allowlist|allow_list|authorized' --type rust -g '!**/tests/**'
```

**Verify:**
- [ ] Empty allowlist denies ALL users (DF-16)
- [ ] User ID matching is numeric only, never username-based (CA-04)
- [ ] Allowlist changes take effect without restart
- [ ] Denied users get a response (not silence)

---

## Phase 5: Memory Backend Durability

**What we're hunting:** Data loss, corruption, search failures, session leaks.

### 5.1 — Write Durability

```bash
rg 'store_entry|save|insert|update|upsert' --type rust -g '*memory*'
```

**Verify:**
- [ ] SQLite operations use transactions for multi-step writes
- [ ] Write failures are propagated (not swallowed)
- [ ] WAL mode is enabled for SQLite (concurrent read/write safety)
- [ ] Markdown backend handles concurrent writes safely (file locking or single-writer)

### 5.2 — Search Quality

```bash
rg 'search|query|find.*entries' --type rust -g '*memory*'
```

**Verify:**
- [ ] Hybrid search (vector 0.7 + keyword 0.3) returns relevant results
- [ ] Word-split AND matching works correctly across `content` and `id` fields
- [ ] Empty search queries return reasonable results (not errors)
- [ ] Search on large history sets performs within acceptable time (<500ms)

### 5.3 — Session Lifecycle

```bash
rg 'SessionManager|session.*create|session.*remove|session.*evict' --type rust
```

**Verify:**
- [ ] LRU eviction at MAX_SESSIONS (1000) doesn't lose active sessions
- [ ] History truncation at MAX_HISTORY_PER_SESSION (200) preserves recent context
- [ ] Session state is consistent after provider switch mid-conversation
- [ ] Abandoned sessions are cleaned up (not leaked forever)

### 5.4 — Failover Behavior

```bash
rg 'ResilientMemory|failover|fallback|degraded' --type rust
```

**Verify:**
- [ ] Primary failure triggers fallback to in-memory cache
- [ ] Recovery sync doesn't lose data written during failover
- [ ] Degraded status is reported via health endpoint
- [ ] Max consecutive failures threshold (3) is appropriate

---

## Phase 6: Provider Integration Stress

**What we're hunting:** Provider-specific edge cases that cause silent failures or incorrect behavior.

### 6.1 — Response Parsing Robustness

For each provider (Anthropic, OpenAI-compat, Codex OAuth):

```bash
rg 'parse|deserialize|from_str|serde' --type rust -g '*providers*' -g '*codex*'
```

**Verify:**
- [ ] Unknown/new content block types are handled gracefully (not panic or silent drop)
- [ ] Malformed JSON responses produce clear error messages
- [ ] Empty responses ("") are handled (not sent to user as blank message)
- [ ] Streaming interruptions mid-token are recovered from
- [ ] Provider returns tool_use but tool doesn't exist → clear error to user

### 6.2 — API Key Lifecycle

```bash
rg 'api_key|rotate_key|current_key|credentials' --type rust -g '!**/tests/**'
```

**Verify:**
- [ ] Key rotation on rate limit actually uses a different key
- [ ] Expired/revoked keys produce auth error → onboarding flow (not infinite retry)
- [ ] Keys are never logged at info level (debug with masking only)
- [ ] Credential detection order handles ambiguous prefixes correctly
- [ ] Vault encryption (ChaCha20-Poly1305) works correctly for stored keys

### 6.3 — Provider Switching

```bash
rg 'switch.*provider|provider.*switch|detect_api_key|create_provider' --type rust
```

**Verify:**
- [ ] Switching provider mid-conversation preserves history in compatible format
- [ ] Cross-provider history sanitization strips provider-specific fields
- [ ] System prompt is regenerated for new provider's format
- [ ] Budget tracker resets model pricing on switch

---

## Phase 7: Tool Execution Safety

**What we're hunting:** Unsafe tool execution, resource leaks, credential exposure.

### 7.1 — Shell Tool Safety

```bash
rg 'shell|exec|command|process' --type rust -g '*tools*'
```

**Verify:**
- [ ] Shell commands run in sandbox with declared resource needs
- [ ] Command timeout exists (tool can't run forever)
- [ ] Output size is bounded (MAX_TOOL_OUTPUT_CHARS: 30K)
- [ ] Exit codes are captured and reported
- [ ] Stderr is captured (not lost)
- [ ] Working directory is scoped (no access outside workspace)

### 7.2 — Browser Tool Safety

```bash
rg 'browser|chromium|playwright|headless' --type rust -g '*tools*'
```

**Verify:**
- [ ] Browser sessions are cleaned up on completion (no zombie processes)
- [ ] Browser pool reclaims contexts properly
- [ ] OTK login session handles timeout/failure gracefully
- [ ] Screenshots don't leak credentials (credential scrubber runs)
- [ ] Navigation errors produce tool error (not panic)

### 7.3 — File Operations Safety

```bash
rg 'file.*op|read_file|write_file|path.*traversal|sanitize.*name' --type rust -g '*tools*'
```

**Verify:**
- [ ] File names are sanitized (directory components stripped — path traversal prevention)
- [ ] File size limits exist for uploads/downloads
- [ ] Temporary files are cleaned up
- [ ] File operations respect workspace boundaries

### 7.4 — Credential Scrubbing

```bash
rg 'credential.*scrub|scrub.*credential|redact|mask' --type rust
```

**Verify:**
- [ ] `credential_scrub.rs` catches API keys, tokens, passwords in tool output
- [ ] Scrubbed content is replaced with `[REDACTED]`, not deleted (context preserved)
- [ ] Scrubber runs BEFORE tool output enters LLM context (isolation boundary)
- [ ] New credential patterns (e.g., new provider key prefixes) are covered

---

## Phase 8: Concurrency & State Safety

**What we're hunting:** Race conditions, deadlocks, state corruption under concurrent load.

### 8.1 — Lock Audit

```bash
rg 'Mutex|RwLock|AtomicU|AtomicBool|Arc<' --type rust -g '!**/tests/**' --stats
```

**For each lock:**
- [ ] Lock scope is minimal (held only for the critical section)
- [ ] No nested locks that could deadlock (lock A → lock B → lock A)
- [ ] RwLock is used where reads dominate (SessionManager, config)
- [ ] Mutex is used where writes are frequent
- [ ] Arc is not leaked (strong count grows unbounded)

### 8.2 — Channel Buffer Pressure

```bash
rg 'channel\(|bounded|unbounded|buffer' --type rust -g '!**/tests/**'
```

**Verify:**
- [ ] Bounded channels have appropriate capacity
- [ ] Channel-full condition is handled (backpressure, not panic)
- [ ] Unbounded channels are justified (or should be converted to bounded)
- [ ] Slow consumers don't cause memory growth

### 8.3 — Async Task Lifecycle

```bash
rg 'tokio::spawn|task::spawn|JoinHandle' --type rust -g '!**/tests/**'
```

**Verify:**
- [ ] Spawned tasks have shutdown signals (CancellationToken, AtomicBool)
- [ ] JoinHandles are awaited (not silently dropped — fires a warning in Tokio)
- [ ] Task panics are caught (via catch_unwind or JoinHandle error)
- [ ] No tasks hold locks across await points (potential deadlock)

---

## Phase 9: Configuration & Startup Safety

**What we're hunting:** Config parsing failures that kill startup, missing defaults, env var issues.

### 9.1 — Config Parsing Robustness

```bash
rg 'load_config|parse_config|config.*from|toml::from' --type rust
```

**Verify:**
- [ ] Missing optional fields have sensible defaults (not panic)
- [ ] Unknown config keys are ignored (forward compatibility)
- [ ] `${VAR}` expansion handles missing env vars gracefully (clear error message)
- [ ] Config validation runs at startup (not first use — fail fast)
- [ ] Invalid config produces specific error ("field X is invalid") not generic parse error

### 9.2 — Startup Order Dependencies

```bash
rg 'init|startup|bootstrap|setup' --type rust -g 'main.rs' -g 'lib.rs'
```

**Verify:**
- [ ] Tracing/logging initializes first (so all subsequent errors are captured)
- [ ] Config loads before anything that reads config
- [ ] Vault initializes before credential loading
- [ ] Provider and channel creation handles missing credentials (onboarding mode)
- [ ] Health endpoint is available immediately (not after full initialization)

### 9.3 — Graceful Degradation

When components fail at startup, verify the system continues with reduced capability:

- [ ] Missing provider → system starts, reports no-provider, enters onboarding
- [ ] Missing channel → system starts, other channels work
- [ ] Missing memory backend → system starts with in-memory fallback
- [ ] Missing vault → system starts, credential operations return clear errors
- [ ] Missing optional features → system starts without them (feature flags)

---

## Phase 10: Watchdog & Recovery Verification

**What we're hunting:** Recovery failures — the mechanisms that are supposed to save us actually working.

### 10.1 — Watchdog Binary Integrity

```bash
# Verify watchdog binary builds independently
cargo build --release -p temm1e-watchdog

# Verify it has minimal dependencies (should be lightweight)
cargo tree -p temm1e-watchdog --depth 1
```

**Verify:**
- [ ] Watchdog binary has zero AI dependencies (no LLM calls)
- [ ] Watchdog binary has zero network dependencies (no HTTP)
- [ ] PID file monitoring works correctly
- [ ] Restart count window prevents infinite restart loops
- [ ] Signal handling (SIGINT, SIGTERM) works on both Unix and Windows

### 10.2 — In-Process Watchdog

```bash
rg 'SubsystemStatus|report.*health|should_shutdown|consecutive_failures' --type rust
```

**Verify:**
- [ ] Provider health reporting triggers on provider errors
- [ ] Memory health reporting triggers on memory backend failures
- [ ] Channel health reporting triggers on channel disconnects
- [ ] `should_shutdown()` at max consecutive failures (5) actually triggers clean shutdown
- [ ] Health status is exposed via `/status` endpoint

### 10.3 — Circuit Breaker Behavior

```bash
rg 'CircuitBreaker|circuit.*state|Closed|Open|HalfOpen' --type rust
```

**Verify:**
- [ ] Closed → Open transition at failure threshold
- [ ] Open → HalfOpen after recovery timeout
- [ ] HalfOpen → Closed on single success
- [ ] HalfOpen → Open on failure (with doubled timeout)
- [ ] Maximum recovery timeout cap (5 min)
- [ ] Circuit state is logged for observability

### 10.4 — Dead Worker Detection

```bash
rg 'dead.*worker|slot.*fail|respawn|worker.*restart' --type rust
```

**Verify:**
- [ ] Failed `slot.tx.send()` removes the dead slot
- [ ] New worker spawns on next incoming message
- [ ] Worker death is logged with error context
- [ ] No message is lost during worker respawn

### 10.5 — Session Rollback on Panic

```bash
rg 'rollback|catch_unwind|session.*restore|history.*restore' --type rust
```

**Verify:**
- [ ] If catch_unwind triggers, session history is rolled back to pre-message state
- [ ] Rolled-back session is still usable (user can send next message normally)
- [ ] Rollback is logged for post-incident analysis

---

## Sweep Report Format

After completing all phases, generate a report in this format. Reports are stored in `docs/full-sweep-<N>/` where N is the sweep number (matching the branch name).

```markdown
# Full Sweep Report — vX.Y.Z — YYYY-MM-DD

## Summary
- **Sweep type:** Full / Targeted (phases X, Y, Z)
- **Baseline:** N tests passing, 0 failures, clippy clean, fmt clean
- **Findings:** N P0, N P1, N P2, N P3, N P4

## Priority Heatmap
| Priority | Count | Agentic Core (DIRECT) | Agentic Core (INDIRECT) | Leaf Crate |
|----------|-------|-----------------------|------------------------|------------|
| P0 EMERGENCY | | | | |
| P1 CRITICAL | | | | |
| P2 HIGH | | | | |
| P3 MEDIUM | | | | |
| P4 LOW | | | | |

## Findings — P0 EMERGENCY
### [P0-001] Title
- **Phase:** N.N
- **File:** path/to/file.rs:line
- **Description:** What's wrong
- **Impact:** What happens if this triggers in production

| Dimension | Value | Rationale |
|-----------|-------|-----------|
| Rchange | X% | ... |
| Runchanged | X% | ... |
| RC | X.X | ... |
| Agentic Core | NONE/INDIRECT/DIRECT | ... |
| Blast Radius | ... | ... |
| Reversibility | ... | ... |
| Data Safety | ... | ... |
| Concurrency | ... | ... |
| Provider Coupling | ... | ... |
| Test Coverage | ... | ... |
| User Visibility | ... | ... |
| Fix Complexity | ... | ... |
| Cross-Platform | ... | ... |
| Incident History | ... | ... |
| Recovery Path | ... | ... |

**Priority Score:** ... → **P0 EMERGENCY**
**Proposed Fix:** ...
**Verified:** [ ] Fix applied and tested

## Findings — P1 CRITICAL
### [P1-001] Title
(same format)

## Findings — P2 HIGH
### [P2-001] Title
(same format)

## Findings — P3 MEDIUM / P4 LOW
(condensed format — matrix table only, grouped)

## Metrics Comparison
| Metric | Before Sweep | After Fixes |
|--------|-------------|-------------|
| Tests passing | | |
| Clippy warnings | 0 | 0 |
| unwrap() in prod code | | |
| Swallowed errors | | |
| Catch-unwind coverage | | |

## Sign-off
- [ ] All P0 findings fixed
- [ ] All P1 findings fixed
- [ ] All compilation gates pass after fixes
- [ ] 10-turn CLI self-test passes
- [ ] Report committed to docs/full-sweep-<N>/
```

---

## Sweep History

Store completed sweep reports in `docs/full-sweep-<N>/` directories:

```
docs/full-sweep-1/
  README.md              -- Summary + priority heatmap
  phase-01-panic.md      -- Phase 1 raw findings with matrices
  phase-02-errors.md     -- Phase 2 raw findings
  phase-03-llm.md        -- Phase 3 raw findings
  phase-04-channels.md   -- Phase 4 raw findings
  phase-05-memory.md     -- Phase 5 raw findings
  phase-06-providers.md  -- Phase 6 raw findings
  phase-07-tools.md      -- Phase 7 raw findings
  phase-08-concurrency.md -- Phase 8 raw findings
  phase-09-config.md     -- Phase 9 raw findings
  phase-10-watchdog.md   -- Phase 10 raw findings
  fix-plan.md            -- Ordered fix plan derived from priority scores
```

---

## Severity Definitions

| Severity | Definition | SLA |
|----------|-----------|-----|
| **CRITICAL** | Can crash the process, lose user data, expose credentials, or cause permanent silence (user sends message, gets nothing back — ever) | Fix before next release. No exceptions. |
| **HIGH** | Degrades user experience significantly: messages delayed >30s, tool failures not reported, cost tracking incorrect, provider errors surfaced as raw JSON | Fix within 1 week |
| **MEDIUM** | Suboptimal behavior that users may notice: unnecessary retries, verbose error messages, slow search, memory not reclaimed promptly | Fix within 1 sprint |
| **LOW** | Code quality issues that don't affect users today but increase future risk: missing logs, inconsistent error variants, unused error paths | Fix opportunistically |
| **INFO** | Observations, architecture notes, improvement ideas. Not bugs. | Track in backlog |

---

## Quick Reference: Key Constants

These are the system limits that define resilience boundaries. Verify they are appropriate during every sweep:

| Constant | Value | Location | Purpose |
|----------|-------|----------|---------|
| `MAX_SESSIONS` | 1000 | `session.rs` | LRU eviction threshold |
| `MAX_HISTORY_PER_SESSION` | 200 | `session.rs` | History truncation limit |
| `MAX_TOOL_OUTPUT_CHARS` | 30,000 | `runtime.rs` | Tool output size cap |
| `max_tool_rounds` | 200 | `runtime.rs` | Tool call loop limit |
| `max_turns` | 200 | agent config | Conversation turn limit |
| `max_context_tokens` | 30,000 | agent config | Context window budget |
| `max_task_duration` | 1800s | agent config | Single task timeout |
| `max_consecutive_failures` | 5 | `watchdog.rs` | Shutdown trigger |
| `max_consecutive_failures` | 3 | `failover.rs` | Memory failover trigger |
| `STOP_TIMEOUT` | 30s | `deploy.rs` | Graceful stop timeout |
| `BUILD_TIMEOUT` | 600s | `deploy.rs` | Build timeout |
| `HEALTH_TIMEOUT` | 30s | `deploy.rs` | Startup health check |
| `base_recovery_timeout` | 10s | `circuit_breaker.rs` | Circuit breaker initial recovery |
| `max_recovery_timeout` | 300s | `circuit_breaker.rs` | Circuit breaker max recovery |
| Input safety margin | 10% | model registry | Token budget headroom |

---

## Automation Hooks

For CI integration, the sweep phases map to scripts:

```bash
# Phase 1: Panic paths
scripts/sweep/panic_audit.sh

# Phase 2: Error handling
scripts/sweep/error_audit.sh

# Phase 3: LLM handling
scripts/sweep/llm_audit.sh

# Phases 4-10: Require manual review + code reading
```

Phases 1-3 can be partially automated with grep/clippy lints. Phases 4-10 require human (or Claude) judgment — reading code, understanding intent, verifying behavior under edge cases.

---

## Philosophy

> A system that "usually works" is a system that sometimes fails users. "Sometimes" is not a number — it's every user who happened to send a message at the wrong time. Extreme resilience means every message gets a response, every tool call gets a result, every error gets a recovery path. The sweep exists to close the gap between "usually" and "always".
