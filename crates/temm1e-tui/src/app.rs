//! Application state and TEA update loop.

use chrono::Utc;

use temm1e_agent::agent_task_status::AgentTaskPhase;

use crate::commands::registry::{CommandContext, CommandRegistry, OverlayKind};
use crate::commands::CommandResult;
use crate::event::Event;
use crate::input::multiline::InputState;
use crate::onboarding::steps::{self, OnboardingStep};
use crate::streaming::renderer::StreamingRenderer;
use crate::streaming::token_counter::TokenCounter;
use crate::theme::Theme;
use crate::widgets::activity_panel::ActivityPanel;
use crate::widgets::copy_picker;
use crate::widgets::markdown::{render_markdown_with_width, RenderedLine};
use crate::widgets::message_list::{DisplayMessage, MessageList, MessageRole, TurnUsage};
use crate::widgets::select_list::SelectState;

/// Active screen / view state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Screen {
    Onboarding,
    Chat,
}

/// Overlay rendered on top of the current screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Overlay {
    None,
    Help,
    Config(OverlayKind),
    CopyPicker,
}

/// One configured provider's API key for the `/keys` overlay.
///
/// The `fingerprint` is the last 4 characters of the full key — safe to
/// display without leaking secrets.
#[derive(Debug, Clone)]
pub struct ApiKeyEntry {
    pub provider: String,
    pub fingerprint: String,
    pub is_active: bool,
}

/// Git repository info for the status bar.
#[derive(Debug, Clone)]
pub struct GitInfo {
    pub repo_name: String,
    pub branch: String,
}

/// A captured code block from rendered markdown, for the yank/copy picker.
#[derive(Debug, Clone)]
pub struct CodeBlock {
    pub lang: String,
    pub text: String,
    pub line_count: usize,
}

/// A completed or in-flight tool call record for the /tools history overlay.
#[derive(Debug, Clone)]
pub struct ToolCallRecord {
    pub turn_number: u32,
    pub tool_name: String,
    pub args_preview: String,
    pub duration_ms: Option<u64>,
    pub ok: Option<bool>,
    pub result_preview: Option<String>,
}

/// Active mouse selection, stored in LOGICAL content coordinates.
///
/// - `anchor.0` / `current.0` = column (terminal x, always 0..=width)
/// - `anchor.1` / `current.1` = absolute line index in the full
///   flattened rendered content (not terminal row). `i64` so the
///   anchor can sit above the viewport after the user scrolls past it.
///
/// `last_term_row` tracks the cursor's current terminal y so the
/// Tick handler can drive continuous auto-scroll while the mouse is
/// HELD at an edge (mouse drag events only fire on position
/// changes, so "hold at edge" would otherwise stall).
///
/// `is_dragging` is true between `Mouse::Down` and `Mouse::Up` — it
/// gates the tick-based auto-scroll so the scroll only advances
/// while the button is actually held.
#[derive(Debug, Clone)]
pub struct MouseSelection {
    pub anchor: (u16, i64),
    pub current: (u16, i64),
    pub last_term_row: u16,
    pub is_dragging: bool,
}

/// Layout + scroll context needed to convert between terminal row
/// coordinates and absolute content line indices. Computed fresh on
/// demand — cheap because `compute_message_area_bounds` is constant-
/// time and `MessageList::line_count` is O(messages).
#[derive(Debug, Clone, Copy)]
pub struct SelectionCtx {
    pub msg_top: u16,
    pub msg_bottom: u16,
    pub view_height: usize,
    pub total_lines: usize,
    pub view_start: usize,
}

impl SelectionCtx {
    /// Convert a terminal row to its absolute line index in the
    /// full flattened rendered content. Returns `i64` because the
    /// result can legitimately go negative (terminal row above the
    /// message area) or past the end.
    pub fn term_to_abs(&self, term_row: u16) -> i64 {
        self.view_start as i64 + (term_row as i64 - self.msg_top as i64)
    }

    /// Convert an absolute line index back to a terminal row, if
    /// and only if it is currently visible in the viewport.
    pub fn abs_to_term(&self, abs: i64) -> Option<u16> {
        let rel = abs - self.view_start as i64;
        if rel < 0 || rel >= self.view_height as i64 {
            return None;
        }
        Some(self.msg_top + rel as u16)
    }
}

/// Root application state (TEA model).
pub struct AppState {
    pub screen: Screen,
    pub overlay: Overlay,
    pub terminal_size: (u16, u16),

    // Chat
    pub message_list: MessageList,

    // Input
    pub input: InputState,

    // Agent
    pub is_agent_working: bool,
    pub activity_panel: ActivityPanel,

    // Streaming
    pub streaming_renderer: Option<StreamingRenderer>,

    // Token tracking
    pub token_counter: TokenCounter,

    // Config
    pub current_model: Option<String>,
    pub current_provider: Option<String>,
    pub selected_mode: Option<String>,

    // Cached config for overlays (populated at startup)
    pub api_keys_cache: Vec<ApiKeyEntry>,
    pub git_info: Option<GitInfo>,

    // Code blocks (A5 — yank picker)
    pub code_blocks: std::collections::VecDeque<CodeBlock>,

    // Mouse capture toggle (A4 — copy mode)
    pub mouse_capture_enabled: bool,
    pub needs_mouse_toggle: bool,

    // Tool call history (D3 — /tools overlay)
    pub tool_call_history: Vec<ToolCallRecord>,
    pub current_turn: u32,

    // Escape cancellation (C4)
    pub pending_cancel: bool,

    // Model hot-swap (bug 4 fix)
    pub pending_model_switch: Option<String>,

    // Mouse-driven native drag-to-select (post-hotfix polish)
    /// Current drag selection. Persists after Mouse::Up so the user
    /// can press Ctrl+C (or click elsewhere) to finalize. `None` when
    /// no active selection.
    pub mouse_selection: Option<MouseSelection>,
    /// Set by Ctrl+C when there is an active selection. The view
    /// layer picks it up on the next render, extracts the selected
    /// cell symbols, copies to clipboard, and clears the selection.
    pub pending_copy_selection: bool,
    /// A single-click position. Set on Mouse::Up when the user
    /// clicked without dragging. The view layer checks whether the
    /// clicked row is inside a rendered code block and, if so,
    /// copies the whole block.
    pub pending_code_click: Option<(u16, u16)>,
    /// Transient feedback shown in the hint bar after a copy. Cleared
    /// on the next mouse interaction.
    pub copy_feedback: Option<String>,

    // Commands
    pub command_registry: CommandRegistry,

    // Theme
    pub theme: Theme,

    // Onboarding
    pub onboarding_step: OnboardingStep,

    // Agent communication
    /// Set by update() when user submits a message; consumed by the event loop to send to agent.
    pub pending_user_message: Option<String>,
    /// API key from onboarding, held for async validation/save.
    pub onboarding_api_key: Option<String>,

    // Exit — Ctrl+C twice like Claude Code
    pub last_ctrl_c: Option<std::time::Instant>,

    // Flags
    pub needs_redraw: bool,
    pub needs_clear: bool,
    pub should_quit: bool,
}

impl AppState {
    pub fn new() -> Self {
        let theme = Theme::detect();
        Self {
            screen: Screen::Chat,
            overlay: Overlay::None,
            terminal_size: crossterm::terminal::size().unwrap_or((80, 24)),
            message_list: MessageList::new(),
            input: InputState::new(),
            is_agent_working: false,
            activity_panel: ActivityPanel::new(),
            streaming_renderer: None,
            token_counter: TokenCounter::new(),
            current_model: None,
            current_provider: None,
            selected_mode: None,
            api_keys_cache: Vec::new(),
            git_info: None,
            code_blocks: std::collections::VecDeque::with_capacity(9),
            // Default: mouse capture ON so the TUI owns the whole
            // terminal buffer (exclusive alt-screen, scroll wheel
            // works in the TUI, no scrollback bleed-through). Users
            // select text natively by holding the terminal's modifier
            // override: Shift on Linux/Windows, Option on macOS.
            // Alt+S toggles capture off for terminals that don't
            // support modifier-override.
            mouse_capture_enabled: true,
            needs_mouse_toggle: false,
            tool_call_history: Vec::new(),
            current_turn: 0,
            pending_cancel: false,
            pending_model_switch: None,
            mouse_selection: None,
            pending_copy_selection: false,
            pending_code_click: None,
            copy_feedback: None,
            command_registry: CommandRegistry::new(),
            theme,
            onboarding_step: OnboardingStep::Welcome,
            pending_user_message: None,
            onboarding_api_key: None,
            last_ctrl_c: None,
            needs_redraw: true,
            needs_clear: false,
            should_quit: false,
        }
    }

    /// Start with onboarding if no credentials are configured.
    pub fn with_onboarding(mut self) -> Self {
        self.screen = Screen::Onboarding;
        self.onboarding_step = OnboardingStep::Welcome;
        self
    }

    /// Start with chat if credentials exist.
    pub fn with_chat(mut self, provider: String, model: String) -> Self {
        self.screen = Screen::Chat;
        self.current_provider = Some(provider);
        self.current_model = Some(model);
        self
    }

    fn command_context(&self) -> CommandContext {
        CommandContext {
            current_model: self.current_model.clone().unwrap_or_default(),
            current_provider: self.current_provider.clone().unwrap_or_default(),
        }
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}

/// TEA update function — processes an event and mutates state.
pub fn update(state: &mut AppState, event: Event) {
    match event {
        Event::Terminal(crossterm::event::Event::Resize(w, h)) => {
            state.terminal_size = (w, h);
            state.needs_clear = true;
            state.needs_redraw = true;
        }
        Event::Terminal(crossterm::event::Event::Key(key)) => {
            if key.kind != crossterm::event::KeyEventKind::Press {
                return;
            }
            handle_key(state, key);
            state.needs_redraw = true;
        }
        Event::Terminal(crossterm::event::Event::Mouse(mouse)) => {
            use crossterm::event::{MouseButton, MouseEventKind};
            match mouse.kind {
                // ── Drag-to-select (native selection inside the TUI) ─
                //
                // Selection is stored in ABSOLUTE content line index,
                // not terminal row, so scrolling the list moves the
                // highlight with the content instead of making it
                // "slide off" the previous selection.
                MouseEventKind::Down(MouseButton::Left) => {
                    let ctx = compute_selection_ctx(state);
                    let abs = ctx.term_to_abs(mouse.row);
                    state.mouse_selection = Some(MouseSelection {
                        anchor: (mouse.column, abs),
                        current: (mouse.column, abs),
                        last_term_row: mouse.row,
                        is_dragging: true,
                    });
                    state.copy_feedback = None;
                    state.needs_redraw = true;
                }
                MouseEventKind::Drag(MouseButton::Left) => {
                    if state.mouse_selection.is_some() {
                        // Update current to the new cursor position
                        let ctx = compute_selection_ctx(state);
                        let abs = ctx.term_to_abs(mouse.row);
                        if let Some(ref mut sel) = state.mouse_selection {
                            sel.current = (mouse.column, abs);
                            sel.last_term_row = mouse.row;
                        }

                        // Immediate auto-scroll at the edges. Works
                        // on every drag event; the Tick handler adds
                        // continuous scrolling while the mouse is
                        // HELD still at the edge without firing new
                        // drag events.
                        if mouse.row <= ctx.msg_top {
                            state.message_list.scroll_up(1);
                            // Recompute current_abs in the new view
                            let new_ctx = compute_selection_ctx(state);
                            let new_abs = new_ctx.term_to_abs(mouse.row);
                            if let Some(ref mut sel) = state.mouse_selection {
                                sel.current = (mouse.column, new_abs);
                            }
                        } else if mouse.row + 1 >= ctx.msg_bottom {
                            state.message_list.scroll_down(1);
                            let new_ctx = compute_selection_ctx(state);
                            let new_abs = new_ctx.term_to_abs(mouse.row);
                            if let Some(ref mut sel) = state.mouse_selection {
                                sel.current = (mouse.column, new_abs);
                            }
                        }
                        state.needs_redraw = true;
                    }
                }
                MouseEventKind::Up(MouseButton::Left) => {
                    if let Some(sel) = state.mouse_selection.as_ref() {
                        if sel.anchor == sel.current {
                            // Pure click with no drag — clear the
                            // selection and record the click for the
                            // view to try the code-block-copy path.
                            state.mouse_selection = None;
                            state.pending_code_click = Some((mouse.column, mouse.row));
                        } else if let Some(ref mut sel_mut) = state.mouse_selection {
                            // Real drag — end the drag but KEEP the
                            // selection highlighted so the user can
                            // press Ctrl+C to finalize.
                            sel_mut.is_dragging = false;
                        }
                        state.needs_redraw = true;
                    }
                }
                // ── Scroll wheel (unchanged) ─────────────────────────
                MouseEventKind::ScrollUp => {
                    state.message_list.scroll_up(3);
                    state.needs_redraw = true;
                }
                MouseEventKind::ScrollDown => {
                    state.message_list.scroll_down(3);
                    state.needs_redraw = true;
                }
                _ => {}
            }
        }
        Event::AgentStatus(status) => {
            state.activity_panel.update_status(&status);
            state.token_counter.turn_input_tokens = status.input_tokens;
            state.token_counter.turn_output_tokens = status.output_tokens;

            // D3 — record tool call events into the /tools history
            match &status.phase {
                AgentTaskPhase::ExecutingTool { tool_name, .. } => {
                    // Only push if this is a new call (dedupe by tool_name matching last record)
                    let should_push = state
                        .tool_call_history
                        .last()
                        .map(|r| r.tool_name != *tool_name || r.duration_ms.is_some())
                        .unwrap_or(true);
                    if should_push {
                        state.tool_call_history.push(ToolCallRecord {
                            turn_number: state.current_turn,
                            tool_name: tool_name.clone(),
                            args_preview: String::new(),
                            duration_ms: None,
                            ok: None,
                            result_preview: None,
                        });
                    }
                }
                AgentTaskPhase::Interrupted { .. } => {
                    // Mark the most recent in-flight tool as cancelled
                    if let Some(last) = state
                        .tool_call_history
                        .iter_mut()
                        .rev()
                        .find(|r| r.duration_ms.is_none())
                    {
                        last.duration_ms = Some(0);
                        last.ok = Some(false);
                        last.result_preview = Some("[cancelled]".to_string());
                    }
                }
                _ => {}
            }

            if matches!(status.phase, AgentTaskPhase::Done) {
                state.is_agent_working = false;
            }
            state.needs_redraw = true;
        }
        Event::StreamChunk(chunk) => {
            if let Some(renderer) = &mut state.streaming_renderer {
                renderer.push(&chunk.delta);
            }
            if chunk.done {
                finalize_streaming(state);
            }
            state.needs_redraw = true;
        }
        Event::AgentResponse(response) => {
            // Record usage (only if this is a real response, not an early reply)
            let is_early_reply = response.input_tokens == 0
                && response.output_tokens == 0
                && response.cost_usd == 0.0;

            if !is_early_reply {
                state.token_counter.record_turn(
                    response.input_tokens,
                    response.output_tokens,
                    response.cost_usd,
                );
            }

            // Display the response (skip if empty)
            if !response.message.text.trim().is_empty() {
                // Capture code blocks for the yank picker (A5)
                for (lang, text, line_count) in
                    crate::widgets::markdown::extract_code_blocks(&response.message.text)
                {
                    state.code_blocks.push_back(CodeBlock {
                        lang,
                        text,
                        line_count,
                    });
                    while state.code_blocks.len() > 9 {
                        state.code_blocks.pop_front();
                    }
                }

                let lines = render_markdown_with_width(
                    &response.message.text,
                    state.theme.text,
                    state.theme.heading,
                    state.theme.code_bg,
                    state.theme.info,
                    state.theme.secondary,
                    state.terminal_size.0 as usize,
                );
                // Early replies don't get usage stats
                let usage = if is_early_reply {
                    None
                } else {
                    Some(TurnUsage {
                        input_tokens: response.input_tokens,
                        output_tokens: response.output_tokens,
                        cost_usd: response.cost_usd,
                        elapsed_ms: 0,
                    })
                };
                state.message_list.push(DisplayMessage {
                    role: MessageRole::Agent,
                    content: lines,
                    timestamp: Utc::now(),
                    usage,
                });
            }

            // Only stop working on the FINAL response (not early replies)
            if !is_early_reply {
                state.is_agent_working = false;
            }
            state.streaming_renderer = None;
            state.needs_redraw = true;
        }
        Event::UserSubmit(text) => {
            handle_user_submit(state, text);
            state.needs_redraw = true;
        }
        Event::Tick => {
            // Continuous auto-scroll while the user is holding the
            // mouse button at an edge of the message area.
            // `is_dragging` is true from Mouse::Down until Mouse::Up,
            // so this does not fire after the user releases.
            //
            // Scrolls at 2 lines per tick (~60 lines/sec at 33ms
            // tick), which feels responsive without runaway scroll.
            if let Some(sel) = state.mouse_selection.as_ref() {
                if sel.is_dragging {
                    let ctx = compute_selection_ctx(state);
                    let tr = sel.last_term_row;
                    let scrolled = if tr <= ctx.msg_top {
                        state.message_list.scroll_up(2);
                        true
                    } else if tr + 1 >= ctx.msg_bottom {
                        state.message_list.scroll_down(2);
                        true
                    } else {
                        false
                    };
                    if scrolled {
                        // Recompute current_abs against the new view:
                        // the cursor stayed at the same terminal row,
                        // but the content under it is now N lines
                        // further along.
                        let new_ctx = compute_selection_ctx(state);
                        if let Some(ref mut sel_mut) = state.mouse_selection {
                            let col = sel_mut.current.0;
                            sel_mut.current = (col, new_ctx.term_to_abs(tr));
                        }
                        state.needs_redraw = true;
                    }
                }
            }

            if state.is_agent_working {
                state.activity_panel.tick();
                state.needs_redraw = true;
            }
        }
        _ => {}
    }
}

/// Handle a key event.
fn handle_key(state: &mut AppState, key: crossterm::event::KeyEvent) {
    use crate::input::handler::{handle_input_event, InputResult};

    // Copy picker overlay has its own keybinds (1-9 to copy, Esc to close)
    if state.overlay == Overlay::CopyPicker {
        use crossterm::event::KeyCode;
        match key.code {
            KeyCode::Esc => {
                state.overlay = Overlay::None;
            }
            KeyCode::Char(c @ '1'..='9') => {
                let idx = (c as u8 - b'1') as usize;
                // Most-recent-first — iter().rev() matches render order
                if let Some(block) = state.code_blocks.iter().rev().nth(idx).cloned() {
                    let toast = match copy_picker::copy_to_clipboard(&block.text) {
                        Ok(()) => format!(
                            "Copied block {} ({}, {} lines)",
                            idx + 1,
                            if block.lang.is_empty() {
                                "text"
                            } else {
                                &block.lang
                            },
                            block.line_count
                        ),
                        Err(e) => format!("Copy failed: {e}"),
                    };
                    push_system_line(state, toast);
                }
                state.overlay = Overlay::None;
            }
            _ => {}
        }
        return;
    }

    // Handle other overlays — Esc closes them
    if state.overlay != Overlay::None {
        if key.code == crossterm::event::KeyCode::Esc {
            state.overlay = Overlay::None;
        }
        return;
    }

    // Handle onboarding
    if state.screen == Screen::Onboarding {
        handle_onboarding_key(state, key);
        return;
    }

    // Chat screen input handling
    let event = crossterm::event::Event::Key(key);
    match handle_input_event(&event, &mut state.input, state.is_agent_working) {
        InputResult::Submit(text) => {
            handle_user_submit(state, text);
        }
        InputResult::Interrupt => {
            if state.mouse_selection.is_some() {
                // Active selection + Ctrl+C → copy selection (standard
                // text-editor UX). The view layer will extract, copy,
                // and clear the selection on the next render.
                state.pending_copy_selection = true;
                state.last_ctrl_c = None;
            } else if state.is_agent_working {
                // Tier C: fire the real interrupt flag via lib.rs event loop
                state.pending_cancel = true;
                state.last_ctrl_c = None;
            } else if let Some(last) = state.last_ctrl_c {
                // Second Ctrl+C within 2 seconds: quit
                if last.elapsed() < std::time::Duration::from_secs(2) {
                    state.should_quit = true;
                } else {
                    // Expired — treat as first press
                    state.last_ctrl_c = Some(std::time::Instant::now());
                    push_system_line(state, "Press Ctrl+C again to exit".to_string());
                }
            } else {
                // First Ctrl+C while idle: show hint
                state.last_ctrl_c = Some(std::time::Instant::now());
                state.input.clear();
                push_system_line(state, "Press Ctrl+C again to exit".to_string());
            }
        }
        InputResult::Quit => {
            // Ctrl+D on empty input — quit immediately
            state.should_quit = true;
        }
        InputResult::Redraw => {
            state.needs_redraw = true;
        }
        InputResult::ToggleActivityPanel => {
            state.activity_panel.toggle();
        }
        InputResult::ScrollUp => {
            state.message_list.scroll_up(3);
        }
        InputResult::ScrollDown => {
            state.message_list.scroll_down(3);
        }
        InputResult::PageUp => {
            let page = state.terminal_size.1.saturating_sub(5) as usize;
            state.message_list.scroll_up(page);
        }
        InputResult::PageDown => {
            let page = state.terminal_size.1.saturating_sub(5) as usize;
            state.message_list.scroll_down(page);
        }
        InputResult::Escape => {
            // Priority order: clear selection → close overlay →
            // cancel working agent → no-op
            if state.mouse_selection.is_some() {
                state.mouse_selection = None;
                state.copy_feedback = None;
            } else if state.overlay != Overlay::None {
                state.overlay = Overlay::None;
            } else if state.is_agent_working {
                state.pending_cancel = true;
            }
        }
        InputResult::YankCodeBlock => {
            if state.code_blocks.is_empty() {
                push_system_line(state, "No code blocks to copy yet.".to_string());
            } else {
                state.overlay = Overlay::CopyPicker;
            }
        }
        InputResult::ToggleMouseCapture => {
            state.mouse_capture_enabled = !state.mouse_capture_enabled;
            state.needs_mouse_toggle = true;
            let msg = if state.mouse_capture_enabled {
                "Mouse capture ON — TUI exclusive mode. Shift+drag (macOS: Option+drag) to select text."
            } else {
                "Mouse capture OFF — full native text selection, but scroll wheel and TUI mouse disabled."
            };
            push_system_line(state, msg.to_string());
        }
        InputResult::TabComplete => {
            // Try command completion
            let text = state.input.text();
            if let Some(query) = text.strip_prefix('/') {
                let completions = state.command_registry.completions(query);
                if completions.len() == 1 {
                    state.input.clear();
                    for c in format!("/{}", completions[0].0).chars() {
                        state.input.insert_char(c);
                    }
                }
            }
        }
        InputResult::Consumed | InputResult::NotHandled => {}
    }
}

/// Compute the `(top_row, bottom_row_exclusive)` of the message area
/// from the current TEA state, using the same layout math as
/// `views/chat.rs::render_chat`. Used by the drag handler to
/// auto-scroll when the cursor hits the edge of the visible
/// messages, so the user can select content outside the current
/// viewport without having to stop, scroll, and restart the drag.
fn compute_message_area_bounds(state: &AppState) -> (u16, u16) {
    let activity_height = state.activity_panel.height();
    let thinking_height = if state.is_agent_working && activity_height == 0 {
        1
    } else {
        activity_height
    };
    let input_height = (state.input.lines.len() as u16).clamp(1, 10);
    // Layout constraints from render_chat: messages, activity,
    // input+1 border, hint (1), status (1). The messages area is
    // everything above those tail rows.
    let tail = thinking_height
        .saturating_add(input_height)
        .saturating_add(1) // input border
        .saturating_add(1) // hint bar
        .saturating_add(1); // status bar
    let bottom = state.terminal_size.1.saturating_sub(tail);
    (0, bottom)
}

/// Build the `SelectionCtx` describing the current viewport mapping
/// between terminal rows and absolute content line indices. Cheap —
/// both helpers it calls are constant-time or linear in message count.
pub fn compute_selection_ctx(state: &AppState) -> SelectionCtx {
    let (msg_top, msg_bottom) = compute_message_area_bounds(state);
    let view_height = msg_bottom.saturating_sub(msg_top) as usize;

    // Total rendered lines = message_list lines + streaming (if active)
    let mut total_lines = state.message_list.line_count();
    if state.is_agent_working {
        if let Some(ref renderer) = state.streaming_renderer {
            total_lines += renderer.lines().len();
        }
    }

    let scroll = state.message_list.scroll_offset;
    let max_offset = total_lines.saturating_sub(view_height);
    let offset = scroll.min(max_offset);
    let view_start = total_lines
        .saturating_sub(offset)
        .saturating_sub(view_height);

    SelectionCtx {
        msg_top,
        msg_bottom,
        view_height,
        total_lines,
        view_start,
    }
}

/// Push a single-line system message to the message list (toast-style).
fn push_system_line(state: &mut AppState, text: String) {
    state.message_list.push(DisplayMessage {
        role: MessageRole::System,
        content: vec![RenderedLine {
            spans: vec![ratatui::text::Span::styled(text, state.theme.secondary)],
            indent: 0,
        }],
        timestamp: Utc::now(),
        usage: None,
    });
}

/// Handle user submitting text.
fn handle_user_submit(state: &mut AppState, text: String) {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return;
    }

    // Block new messages while agent is working (slash commands still allowed)
    if state.is_agent_working && !trimmed.starts_with('/') {
        state.message_list.push(DisplayMessage {
            role: MessageRole::System,
            content: vec![RenderedLine {
                spans: vec![ratatui::text::Span::styled(
                    "Tem is still thinking... wait for a response or press Ctrl+C to interrupt."
                        .to_string(),
                    state.theme.secondary,
                )],
                indent: 0,
            }],
            timestamp: Utc::now(),
            usage: None,
        });
        return;
    }

    // Try slash command first
    let ctx = state.command_context();
    if let Some(result) = state.command_registry.dispatch(trimmed, &ctx) {
        match result {
            CommandResult::DisplayMessage(msg) => {
                state.message_list.push(DisplayMessage {
                    role: MessageRole::System,
                    content: vec![RenderedLine {
                        spans: vec![ratatui::text::Span::styled(msg, state.theme.info)],
                        indent: 0,
                    }],
                    timestamp: Utc::now(),
                    usage: None,
                });
            }
            CommandResult::ShowOverlay(kind) => {
                if kind == OverlayKind::Help {
                    state.overlay = Overlay::Help;
                } else {
                    state.overlay = Overlay::Config(kind);
                }
            }
            CommandResult::ClearChat => {
                state.message_list.clear();
            }
            CommandResult::Quit => {
                state.should_quit = true;
            }
            CommandResult::Silent => {}
            CommandResult::SwitchModel(model) => {
                if state.is_agent_working {
                    push_system_line(
                        state,
                        "Cannot switch model while Tem is working. \
                         Press Esc to cancel first."
                            .to_string(),
                    );
                } else {
                    push_system_line(state, format!("Switching to model '{model}'..."));
                    state.pending_model_switch = Some(model);
                }
            }
            CommandResult::Error(msg) => {
                state.message_list.push(DisplayMessage {
                    role: MessageRole::System,
                    content: vec![RenderedLine {
                        spans: vec![ratatui::text::Span::styled(msg, state.theme.error)],
                        indent: 0,
                    }],
                    timestamp: Utc::now(),
                    usage: None,
                });
            }
        }
        return;
    }

    // Regular message — add to display and prepare for agent
    let lines = render_markdown_with_width(
        trimmed,
        state.theme.text,
        state.theme.heading,
        state.theme.code_bg,
        state.theme.info,
        state.theme.secondary,
        state.terminal_size.0 as usize,
    );
    state.message_list.push(DisplayMessage {
        role: MessageRole::User,
        content: lines,
        timestamp: Utc::now(),
        usage: None,
    });

    // Start streaming renderer for the response
    state.streaming_renderer = Some(StreamingRenderer::new(
        state.theme.text,
        state.theme.heading,
        state.theme.code_bg,
        state.theme.info,
        state.theme.secondary,
    ));

    // Send to agent via the event loop
    state.pending_user_message = Some(trimmed.to_string());
    state.is_agent_working = true;
    state.activity_panel.reset();
    state.token_counter.reset_turn();
    // Increment turn counter for D3 tool history grouping
    state.current_turn = state.current_turn.saturating_add(1);
}

/// Finalize streaming — move rendered content to message list.
fn finalize_streaming(state: &mut AppState) {
    if let Some(renderer) = state.streaming_renderer.take() {
        let lines = renderer.lines().to_vec();
        state.message_list.push(DisplayMessage {
            role: MessageRole::Agent,
            content: lines,
            timestamp: Utc::now(),
            usage: Some(TurnUsage {
                input_tokens: state.token_counter.turn_input_tokens,
                output_tokens: state.token_counter.turn_output_tokens,
                cost_usd: state.token_counter.turn_cost_usd,
                elapsed_ms: 0,
            }),
        });
    }
}

/// Handle key events during onboarding.
fn handle_onboarding_key(state: &mut AppState, key: crossterm::event::KeyEvent) {
    use crossterm::event::KeyCode;

    match &mut state.onboarding_step {
        OnboardingStep::Welcome => {
            if key.code == KeyCode::Enter {
                let items = steps::mode_select_items();
                state.onboarding_step = OnboardingStep::SelectMode(SelectState::new(items));
            }
        }
        OnboardingStep::SelectMode(select) => match key.code {
            KeyCode::Up => select.move_up(),
            KeyCode::Down => select.move_down(),
            KeyCode::Enter => {
                if let Some(mode) = select.selected_value().cloned() {
                    state.selected_mode = Some(mode);
                    let items = steps::provider_select_items();
                    state.onboarding_step = OnboardingStep::SelectProvider(SelectState::new(items));
                }
            }
            KeyCode::Esc => {
                state.onboarding_step = OnboardingStep::Welcome;
            }
            _ => {}
        },
        OnboardingStep::SelectProvider(select) => match key.code {
            KeyCode::Up => select.move_up(),
            KeyCode::Down => select.move_down(),
            KeyCode::Enter => {
                if let Some(provider) = select.selected_value().cloned() {
                    state.onboarding_step = OnboardingStep::EnterApiKey {
                        provider,
                        input: String::new(),
                        error: None,
                    };
                }
            }
            KeyCode::Esc => {
                let items = steps::mode_select_items();
                state.onboarding_step = OnboardingStep::SelectMode(SelectState::new(items));
            }
            _ => {}
        },
        OnboardingStep::EnterApiKey {
            provider,
            input,
            error,
        } => match key.code {
            KeyCode::Char(c) => {
                input.push(c);
                *error = None;
            }
            KeyCode::Backspace => {
                input.pop();
            }
            KeyCode::Enter => {
                if input.is_empty() {
                    *error = Some("Please enter an API key".to_string());
                } else {
                    let provider = provider.clone();
                    state.onboarding_api_key = Some(input.clone());
                    state.onboarding_step = OnboardingStep::ValidatingKey { provider };
                    // Validation handled asynchronously by lib.rs event loop
                }
            }
            KeyCode::Esc => {
                let items = steps::provider_select_items();
                state.onboarding_step = OnboardingStep::SelectProvider(SelectState::new(items));
            }
            _ => {}
        },
        OnboardingStep::ValidatingKey { .. } => {
            // Waiting for async validation — ignore keys except Esc
            if key.code == KeyCode::Esc {
                let items = steps::provider_select_items();
                state.onboarding_step = OnboardingStep::SelectProvider(SelectState::new(items));
            }
        }
        OnboardingStep::SelectModel(select) => match key.code {
            KeyCode::Up => select.move_up(),
            KeyCode::Down => select.move_down(),
            KeyCode::Enter => {
                if let Some(model) = select.selected_value().cloned() {
                    let provider = state
                        .current_provider
                        .clone()
                        .unwrap_or_else(|| "unknown".to_string());
                    state.current_model = Some(model.clone());
                    state.onboarding_step = OnboardingStep::Confirm { provider, model };
                }
            }
            KeyCode::Esc => {
                let provider = state.current_provider.clone().unwrap_or_default();
                state.onboarding_step = OnboardingStep::EnterApiKey {
                    provider,
                    input: String::new(),
                    error: None,
                };
            }
            _ => {}
        },
        OnboardingStep::Confirm { provider, model: _ } => match key.code {
            KeyCode::Enter => {
                state.onboarding_step = OnboardingStep::Saving;
                // Save will be handled asynchronously by the event loop
            }
            KeyCode::Esc => {
                let items = steps::model_select_items(provider);
                state.onboarding_step = OnboardingStep::SelectModel(SelectState::new(items));
            }
            _ => {}
        },
        OnboardingStep::Saving => {
            // Waiting for async save — ignore keys
        }
        OnboardingStep::Done => {
            state.screen = Screen::Chat;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn initial_state() {
        let state = AppState::new();
        assert_eq!(state.screen, Screen::Chat);
        assert!(!state.is_agent_working);
        assert!(!state.should_quit);
        assert!(state.input.is_empty());
    }

    #[test]
    fn with_onboarding() {
        let state = AppState::new().with_onboarding();
        assert_eq!(state.screen, Screen::Onboarding);
    }

    #[test]
    fn with_chat() {
        let state =
            AppState::new().with_chat("anthropic".to_string(), "claude-sonnet-4-6".to_string());
        assert_eq!(state.screen, Screen::Chat);
        assert_eq!(state.current_provider.as_deref(), Some("anthropic"));
        assert_eq!(state.current_model.as_deref(), Some("claude-sonnet-4-6"));
    }

    #[test]
    fn slash_command_dispatch() {
        let mut state = AppState::new();
        handle_user_submit(&mut state, "/help".to_string());
        assert_eq!(state.overlay, Overlay::Help);
    }

    #[test]
    fn slash_command_quit() {
        let mut state = AppState::new();
        handle_user_submit(&mut state, "/quit".to_string());
        assert!(state.should_quit);
    }

    #[test]
    fn regular_message_starts_agent() {
        let mut state = AppState::new();
        handle_user_submit(&mut state, "Hello there".to_string());
        assert!(state.is_agent_working);
        assert_eq!(state.message_list.messages.len(), 1);
        assert_eq!(state.message_list.messages[0].role, MessageRole::User);
    }
}
