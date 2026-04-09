# 00 — Findings

Synthesis of parallel research from three Explore agents over the
`temm1e-tui`, `temm1e-agent`, `temm1e-providers`, and `temm1e-tools`
crates. All claims are backed by file:line references verified against
HEAD.

## 1. Original problem inventory (from user)

| # | Problem | Status |
|---|---------|--------|
| 1 | `/help` in TUI is outdated | Confirmed — help text is partially hardcoded and does not reflect the actual command surface or new keybinds that will land in v4.8.0. |
| 2 | No git repo / branch display | Confirmed — status bar renders only model/provider/tokens/cost. |
| 3 | Cannot select text with mouse | Confirmed — root cause is `lib.rs:114` unconditionally calling `EnableMouseCapture`. |
| 4 | Code blocks lack a copy button | Confirmed — syntect highlighting happens at render time; raw text is discarded after rendering. |
| 5 | Observability weak (`thinking (68s)` is opaque) | Confirmed — the `ExecutingTool` phase carries only tool name, no args, no result, no duration. |
| 6 | Cannot stop Tem via Escape | Confirmed — but investigation revealed a production-tested `Arc<AtomicBool>` interrupt path at `runtime.rs:919-944`, already used by the gateway worker. Tier C reuses that. Ctrl+C in `app.rs:283` is also currently a lie (sets `is_agent_working = false` without stopping the agent); Tier C fixes both. |
| 7 | Want more polish | Addressed as Tier D (5 optional picks). |

## 2. Empty command overlays (CRITICAL BUG — newly discovered)

During research, the user reported a previously unknown bug:
**most slash commands open an empty panel**. Investigation confirmed the
root cause.

### Affected commands

| Command | Result today | Evidence |
|---------|--------------|----------|
| `/help` | ✓ renders correctly | Has a dedicated render path: `views/help.rs:12-74` reads from `CommandRegistry::all_commands()` and builds real content. |
| `/config` | ✗ empty box | `views/config_panel.rs:12-48` stub |
| `/keys` | ✗ empty box | same stub |
| `/usage` | ✗ empty box | same stub |
| `/status` | ✗ empty box | same stub |
| `/model` | ✗ empty box | same stub |
| `/compact` | not a stub, but a no-op | `commands/builtin.rs:67-70` returns `DisplayMessage` only |
| `/clear` | ✓ | `CommandResult::ClearChat` routes to `message_list.clear()` at `app.rs:427` |
| `/quit` | ✓ | routes to `should_quit = true` |

### Root cause

`crates/temm1e-tui/src/views/config_panel.rs:12`:

```rust
pub fn render_config_overlay(kind: &OverlayKind, theme: &Theme, area: Rect, buf: &mut Buffer) {
    // ... centered_rect, Clear, block/title by kind ...

    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            format!("  {} panel", title.trim()),   // ← renders "Config panel", "Keys panel", etc.
            theme.text,
        )),
        Line::from(""),
        Line::from(Span::styled("  Press Esc to close", theme.secondary)),
    ];
```

Two fatal properties:

1. **The function signature has no `&AppState`.** There is no data source
   it could possibly read from, even if the body were expanded.
2. **The body renders a hardcoded 4-line placeholder for every `OverlayKind`.**
   Only `/help` escapes this because it's routed to `views/help.rs`
   separately (`app.rs:420-424` branches on `OverlayKind::Help`).

### Why this wasn't caught earlier

- The 5 broken commands open a *visible* popup with a correct title, so
  from 10 feet away they look functional.
- No automated test asserts content inside the overlays.
- The help overlay works perfectly, setting a false baseline for the
  other overlays.

### Fix complexity

**Low.** Mechanical. All data already exists in `AppState`:

| Overlay | Data source | Status |
|---------|------------|--------|
| `/config` | `AppState::current_provider`, `current_model`, `selected_mode` | present |
| `/keys` | Credentials from `temm1e_core::config::credentials` | needs a small cache in `AppState` |
| `/usage` | `AppState::token_counter` | present |
| `/status` | `AppState::activity_panel`, `is_agent_working` | present |
| `/model` | Model registry from `temm1e_core::types::model_registry` | present (static) |

The fix is:
1. Change `render_config_overlay()` signature to take `&AppState`.
2. Branch on `kind` and render real content per overlay.
3. For `/keys` and `/model`, add the small state needed.
4. Update the caller (`lib.rs` render dispatch) to thread state through.

This fix is folded into Tier A as item **A1** because it's pure TUI work
with zero risk.

## 3. Load-bearing discoveries (what already exists)

Good news: most of the infrastructure needed for v4.8.0 is already
present. We are connecting wires, not laying new cable.

### 3.1 Activity panel and tool phase tracking

`crates/temm1e-tui/src/widgets/activity_panel.rs` already exists and
already consumes `AgentTaskStatus` phases via the watch channel. The
toggle keybind is `Ctrl+O` (`input/keybindings.rs`), the render method
is `render_lines()`, and it tracks tool-call state from `ExecutingTool`
phases (via a `ToolCallEntry` vec and a partial-match phase stepper).

The existing render is richer than the collapsed view suggests:
- Header line with elapsed time and total tokens
- 5-step phase stepper (Preparing → Classifying → Calling Provider → Executing Tools → Finishing)
- Per-tool entries with `▶`/`✓`/`✗` icons and per-tool elapsed time
- Optional output lines under each entry (already expandable)

What's missing:
- Args preview per tool
- Result preview per tool
- Duration in ms (currently uses `.1s` seconds)
- `ToolCompleted` transition rendering (the stepper freezes on `Executing Tools` when a tool finishes)

Tier B extends the data model and rewrites the stepper into a
streaming trace, so the existing code is a sound starting point.

### 3.2 `AgentTaskPhase::Interrupted { round }` variant

`crates/temm1e-agent/src/agent_task_status.rs:34-35`:

```rust
/// Interrupted by user or system.
Interrupted { round: u32 },
```

Already defined. Never emitted today. The TUI's collapsed-thinking match
at `views/chat.rs:108-121` already has a `AgentTaskPhase::Interrupted { .. }`
arm returning `"Interrupted"`. The Display impl at `agent_task_status.rs:92`
already prints `"Interrupted at round {round}"`. The type infrastructure
for Tier C is complete — we just need to emit it.

### 3.3 Existing `Arc<AtomicBool>` interrupt mechanism — FULLY WIRED

`crates/temm1e-agent/src/runtime.rs:919-944`:

```rust
// Tool-use loop
let mut interrupted = false;
loop {
    rounds += 1;

    // Check for preemption between rounds
    if let Some(ref flag) = interrupt {
        if flag.load(Ordering::Relaxed) {
            info!("Agent interrupted by higher-priority message after {} rounds", rounds - 1);
            if let Some(ref tx) = status_tx {
                tx.send_modify(|s| {
                    s.phase = AgentTaskPhase::Interrupted { round: rounds as u32 };
                });
            }
            interrupted = true;
            break;
        }
    }
    // ... provider + tool execution ...
}
// Post-loop:
// runtime.rs:2051 — already preserves Interrupted phase
if !matches!(s.phase, AgentTaskPhase::Interrupted { .. }) {
    s.phase = AgentTaskPhase::Done;
}
// runtime.rs:2062
let text = if interrupted { "Task stopped.".to_string() } else { ... };
```

**This is production-tested code.** The gateway worker at
`main.rs:2938, 4627` already uses this path for "higher-priority
message preemption" — when a new user message arrives while the agent
is processing a long task, the gateway sets the flag, the runtime
detects it at the next round boundary, and unwinds cleanly.

**Tier C's insight:** we don't need to add new cancellation code. We
just need to set the flag from the TUI on Escape. Runtime changes: zero.

Note: `process_message()` at `runtime.rs:362` ALSO accepts an unused
`cancel: Option<CancellationToken>` parameter (bound to `_cancel` at
line 375 with a comment "for future Phase 2 use"). That is reserved
for future fine-grained cancellation in v4.9.0 and is NOT used by
v4.8.0's Tier C.

### 3.4 Watch channel + 10 emission checkpoints

`tokio::sync::watch::Sender<AgentTaskStatus>` is created at
`runtime.rs:161`, 10 emission points exist in the agent loop, and the
TUI already subscribes at `crates/temm1e-tui/src/lib.rs:199`:

```rust
handle.status_rx.changed().await.ok();
Some(handle.status_rx.borrow().clone())
```

The event loop converts watch notifications into `Event::AgentStatus`
and delivers to `app.rs:172-181`:

```rust
Event::AgentStatus(status) => {
    state.activity_panel.update_status(&status);
    state.token_counter.turn_input_tokens = status.input_tokens;
    state.token_counter.turn_output_tokens = status.output_tokens;

    if matches!(status.phase, AgentTaskPhase::Done) {
        state.is_agent_working = false;
    }
    state.needs_redraw = true;
}
```

This is the observability backbone for Tier B. All enhancements plug in
here without new channels.

### 3.5 Session history is lock-free

`session.history: Vec<ChatMessage>` is NOT wrapped in a `Mutex` or
`RwLock`. Mutations are in-memory only until turn completion. Persistence
happens at `agent_bridge.rs:238` via `memory_clone.store(entry).await`
AFTER the turn returns `Ok`. This means cancelled turns automatically
roll back — a cancelled mid-turn state is never persisted.

This is a major de-risker for Tier C: no rollback logic is needed.

### 3.6 Shell tool cleanup is automatic

`tokio::process::Command` kills child processes on drop (SIGTERM on Unix,
`TerminateProcess()` on Windows). The shell tool at
`crates/temm1e-tools/src/shell.rs:79-87` does not retain the child
handle, so dropping the tool future is sufficient cleanup.

Browser tool is the exception — see Tier C analysis.

## 4. Missing infrastructure (what we need to add)

| Item | What | Scope |
|------|------|-------|
| `arboard` dependency | cross-platform clipboard (with OSC 52 fallback) | `crates/temm1e-tui/Cargo.toml` |
| Code block metadata tracking | ring buffer of `(lang, raw_text)` during markdown render | `widgets/markdown.rs`, `app.rs` |
| `CancellationToken` in `AgentHandle` | field + fresh child per message | `agent_bridge.rs` |
| `ExecutingTool` enrichment | `args_preview` + `started_at_ms` fields | `agent_task_status.rs` |
| `ToolCompleted` phase variant | new enum variant with duration + result | `agent_task_status.rs` |
| Git info capture | one-shot shell-out at TUI startup + tick refresh | `app.rs` or `lib.rs` |
| Per-overlay renderers | split `config_panel.rs` by `OverlayKind` | `views/config_panel.rs` |
| Keybind hint bar | 1-line widget above status bar | `widgets/hint_bar.rs` (new) |
| Copy picker overlay | numbered block picker | `widgets/copy_picker.rs` (new) |
| API key cache in `AppState` | avoid async call in render path | `app.rs` |

## 5. Pattern match audit summary (for Tier B)

Full audit in [`02-tier-b-zero-risk-report.md`](./02-tier-b-zero-risk-report.md).

**Serialization check**: `AgentTaskPhase` is `#[derive(Debug, Clone)]` only.
NOT `Serialize`, NOT `Deserialize`, NOT `Hash`. Never persisted.
Zero persistence risk.

Exhaustive matches that MUST be updated (caught by rustc):

| File | Line | Why |
|------|------|-----|
| `crates/temm1e-agent/src/agent_task_status.rs` | 76 | `impl Display for AgentTaskPhase` — exhaustive, 7 arms |
| `crates/temm1e-tui/src/views/chat.rs` | 108 | collapsed "thinking" indicator — exhaustive, 7 arms |

**Latent semantic bug found (rustc cannot catch)**:

| File | Line | Pattern | Issue |
|------|------|---------|-------|
| `widgets/activity_panel.rs` | 149-164 | 5-entry `matches!()` stepper array | When `phase == ToolCompleted`, all 5 return false → stepper renders every phase as "done". Fix: add `ToolCompleted` to the "Executing Tools" row. |

The fix lands inline in the same atomic commit as the B1/B2 enum change.
B4 rewrites the render function entirely, providing a second safety net.

Single-variant `matches!()` with catch-all (safe — no update required):

| File | Line | Pattern |
|------|------|---------|
| `runtime.rs` | 2051 | `matches!(_, Interrupted { .. })` |
| `app.rs` | 177 | `matches!(_, Done)` |
| `tests/runtime_integration.rs` | 210,241,265,333 | `matches!(_, Done)` |
| `agent_task_status.rs` | 111, 123, 161 | test matches! |

`if let ExecutingTool { .., .. }` destructures at `activity_panel.rs:83`
use `..` and tolerate new fields automatically.

**Risk: ZERO** after the inline fix + atomic commit strategy. Every
known failure mode is either compile-time-caught by rustc or
explicitly mitigated. Zero persistence, zero external consumers, zero
bisect window.

## 6. Cross-platform considerations

Full matrix in the individual tier reports. Summary:

| Concern | Platforms | Resolution |
|---------|-----------|-----------|
| `arboard` clipboard | macOS ✓, Windows ✓, Linux X11 ✓, Wayland (feature flag), headless ✗ | primary: arboard; fallback: OSC 52 escape sequence for SSH/headless |
| Git detection | Windows ✓ (git for Windows), macOS ✓, Linux ✓ | shell out to `git rev-parse --show-toplevel` + `git branch --show-current`; handle detached HEAD via `git symbolic-ref --short HEAD` fallback; gracefully show nothing if not in a repo |
| Mouse capture toggle | All major terminals ✓ | no known regressions; tmux has its own mouse setting (document limitation) |
| `CancellationToken` | all platforms ✓ | pure Rust, OS-independent |
| Shell tool kill on cancel | Unix SIGTERM, Windows TerminateProcess | both handled by `tokio::process::Command` on drop |

## 7. Release context

- Target version: **v4.8.0**
- Branch: `tui-enhancement`
- Release protocol: `docs/RELEASE_PROTOCOL.md` must be followed before push
- Self-test protocol: multi-turn CLI self-test from MEMORY.md is mandatory
- Current workspace has one unrelated modification (`crates/temm1e-cambium/tests/real_code_grow_test.rs`) which is out of scope and must not be touched

Next: see tier reports.
