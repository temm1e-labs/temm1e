# 01 — Tier A Zero-Risk Report (Pure TUI Polish)

**Risk tier:** ZERO — no changes to the agent loop, provider path, or
message handling. All changes confined to `temm1e-tui`.

**Items:** 6 (A1 is the newly discovered empty-commands bug)

| ID | Item | New / Original | Files touched |
|----|------|----------------|---------------|
| A1 | Fix empty command overlays | **NEW (bug fix)** | 2 |
| A2 | Rewrite `/help` | original | 1 |
| A3 | Git repo + branch in status bar | original | 3 |
| A4 | Text selection copy mode (`Alt+S` toggle) | original | 3 |
| A5 | Code block yank (`Ctrl+Y` picker + `arboard`) | original | 4 + 1 new + Cargo.toml |
| A6 | Keybind hint bar | original | 2 + 1 new |

---

## A1. Fix empty command overlays

### Current state

`crates/temm1e-tui/src/views/config_panel.rs:12-48` is a single stub:

```rust
pub fn render_config_overlay(kind: &OverlayKind, theme: &Theme, area: Rect, buf: &mut Buffer) {
    let popup = centered_rect(...);
    Clear.render(popup, buf);
    let title = match kind {
        OverlayKind::ModelPicker => " Model ",
        OverlayKind::Config => " Configuration ",
        OverlayKind::Keys => " API Keys ",
        OverlayKind::Usage => " Usage ",
        OverlayKind::Status => " Status ",
        OverlayKind::Help => " Help ",
    };
    // ...
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(format!("  {} panel", title.trim()), theme.text)),
        Line::from(""),
        Line::from(Span::styled("  Press Esc to close", theme.secondary)),
    ];
    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    para.render(inner, buf);
}
```

Function has no access to `AppState`. Same 4-line placeholder rendered
for every overlay kind. `/help` works because `app.rs:420-424` routes
`OverlayKind::Help` to `views/help.rs` separately.

### Proposed fix

**1. Split the function into per-kind renderers.** Change signature:

```rust
pub fn render_config_overlay(
    kind: &OverlayKind,
    state: &AppState,              // NEW
    area: Rect,
    buf: &mut Buffer,
) {
    match kind {
        OverlayKind::Config => render_config(state, area, buf),
        OverlayKind::Keys => render_keys(state, area, buf),
        OverlayKind::Usage => render_usage(state, area, buf),
        OverlayKind::Status => render_status(state, area, buf),
        OverlayKind::ModelPicker => render_model_picker(state, area, buf),
        OverlayKind::Help => unreachable!("/help is routed via views/help.rs"),
    }
}
```

**2. Per-overlay content:**

| Overlay | Reads from | Rendered content |
|---------|-----------|------------------|
| `/config` | `state.current_provider`, `state.current_model`, `state.selected_mode`, terminal size, theme mode | `Provider: anthropic · Model: claude-sonnet-4-6 · Mode: work · Terminal: 120x40 · Theme: auto` |
| `/keys` | new `state.api_keys_cache: Vec<(provider, masked_fingerprint)>` | list of configured providers with last-4-chars mask, a hint to use `/addkey` to add, current active provider highlighted |
| `/usage` | `state.token_counter` | input/output tokens, total cost, turn count, average tokens per turn |
| `/status` | `state.is_agent_working`, `state.activity_panel.phase`, elapsed time, `state.token_counter` | current phase, elapsed time, last tool, tools this session, current cost, session length |
| `/model` | `temm1e_core::types::model_registry` (static) + `state.current_model` | list of models for current provider, highlight active, show context window + input/output cost per 1M tokens |

**3. Minimal `AppState` additions (`app.rs`):**

```rust
// New field — populated once at startup from credentials file
pub api_keys_cache: Vec<ApiKeyEntry>,

// ApiKeyEntry struct in same file
#[derive(Debug, Clone)]
pub struct ApiKeyEntry {
    pub provider: String,           // "anthropic", "openai", ...
    pub fingerprint: String,        // last 4 chars of key, e.g. "...ab3c"
    pub is_active: bool,
}
```

Populate from `temm1e_core::config::credentials::load_all()` (or
equivalent) at TUI startup in `lib.rs:launch_tui()`. If the async call
is needed, do it BEFORE the event loop starts, cache the result in
`AppState`, and refresh on explicit `/refresh-keys` (out of scope for
v4.8.0, cache-on-startup is sufficient).

**4. Update call site in `lib.rs`:**

Find the render dispatch that calls `render_config_overlay(&kind, &state.theme, area, buf)` and change to pass `&state` instead of just `&state.theme`.

### Scenarios that must continue to work

| Scenario | Expected |
|---------|----------|
| `/help` opens | Shows real command list + keybinds (unchanged — routes to `views/help.rs`) |
| `/config` opens | Shows real config lines instead of "Config panel" |
| `/config` opens before any agent activity | Shows correct defaults, no panic |
| `/keys` opens with no keys configured | Shows "No API keys configured. Use /addkey to add one." |
| `/keys` opens with 1 key | Shows the one key with active marker |
| `/keys` opens with 3 keys across 3 providers | Shows all 3, active one highlighted |
| `/usage` opens before any turn | Shows "No usage yet" |
| `/usage` opens after 5 turns | Shows cumulative tokens, cost, turn count |
| `/status` opens while idle | Shows "Idle · tools used: N · cost: $X.XX" |
| `/status` opens while agent is thinking | Shows "CallingProvider round 2 · elapsed 3.2s · 1 tool used" |
| `/model` opens for anthropic | Lists claude models, current one highlighted |
| `/model` opens for an unknown provider | Shows "No models registered for provider X" |
| Open `/config`, press Esc | Overlay closes, no panic |
| Open `/config`, press `q` | `q` is a no-op, overlay stays open (current behavior at `app.rs:260-266` — only Esc closes overlays) |
| Resize terminal while overlay open | Overlay re-renders at new size, no panic |

### Risk scenarios (what could go wrong)

| Concern | Analysis | Mitigation |
|---------|----------|-----------|
| Reading `state.api_keys_cache` when credentials file doesn't exist | Cache is empty `Vec`, render shows "No keys" | Initialize to empty `Vec::new()`, not `Option` |
| Reading `state.token_counter` before any turn | Struct has zero defaults | Render shows zeros, that's fine |
| Reading model registry for a provider with no entries | `model_registry::for_provider()` returns empty slice | Show "no models" message |
| Overlay open during a resize that makes area < popup | `popup_width = 50.min(area.width.saturating_sub(4))` already guards | No change needed |
| Concurrent `update_status` mutating `AppState` while overlay renders | TEA update loop is single-threaded, renders happen between updates | No concern |
| AppState Debug output leaking API keys | `ApiKeyEntry::fingerprint` is only the last 4 chars | Safe; never store full key |

### Cross-platform

No platform-specific code. Pure in-memory state rendering.

### Risk rating

**ZERO.** All changes are additive. The only "removal" is the 4-line
placeholder, which already renders nothing useful. No existing
functional behavior depends on the placeholder.

---

## A2. Rewrite `/help`

### Current state

`crates/temm1e-tui/src/views/help.rs:12-74` renders commands dynamically
from `CommandRegistry::all_commands()`, but the keyboard shortcuts
section is hardcoded (see file). Shortcuts listed today:

- Enter, Shift+Enter, Tab, Ctrl+C, Ctrl+D, Ctrl+L, Ctrl+O, PageUp/Down, Esc

Missing from today's help (already exist in `input/keybindings.rs:59-96`):

- Ctrl+A (home), Ctrl+E (end), Ctrl+K (kill to end), Ctrl+U (kill to start)
- Arrow keys (nav)
- Shift+Up/Down (scroll)

Missing because they don't exist yet (landing in v4.8.0):

- Ctrl+Y (yank code block)
- Alt+S (select mode)
- Esc (cancel Tem mid-task)

Also: the command `/compact` is listed in the registry but is a stub.

### Proposed fix

Rewrite `views/help.rs` to:

1. Keep dynamic command enumeration via `CommandRegistry::all_commands()`.
2. Replace hardcoded shortcuts section with a structured list organized by category:
   - **Editing**: Enter, Shift+Enter, Ctrl+A/E/K/U, Backspace, arrow keys
   - **Navigation**: PageUp/PageDown, Shift+Up/Down, Ctrl+L (redraw)
   - **Copy & Cancel** (NEW section): Ctrl+Y (yank code block), Alt+S (select mode toggle), Esc (cancel Tem), Ctrl+C (interrupt / quit)
   - **Overlays**: Ctrl+O (activity panel), Esc (close overlay)
   - **Session**: Ctrl+D (quit on empty input), Tab (complete command)
3. Remove `/compact` from the command list **OR** implement it minimally (see below).
4. Sort commands alphabetically.
5. Add a footer hint: `Press any key to close · Type /help to reopen`.

### Decision: `/compact` status

`commands/builtin.rs:67-70`:

```rust
// Current stub:
CommandResult::DisplayMessage("Compact feature coming soon".to_string())
```

Options:

- **(a)** Remove from registry entirely in v4.8.0.
- **(b)** Implement minimally — clear `streaming_renderer`, reset `activity_panel`, keep message history.
- **(c)** Leave as-is with the "coming soon" message.

**Recommendation:** (a) — remove from the registry. The command surface
must match what works. Reintroduce in a future release with a real
implementation. This is consistent with the "no stubs" rule in
`feedback_no_stubs.md`.

### Scenarios

| Scenario | Expected |
|---------|----------|
| Press `?` or type `/help` | Overlay opens with all sections |
| Tab complete from `/h` | Completes to `/help` (registry-driven) |
| Tab complete from `/c` | Completes to `/config` or `/clear` (multi-match) — no `/compact` |
| Help shown on a 60x20 terminal | Content scrolls if too tall OR sections clamped — verify with existing layout logic in `views/help.rs` |
| Help shown on a 200x50 terminal | Content centers without stretching |

### Risk rating

**ZERO.** Text and registry changes only.

---

## A3. Git repo + branch in status bar

### Proposed approach

**Data capture** (`app.rs` or a new `widgets/git_info.rs`):

```rust
#[derive(Debug, Clone)]
pub struct GitInfo {
    pub repo_name: String,       // e.g. "skyclaw" — basename of toplevel
    pub branch: String,           // e.g. "tui-enhancement"
    pub is_detached: bool,        // true if branch detection returned empty
    pub is_dirty: Option<bool>,   // reserved for future; start as None
}

pub fn detect_git_info() -> Option<GitInfo> {
    // 1. git rev-parse --show-toplevel
    //    - exit 128 → not a git repo → None
    //    - spawn error (git missing) → None
    //    - success → capture path, take basename
    //
    // 2. git branch --show-current
    //    - empty string → detached HEAD; try `git symbolic-ref --short HEAD`
    //    - still empty → use short commit hash via `git rev-parse --short HEAD`
    //    - success → use output trimmed
    //
    // 3. Return Some(GitInfo)
}
```

Store in `AppState::git_info: Option<GitInfo>`, populated once at
startup. Refresh on `Tick` every 10 ticks (~5s) — captures branch
switches made in another terminal while TEMM1E is open.

**Rendering** (`widgets/status_bar.rs`):

Current status bar shows: model, provider, tokens, cost. Add a new
right-aligned section for git info when present:

```
  claude-sonnet-4-6 · anthropic           3142/1203 · $0.0432    ▣ skyclaw · tui-enhancement
```

If `git_info` is `None`: no gap, no indicator. Graceful degradation.

Layout: use ratatui `Layout::horizontal` with 3 constraints (left / center / right) instead of the current 2-section layout.

### Edge cases (from cross-platform research)

| Case | Behavior |
|------|----------|
| Not in a git repo | `Option::None`, render nothing |
| `git` binary missing | spawn error → `None`, render nothing |
| Bare repo | rev-parse returns path, branch returns name — works |
| Git submodule | rev-parse returns submodule's root — correct for "where am I" intent |
| Git worktree | rev-parse returns worktree path — correct |
| Detached HEAD | `branch --show-current` returns empty → fallback to `symbolic-ref --short HEAD`; if that fails, show short commit hash with prefix `@` (e.g. `▣ skyclaw · @a1b2c3d`) |
| Empty repo (no commits) | rev-parse works; branch command may fail → show repo name only |
| Windows (PowerShell, cmd, Git Bash) | `git` CLI is identical across shells — works |
| WSL2 | works |
| Running outside of any directory we can resolve | `std::env::current_dir()` fallback — if that fails, `None` |

### Why not `gix`?

Adding `gix` (Rust-native git) would add ~2 min of compile time and
significant binary size for a feature that works perfectly with the
already-installed `git` CLI. If the user doesn't have git installed, they
don't need git info in the status bar. Shell-out is the right tradeoff.

### Scenarios that must not break

- Status bar renders without git info on non-repo directories
- Status bar renders correctly when terminal is narrow (<80 cols) — truncate git info before token counts, never the model name
- No tick-based git refresh blocks the event loop (use a bounded spawn or cache aggressively)

### Risk rating

**ZERO.** Additive UI with graceful degradation on failure.

### Secondary question: system prompt injection?

The original plan asked whether git info should also be injected into
the system prompt. **Recommendation: NO.**

Reasoning:
1. The `shell` tool can query git at any time — the agent already has the capability.
2. Injecting repo/branch into every system prompt bloats context for every turn.
3. The status bar serves the human's mental model; the agent doesn't need it as ambient context.

If the user wants the agent to know the repo context, they can type it
or ask a question like "what repo are we in?" and Tem will shell out.

---

## A4. Text selection copy mode (`Alt+S` toggle)

### Current state

`crates/temm1e-tui/src/lib.rs:114` unconditionally calls:

```rust
execute!(stdout, crossterm::event::EnableMouseCapture)
```

This tells the terminal "I own all mouse events" — the terminal emulator
then stops delivering mouse events to its own text-selection handler.
As a result, users can't select text with the mouse.

Disabled at `lib.rs:67` in `restore_terminal()` via `DisableMouseCapture`.

### Proposed approach

Add a toggle to switch mouse capture off/on without exiting the TUI.

**State** (`app.rs`):

```rust
pub mouse_capture_enabled: bool,   // starts true
```

**Key binding** (`input/keybindings.rs`):

Map `Alt+S` → `InputResult::ToggleMouseCapture` (new variant).

**Handler** (`app.rs::handle_key`):

```rust
InputResult::ToggleMouseCapture => {
    state.mouse_capture_enabled = !state.mouse_capture_enabled;
    state.needs_mouse_toggle = true;  // signals lib.rs event loop
}
```

**Side effect** (`lib.rs` event loop):

After each `update()` call, check `state.needs_mouse_toggle`. If set:

```rust
if state.mouse_capture_enabled {
    execute!(stdout, EnableMouseCapture)?;
} else {
    execute!(stdout, DisableMouseCapture)?;
}
state.needs_mouse_toggle = false;
```

**UI indication** (in hint bar from A6):

When `!mouse_capture_enabled`, the hint bar shows:
`SELECT MODE · mouse scroll disabled · Alt+S to resume`

### Scenarios

| Scenario | Expected |
|---------|----------|
| Press Alt+S while idle | Mouse capture disabled, hint bar updates |
| Press Alt+S again | Mouse capture re-enabled, hint bar reverts |
| Select text with mouse while disabled | Terminal's native selection works (Cmd+C on macOS, right-click on Linux, etc.) |
| Press PgUp while disabled | Keyboard scroll still works |
| Scroll wheel while disabled | Terminal may scroll its own buffer, TUI ignores it (not a bug — that's the point) |
| Exit TUI while disabled | `restore_terminal()` runs `DisableMouseCapture` (idempotent on an already-disabled state) |
| Crash while disabled | Panic hook restores terminal; mouse capture state irrelevant |
| tmux with `set -g mouse on` | tmux still intercepts; document as known limitation |

### Cross-platform matrix

| Terminal | Toggle works | Text selection after disable |
|---------|--------------|-----------------------------|
| macOS Terminal.app | ✓ | ✓ |
| iTerm2 | ✓ | ✓ |
| kitty | ✓ | ✓ |
| alacritty | ✓ | ✓ |
| WezTerm | ✓ | ✓ |
| Windows Terminal | ✓ | ✓ |
| gnome-terminal | ✓ | ✓ |
| VS Code terminal | ✓ (via host terminal) | ✓ |
| tmux | toggle works, but tmux has its own mouse mode | user must also disable tmux mouse or use tmux's copy mode |
| SSH sessions | toggle works, native selection flows through terminal | ✓ |

No regressions. tmux behavior documented in help.

### Risk rating

**ZERO.** Toggle-based, fully reversible, no existing behavior removed.

---

## A5. Code block yank (`Ctrl+Y` + `arboard` + OSC 52 fallback)

### Dependency added

`crates/temm1e-tui/Cargo.toml`:

```toml
arboard = { version = "3", default-features = false, features = ["wayland-data-control"] }
```

The `wayland-data-control` feature enables pure Wayland without XWayland
dependency. `default-features = false` avoids the `image` feature
(images aren't needed for text copy).

### Code block metadata capture

`widgets/markdown.rs` today extracts code block language (`code_lang` at
line 85-89) and syntax-highlights the content inline. The raw text is
lost after highlighting.

Change: accumulate a `Vec<CodeBlock>` during rendering:

```rust
// In widgets/markdown.rs module scope
#[derive(Debug, Clone)]
pub struct CodeBlock {
    pub lang: String,         // e.g. "rust", or "" if unspecified
    pub text: String,         // raw, pre-highlight
    pub line_count: usize,
}
```

`render_markdown_with_width()` returns `(Vec<RenderedLine>, Vec<CodeBlock>)`
OR accepts a `&mut Vec<CodeBlock>` for accumulation. Preferred:
accumulate into a caller-provided Vec to avoid changing the public return
type.

### Ring buffer in `AppState`

```rust
pub code_blocks: VecDeque<CodeBlock>,  // cap = 9
```

After each message render, push new blocks onto the buffer. When length
exceeds 9, drop from the front. Indices 1-9 are assigned for display as
the picker renders (always "most recent = #1").

### Copy picker overlay

New file: `crates/temm1e-tui/src/widgets/copy_picker.rs`

```rust
pub fn render_copy_picker(
    blocks: &VecDeque<CodeBlock>,
    theme: &Theme,
    area: Rect,
    buf: &mut Buffer,
) {
    // Draw centered popup listing each block:
    //   1. [rust]     fn main() {  (first 50 chars)
    //   2. [python]   def foo():
    //   3. [bash]     echo "hi"
    //   ...
    // Press 1-9 to copy, Esc to cancel.
}
```

### Keybinding

`Ctrl+Y` → opens copy picker overlay:
- `Ctrl+Y` alone → open picker
- `1-9` while picker open → copy that block, close picker, show toast
- `Esc` → close without copying

### Clipboard write logic

```rust
fn copy_to_clipboard(text: &str) -> Result<(), String> {
    // Try arboard first.
    match arboard::Clipboard::new() {
        Ok(mut cb) => {
            cb.set_text(text.to_string()).map_err(|e| e.to_string())
        }
        Err(_) => {
            // Fall back to OSC 52 escape sequence.
            write_osc52(text).map_err(|e| e.to_string())
        }
    }
}

fn write_osc52(text: &str) -> std::io::Result<()> {
    use std::io::Write;
    let encoded = base64::encode(text);
    // 'c' = clipboard; 'p' = primary; use 'c' for universal selection
    let mut stdout = std::io::stdout();
    write!(stdout, "\x1b]52;c;{}\x07", encoded)?;
    stdout.flush()
}
```

On success, push a transient system message:
`Copied block 2 (rust, 47 lines)`.

### Cross-platform matrix

| Platform | arboard | OSC 52 fallback | Works? |
|---------|---------|-----------------|--------|
| macOS | ✓ native | unnecessary | ✓ |
| Windows 10/11 | ✓ native Win32 | unnecessary | ✓ |
| Linux X11 | ✓ | unnecessary | ✓ |
| Linux Wayland | ✓ (with feature flag) | fallback if flag missing | ✓ |
| WSL2 | ✓ (via X11 passthrough) | fallback | ✓ |
| Headless SSH | ✗ | ✓ if terminal supports OSC 52 | ✓ for alacritty, kitty, iTerm2, tmux (with `allow-passthrough`), Windows Terminal |
| GNOME Terminal over SSH | ✗ | ✗ (GNOME Terminal doesn't support OSC 52) | degraded — clipboard write fails silently; show error toast |
| Pure headless batch mode | ✗ | ✗ | not applicable — TUI isn't running in batch mode |

Document the limitation in `/help`.

### Scenarios

| Scenario | Expected |
|---------|----------|
| Ctrl+Y with no code blocks yet | Show toast: "No code blocks to copy" |
| Ctrl+Y with 1 block | Copy picker opens showing 1 entry |
| Ctrl+Y with 12 blocks | Picker shows most recent 9 |
| Press 1 in picker | Copies block #1, closes picker, shows success toast |
| Press 5 in picker when only 3 exist | No-op (or feedback "only 3 blocks available") |
| Press Esc in picker | Closes without copying |
| Clipboard write fails | Show error toast; do not crash |
| Rapid Ctrl+Y, Ctrl+Y | Second opens a fresh picker (idempotent) |
| Ctrl+Y during agent work | Picker opens; non-blocking |
| Messages cleared via `/clear` | `code_blocks` ring buffer should also clear |
| Terminal too narrow for picker | Popup sizing already guards (`popup_width = 50.min(area.width.saturating_sub(4))`) |
| Block contains unicode emoji and CJK | `arboard::set_text` handles UTF-8; no truncation needed; picker preview uses char_indices for safety |

### UTF-8 safety reminder

Per the resilience architecture in `MEMORY.md`, **never use `&text[..N]`
on user content**. The picker preview string must use
`text.chars().take(50).collect::<String>()` or equivalent.

### Risk rating

**ZERO.** New dependency, new overlay, new keybinding. No existing code
paths changed. Graceful degradation on clipboard failure.

---

## A6. Keybind hint bar

### Current state

Layout (`views/chat.rs:26-34`) has four sections: messages, activity,
input, status bar. No hint bar.

### Proposed layout

Insert a 1-line hint bar between the input area and the status bar:

```
Layout::Vertical [
    Constraint::Min(3),                    // Messages
    Constraint::Length(thinking_height),   // Activity / thinking
    Constraint::Length(input_height + 1),  // Input + border
    Constraint::Length(1),                 // Hint bar (NEW)
    Constraint::Length(1),                 // Status bar
]
```

### Content (context-aware)

| State | Hint text |
|-------|-----------|
| Idle | `Enter submit · ^C stop · ^Y yank · Alt+S select · ^O activity · ? help` |
| Agent working (`is_agent_working = true`) | `Esc cancel · ^O activity · ^C force-stop` |
| Overlay open (help/config/etc.) | `Esc close · ↑↓ nav` |
| Copy mode (`!mouse_capture_enabled`) | `SELECT MODE · Alt+S resume · select with mouse` |
| Scroll mode (`message_list.scroll_offset > 0`) | `SCROLL · G bottom · Esc exit scroll` |
| Copy picker open | `1-9 copy · Esc cancel` |
| Onboarding | `↑↓ nav · Enter confirm · Esc back` |

Use a helper: `fn hint_for_state(state: &AppState) -> &str` that branches
on state flags. Render with `theme.secondary.add_modifier(Modifier::DIM)`
so it doesn't compete with the status bar.

### Scenarios

| Scenario | Expected |
|---------|----------|
| Idle state | Shows idle hints |
| User submits message | Transitions to "agent working" hints |
| User presses Esc during agent work | Transitions to idle hints on cancel completion |
| Terminal too narrow (<60 cols) | Truncate hint text with ellipsis |
| Terminal at 200 cols | Hint text stays left-aligned, dim spaces fill to end |

### Risk rating

**ZERO.** New passive UI element.

---

## Tier A aggregate risk

All 6 items are additive or pure refactors within `temm1e-tui`. No item
changes the agent loop, provider path, or message handling. No
existing user flow is removed or regressed.

**Aggregate Tier A risk: ZERO.**

### Cumulative file changes

| File | Change |
|------|--------|
| `crates/temm1e-tui/src/views/config_panel.rs` | Rewrite per-kind rendering (A1) |
| `crates/temm1e-tui/src/views/help.rs` | Rewrite keybind section (A2) |
| `crates/temm1e-tui/src/views/chat.rs` | Add hint bar row to layout (A6) |
| `crates/temm1e-tui/src/app.rs` | Add state fields: `api_keys_cache`, `git_info`, `code_blocks`, `mouse_capture_enabled`, `needs_mouse_toggle`; handle new keybinds |
| `crates/temm1e-tui/src/lib.rs` | Populate state at startup, thread state into render dispatch, react to `needs_mouse_toggle` |
| `crates/temm1e-tui/src/widgets/status_bar.rs` | Add right-aligned git info section (A3) |
| `crates/temm1e-tui/src/widgets/markdown.rs` | Accumulate code blocks during render (A5) |
| `crates/temm1e-tui/src/widgets/hint_bar.rs` | NEW (A6) |
| `crates/temm1e-tui/src/widgets/copy_picker.rs` | NEW (A5) |
| `crates/temm1e-tui/src/input/keybindings.rs` | Add `Ctrl+Y`, `Alt+S` bindings |
| `crates/temm1e-tui/src/input/handler.rs` | Map new InputResult variants |
| `crates/temm1e-tui/src/commands/builtin.rs` | Remove `/compact` stub (A2) |
| `crates/temm1e-tui/Cargo.toml` | Add `arboard`, `base64` (A5) |

13 files total, 2 new files, 1 Cargo.toml change.
