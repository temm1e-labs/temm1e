# 04 — Tier D Zero-Risk Report (Polish Picks)

**Risk tier:** ZERO — all items are additive TUI enhancements with no
agent loop changes.

**Items:** 5 (optional; user selects 3-4 to ship in v4.8.0)

| ID | Item | Value | Files |
|----|------|-------|-------|
| D1 | Collapsible long tool outputs | Prevents activity panel from being dominated by one noisy shell command | 1 |
| D2 | Session state indicator in status bar | At-a-glance "what is Tem doing right now" signal | 1 |
| D3 | `/tools` command — tool call history overlay | Audit view for what Tem did this session | 2 (+ 1 new) |
| D4 | Scroll mode hint + `G`/`g` jump | Muscle memory from vim/less | 1 |
| D5 | Context window usage meter | Visual warning before hitting limits | 1 |

---

## D1. Collapsible long tool outputs

### Problem

A shell command like `find / -name '*.log'` can produce 100+ lines of
output. Today, a single noisy tool dominates the activity panel.

### Approach

Track `line_count` in `ToolEvent` (from Tier B). If the completed
event's `result_preview` covers more than 10 lines of content (or the
raw result is > 500 bytes), collapse it:

```
✓ shell → 243 lines [+expand]                    0.6s
```

Press `Space` (or some focus-aware key) to cycle focus through tool
events and expand the focused one:

```
✓ shell → 243 lines [-collapse]                  0.6s
     1: /var/log/system.log
     2: /var/log/install.log
     3: /var/log/daily.out
     [...240 more lines available]
```

### Scope constraints

- Only works when activity panel is expanded (`Ctrl+O`)
- Focus cursor uses `Tab` to cycle between events
- `Esc` exits focus mode
- Max 20 lines shown when expanded; rest is "[...N more lines available]"
- NO scrolling within a single expanded event (keep it simple)

### State

```rust
pub struct ActivityPanel {
    // existing fields
    pub focused_event: Option<usize>,    // index into tool_history
    pub expanded_event: Option<usize>,   // which event is currently expanded
}
```

### Risk

**ZERO.** Pure rendering.

---

## D2. Session state indicator in status bar

### Problem

When the activity panel is collapsed (Ctrl+O to toggle), the only
indication of what's happening is the collapsed thinking line. Users
want a persistent "is Tem idle / thinking / running tool X?" signal in
the permanent status bar.

### Approach

Left side of the status bar, next to the model name:

```
● idle             claude-sonnet-4-6 · anthropic           3142/1203 · $0.0432    ▣ skyclaw · tui
◐ thinking         claude-sonnet-4-6 · anthropic           3142/1203 · $0.0432    ▣ skyclaw · tui
◉ tool:shell       claude-sonnet-4-6 · anthropic           3142/1203 · $0.0432    ▣ skyclaw · tui
⊗ cancelled        claude-sonnet-4-6 · anthropic           3142/1203 · $0.0432    ▣ skyclaw · tui
```

States (from `AgentTaskPhase`):
- `Preparing` / `Classifying` → `◐ preparing`
- `CallingProvider` → `◐ thinking`
- `ExecutingTool { tool_name, .. }` → `◉ tool:{name}` (truncate to 10 chars)
- `ToolCompleted` → `◐ thinking` (transitioning to next provider call)
- `Finishing` → `⧖ finishing`
- `Done` → `● idle` (transition after render)
- `Interrupted` → `⊗ cancelled` (sticky until next turn)

Colors:
- `●` dim for idle
- `◐` accent for provider/preparing
- `◉` tool_running for active tool
- `⊗` warn for cancel

### Scenarios

| Scenario | Expected indicator |
|---------|-------------------|
| Idle, no messages sent | `● idle` |
| User submits message | `◐ preparing` → `◐ thinking` within milliseconds |
| Tool running | `◉ tool:shell` (truncated) |
| Cancel | `⊗ cancelled` for ~2s, then back to `● idle` on next message |
| After `/clear` | `● idle` |

### Risk

**ZERO.** Driven by existing `AgentTaskPhase` — no new data needed.

---

## D3. `/tools` command — tool call history overlay

### Problem

Users want to audit what Tem did during a session — which tools were
called, what were the arguments, how long did each take, success/fail.

### Approach

New slash command `/tools` opens an overlay listing all tool calls this
session (across all turns):

```
┌─ Tool Call History ─────────────────────────────────── 12 calls ─┐
│                                                                    │
│  turn 1                                                            │
│  ✓ shell        ls -la                          0.4s  17 files    │
│  ✓ file_read    Cargo.toml                      0.1s  47 lines    │
│                                                                    │
│  turn 2                                                            │
│  ✓ shell        git status                      0.3s  4 files     │
│  ✓ file_write   plan.md (2.1 KB)                0.2s  ok          │
│  ✗ shell        cargo check                     18.3s 12 errors   │
│                                                                    │
│  turn 3                                                            │
│  ✓ shell        cargo check                     4.2s  ok          │
│  ▸ browser      https://docs.rs/arboard          (running)         │
│                                                                    │
│  Press Esc to close · ↑↓ scroll                                    │
└────────────────────────────────────────────────────────────────────┘
```

### State

New field in `AppState`:

```rust
pub tool_call_history: Vec<ToolCallRecord>,

#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub turn_number: u32,
    pub tool_name: String,
    pub args_preview: String,
    pub started_at: std::time::Instant,
    pub duration_ms: Option<u64>,
    pub ok: Option<bool>,
    pub result_preview: Option<String>,
}
```

Populated from the same watch channel that feeds the activity panel.
`turn_number` incremented on each user submit.

### Overlay state

```rust
pub enum Overlay {
    None,
    Help,
    Config(OverlayKind),
    Tools,              // NEW
    CopyPicker,         // NEW (from A5)
}
```

And `OverlayKind::Tools` added to `commands/registry.rs` if we want it
routable through the config panel — or treat it as a separate top-level
overlay variant.

### Keybinds in the overlay

- `Esc` → close
- `↑ / ↓` → scroll (if list exceeds overlay height)
- `g / G` → jump to top / bottom
- `/` → (future: search by tool name)

### Risk

**ZERO.** Pure additive UI.

### Implementation cost

- 1 new slash command in `commands/builtin.rs`
- 1 new file `views/tools_overlay.rs` (or fold into `views/config_panel.rs` — prefer its own file for clarity)
- Track turn_number + append records in `app.rs::handle_user_submit()` and in the status watch handler

---

## D4. Scroll mode hint + `G` / `g` jump

### Problem

When users scroll back in the message list, there's no indicator that
they're in a scrollback state, and no fast way to jump to the latest
message.

### Approach

**Detection:** `state.message_list.scroll_offset > 0` means user is not
at bottom.

**Hint bar feedback** (from A6): when in scroll mode, show:
`SCROLL · G bottom · g top · Esc exit`

**New keybinds (in scroll mode only):**

- `G` (Shift+g) → `scroll_offset = 0` (jump to bottom)
- `g g` (double-press) → scroll to top (vim-style, requires state tracking)
- `Esc` → exit scroll mode (scroll to bottom)

### Vim-style `gg` implementation

Track `last_g_press: Option<Instant>` in `AppState`:

```rust
KeyCode::Char('g') if !shift => {
    if let Some(last) = state.last_g_press {
        if last.elapsed() < Duration::from_millis(500) {
            // double-g → top
            state.message_list.scroll_offset = state.message_list.messages.len();
            state.last_g_press = None;
        } else {
            state.last_g_press = Some(Instant::now());
        }
    } else {
        state.last_g_press = Some(Instant::now());
    }
}
```

### Scenarios

| Scenario | Expected |
|---------|----------|
| At bottom, no scroll offset | Normal hint bar, no scroll mode |
| PageUp pressed | Enter scroll mode, hint updates |
| `G` in scroll mode | Jump to bottom, exit scroll mode |
| `g g` within 500ms | Jump to top |
| `g` then wait 600ms then `g` | Two separate single-g presses (no-op each) |
| Input area has focus, user types "go" | `g` is routed to input, not scroll (scroll mode keys only apply when input is empty or a modifier distinguishes them) |

### Edge case: input vs scroll keys

We need to distinguish "g while typing a message" from "g to scroll". Options:

- **(a)** Scroll keys only fire when input is empty.
- **(b)** Require a modifier (e.g., `Alt+G`).
- **(c)** Toggle a "navigation mode" with a dedicated key.

**Recommendation: (a).** Simplest, most discoverable. Users already
understand that scroll keys apply only to an empty input.

### Risk

**ZERO.**

---

## D5. Context window usage meter

### Problem

Users don't know how close they are to the model's context limit. By
the time they see an error, they've already hit it.

### Approach

New status bar element, between the token counts and the git info:

```
  3142/1203 · $0.0432    ▓▓▓▓░░░░░░ 42% / 200k    ▣ skyclaw · tui
```

Or with a warning when >80%:

```
  3142/1203 · $0.0432    ▓▓▓▓▓▓▓▓░░ 82% / 200k ⚠    ▣ skyclaw · tui
```

### Data source

- Current `input_tokens` from `state.token_counter` — reflects the
  accumulated conversation context as seen by the last provider call
- Model's context window from `temm1e_core::types::model_registry` —
  already has per-model limits per memory ("v2.3.1: Model registry —
  per-model context window/output limits")

### Meter rendering

10 blocks wide:
- `▓` for filled blocks (usage)
- `░` for empty blocks (remaining)
- Fill percentage: `(input_tokens / context_window) * 10` rounded down

Color:
- Green (success) when < 50%
- Normal (accent) when 50-80%
- Warn (warn) when 80-95%
- Error (error) when >= 95%

### Scenarios

| Scenario | Expected |
|---------|----------|
| Fresh session, 0 tokens | `░░░░░░░░░░  0% / 200k` |
| Mid-session, 40k tokens on 200k model | `▓▓░░░░░░░░ 20% / 200k` |
| Close to limit, 190k / 200k | `▓▓▓▓▓▓▓▓▓░ 95% / 200k ⚠` (error color) |
| Unknown model / missing registry entry | Hide meter |
| Model registry has context window 1M | `0% / 1.0M` (abbreviate) |
| Narrow terminal (<100 cols) | Meter is dropped from status bar before git info |

### Risk

**ZERO.** Reads existing state.

---

## Tier D aggregate risk

**ZERO.** All 5 items are additive TUI enhancements that read existing
state and render new visual elements. No agent loop changes, no new
dependencies (arboard is already pulled in by Tier A).

### Cumulative files for all 5

| File | Items |
|------|-------|
| `widgets/activity_panel.rs` | D1 (focus + expansion) |
| `widgets/status_bar.rs` | D2, D5 |
| `views/chat.rs` | D4 (scroll mode detection) |
| `app.rs` | D1, D3, D4 state |
| `commands/builtin.rs` | D3 (/tools command) |
| `views/tools_overlay.rs` | D3 (new file) |
| `input/keybindings.rs` | D4 |

6 files touched + 1 new.

### Recommended picks

If the user wants to ship only 3 of 5, prioritize by developer value:

1. **D5 (context meter)** — highest value, prevents surprise errors
2. **D2 (state indicator)** — complements collapsed panel perfectly
3. **D3 (/tools history)** — audit view is a developer favorite

Deferrable to v4.8.1:
- D1 (collapsible outputs) — nice to have, rare pain point
- D4 (scroll jumps) — power user feature, vim muscle memory

Final decision left to user.
