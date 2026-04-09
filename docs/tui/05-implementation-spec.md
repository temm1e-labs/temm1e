# 05 — Implementation Spec

**Purpose:** File-by-file exact change plan with line references and
code snippets. After user approval of tiers, this becomes the
implementation bible. Follow it in order.

---

## Change manifest

**Total files:** 18 modified + 4 new + 1 Cargo.toml

| Crate | Modified | New |
|-------|----------|-----|
| `temm1e-agent` | 2 files (`agent_task_status.rs`, `runtime.rs`) | 0 |
| `temm1e-tui` | 15 files | 4 files |
| `temm1e-tools` | 1 file (verification, possibly no change) | 0 |
| `crates/temm1e-tui/Cargo.toml` | ✓ | — |
| Workspace `Cargo.toml` | ✓ (version bump) | — |

---

## Commit ordering

Implement in this order. Each commit should pass `cargo check`, `clippy`,
`fmt`, and `test --workspace` independently.

| # | Commit | Tier | Files | Purpose |
|---|--------|------|-------|---------|
| 1 | `tui: fix empty command overlays` | A1 | `config_panel.rs`, `app.rs`, `lib.rs` | Critical bug fix |
| 2 | `tui: rewrite /help for v4.8.0` | A2 | `views/help.rs`, `commands/builtin.rs` | Remove /compact stub, refresh keybinds |
| 3 | `tui: git repo + branch in status bar` | A3 | `app.rs`, `lib.rs`, `widgets/status_bar.rs` | Git info capture + render |
| 4 | `tui: keybind hint bar` | A6 | `views/chat.rs`, `app.rs`, new `widgets/hint_bar.rs` | Context-sensitive hints |
| 5 | `tui: text selection copy mode (Alt+S)` | A4 | `app.rs`, `lib.rs`, `input/keybindings.rs`, `input/handler.rs` | Toggle mouse capture |
| 6 | `tui: code block yank (Ctrl+Y) + arboard` | A5 | `Cargo.toml`, `widgets/markdown.rs`, new `widgets/copy_picker.rs`, `app.rs`, `input/keybindings.rs` | Clipboard integration |
| 7 | `agent+tui: enriched tool phase events + streaming trace` | B1+B2+B3+B4+B5 | `agent_task_status.rs`, `runtime.rs`, `widgets/activity_panel.rs`, `views/chat.rs` | **Atomic Tier B commit.** Enum enrichment + runtime emission + activity panel rewrite + collapsed thinking rewrite, all in ONE commit. This includes the one-line semantic-bug fix at `activity_panel.rs:161` for the stepper array. Single commit ensures no bisect window. See `02-tier-b-zero-risk-report.md` §1.3-1.4 for rationale. |
| 10 | `tui: Escape + Ctrl+C fire existing interrupt flag` | C1+C2+C3+C4+C5 | `agent_bridge.rs`, `app.rs`, `lib.rs`, `widgets/activity_panel.rs` | **Single commit for all of Tier C.** Adds `interrupt_flag: Arc<AtomicBool>` to `AgentHandle`, resets at turn start, passes into `process_message()` `interrupt` parameter (currently None), Escape/Ctrl+C set it. Reuses existing runtime interrupt path at `runtime.rs:919-944`. ZERO runtime changes. See `03-tier-c-zero-risk-report.md`. |
| 14 | `tui: state indicator + context meter (D2, D5)` | D2, D5 | `widgets/status_bar.rs` | Polish picks |
| 15 | `tui: /tools history overlay (D3)` | D3 | new `views/tools_overlay.rs`, `commands/builtin.rs`, `app.rs` | Audit view |
| 16 | `release: v4.8.0` | — | `Cargo.toml`, `README.md`, `CHANGELOG.md`, `CLAUDE.md` | Release protocol |

(D1 and D4 optional, can be appended or deferred.)

---

## Detailed changes per file

### 1. `crates/temm1e-tui/Cargo.toml`

**Before:**

```toml
# existing dependencies...
```

**After:**

```toml
# Add:
arboard = { version = "3", default-features = false, features = ["wayland-data-control"] }
base64 = "0.22"  # for OSC 52 encoding
```

**Rationale:** `arboard` primary clipboard, `base64` for OSC 52 fallback.

---

### 2. `crates/temm1e-tui/src/views/config_panel.rs` (A1 fix)

**Replace entire file.** New signature takes `&AppState`.

```rust
//! Configuration overlay panels (model picker, keys, usage, status).

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use crate::app::AppState;
use crate::commands::registry::OverlayKind;
use temm1e_agent::agent_task_status::AgentTaskPhase;

pub fn render_config_overlay(
    kind: &OverlayKind,
    state: &AppState,
    area: Rect,
    buf: &mut Buffer,
) {
    let popup_width = 60.min(area.width.saturating_sub(4));
    let popup_height = 20.min(area.height.saturating_sub(4));
    let popup = centered_rect(popup_width, popup_height, area);

    Clear.render(popup, buf);

    let (title, lines) = match kind {
        OverlayKind::Config => (" Configuration ", render_config_lines(state)),
        OverlayKind::Keys => (" API Keys ", render_keys_lines(state)),
        OverlayKind::Usage => (" Usage ", render_usage_lines(state)),
        OverlayKind::Status => (" Status ", render_status_lines(state)),
        OverlayKind::ModelPicker => (" Models ", render_model_lines(state)),
        OverlayKind::Help => unreachable!("/help routed via views/help.rs"),
    };

    let block = Block::default()
        .title(title)
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(state.theme.accent);
    let inner = block.inner(popup);
    block.render(popup, buf);

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    para.render(inner, buf);
}

fn render_config_lines(state: &AppState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    lines.push(row("Provider", state.current_provider.as_deref().unwrap_or("(not set)"), state));
    lines.push(row("Model", state.current_model.as_deref().unwrap_or("(not set)"), state));
    lines.push(row("Mode", state.selected_mode.as_deref().unwrap_or("work"), state));
    lines.push(row("Terminal", &format!("{}x{}", state.terminal_size.0, state.terminal_size.1), state));
    lines.push(Line::from(""));
    lines.push(hint("Press Esc to close", state));
    lines
}

fn render_keys_lines(state: &AppState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    if state.api_keys_cache.is_empty() {
        lines.push(info("No API keys configured.", state));
        lines.push(Line::from(""));
        lines.push(info("Use /addkey to add one, or paste a key in chat.", state));
    } else {
        for entry in &state.api_keys_cache {
            let marker = if entry.is_active { "● " } else { "  " };
            let line = format!("{marker}{:<12} …{}", entry.provider, entry.fingerprint);
            let style = if entry.is_active { state.theme.accent } else { state.theme.text };
            lines.push(Line::from(Span::styled(format!("  {line}"), style)));
        }
    }
    lines.push(Line::from(""));
    lines.push(hint("Press Esc to close", state));
    lines
}

fn render_usage_lines(state: &AppState) -> Vec<Line<'static>> {
    let tc = &state.token_counter;
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    lines.push(row("Total input", &tc.total_input_tokens.to_string(), state));
    lines.push(row("Total output", &tc.total_output_tokens.to_string(), state));
    lines.push(row("Total cost", &format!("${:.4}", tc.total_cost_usd), state));
    lines.push(Line::from(""));
    lines.push(row("Current turn in", &tc.turn_input_tokens.to_string(), state));
    lines.push(row("Current turn out", &tc.turn_output_tokens.to_string(), state));
    lines.push(Line::from(""));
    lines.push(hint("Press Esc to close", state));
    lines
}

fn render_status_lines(state: &AppState) -> Vec<Line<'static>> {
    let panel = &state.activity_panel;
    let elapsed = panel.started_at.elapsed();
    let phase = match &panel.phase {
        AgentTaskPhase::Preparing => "Preparing".to_string(),
        AgentTaskPhase::Classifying => "Classifying".to_string(),
        AgentTaskPhase::CallingProvider { round } => format!("Thinking (round {round})"),
        AgentTaskPhase::ExecutingTool { tool_name, .. } => format!("Running {tool_name}"),
        AgentTaskPhase::ToolCompleted { tool_name, duration_ms, .. } => format!("Completed {tool_name} ({duration_ms}ms)"),
        AgentTaskPhase::Finishing => "Finishing".to_string(),
        AgentTaskPhase::Done => "Idle".to_string(),
        AgentTaskPhase::Interrupted { round } => format!("Cancelled at round {round}"),
    };
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    lines.push(row("State", if state.is_agent_working { "working" } else { "idle" }, state));
    lines.push(row("Phase", &phase, state));
    lines.push(row("Elapsed", &format!("{:.1}s", elapsed.as_secs_f64()), state));
    lines.push(row("Tools used", &state.token_counter.total_cost_usd.to_string(), state));
    lines.push(Line::from(""));
    lines.push(hint("Press Esc to close", state));
    lines
}

fn render_model_lines(state: &AppState) -> Vec<Line<'static>> {
    use temm1e_core::types::model_registry;
    let provider = state.current_provider.as_deref().unwrap_or("");
    let models = model_registry::for_provider(provider);
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    if models.is_empty() {
        lines.push(info(&format!("No models registered for provider '{provider}'."), state));
    } else {
        for model in models {
            let is_current = state.current_model.as_deref() == Some(model.id);
            let marker = if is_current { "● " } else { "  " };
            let line = format!("{marker}{:<30} ctx: {:>7} cost: ${:.2}/{:.2}",
                model.id, model.context_window, model.input_cost_per_1m, model.output_cost_per_1m);
            let style = if is_current { state.theme.accent } else { state.theme.text };
            lines.push(Line::from(Span::styled(format!("  {line}"), style)));
        }
    }
    lines.push(Line::from(""));
    lines.push(hint("Press Esc to close", state));
    lines
}

// --- helpers ---

fn row(label: &str, value: &str, state: &AppState) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {:<18}", label), state.theme.secondary),
        Span::styled(value.to_string(), state.theme.text),
    ])
}

fn hint(text: &str, state: &AppState) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), state.theme.secondary))
}

fn info(text: &str, state: &AppState) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), state.theme.info))
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.left() + (area.width.saturating_sub(width)) / 2;
    let y = area.top() + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
```

**NOTE**: the exact `model_registry::for_provider()` API and the
`ModelEntry` fields need to be verified against
`crates/temm1e-core/src/types/model_registry.rs` during implementation.
Field names `id`, `context_window`, `input_cost_per_1m`,
`output_cost_per_1m` are illustrative; adapt to actual field names.

---

### 3. `crates/temm1e-tui/src/app.rs` (multiple tiers)

**Add new state fields:**

```rust
// From A1 (keys cache)
pub api_keys_cache: Vec<ApiKeyEntry>,

// From A3 (git info)
pub git_info: Option<GitInfo>,

// From A4 (mouse toggle)
pub mouse_capture_enabled: bool,
pub needs_mouse_toggle: bool,

// From A5 (code blocks)
pub code_blocks: std::collections::VecDeque<CodeBlock>,

// From C4 (cancel)
pub pending_cancel: bool,

// From D3 (tool history)
pub tool_call_history: Vec<ToolCallRecord>,

// From D4 (scroll mode vim)
pub last_g_press: Option<std::time::Instant>,

// Turn counter for D3
pub current_turn: u32,
```

**Add supporting types (same file or new module):**

```rust
#[derive(Debug, Clone)]
pub struct ApiKeyEntry {
    pub provider: String,
    pub fingerprint: String,
    pub is_active: bool,
}

#[derive(Debug, Clone)]
pub struct GitInfo {
    pub repo_name: String,
    pub branch: String,
}

#[derive(Debug, Clone)]
pub struct CodeBlock {
    pub lang: String,
    pub text: String,
    pub line_count: usize,
}

#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub turn_number: u32,
    pub tool_name: String,
    pub args_preview: String,
    pub duration_ms: Option<u64>,
    pub ok: Option<bool>,
    pub result_preview: Option<String>,
}
```

**Initialize in `AppState::new()`:**

```rust
Self {
    // ... existing fields ...
    api_keys_cache: Vec::new(),
    git_info: None,
    mouse_capture_enabled: true,
    needs_mouse_toggle: false,
    code_blocks: std::collections::VecDeque::with_capacity(9),
    pending_cancel: false,
    tool_call_history: Vec::new(),
    last_g_press: None,
    current_turn: 0,
}
```

**Update Escape handler in `handle_key` (line 359-361):**

```rust
InputResult::Escape => {
    if state.overlay != Overlay::None {
        state.overlay = Overlay::None;
    } else if state.is_agent_working {
        state.pending_cancel = true;
    }
}
```

**Update `handle_user_submit` to increment `current_turn`:**

```rust
state.current_turn = state.current_turn.saturating_add(1);
// ... existing logic ...
```

**Add handler for `Ctrl+Y`:**

```rust
InputResult::YankCodeBlock => {
    if state.code_blocks.is_empty() {
        // Show "no code blocks" toast
        push_system_message(state, "No code blocks to copy");
    } else {
        state.overlay = Overlay::CopyPicker;
    }
}
```

**Add handler for `Alt+S`:**

```rust
InputResult::ToggleMouseCapture => {
    state.mouse_capture_enabled = !state.mouse_capture_enabled;
    state.needs_mouse_toggle = true;
}
```

**Update `Event::AgentStatus` handler** (around line 172) to populate `tool_call_history`:

```rust
Event::AgentStatus(status) => {
    state.activity_panel.update_status(&status);
    state.token_counter.turn_input_tokens = status.input_tokens;
    state.token_counter.turn_output_tokens = status.output_tokens;

    // NEW: populate D3 history
    match &status.phase {
        AgentTaskPhase::ExecutingTool { tool_name, args_preview, .. } => {
            state.tool_call_history.push(ToolCallRecord {
                turn_number: state.current_turn,
                tool_name: tool_name.clone(),
                args_preview: args_preview.clone(),
                duration_ms: None,
                ok: None,
                result_preview: None,
            });
        }
        AgentTaskPhase::ToolCompleted { tool_name, duration_ms, ok, result_preview, .. } => {
            if let Some(last) = state.tool_call_history.iter_mut().rev()
                .find(|r| r.tool_name == *tool_name && r.duration_ms.is_none())
            {
                last.duration_ms = Some(*duration_ms);
                last.ok = Some(*ok);
                last.result_preview = Some(result_preview.clone());
            }
        }
        _ => {}
    }

    if matches!(status.phase, AgentTaskPhase::Done) {
        state.is_agent_working = false;
    }
    state.needs_redraw = true;
}
```

**Overlay enum extension:**

```rust
pub enum Overlay {
    None,
    Help,
    Config(OverlayKind),
    CopyPicker,     // NEW (A5)
    Tools,          // NEW (D3)
}
```

---

### 4. `crates/temm1e-tui/src/lib.rs` (multiple tiers)

**Populate state at startup** (in `launch_tui`):

```rust
// Before event loop:
state.git_info = detect_git_info();
state.api_keys_cache = load_api_keys_cache();
```

**Helper functions (same file or new `git.rs`):**

```rust
fn detect_git_info() -> Option<GitInfo> {
    use std::process::Command;

    let toplevel = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !toplevel.status.success() {
        return None;
    }
    let path = String::from_utf8(toplevel.stdout).ok()?;
    let path = path.trim();
    let repo_name = std::path::Path::new(path)
        .file_name()?
        .to_string_lossy()
        .to_string();

    let branch = Command::new("git")
        .args(["branch", "--show-current"])
        .output()
        .ok()?;
    let mut branch_str = String::from_utf8(branch.stdout).ok()?.trim().to_string();

    // Detached HEAD fallback
    if branch_str.is_empty() {
        let symref = Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .output()
            .ok()?;
        branch_str = String::from_utf8(symref.stdout).ok()?.trim().to_string();
    }
    if branch_str.is_empty() {
        // Still empty — use short commit hash
        let head = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
            .ok()?;
        branch_str = format!("@{}", String::from_utf8(head.stdout).ok()?.trim());
    }

    Some(GitInfo { repo_name, branch: branch_str })
}

fn load_api_keys_cache() -> Vec<ApiKeyEntry> {
    // Call into temm1e_core::config::credentials::load_all() or equivalent
    // Return masked fingerprints
    // Empty Vec on failure
    Vec::new()  // placeholder during initial impl
}
```

**Handle `needs_mouse_toggle` after each TEA update:**

```rust
if state.needs_mouse_toggle {
    if state.mouse_capture_enabled {
        let _ = execute!(stdout, crossterm::event::EnableMouseCapture);
    } else {
        let _ = execute!(stdout, crossterm::event::DisableMouseCapture);
    }
    state.needs_mouse_toggle = false;
}
```

**Handle `pending_cancel`:**

```rust
if state.pending_cancel {
    if let Some(token) = agent_handle.current_turn_cancel.lock().unwrap().as_ref() {
        token.cancel();
    }
    state.pending_cancel = false;
    state.activity_panel.set_cancelling();
}
```

**Thread `&state` into overlay render dispatch:**

Find where `render_config_overlay(&kind, &state.theme, area, buf)` is
called. Change to `render_config_overlay(&kind, state, area, buf)`.

**Tick-based git refresh** (in event loop `Event::Tick`):

```rust
if tick_count % 10 == 0 {
    state.git_info = detect_git_info();
    state.needs_redraw = true;
}
```

---

### 5. `crates/temm1e-tui/src/widgets/status_bar.rs`

**Expand layout to 3 sections (left, center, right):**

```rust
pub fn render_status_bar(state: &AppState, area: Rect, buf: &mut Buffer) {
    let chunks = Layout::horizontal([
        Constraint::Length(30),   // left: state indicator (D2)
        Constraint::Min(20),      // center: model/provider/tokens/cost
        Constraint::Length(40),   // right: context meter (D5) + git (A3)
    ]).split(area);

    render_state_indicator(state, chunks[0], buf);       // D2
    render_model_and_usage(state, chunks[1], buf);       // existing
    render_right_section(state, chunks[2], buf);         // D5 + A3
}
```

**`render_state_indicator` (D2):**

```rust
fn render_state_indicator(state: &AppState, area: Rect, buf: &mut Buffer) {
    let (symbol, label, style) = match &state.activity_panel.phase {
        AgentTaskPhase::Preparing | AgentTaskPhase::Classifying =>
            ("◐", "preparing", state.theme.phase_active),
        AgentTaskPhase::CallingProvider { .. } =>
            ("◐", "thinking", state.theme.phase_active),
        AgentTaskPhase::ExecutingTool { tool_name, .. } => {
            let truncated = tool_name.chars().take(10).collect::<String>();
            ("◉", Box::leak(format!("tool:{truncated}").into_boxed_str()) as &str, state.theme.tool_running)
        }
        AgentTaskPhase::ToolCompleted { .. } =>
            ("◐", "thinking", state.theme.phase_active),
        AgentTaskPhase::Finishing =>
            ("⧖", "finishing", state.theme.phase_active),
        AgentTaskPhase::Done =>
            ("●", "idle", state.theme.secondary),
        AgentTaskPhase::Interrupted { .. } =>
            ("⊗", "cancelled", state.theme.error),
    };

    // Override: if not actually working, show idle
    if !state.is_agent_working && !matches!(state.activity_panel.phase, AgentTaskPhase::Interrupted { .. }) {
        // Render idle
        let line = Line::from(vec![
            Span::styled("● ", state.theme.secondary),
            Span::styled("idle", state.theme.secondary),
        ]);
        buf.set_line(area.left(), area.top(), &line, area.width);
        return;
    }

    let line = Line::from(vec![
        Span::styled(format!("{symbol} "), style),
        Span::styled(label.to_string(), style),
    ]);
    buf.set_line(area.left(), area.top(), &line, area.width);
}
```

**`render_right_section` (A3 + D5):**

```rust
fn render_right_section(state: &AppState, area: Rect, buf: &mut Buffer) {
    let mut spans = Vec::new();

    // D5: context meter
    if let Some(window) = context_window_for(&state.current_model) {
        let used = state.token_counter.turn_input_tokens as u64;
        let pct = ((used * 100) / window.max(1)).min(100) as u32;
        let filled = (pct as usize * 10) / 100;
        let meter: String = (0..10).map(|i| if i < filled { '▓' } else { '░' }).collect();
        let meter_style = if pct >= 95 {
            state.theme.error
        } else if pct >= 80 {
            state.theme.error.add_modifier(Modifier::DIM)
        } else {
            state.theme.secondary
        };
        spans.push(Span::styled(format!("{meter} {}% / {} ", pct, format_tokens(window)), meter_style));
    }

    // A3: git info
    if let Some(ref git) = state.git_info {
        spans.push(Span::styled("▣ ", state.theme.secondary));
        spans.push(Span::styled(git.repo_name.clone(), state.theme.accent));
        spans.push(Span::styled(" · ", state.theme.secondary));
        spans.push(Span::styled(git.branch.clone(), state.theme.text));
    }

    let line = Line::from(spans).alignment(Alignment::Right);
    buf.set_line(area.left(), area.top(), &line, area.width);
}

fn context_window_for(model: &Option<String>) -> Option<u64> {
    // Look up in temm1e_core::types::model_registry
    None  // placeholder; wire to registry during implementation
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 { format!("{:.1}M", n as f64 / 1_000_000.0) }
    else if n >= 1_000 { format!("{}k", n / 1_000) }
    else { n.to_string() }
}
```

---

### 6. `crates/temm1e-tui/src/widgets/hint_bar.rs` (NEW — A6)

```rust
//! Context-sensitive keybind hint bar.
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use crate::app::{AppState, Overlay};

pub fn render_hint_bar(state: &AppState, area: Rect, buf: &mut Buffer) {
    let hint = hint_for_state(state);
    let line = Line::from(Span::styled(
        format!(" {hint}"),
        state.theme.secondary.add_modifier(Modifier::DIM),
    ));
    buf.set_line(area.left(), area.top(), &line, area.width);
}

fn hint_for_state(state: &AppState) -> &'static str {
    match &state.overlay {
        Overlay::Help => "Esc close",
        Overlay::Config(_) => "Esc close",
        Overlay::CopyPicker => "1-9 copy · Esc cancel",
        Overlay::Tools => "↑↓ scroll · g/G top/bot · Esc close",
        Overlay::None => {
            if state.is_agent_working {
                "Esc cancel · ^O activity · ^C force-stop"
            } else if !state.mouse_capture_enabled {
                "SELECT MODE · Alt+S resume · select with mouse"
            } else if state.message_list.scroll_offset() > 0 {
                "SCROLL · G bottom · g top · Esc exit scroll"
            } else {
                "Enter submit · ^C stop · ^Y yank · Alt+S select · ^O activity · ? help"
            }
        }
    }
}
```

**Layout update in `views/chat.rs`:**

```rust
let chunks = Layout::vertical([
    Constraint::Min(3),
    Constraint::Length(thinking_height),
    Constraint::Length(input_height + 1),
    Constraint::Length(1),   // NEW: hint bar
    Constraint::Length(1),   // status bar
]).split(area);

// ... existing rendering for chunks[0..2] ...
crate::widgets::hint_bar::render_hint_bar(state, chunks[3], buf);
crate::widgets::status_bar::render_status_bar(state, chunks[4], buf);
```

---

### 7. `crates/temm1e-tui/src/widgets/copy_picker.rs` (NEW — A5)

```rust
//! Numbered code block copy picker overlay.
use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};
use crate::app::AppState;

pub fn render_copy_picker(state: &AppState, area: Rect, buf: &mut Buffer) {
    let popup_width = 80.min(area.width.saturating_sub(4));
    let popup_height = 16.min(area.height.saturating_sub(4));
    let popup = centered_rect(popup_width, popup_height, area);

    Clear.render(popup, buf);

    let block = Block::default()
        .title(" Yank Code Block ")
        .title_alignment(Alignment::Center)
        .borders(Borders::ALL)
        .border_style(state.theme.accent);
    let inner = block.inner(popup);
    block.render(popup, buf);

    let mut lines = vec![Line::from("")];
    for (i, block) in state.code_blocks.iter().rev().take(9).enumerate() {
        let num = i + 1;
        let preview: String = block.text
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("")
            .chars()
            .take(50)
            .collect();
        lines.push(Line::from(vec![
            Span::styled(format!("  {num}. "), state.theme.accent),
            Span::styled(format!("[{}] ", block.lang), state.theme.secondary),
            Span::styled(preview, state.theme.text),
            Span::styled(format!("  ({} lines)", block.line_count), state.theme.secondary),
        ]));
    }
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled("  Press 1-9 to copy · Esc to cancel", state.theme.secondary)));

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    para.render(inner, buf);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.left() + (area.width.saturating_sub(width)) / 2;
    let y = area.top() + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

pub fn copy_to_clipboard(text: &str) -> Result<(), String> {
    match arboard::Clipboard::new() {
        Ok(mut cb) => cb.set_text(text.to_string()).map_err(|e| e.to_string()),
        Err(_) => write_osc52(text).map_err(|e| e.to_string()),
    }
}

fn write_osc52(text: &str) -> std::io::Result<()> {
    use base64::{engine::general_purpose, Engine as _};
    use std::io::Write;
    let encoded = general_purpose::STANDARD.encode(text);
    let mut stdout = std::io::stdout();
    write!(stdout, "\x1b]52;c;{}\x07", encoded)?;
    stdout.flush()
}
```

**Picker key handler** (in `app.rs::handle_key`):

```rust
if matches!(state.overlay, Overlay::CopyPicker) {
    use crossterm::event::KeyCode;
    match key.code {
        KeyCode::Esc => state.overlay = Overlay::None,
        KeyCode::Char(c @ '1'..='9') => {
            let idx = (c as u8 - b'1') as usize;
            if let Some(block) = state.code_blocks.iter().rev().nth(idx).cloned() {
                match crate::widgets::copy_picker::copy_to_clipboard(&block.text) {
                    Ok(()) => push_system_message(state, &format!(
                        "Copied block {} ({}, {} lines)", idx + 1, block.lang, block.line_count
                    )),
                    Err(e) => push_system_message(state, &format!("Copy failed: {e}")),
                }
            }
            state.overlay = Overlay::None;
        }
        _ => {}
    }
    return;
}
```

---

### 8. `crates/temm1e-tui/src/widgets/markdown.rs`

**Add `CodeBlock` return value**. Change signature of `render_markdown_with_width`:

```rust
pub fn render_markdown_with_width(
    text: &str,
    // ... existing style params ...
    width: usize,
    code_blocks_out: &mut Vec<crate::app::CodeBlock>,   // NEW
) -> Vec<RenderedLine> {
    // ... existing rendering ...
    // When entering a code block (see line 85-89 area):
    code_blocks_out.push(crate::app::CodeBlock {
        lang: code_lang.clone(),
        text: code_text_accumulator.clone(),
        line_count: code_text_accumulator.lines().count(),
    });
    // ... rest of rendering ...
}
```

**Update all call sites** (grep `render_markdown_with_width`):

- `app.rs::finalize_streaming()` — add a local Vec, extend into `state.code_blocks`
- `app.rs::Event::AgentResponse` — same
- `app.rs::handle_user_submit` — user messages also have code blocks (optional; skip if simplification needed)

**Ring buffer management after push:**

```rust
while state.code_blocks.len() > 9 {
    state.code_blocks.pop_front();
}
```

---

### 9. `crates/temm1e-tui/src/input/keybindings.rs`

**Add new bindings:**

```rust
// Ctrl+Y for yank
KeyCode::Char('y') if key.modifiers == KeyModifiers::CONTROL => {
    return InputResult::YankCodeBlock;
}

// Alt+S for mouse toggle
KeyCode::Char('s') if key.modifiers == KeyModifiers::ALT => {
    return InputResult::ToggleMouseCapture;
}
```

### 10. `crates/temm1e-tui/src/input/handler.rs`

**Add new `InputResult` variants:**

```rust
pub enum InputResult {
    // ... existing ...
    YankCodeBlock,
    ToggleMouseCapture,
}
```

---

### 11. `crates/temm1e-tui/src/views/help.rs` (A2)

**Rewrite keybind section** to include all current + new bindings in
organized categories (Editing, Navigation, Copy & Cancel, Overlays,
Session). Also ensure commands come from registry and `/compact` is
removed from the registry.

---

### 12. `crates/temm1e-tui/src/commands/builtin.rs` (A2 + D3)

**Remove `/compact` stub.** Add `/tools` command:

```rust
Command {
    name: "tools",
    description: "Show tool call history for this session",
    handler: |_args, _ctx| CommandResult::ShowOverlay(OverlayKind::Tools),
},
```

**Add `OverlayKind::Tools` variant** in `commands/registry.rs`:

```rust
pub enum OverlayKind {
    ModelPicker,
    Config,
    Keys,
    Usage,
    Status,
    Help,
    Tools,     // NEW
}
```

---

### 13. `crates/temm1e-agent/src/agent_task_status.rs` (B1, B2)

**Update enum:**

```rust
pub enum AgentTaskPhase {
    Preparing,
    Classifying,
    CallingProvider { round: u32 },
    ExecutingTool {
        round: u32,
        tool_name: String,
        tool_index: u32,
        tool_total: u32,
        args_preview: String,
        started_at_ms: u64,
    },
    ToolCompleted {                    // NEW
        round: u32,
        tool_name: String,
        tool_index: u32,
        tool_total: u32,
        duration_ms: u64,
        ok: bool,
        result_preview: String,
    },
    Finishing,
    Done,
    Interrupted { round: u32 },
}
```

**Update Display impl:**

```rust
Self::ExecutingTool { round, tool_name, tool_index, tool_total, .. } => write!(
    f,
    "Running {tool_name} ({}/{tool_total}, round {round})",
    tool_index + 1
),
Self::ToolCompleted { tool_name, duration_ms, ok, .. } => {
    let sym = if *ok { "✓" } else { "✗" };
    write!(f, "{sym} {tool_name} ({duration_ms}ms)")
}
```

**Update existing tests** at lines 128-146 — the `phases` Vec in
`phase_variants_clone_correctly` needs the new variants and
`ExecutingTool` gets new fields.

---

### 14. `crates/temm1e-agent/src/runtime.rs` (B3 only — C2/C3 dropped)

**Tier C no longer touches runtime.rs.** The original plan was to wrap
provider and tool calls in `tokio::select!` with `cancel.cancelled()`.
Investigation revealed the existing `Arc<AtomicBool>` interrupt path
at lines 919-944 is already wired and production-tested via the
gateway worker's higher-priority-message preemption. Tier C reuses
that path instead. Runtime changes for Tier C: ZERO.

Only Tier B (B3) touches this file:

**Phase 1 (B3): Enrich tool start emission.**

At ~line 1783, add `args_preview` and `started_at_ms`:

```rust
let args_preview = truncate_preview(&input, 80);
let tool_start = Instant::now();

if let Some(tx) = status_tx.as_ref() {
    tx.send_modify(|s| {
        let elapsed_ms = s.started_at.elapsed().as_millis() as u64;
        s.phase = AgentTaskPhase::ExecutingTool {
            round,
            tool_name: tool_name.clone(),
            tool_index,
            tool_total,
            args_preview: args_preview.clone(),
            started_at_ms: elapsed_ms,
        };
    });
}
```

**Phase 2 (B3): Emit ToolCompleted after tool result.**

Immediately after the tool returns (inside the loop):

```rust
let duration_ms = tool_start.elapsed().as_millis() as u64;
let (ok, result_preview) = match &tool_result {
    Ok(output) => (true, result_preview_of(&output.content, 80)),
    Err(e) => (false, result_preview_of(&e.to_string(), 80)),
};

if let Some(tx) = status_tx.as_ref() {
    tx.send_modify(|s| {
        s.phase = AgentTaskPhase::ToolCompleted {
            round,
            tool_name: tool_name.clone(),
            tool_index,
            tool_total,
            duration_ms,
            ok,
            result_preview: result_preview.clone(),
        };
        if ok {
            s.tools_executed = s.tools_executed.saturating_add(1);
        }
    });
}
```

**(C2 and C3 dropped per the Tier C pivot.)**

**Helper functions at module scope:**

```rust
fn truncate_preview(input: &serde_json::Value, max_chars: usize) -> String {
    let s = serde_json::to_string(input).unwrap_or_else(|_| "{}".to_string());
    if s.chars().count() <= max_chars {
        return s;
    }
    let mut out: String = s.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn result_preview_of(text: &str, max_chars: usize) -> String {
    let first = text.lines().find(|l| !l.trim().is_empty()).unwrap_or("").trim();
    if first.chars().count() <= max_chars {
        return first.to_string();
    }
    let mut out: String = first.chars().take(max_chars.saturating_sub(1)).collect();
    out.push('…');
    out
}
```

---

### 15. `crates/temm1e-tui/src/agent_bridge.rs` (C1)

**Add `interrupt_flag` field to `AgentHandle`** — uses the existing
production `Arc<AtomicBool>` interrupt path (not `CancellationToken`):

```rust
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

pub struct AgentHandle {
    pub inbound_tx: mpsc::Sender<InboundMessage>,
    pub status_rx: watch::Receiver<AgentTaskStatus>,
    pub event_rx: mpsc::UnboundedReceiver<Event>,
    pub interrupt_flag: Arc<AtomicBool>,   // NEW
}
```

**In `spawn_agent`:**

```rust
let interrupt_flag = Arc::new(AtomicBool::new(false));
let interrupt_for_task = interrupt_flag.clone();

tokio::spawn(async move {
    while let Some(msg) = inbound_rx.recv().await {
        // CRITICAL: reset before each turn, otherwise a stale set
        // from the previous turn cancels the new one immediately
        interrupt_for_task.store(false, Ordering::Relaxed);

        let result = runtime.process_message(
            &msg,
            &mut session,
            Some(interrupt_for_task.clone()),   // ← was None, now the real flag
            None,                                // pending
            Some(reply_tx.clone()),
            Some(status_tx.clone()),
            None,                                // cancel (stays None — reserved for v4.9.0)
        ).await;

        // ... existing persistence logic ...
    }
});

AgentHandle {
    inbound_tx,
    status_rx,
    event_rx,
    interrupt_flag,
}
```

**Why this is ZERO risk:** the runtime already checks this flag at
`runtime.rs:927-944` (between rounds, polling). Production tested via
gateway worker higher-priority-message preemption. We are setting a
flag that the runtime already knows how to observe.

---

### 16. `crates/temm1e-tui/src/widgets/activity_panel.rs` (B4, C5)

- Add `tool_history: VecDeque<ToolEvent>` field with cap 5
- Add `is_cancelling: bool` field
- Add `set_cancelling(&mut self)` method
- Update `update_status()` to populate history and handle `Interrupted`
- Rewrite `render_lines()` to show streaming trace

(Full code in the Tier B report section B4.)

---

### 17. `crates/temm1e-tui/src/views/chat.rs` (A6, B5)

- Add hint bar row to layout
- Rewrite collapsed thinking match (B5)

(Full code in the Tier B report section B5.)

---

### 18. `crates/temm1e-tui/src/views/tools_overlay.rs` (NEW — D3)

New file for the `/tools` history overlay. Reads
`state.tool_call_history`, groups by turn, renders.

---

### 19. `crates/temm1e-tools/src/browser_session.rs` (verification only)

**Read this file during implementation** to verify that browser tool
context cleanup happens on drop. If yes, no change. If no, add an
explicit cleanup path in the Tier C cancellation branch.

---

### 20. Workspace `Cargo.toml`

Bump version to `4.8.0`:

```toml
[workspace.package]
version = "4.8.0"
```

---

### 21. `README.md` (release protocol)

Per `docs/RELEASE_PROTOCOL.md`:
- Version badge
- Hero metrics table
- Release timeline entry for v4.8.0
- Update example showing TUI highlights
- Crate count check (still 24)
- Test count check

### 22. `CHANGELOG.md`

v4.8.0 entry summarizing:
- Bug fix: empty command overlays
- Observability: streaming tool trace, enriched phase data
- Cancellation: Escape to stop Tem mid-task
- Developer UX: code block yank, git status, copy mode, hint bar
- Polish: state indicator, context meter, /tools history

### 23. `CLAUDE.md`

Update only if crate count or commands change materially.

---

## Line reference verification

Before each commit, re-verify the line references in this spec against
HEAD. Line numbers drift as commits land. Use the file+function+pattern
combination (not raw line numbers) as the source of truth.

---

## Dependency chain

```
A1 ─ independent (fix stub)
A2 ─ independent (rewrite help)
A3 ─ independent (git detect)
A4 ─ independent (mouse toggle)
A5 ─ depends on A4 (both add keybinds)
A6 ─ depends on A4, A5 (hint bar reads their state)

B1, B2 ─ independent enum changes
B3 ─ depends on B1, B2
B4 ─ depends on B3 (needs data)
B5 ─ depends on B4 (reads history len)

C1 ─ independent plumbing
C2 ─ depends on C1
C3 ─ depends on C1
C4 ─ depends on C1
C5 ─ depends on C2, C3, C4, B5

D1 ─ depends on B4
D2 ─ independent
D3 ─ depends on B3 (needs tool events)
D4 ─ independent
D5 ─ independent
```

Implementation order follows the commit sequence above, which respects
this dependency graph.
