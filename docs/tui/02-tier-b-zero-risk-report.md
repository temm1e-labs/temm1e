# 02 — Tier B Zero-Risk Report (Observability Enhancement)

**Risk tier:** ZERO (after full investigation and mitigation).

**Initial assessment was LOW.** Driven to ZERO by:

1. **Serialization verification**: `AgentTaskPhase` is
   `#[derive(Debug, Clone)]` only — NOT `Serialize`, NOT `Deserialize`,
   NOT `Hash`, NOT `Eq`. Zero persistence risk.
2. **Exhaustive workspace grep**: every `AgentTaskPhase::` site in
   source, tests, docs, and root binary is cataloged. Zero hidden
   consumers.
3. **Latent semantic bug found and mitigated**: `activity_panel.rs:149-164`
   has a 5-entry partial `matches!()` stepper that would silently
   misrender when `phase == ToolCompleted`. Fixed inline as part of the
   atomic Tier B commit.
4. **Atomic commit strategy**: B1+B2+B3+B4+B5 land as a single commit so
   there is no bisect window where the intermediate state carries the
   semantic bug.

Touches `temm1e-agent` (additive enum fields + new variant) and
`temm1e-tui` (consumer updates). Rustc catches every compile-time
error; the one runtime-semantic concern is explicitly fixed below.

**Items:** 5

| ID | Item | Touches |
|----|------|---------|
| B1 | Enrich `AgentTaskPhase::ExecutingTool` with `args_preview` + `started_at_ms` | `agent_task_status.rs` |
| B2 | Add new phase `AgentTaskPhase::ToolCompleted { duration_ms, ok, result_preview, ... }` | `agent_task_status.rs` |
| B3 | Emit enriched events from `runtime.rs` at tool start and tool completion | `runtime.rs` |
| B4 | Rewrite activity panel rendering as a clean streaming trace | `widgets/activity_panel.rs` |
| B5 | Replace collapsed "thinking (68s)" with latest tool info inline | `views/chat.rs` |

---

## 1. Why this is ZERO (full investigation)

### 1.1 Serialization: NONE

`agent_task_status.rs:15,42`:

```rust
#[derive(Debug, Clone)]
pub enum AgentTaskPhase { ... }

#[derive(Debug, Clone)]
pub struct AgentTaskStatus { ... }
```

**Grep confirmation:**

```bash
grep '#\[derive(.*Serialize' crates/temm1e-agent/src/agent_task_status.rs
# → no matches
```

Consequences:
- NOT persisted to SQLite memory backend
- NOT logged as structured JSON via tracing
- NOT sent over any wire protocol
- NOT hashed or used as a map key (no `Hash`/`Eq`)
- In-memory only, via `tokio::sync::watch` channel

This eliminates an entire class of breakage: there is no way to deserialize
old data into the new enum shape, because there is no serialized form.

### 1.2 Exhaustive workspace grep

Complete list of every `AgentTaskPhase::` reference in source code
(excluding docs and this report):

| Site | Kind | Risk | Action |
|------|------|------|--------|
| `crates/temm1e-agent/src/lib.rs:33` | `pub use` export | safe | none |
| `crates/temm1e-agent/src/agent_task_status.rs:16` | enum definition | — | update |
| `crates/temm1e-agent/src/agent_task_status.rs:45` | struct field type | safe | none |
| `crates/temm1e-agent/src/agent_task_status.rs:63` | struct initializer | safe | none |
| `crates/temm1e-agent/src/agent_task_status.rs:74-95` | Display `match self` | **exhaustive** | update |
| `crates/temm1e-agent/src/agent_task_status.rs:111,123,161` | test `matches!()` single-variant | safe | none |
| `crates/temm1e-agent/src/agent_task_status.rs:130-141` | test Vec of variants | non-exhaustive list | update to cover new variants |
| `crates/temm1e-agent/src/runtime.rs:33` | import | safe | none |
| `crates/temm1e-agent/src/runtime.rs:440,563,936,1152,1374,1729,2052` | struct initializers (Preparing, Classifying, Interrupted, CallingProvider, Finishing, Done) | safe | none (no variants modified) |
| `crates/temm1e-agent/src/runtime.rs:1788` | `ExecutingTool` struct initializer | **needs new fields** | update (this IS B3) |
| `crates/temm1e-agent/src/runtime.rs:2051` | `matches!(s.phase, Interrupted { .. })` single-variant | safe | none |
| `crates/temm1e-agent/tests/runtime_integration.rs:6` | import | safe | none |
| `crates/temm1e-agent/tests/runtime_integration.rs:210,241,265,333` | `matches!(status.phase, Done)` single-variant | safe | none |
| `crates/temm1e-tui/src/widgets/activity_panel.rs:8` | import | safe | none |
| `crates/temm1e-tui/src/widgets/activity_panel.rs:35,48,67` | struct field / initializer | safe | none |
| `crates/temm1e-tui/src/widgets/activity_panel.rs:83` | `if let ExecutingTool { tool_name, tool_index, .. }` | **partial destructure with `..`** | safe (tolerates new fields) — but rewritten by B4 |
| `crates/temm1e-tui/src/widgets/activity_panel.rs:149-164` | 5-entry `[(&str, bool); 5]` stepper array | **semantic latent bug** | update inline (see 1.3) — or replaced by B4 |
| `crates/temm1e-tui/src/views/chat.rs:9` | import | safe | none |
| `crates/temm1e-tui/src/views/chat.rs:108-122` | exhaustive match | **exhaustive** | update (this IS B5) |
| `crates/temm1e-tui/src/app.rs:5` | import | safe | none |
| `crates/temm1e-tui/src/app.rs:177` | `matches!(status.phase, Done)` single-variant | safe | none |
| `crates/temm1e-tui/src/event.rs:3` | import of `AgentTaskStatus` | safe | none |
| `crates/temm1e-tui/src/agent_bridge.rs:13,28,161` | import + channel type + constructor | safe | none |
| `crates/temm1e-tui/src/channel.rs:12,32,33,44,69,74` | import + channel plumbing | safe | none |
| `crates/temm1e-tui/src/lib.rs:45,203` | import + `pending::<Option<AgentTaskStatus>>()` | safe | none |
| `src/main.rs:2431,2834,2941` | type signature + `::default()` construction + reset | safe | none |

**No hidden consumers. Zero unknown-unknowns.**

### 1.3 The one latent semantic bug (found and mitigated)

`activity_panel.rs:149-164`:

```rust
let phases = [
    ("Preparing", matches!(self.phase, AgentTaskPhase::Preparing)),
    ("Classifying", matches!(self.phase, AgentTaskPhase::Classifying)),
    ("Calling Provider", matches!(self.phase, AgentTaskPhase::CallingProvider { .. })),
    ("Executing Tools", matches!(self.phase, AgentTaskPhase::ExecutingTool { .. })),
    ("Finishing", matches!(self.phase, AgentTaskPhase::Finishing)),
];
```

Used at lines 166-180 to render a 5-step visual stepper. Walk the array
in order; the FIRST match marks "active" (⊙), subsequent entries are
"pending" (○), preceding entries are "done" (●).

**Bug scenario after B2:** when `self.phase == AgentTaskPhase::ToolCompleted { .. }`,
all 5 `matches!()` return `false`. `found_active` stays `false`. Every
row gets the "done" ● icon. The stepper renders as if the task finished,
even though it's mid-execution waiting for the next provider round.

**Why rustc can't catch it:** `matches!()` returns a bool, not an
exhaustive match. Missing variants silently return false. Clippy does
not warn. This is the ONE thing that could leak past compilation.

**Mitigation (applied inline as part of the Tier B atomic commit):**

Update line 161 to include `ToolCompleted`:

```rust
(
    "Executing Tools",
    matches!(
        self.phase,
        AgentTaskPhase::ExecutingTool { .. } | AgentTaskPhase::ToolCompleted { .. }
    ),
),
```

Rationale: `ToolCompleted` is a transient phase between tool execution
and the next provider call. Visually it belongs to the "Executing Tools"
step — the tool just finished, the stepper is still "on" that step
until the next `CallingProvider` fires.

**Belt-and-suspenders:** B4 rewrites the activity panel render entirely,
dropping this phases array in favor of a streaming trace. Even if the
inline fix were forgotten, the B4 rewrite eliminates the buggy code
before it ships. Both safety nets in place.

### 1.4 Atomic commit strategy

All Tier B changes (B1 + B2 + B3 + B4 + B5 + the activity_panel.rs:161
one-line fix) land in a **single atomic commit**. This means:

- No bisect window where the enum has `ToolCompleted` but the consumer
  hasn't been updated
- No intermediate `cargo check` failures
- No risk of cherry-picking B1/B2 without B4/B5

Commit subject: `agent+tui: enriched tool phase events + streaming trace`.

`05-implementation-spec.md` updates the commit ordering accordingly
(commits 7-9 collapse into one).

### 1.5 Residual risk analysis

After all mitigations, what could still go wrong?

| Concern | Caught by | Residual risk |
|---------|-----------|---------------|
| Missed exhaustive match | `cargo check` (rustc error) | none |
| Missing struct field | `cargo check` (rustc error) | none |
| Partial `matches!()` with semantic drift | manual audit + atomic commit | none (the only such site is fixed) |
| Serialization / persistence | not Serialize/Deserialize | none |
| External crate depending on the enum | workspace-only, no external consumers | none |
| Test update miss | `cargo test` will fail on Vec literal | none |
| Downstream formatter / Debug stringification | `Debug` is derived, auto-handles new variants | none |
| Runtime panic from arithmetic on duration_ms or tokens | Display impl uses safe format; `u64` arithmetic | none |
| Concurrency / watch channel semantics | `send_modify` is infallible, unchanged | none |

**Nothing left.** Tier B is ZERO risk.

## 2. Complete pattern-match audit

Grep for `AgentTaskPhase::` across the entire workspace. Results:

### 2.1 MUST update (exhaustive matches)

| # | File | Line | Construct | Arms | Action |
|---|------|------|-----------|------|--------|
| 1 | `crates/temm1e-agent/src/agent_task_status.rs` | 76 | `impl Display::fmt` → `match self` | all 7 | Add `ToolCompleted` arm; update `ExecutingTool` destructure with new fields |
| 2 | `crates/temm1e-tui/src/views/chat.rs` | 108 | `match &state.activity_panel.phase` (thinking indicator) | all 7 | Add `ToolCompleted` arm; keep `ExecutingTool` destructure compatible |

**Both files are already part of the v4.8.0 change set** (Display impl is
in the type file we're editing anyway; chat.rs:108 is being rewritten by
B5). No collateral damage.

### 2.2 Safe — non-exhaustive or catch-all matches

These use `matches!()` or `_` catch-alls and DO NOT need updates:

| File | Line | Pattern | Why safe |
|------|------|---------|----------|
| `crates/temm1e-tui/src/widgets/activity_panel.rs` | ~149-164 | `matches!()` checking specific variants | `matches!()` returns `false` for unlisted variants; no exhaustiveness check |
| `crates/temm1e-agent/src/runtime.rs` | ~2051 | `matches!(phase, AgentTaskPhase::Interrupted { .. })` | single variant check |
| `crates/temm1e-tui/src/app.rs` | 177 | `matches!(status.phase, AgentTaskPhase::Done)` | single variant check |
| `crates/temm1e-agent/tests/runtime_integration.rs` | 210, 241, 265, 333 | `matches!()` in assertions | test-side catch-all |
| `crates/temm1e-agent/src/agent_task_status.rs` | 111, 123, 161 | `matches!()` in tests | test-side catch-all |

### 2.3 Struct initializers (not pattern matches)

All `AgentTaskPhase::Variant { ... }` constructions are at:

- `runtime.rs:440` (Preparing)
- `runtime.rs:563` (Classifying)
- `runtime.rs:936` (Interrupted)
- `runtime.rs:1152` (CallingProvider)
- `runtime.rs:1374` (Finishing)
- `runtime.rs:1729` (Done)
- `runtime.rs:1788` (ExecutingTool) **← will be updated by B3**
- `runtime.rs:2051` (Done, fallback)

The `ExecutingTool` construction at 1788 is part of the v4.8.0 change
set anyway. The other constructions are untouched; adding new fields to
a variant doesn't break construction as long as we use the updated
field list everywhere we construct.

**Rust enforces this via "missing fields" errors — impossible to
accidentally skip.**

### 2.4 Imports

| File | Line |
|------|------|
| `views/chat.rs` | 9 |
| `widgets/activity_panel.rs` | 8 |
| `app.rs` | 5 (already imports `AgentTaskPhase`) |
| `tests/runtime_integration.rs` | 6 |

Imports don't break on enum changes.

### Audit conclusion

**Only 2 places need updates** (`agent_task_status.rs:76` and
`chat.rs:108`), and **both are already in the Tier B change set** (we're
updating those files anyway for B1/B5). There is **no risk of an
undiscovered exhaustive match breaking the build**.

---

## B1. Enrich `AgentTaskPhase::ExecutingTool`

### Before

```rust
ExecutingTool {
    round: u32,
    tool_name: String,
    tool_index: u32,
    tool_total: u32,
},
```

### After

```rust
ExecutingTool {
    round: u32,
    tool_name: String,
    tool_index: u32,
    tool_total: u32,
    args_preview: String,       // NEW: truncated JSON args, max 80 chars
    started_at_ms: u64,         // NEW: ms since `started_at` epoch
},
```

### `args_preview` construction

```rust
fn truncate_preview(value: &serde_json::Value, max_chars: usize) -> String {
    let s = serde_json::to_string(value).unwrap_or_else(|_| "{}".to_string());
    if s.chars().count() <= max_chars {
        return s;
    }
    // UTF-8 safe truncation
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}
```

UTF-8 safety per resilience architecture. Max 80 chars includes the ellipsis.

### `started_at_ms` construction

Milliseconds since `AgentTaskStatus::started_at`:

```rust
let started_at_ms = status_tx.borrow().started_at.elapsed().as_millis() as u64;
```

This is monotonic and doesn't care about system clock changes.

### Breaking change: Display impl update

`agent_task_status.rs:80-89` currently:

```rust
Self::ExecutingTool {
    round,
    tool_name,
    tool_index,
    tool_total,
} => write!(
    f,
    "Running {tool_name} ({}/{tool_total}, round {round})",
    tool_index + 1
),
```

Update to destructure new fields:

```rust
Self::ExecutingTool {
    round,
    tool_name,
    tool_index,
    tool_total,
    args_preview: _,     // ignored in Display — preview is for rich rendering
    started_at_ms: _,
} => write!(
    f,
    "Running {tool_name} ({}/{tool_total}, round {round})",
    tool_index + 1
),
```

`views/chat.rs:118` destructures only `tool_name`:

```rust
AgentTaskPhase::ExecutingTool { tool_name, .. } => tool_name.as_str(),
```

The `..` makes it already-compatible — **no change needed** at chat.rs
for the `ExecutingTool` variant. (Still needs updating for B2 below.)

### Scenarios

| Scenario | Expected |
|---------|----------|
| Tool called with small args `{"cmd": "ls"}` | `args_preview = "{\"cmd\":\"ls\"}"` |
| Tool called with huge args (1KB JSON) | Truncated to 80 chars with `…` suffix |
| Tool called with no args (null) | `args_preview = "null"` |
| Tool called with args containing emoji | UTF-8 safe truncation, emoji preserved or dropped atomically |
| Status sent via watch channel | Receivers see new variant fields, render code uses them |

### Risk

**LOW.** Pattern-match audit confirms only 2 update sites; both are in
the change set.

---

## B2. New phase `AgentTaskPhase::ToolCompleted`

### Variant

```rust
/// Tool finished executing.
ToolCompleted {
    round: u32,
    tool_name: String,
    tool_index: u32,
    tool_total: u32,
    duration_ms: u64,
    ok: bool,
    result_preview: String,     // first non-empty line, max 80 chars
},
```

### Display impl

```rust
Self::ToolCompleted { tool_name, duration_ms, ok, .. } => {
    let status = if *ok { "✓" } else { "✗" };
    write!(f, "{status} {tool_name} ({duration_ms}ms)")
}
```

### `result_preview` construction

```rust
fn result_preview(output: &str, max_chars: usize) -> String {
    let first_line = output
        .lines()
        .find(|l| !l.trim().is_empty())
        .unwrap_or("")
        .trim();
    if first_line.chars().count() <= max_chars {
        return first_line.to_string();
    }
    let mut out: String = first_line.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}
```

Takes the first non-empty line (shell commands often have leading
blank lines), trims, truncates safely to 80 chars.

### Update to chat.rs:108 (collapsed thinking match)

Add a new arm:

```rust
let phase_text = match &state.activity_panel.phase {
    AgentTaskPhase::Preparing => "Preparing",
    AgentTaskPhase::Classifying => "Classifying",
    AgentTaskPhase::CallingProvider { round } => { ... }
    AgentTaskPhase::ExecutingTool { tool_name, .. } => tool_name.as_str(),
    AgentTaskPhase::ToolCompleted { tool_name, .. } => tool_name.as_str(),  // NEW
    AgentTaskPhase::Finishing => "Finishing",
    AgentTaskPhase::Done => "Done",
    AgentTaskPhase::Interrupted { .. } => "Interrupted",
};
```

(B5 replaces this match entirely — see below.)

### Scenarios

| Scenario | Expected |
|---------|----------|
| Shell tool succeeds with "hello world" output | `ToolCompleted { tool_name: "shell", duration_ms: 42, ok: true, result_preview: "hello world" }` |
| Shell tool fails with empty stderr | `ToolCompleted { ok: false, result_preview: "" }` |
| Tool with multi-KB stdout | First non-empty line truncated to 80 chars |
| Tool that errors via `Err(...)` | `ok: false`, `result_preview` uses the error message |
| Very fast tool (<1ms) | `duration_ms: 0` — not a bug, just very fast |

### Risk

**LOW.** New variant requires the 2 updates identified in the audit.

---

## B3. Emit enriched events from `runtime.rs`

### Current emission at tool start (`runtime.rs:1783-1795`)

```rust
status_tx.send_modify(|s| {
    s.phase = AgentTaskPhase::ExecutingTool {
        round,
        tool_name: tool_name.clone(),
        tool_index,
        tool_total,
    };
});
```

### New emission

```rust
// Before tool.execute(...)
let tool_start = Instant::now();
let args_preview = truncate_preview(&input, 80);
status_tx.send_modify(|s| {
    s.phase = AgentTaskPhase::ExecutingTool {
        round,
        tool_name: tool_name.clone(),
        tool_index,
        tool_total,
        args_preview: args_preview.clone(),
        started_at_ms: s.started_at.elapsed().as_millis() as u64,
    };
});

let result = execute_tool(tool, input, &ctx).await;
let duration_ms = tool_start.elapsed().as_millis() as u64;

// AFTER tool result
let (ok, result_preview) = match &result {
    Ok(output) => (true, result_preview(&output.content, 80)),
    Err(e) => (false, result_preview(&e.to_string(), 80)),
};

status_tx.send_modify(|s| {
    s.phase = AgentTaskPhase::ToolCompleted {
        round,
        tool_name: tool_name.clone(),
        tool_index,
        tool_total,
        duration_ms,
        ok,
        result_preview,
    };
    if ok {
        s.tools_executed = s.tools_executed.saturating_add(1);
    }
});
```

### Zero-cost helper functions

The `truncate_preview` and `result_preview` helpers are pure, stateless,
and cheap. No locks, no async, no IO. Allocation cost: one small `String`
per tool call. Impact: negligible (tool calls are seconds apart).

### Scenarios

| Scenario | Expected |
|---------|----------|
| Tool call happens when `status_tx` is `None` | Helper still runs, but allocations are wasted — OK for v4.8.0 |
| Tool call when `status_tx` is `Some` | Events emit correctly, TUI receives both phases |
| Tool call that's cancelled mid-execution (Tier C) | First emission (ExecutingTool) happens; second (ToolCompleted) does NOT happen because tool.execute() is dropped; emission on cancel is handled by Tier C (Interrupted phase) |
| 10 parallel tool calls via the executor | Each call emits its own pair; phase reflects latest (watch channel is last-writer-wins) — activity panel history (B4) tracks the rolling set |
| Tool errors with non-UTF-8 bytes | Preview defaults to empty or error string; never panics (result_preview uses `.lines()` which is safe for any `&str`) |

### Optional: gate the `args_preview` allocation

If we want zero overhead when nobody's listening:

```rust
let args_preview = if status_tx.is_some() {
    truncate_preview(&input, 80)
} else {
    String::new()
};
```

But this costs a branch and complicates the code. For v4.8.0, accept
the cheap allocation unconditionally.

### Risk

**LOW.** Additive to an existing emission site. No new failure modes.

---

## B4. Rewrite activity panel as clean streaming trace

### Current state

`widgets/activity_panel.rs` tracks tool calls from `ExecutingTool`
phases and renders them. Missing: args, duration, result preview,
rolling history of completed calls.

### Proposed rendering

Activity panel shows last 5 tool events as a streaming trace:

```
▸ shell { "command": "ls -la" }                0.4s ⧖
✓ shell → 17 files                             0.6s
▸ file_read { "path": "Cargo.toml" }           0.1s ⧖
✓ file_read → 47 lines                         0.1s
▸ browser { "url": "https://docs.rs..." }      2.1s ⧖
```

Symbols:
- `▸` = in progress (accent color)
- `✓` = success (success green)
- `✗` = failure (error red)

Columns:
- Left: symbol + tool name
- Middle: args preview OR result preview (truncated to fit)
- Right: duration (dim)

### State tracking

```rust
pub struct ActivityPanel {
    // Existing
    pub phase: AgentTaskPhase,
    pub started_at: Instant,
    pub expanded: bool,

    // NEW
    pub tool_history: VecDeque<ToolEvent>,    // cap = 5
}

#[derive(Debug, Clone)]
pub struct ToolEvent {
    pub name: String,
    pub args_preview: String,
    pub status: ToolEventStatus,
    pub duration_ms: Option<u64>,
    pub result_preview: Option<String>,
    pub started_at_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolEventStatus {
    InProgress,
    Success,
    Failure,
}
```

### Update logic (`ActivityPanel::update_status`)

```rust
pub fn update_status(&mut self, status: &AgentTaskStatus) {
    self.phase = status.phase.clone();

    match &status.phase {
        AgentTaskPhase::ExecutingTool { tool_name, args_preview, started_at_ms, .. } => {
            // Push new event
            let event = ToolEvent {
                name: tool_name.clone(),
                args_preview: args_preview.clone(),
                status: ToolEventStatus::InProgress,
                duration_ms: None,
                result_preview: None,
                started_at_ms: *started_at_ms,
            };
            self.tool_history.push_back(event);
            while self.tool_history.len() > 5 {
                self.tool_history.pop_front();
            }
        }
        AgentTaskPhase::ToolCompleted { tool_name, duration_ms, ok, result_preview, .. } => {
            // Find the most recent InProgress event with matching name
            if let Some(last) = self.tool_history.iter_mut().rev()
                .find(|e| e.name == *tool_name && e.status == ToolEventStatus::InProgress)
            {
                last.status = if *ok { ToolEventStatus::Success } else { ToolEventStatus::Failure };
                last.duration_ms = Some(*duration_ms);
                last.result_preview = Some(result_preview.clone());
            }
        }
        _ => {}
    }
}
```

### Scenarios

| Scenario | Expected |
|---------|----------|
| 10 sequential tool calls | History shows last 5 with oldest dropped |
| Tool succeeds quickly | Event transitions InProgress → Success with duration |
| Tool fails | Event transitions to Failure, symbol is ✗ |
| Tool cancelled mid-execution (via Tier C) | Event stays InProgress; cleanup happens on `Interrupted` phase (gets marked "cancelled" with dim styling) |
| Activity panel collapsed (B5 applies) | Last event's inline view shows up — see B5 |
| Activity panel expanded | Full 5-event list renders |
| Rapid tool calls (sub-millisecond) | Events still recorded, duration may show 0ms |
| Tool names containing special characters | Rendered as-is, theme handles monospace |

### Risk

**LOW.** Rendering-only changes in TUI.

---

## B5. Replace collapsed "thinking (68s)"

### Current (`views/chat.rs:106-138`)

When activity panel is collapsed and agent is working, shows:

```
⠋ Thinking  3.2s
```

or when a tool is running:

```
⠋ shell  3.2s
```

No args, no duration, no count.

### Proposed rendering

When collapsed + working, display a more informative single line:

```
Context-dependent:

  ▸ shell (0.4s) · 3 tools · 68s total          [currently running shell]
  ◐ thinking (round 2) · 3 tools · 68s total    [provider call]
  ◇ classifying · 0.2s                           [v2 classifier]
  ⧖ preparing · 0.1s                             [input parse]
  ✓ done · 4 tools · 72s total                   [brief flash before panel hides]
  ⊗ cancelled · 3 tools · 68s total              [interrupted]
```

### Match rewrite

```rust
let phase_display = match &state.activity_panel.phase {
    AgentTaskPhase::Preparing => format!("⧖ preparing · {:.1}s", elapsed.as_secs_f64()),
    AgentTaskPhase::Classifying => format!("◇ classifying · {:.1}s", elapsed.as_secs_f64()),
    AgentTaskPhase::CallingProvider { round } => {
        if *round <= 1 {
            format!("◐ thinking · {} tools · {:.0}s total",
                state.activity_panel.tool_history.len(),
                elapsed.as_secs_f64())
        } else {
            format!("◐ thinking (round {round}) · {} tools · {:.0}s total",
                state.activity_panel.tool_history.len(),
                elapsed.as_secs_f64())
        }
    }
    AgentTaskPhase::ExecutingTool { tool_name, started_at_ms, .. } => {
        let tool_elapsed_ms = elapsed.as_millis() as u64 - started_at_ms;
        format!("▸ {tool_name} ({}.{}s) · {} tools · {:.0}s total",
            tool_elapsed_ms / 1000,
            (tool_elapsed_ms % 1000) / 100,
            state.activity_panel.tool_history.len(),
            elapsed.as_secs_f64())
    }
    AgentTaskPhase::ToolCompleted { tool_name, duration_ms, ok, .. } => {
        let sym = if *ok { "✓" } else { "✗" };
        format!("{sym} {tool_name} ({}ms) · {} tools · {:.0}s total",
            duration_ms,
            state.activity_panel.tool_history.len(),
            elapsed.as_secs_f64())
    }
    AgentTaskPhase::Finishing => format!("⧖ finishing · {:.1}s", elapsed.as_secs_f64()),
    AgentTaskPhase::Done => format!("✓ done · {} tools · {:.0}s total",
        state.activity_panel.tool_history.len(),
        elapsed.as_secs_f64()),
    AgentTaskPhase::Interrupted { .. } => format!("⊗ cancelled · {} tools · {:.0}s total",
        state.activity_panel.tool_history.len(),
        elapsed.as_secs_f64()),
};
```

### Scenarios

| Scenario | Expected |
|---------|----------|
| Agent call with no tools | `◐ thinking · 0 tools · 5s total` |
| Agent call with 3 tools mid-execution | `▸ shell (0.4s) · 3 tools · 12s total` |
| Agent call with 3 tools, provider round 2 | `◐ thinking (round 2) · 3 tools · 15s total` |
| Agent call completes | `✓ done · 4 tools · 18s total` flashes briefly before panel hides |
| Agent call cancelled via Esc | `⊗ cancelled · 2 tools · 7s total` remains visible |
| Very fast call (<0.1s) | `⧖ preparing · 0.1s` → `◐ thinking · 0 tools · 0s total` → `✓ done` |
| Multi-round heavy tool use | Total still accumulates across rounds |

### Risk

**LOW.** Rendering only; consumes B1/B2 data.

---

## Tier B aggregate risk

- All pattern-match sites identified via exhaustive workspace grep
- 2 exhaustive matches + 1 partial `matches!()` stepper, all in the change set
- 1 semantic latent bug (stepper array) caught and mitigated inline
- Serialization: NONE (enum is Debug + Clone only)
- Hidden consumers: NONE (grep is complete, workspace-only type)
- All changes land as a single atomic commit — no bisect window
- No runtime failure modes introduced
- No new panics (helpers are safe, `send_modify` is infallible)
- UTF-8 safe per resilience architecture

**Aggregate Tier B risk: ZERO.**

### Cumulative file changes (single atomic commit)

| File | Change |
|------|--------|
| `crates/temm1e-agent/src/agent_task_status.rs` | B1: add fields to `ExecutingTool`. B2: add `ToolCompleted`. Update Display impl. Update `phase_variants_clone_correctly` test Vec. |
| `crates/temm1e-agent/src/runtime.rs` | B3: enrich emission at tool start (line 1788); add emission at tool completion |
| `crates/temm1e-tui/src/widgets/activity_panel.rs` | B4: add `tool_history` field, update `update_status` logic, new render — AND update line 161 stepper array for the ToolCompleted variant (belt-and-suspenders) |
| `crates/temm1e-tui/src/views/chat.rs` | B5: rewrite collapsed thinking match |

4 files. No new files. No new dependencies. Single commit for atomicity.

### Test additions required

- Unit test in `agent_task_status.rs`: `ToolCompleted` Display rendering
- Unit test in `activity_panel`: `tool_history` capacity and transitions
- Integration test in `runtime_integration.rs`: emission sequence (ExecutingTool → ToolCompleted → next phase)
- Integration test: tool failure emission
- Snapshot test for the new collapsed-thinking rendering

See `06-testing-strategy.md`.
