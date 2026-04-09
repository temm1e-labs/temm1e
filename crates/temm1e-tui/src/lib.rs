//! TEMM1E Interactive TUI — Claude Code-level terminal experience.
//!
//! Launch via `temm1e tui`. Provides:
//! - Markdown rendering with syntax highlighting
//! - Real-time agent observability (collapsible activity panel)
//! - Arrow-key onboarding wizard
//! - Slash command system with tab completion
//! - Tem's 7-color design palette
//!
//! # Architecture
//!
//! Uses the TEA (The Elm Architecture) pattern:
//! - `AppState` is the single source of truth
//! - `update()` processes events and mutates state
//! - `view()` renders state to the terminal via ratatui

pub mod agent_bridge;
pub mod app;
pub mod channel;
pub mod commands;
pub mod event;
pub mod input;
pub mod onboarding;
pub mod streaming;
pub mod theme;
pub mod views;
pub mod widgets;

#[cfg(test)]
mod testing;

use std::io;
use std::time::Duration;

use crossterm::event::EventStream;
use crossterm::terminal::{
    disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen,
};
use crossterm::{cursor, execute};
use futures::StreamExt;
use ratatui::backend::CrosstermBackend;
use ratatui::Terminal;
use tokio::sync::mpsc;

use temm1e_agent::agent_task_status::AgentTaskStatus;
use temm1e_core::config::credentials::{
    load_active_provider_keys, load_credentials_file, load_saved_credentials, save_credentials,
};
use temm1e_core::types::config::Temm1eConfig;
use temm1e_core::types::model_registry::default_model;

use agent_bridge::{validate_provider_key, AgentHandle, AgentSetup};
use app::{update, ApiKeyEntry, AppState, GitInfo, MouseSelection, Overlay, Screen};
use event::Event;
use onboarding::steps::{model_select_items, OnboardingStep};
use widgets::select_list::SelectState;

/// Restore the terminal to normal mode. Safe to call multiple times.
fn restore_terminal() {
    // Drain pending input so stale keypresses don't leak into the shell
    while crossterm::event::poll(std::time::Duration::from_millis(1)).unwrap_or(false) {
        let _ = crossterm::event::read();
    }
    let _ = disable_raw_mode();
    let _ = execute!(
        io::stdout(),
        crossterm::event::DisableMouseCapture,
        LeaveAlternateScreen,
        cursor::Show
    );
    use std::io::Write;
    let _ = io::stdout().flush();
    // Nuclear reset: stty sane guarantees the terminal is usable
    // even if crossterm's disable_raw_mode() failed on macOS
    let _ = std::process::Command::new("stty")
        .arg("sane")
        .stdin(std::process::Stdio::inherit())
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status();
    // Drain again after stty — catch any keys pressed during restoration
    while crossterm::event::poll(std::time::Duration::from_millis(1)).unwrap_or(false) {
        let _ = crossterm::event::read();
    }
}

/// Terminal cleanup guard — restores terminal even on panic.
struct TerminalGuard;

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        restore_terminal();
    }
}

/// Launch the interactive TUI.
///
/// This is the main entry point called from `temm1e tui`.
pub async fn launch_tui(config: Temm1eConfig) -> anyhow::Result<()> {
    // 1. Set up terminal (logging already redirected to file by main.rs)
    // Install panic hook that restores terminal
    let original_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        restore_terminal();
        original_hook(info);
    }));

    enable_raw_mode()?;
    let mut stdout = io::stdout();
    // Mouse capture is ON so the TUI owns the whole terminal buffer
    // (exclusive alt-screen, scroll wheel scrolls the message list,
    // no scrollback bleed-through). Users select text natively by
    // holding the terminal's modifier override while dragging:
    //
    //   - macOS (iTerm2, Terminal.app): Option+drag
    //   - Linux (alacritty, kitty, GNOME): Shift+drag
    //   - Windows Terminal: Shift+drag
    //
    // For terminals that don't support the override, Alt+S toggles
    // capture entirely (disables scroll wheel, enables native drag).
    execute!(
        stdout,
        EnterAlternateScreen,
        cursor::Hide,
        crossterm::event::EnableMouseCapture
    )?;
    let _guard = TerminalGuard;

    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;

    // 2. Create event channels
    let (event_tx, mut event_rx) = mpsc::unbounded_channel::<Event>();

    // 3. Initialize app state + agent
    let mut agent_handle: Option<AgentHandle> = None;
    let mut state = if let Some((provider, key, model)) = load_saved_credentials() {
        // Try to spawn agent with saved credentials
        match agent_bridge::spawn_agent(
            AgentSetup {
                provider_name: provider.clone(),
                api_key: key,
                model: model.clone(),
                base_url: load_active_provider_keys().and_then(|(_, _, _, burl)| burl),
                config: config.clone(),
                mode: None, // Use default when loading saved credentials
            },
            event_tx.clone(),
        )
        .await
        {
            Ok(handle) => {
                agent_handle = Some(handle);
                AppState::new().with_chat(provider, model)
            }
            Err(e) => {
                tracing::warn!(error = %e, "Failed to start agent — entering onboarding");
                AppState::new().with_onboarding()
            }
        }
    } else {
        AppState::new().with_onboarding()
    };

    // Populate startup caches (git repo info, API keys cache)
    state.git_info = detect_git_info();
    state.api_keys_cache = load_api_keys_cache();

    // Show welcome message if agent is ready
    if agent_handle.is_some() {
        use widgets::markdown::render_markdown_with_width;
        use widgets::message_list::{DisplayMessage, MessageRole};
        let welcome = format!(
            "Welcome to TEMM1E TUI! Provider: {} | Model: {}\n\
             Type a message and press Enter. /help for commands.",
            state.current_provider.as_deref().unwrap_or("?"),
            state.current_model.as_deref().unwrap_or("?"),
        );
        let (tw, _) = crossterm::terminal::size().unwrap_or((80, 24));
        let lines = render_markdown_with_width(
            &welcome,
            state.theme.info,
            state.theme.heading,
            state.theme.code_bg,
            state.theme.info,
            state.theme.secondary,
            tw as usize,
        );
        state.message_list.push(DisplayMessage {
            role: MessageRole::System,
            content: lines,
            timestamp: chrono::Utc::now(),
            usage: None,
        });
    }

    // 4. Set up crossterm event stream
    let mut crossterm_events = EventStream::new();

    // 5. Tick timer for animations (30 FPS)
    let mut tick_interval = tokio::time::interval(Duration::from_millis(33));
    tick_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    // 6. Initial draw
    terminal.draw(|frame| view(&mut state, frame))?;
    state.needs_redraw = false;

    // Draw throttle — cap render rate at ~60 FPS so rapid-fire
    // mouse drag events don't pile up into back-to-back full
    // re-renders. Deferred draws catch up via the tick interval
    // (every 33ms), so the final frame after a drag always happens.
    let mut last_draw_at = std::time::Instant::now();
    const MIN_DRAW_INTERVAL: Duration = Duration::from_millis(16);

    // 7. Main event loop
    loop {
        // Build select branches dynamically based on whether we have an agent
        let status_changed = async {
            if let Some(ref mut handle) = agent_handle {
                handle.status_rx.changed().await.ok();
                Some(handle.status_rx.borrow().clone())
            } else {
                // Never resolves if no agent
                std::future::pending::<Option<AgentTaskStatus>>().await
            }
        };

        tokio::select! {
            // Crossterm terminal events
            maybe_event = crossterm_events.next() => {
                if let Some(Ok(event)) = maybe_event {
                    update(&mut state, Event::Terminal(event));
                }
            }
            // Internal events (agent responses, status updates)
            Some(event) = event_rx.recv() => {
                update(&mut state, event);
            }
            // Agent status updates via watch channel
            Some(status) = status_changed => {
                update(&mut state, Event::AgentStatus(status));
            }
            // Tick timer
            _ = tick_interval.tick() => {
                update(&mut state, Event::Tick);
            }
        }

        // Handle mouse capture toggle (A4) — must run on the real stdout
        if state.needs_mouse_toggle {
            let mut stdout = io::stdout();
            if state.mouse_capture_enabled {
                let _ = execute!(stdout, crossterm::event::EnableMouseCapture);
            } else {
                let _ = execute!(stdout, crossterm::event::DisableMouseCapture);
            }
            state.needs_mouse_toggle = false;
        }

        // Handle Escape/Ctrl+C cancel (Tier C) — fire the interrupt flag
        // that the agent loop polls between rounds.
        if state.pending_cancel {
            if let Some(ref handle) = agent_handle {
                handle
                    .interrupt_flag
                    .store(true, std::sync::atomic::Ordering::Relaxed);
            }
            state.pending_cancel = false;
        }

        // Handle /model hot-swap — tear down the current agent task
        // and spawn a new one with the new model. Only runs while
        // idle (the command handler rejects mid-turn switches).
        if let Some(new_model) = state.pending_model_switch.take() {
            handle_model_switch(&mut state, &mut agent_handle, &event_tx, &config, new_model).await;
        }

        // Handle user message submission → send to agent
        if let Some(text) = state.pending_user_message.take() {
            if let Some(ref handle) = agent_handle {
                // Detect file paths in user input and attach them
                let (msg_text, attachments) = parse_file_references(&text);
                let msg = temm1e_core::types::message::InboundMessage {
                    id: uuid::Uuid::new_v4().to_string(),
                    channel: "tui".to_string(),
                    chat_id: "tui".to_string(),
                    user_id: "local".to_string(),
                    username: Some(whoami()),
                    text: Some(msg_text),
                    attachments,
                    reply_to: None,
                    timestamp: chrono::Utc::now(),
                };
                let _ = handle.inbound_tx.send(msg).await;
            }
        }

        // Handle onboarding async operations
        handle_onboarding_async(&mut state, &mut agent_handle, &event_tx, &config).await;

        // Check quit
        if state.should_quit {
            break;
        }

        // Handle terminal resize — clear buffer for clean redraw
        if state.needs_clear {
            terminal.clear()?;
            state.needs_clear = false;
        }

        // Render if needed — throttled to MIN_DRAW_INTERVAL
        if state.needs_redraw && last_draw_at.elapsed() >= MIN_DRAW_INTERVAL {
            terminal.draw(|frame| view(&mut state, frame))?;
            state.needs_redraw = false;
            last_draw_at = std::time::Instant::now();
        }
    }

    // Explicit terminal restoration before returning
    drop(terminal); // drop ratatui terminal first — releases stdout
    restore_terminal();
    // Guard will also call restore_terminal() — safe to call multiple times

    Ok(())
}

/// Handle async onboarding transitions (key validation, credential saving).
async fn handle_onboarding_async(
    state: &mut AppState,
    agent_handle: &mut Option<AgentHandle>,
    event_tx: &mpsc::UnboundedSender<Event>,
    config: &Temm1eConfig,
) {
    match &state.onboarding_step {
        OnboardingStep::ValidatingKey { provider } => {
            if let Some(ref api_key) = state.onboarding_api_key.clone() {
                let provider = provider.clone();
                let model = default_model(&provider).to_string();

                match validate_provider_key(&provider, api_key, &model, None).await {
                    Ok(()) => {
                        state.current_provider = Some(provider.clone());
                        let items = model_select_items(&provider);
                        state.onboarding_step =
                            OnboardingStep::SelectModel(SelectState::new(items));
                    }
                    Err(e) => {
                        state.onboarding_step = OnboardingStep::EnterApiKey {
                            provider,
                            input: String::new(),
                            error: Some(format!("Validation failed: {}", e)),
                        };
                    }
                }
                state.needs_redraw = true;
            }
        }
        OnboardingStep::Saving => {
            if let (Some(ref provider), Some(ref api_key)) =
                (&state.current_provider, &state.onboarding_api_key.clone())
            {
                let model = state
                    .current_model
                    .clone()
                    .unwrap_or_else(|| default_model(provider).to_string());

                match save_credentials(provider, api_key, &model, None).await {
                    Ok(()) => {
                        match agent_bridge::spawn_agent(
                            AgentSetup {
                                provider_name: provider.clone(),
                                api_key: api_key.clone(),
                                model: model.clone(),
                                base_url: None,
                                config: config.clone(),
                                mode: state.selected_mode.clone(),
                            },
                            event_tx.clone(),
                        )
                        .await
                        {
                            Ok(handle) => {
                                *agent_handle = Some(handle);
                                // Skip Done screen — go straight to chat
                                state.screen = Screen::Chat;
                                state.onboarding_step = OnboardingStep::Done;
                            }
                            Err(e) => {
                                state.onboarding_step = OnboardingStep::EnterApiKey {
                                    provider: provider.clone(),
                                    input: String::new(),
                                    error: Some(format!("Agent failed to start: {}", e)),
                                };
                            }
                        }
                    }
                    Err(e) => {
                        state.onboarding_step = OnboardingStep::EnterApiKey {
                            provider: provider.clone(),
                            input: String::new(),
                            error: Some(format!("Failed to save: {}", e)),
                        };
                    }
                }
                state.onboarding_api_key = None;
                state.needs_redraw = true;
            }
        }
        _ => {}
    }
}

/// Parse file references from user input.
///
/// Supports:
/// - `/file <path>` — explicit file attachment
/// - Bare paths starting with `/` or `~/` or `./` — auto-detected
fn parse_file_references(text: &str) -> (String, Vec<temm1e_core::types::message::AttachmentRef>) {
    let trimmed = text.trim();

    // Explicit /file command
    if let Some(path_str) = trimmed.strip_prefix("/file ") {
        let path = std::path::Path::new(path_str.trim());
        let expanded = if path_str.trim().starts_with('~') {
            dirs::home_dir()
                .unwrap_or_default()
                .join(path_str.trim().trim_start_matches("~/"))
        } else {
            path.to_path_buf()
        };
        if expanded.exists() {
            let att = temm1e_core::types::message::AttachmentRef {
                file_id: expanded.to_string_lossy().to_string(),
                file_name: expanded
                    .file_name()
                    .map(|n| n.to_string_lossy().to_string()),
                mime_type: None,
                size: std::fs::metadata(&expanded).ok().map(|m| m.len() as usize),
            };
            return (format!("[file: {}]", expanded.display()), vec![att]);
        }
        return (format!("[file not found: {}]", expanded.display()), vec![]);
    }

    // Auto-detect bare file paths (e.g. dragged into terminal)
    // Check if the entire input looks like a file path
    let expanded = if trimmed.starts_with('~') {
        dirs::home_dir()
            .unwrap_or_default()
            .join(trimmed.trim_start_matches("~/"))
    } else {
        std::path::PathBuf::from(trimmed)
    };

    if (trimmed.starts_with('/') || trimmed.starts_with("~/"))
        && !trimmed.contains(' ')
        && expanded.exists()
    {
        let att = temm1e_core::types::message::AttachmentRef {
            file_id: expanded.to_string_lossy().to_string(),
            file_name: expanded
                .file_name()
                .map(|n| n.to_string_lossy().to_string()),
            mime_type: None,
            size: std::fs::metadata(&expanded).ok().map(|m| m.len() as usize),
        };
        return (
            format!("[file: {}] Analyze this file.", expanded.display()),
            vec![att],
        );
    }

    (trimmed.to_string(), vec![])
}

/// Get the current OS username.
fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}

/// Validate + apply a model hot-swap. Called from the event loop
/// when `state.pending_model_switch` is set. Drops the old agent
/// handle (its task exits when the sender is dropped), spawns a new
/// agent with the new model, saves the new model to credentials.toml
/// so the next launch uses it, and updates `state.current_model`.
async fn handle_model_switch(
    state: &mut AppState,
    agent_handle: &mut Option<AgentHandle>,
    event_tx: &mpsc::UnboundedSender<Event>,
    config: &Temm1eConfig,
    new_model: String,
) {
    use temm1e_core::types::model_registry::available_models_for_provider;

    let Some(provider) = state.current_provider.clone() else {
        push_system_line_via_tx(
            event_tx,
            "Cannot switch model: no provider is configured.".to_string(),
        );
        return;
    };

    // Validate against the known list for the current provider.
    let known = available_models_for_provider(&provider);
    if !known.is_empty() && !known.contains(&new_model.as_str()) {
        push_system_line_via_tx(
            event_tx,
            format!(
                "Unknown model '{}' for provider '{}'. Valid: {}",
                new_model,
                provider,
                known.join(", ")
            ),
        );
        return;
    }

    // Read the current credentials to recover the API key + base_url.
    let Some((_name, keys, _model, base_url)) = load_active_provider_keys() else {
        push_system_line_via_tx(
            event_tx,
            "Cannot switch model: no saved credentials for the active provider.".to_string(),
        );
        return;
    };
    let Some(api_key) = keys.into_iter().next() else {
        push_system_line_via_tx(
            event_tx,
            "Cannot switch model: no API key found for the active provider.".to_string(),
        );
        return;
    };

    // Drop the old handle — its task exits when `inbound_tx` is dropped.
    // Happens automatically when we overwrite agent_handle below.
    let old = agent_handle.take();
    drop(old);

    match agent_bridge::spawn_agent(
        AgentSetup {
            provider_name: provider.clone(),
            api_key: api_key.clone(),
            model: new_model.clone(),
            base_url: base_url.clone(),
            config: config.clone(),
            mode: state.selected_mode.clone(),
        },
        event_tx.clone(),
    )
    .await
    {
        Ok(handle) => {
            *agent_handle = Some(handle);
            state.current_model = Some(new_model.clone());
            // Persist so the next launch uses the new model
            if let Err(e) =
                save_credentials(&provider, &api_key, &new_model, base_url.as_deref()).await
            {
                tracing::warn!(error = %e, "Failed to persist model switch");
            }
            push_system_line_via_tx(event_tx, format!("✓ Switched to model '{new_model}'"));
        }
        Err(e) => {
            push_system_line_via_tx(event_tx, format!("✗ Model switch failed: {e}"));
        }
    }
}

/// Push a system message through the event channel so the TUI
/// renders it on the next iteration. Used by async side effects
/// that don't hold a mutable `AppState` reference.
fn push_system_line_via_tx(event_tx: &mpsc::UnboundedSender<Event>, text: String) {
    let _ = event_tx.send(Event::AgentResponse(event::AgentResponseEvent {
        message: temm1e_core::types::message::OutboundMessage {
            chat_id: "tui".to_string(),
            text,
            reply_to: None,
            parse_mode: None,
        },
        input_tokens: 0,
        output_tokens: 0,
        cost_usd: 0.0,
    }));
}

/// Detect git repository + branch. Returns `None` if:
/// - not inside a git repo
/// - `git` binary is missing
/// - any subcommand fails
///
/// Silent failure by design — status bar degrades gracefully.
fn detect_git_info() -> Option<GitInfo> {
    use std::process::Command;

    // Repo toplevel
    let toplevel = Command::new("git")
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()?;
    if !toplevel.status.success() {
        return None;
    }
    let path = String::from_utf8(toplevel.stdout).ok()?;
    let path_trimmed = path.trim();
    if path_trimmed.is_empty() {
        return None;
    }
    let repo_name = std::path::Path::new(path_trimmed)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "repo".to_string());

    // Branch — three-stage fallback: branch --show-current, symbolic-ref, short hash
    let mut branch = String::new();
    if let Ok(out) = Command::new("git")
        .args(["branch", "--show-current"])
        .output()
    {
        if out.status.success() {
            branch = String::from_utf8(out.stdout)
                .unwrap_or_default()
                .trim()
                .to_string();
        }
    }
    if branch.is_empty() {
        if let Ok(out) = Command::new("git")
            .args(["symbolic-ref", "--short", "HEAD"])
            .output()
        {
            if out.status.success() {
                branch = String::from_utf8(out.stdout)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
            }
        }
    }
    if branch.is_empty() {
        if let Ok(out) = Command::new("git")
            .args(["rev-parse", "--short", "HEAD"])
            .output()
        {
            if out.status.success() {
                let h = String::from_utf8(out.stdout)
                    .unwrap_or_default()
                    .trim()
                    .to_string();
                if !h.is_empty() {
                    branch = format!("@{h}");
                }
            }
        }
    }
    if branch.is_empty() {
        branch = "(unknown)".to_string();
    }

    Some(GitInfo { repo_name, branch })
}

/// Build the API keys cache from the credentials file. Returns an
/// empty Vec on any error — overlays degrade gracefully.
fn load_api_keys_cache() -> Vec<ApiKeyEntry> {
    let Some(creds) = load_credentials_file() else {
        return Vec::new();
    };
    let active = creds.active.clone();
    creds
        .providers
        .iter()
        .map(|p| {
            // Pick the first non-placeholder key for the fingerprint; if
            // all keys are placeholders, use "?" as a marker.
            let fingerprint = p
                .keys
                .iter()
                .find(|k| !temm1e_core::config::credentials::is_placeholder_key(k) && k.len() >= 4)
                .map(|k| {
                    k.chars()
                        .rev()
                        .take(4)
                        .collect::<String>()
                        .chars()
                        .rev()
                        .collect::<String>()
                })
                .unwrap_or_else(|| "????".to_string());
            ApiKeyEntry {
                provider: p.name.clone(),
                fingerprint,
                is_active: p.name == active,
            }
        })
        .collect()
}

/// TEA view function — renders AppState to a ratatui frame.
///
/// Takes `&mut AppState` so the post-render selection pass can:
/// - extract cell symbols when a drag-to-select just ended, and
/// - clear the corresponding pending flags.
fn view(state: &mut AppState, frame: &mut ratatui::Frame) {
    let area = frame.area();

    match state.screen {
        Screen::Onboarding => {
            views::onboarding::render_onboarding(
                &state.onboarding_step,
                &state.theme,
                area,
                frame.buffer_mut(),
            );
        }
        Screen::Chat => {
            views::chat::render_chat(state, area, frame.buffer_mut());

            // Render overlay on top
            match &state.overlay {
                Overlay::Help => {
                    views::help::render_help(
                        &state.command_registry,
                        &state.theme,
                        area,
                        frame.buffer_mut(),
                    );
                }
                Overlay::Config(kind) => {
                    views::config_panel::render_config_overlay(
                        kind,
                        state,
                        area,
                        frame.buffer_mut(),
                    );
                }
                Overlay::CopyPicker => {
                    widgets::copy_picker::render_copy_picker(state, area, frame.buffer_mut());
                }
                Overlay::None => {}
            }
        }
    }

    // ── Mouse selection: highlight + optional copy on release ─
    // Runs after all other rendering so it overlays the final buffer.
    // Extracts text from the just-rendered cells on pending_copy, then
    // applies a REVERSED-style highlight so the user sees the range.
    if let Some(sel) = state.mouse_selection.clone() {
        let buf = frame.buffer_mut();
        let (start, end) = normalized_selection(&sel, buf.area);

        // Extract BEFORE highlighting so the highlight doesn't need
        // to be undone for the extraction — symbol content is
        // independent of style modifiers.
        if state.pending_copy_selection {
            state.pending_copy_selection = false;
            let text = extract_selection_text(buf, start, end);
            if !text.trim().is_empty() {
                match widgets::copy_picker::copy_to_clipboard(&text) {
                    Ok(()) => {
                        let char_count = text.trim().chars().count();
                        state.copy_feedback =
                            Some(format!("Copied {char_count} chars to clipboard"));
                    }
                    Err(e) => {
                        state.copy_feedback = Some(format!("Copy failed: {e}"));
                    }
                }
            }
            // Clear the selection after copy so the highlight goes
            // away on the NEXT frame (we still render this frame
            // with the highlight so the user sees what was copied).
            state.mouse_selection = None;
            highlight_selection_cells(buf, start, end);
        } else {
            highlight_selection_cells(buf, start, end);
        }
    }
}

/// Normalize a `MouseSelection` into a (start, end) pair in reading
/// order (earlier cell first) and clamp both to the buffer area.
fn normalized_selection(
    sel: &MouseSelection,
    area: ratatui::layout::Rect,
) -> ((u16, u16), (u16, u16)) {
    let clamp = |(x, y): (u16, u16)| -> (u16, u16) {
        let right = area.right().saturating_sub(1).max(area.left());
        let bottom = area.bottom().saturating_sub(1).max(area.top());
        (x.clamp(area.left(), right), y.clamp(area.top(), bottom))
    };
    let a = clamp(sel.anchor);
    let c = clamp(sel.current);
    // Reading order: earlier row first; within same row, earlier column.
    if (a.1, a.0) <= (c.1, c.0) {
        (a, c)
    } else {
        (c, a)
    }
}

/// Extract the selected cell symbols as plain text. Each row is
/// trimmed of trailing spaces (terminal cells are padded) and
/// separated by `\n`. UTF-8 safe because `Cell::symbol()` returns a
/// complete grapheme per cell.
///
/// Strips code-block decoration added by `widgets/markdown.rs`:
///
///   - Rows whose first non-space char is `╭` or `╰` are the top
///     and bottom borders of a rendered code block — skipped
///     entirely so the user's paste doesn't get `╭── rust ──` junk.
///   - Rows starting with ` │ ` (space + U+2502 + space) are code
///     content with a box-drawing gutter prefix — the prefix is
///     stripped, indentation inside the prefix is preserved.
///
/// Everything else is passed through unchanged.
fn extract_selection_text(
    buf: &ratatui::buffer::Buffer,
    start: (u16, u16),
    end: (u16, u16),
) -> String {
    let mut rows: Vec<String> = Vec::new();
    let left_edge = buf.area.left();
    let right_edge = buf.area.right();
    let bottom_edge = buf.area.bottom();

    for y in start.1..=end.1 {
        if y >= bottom_edge {
            break;
        }
        // Always read the FULL row for decoration detection — the
        // user might have dragged into the middle of a code block,
        // and we still want to strip the `│ ` prefix even though
        // they didn't select the leading cells.
        let mut row = String::new();
        for x in left_edge..right_edge {
            row.push_str(buf[(x, y)].symbol());
        }
        let row_trimmed_end = row.trim_end().to_string();

        // Detect code-block borders (top: ╭, bottom: ╰) and skip.
        let leading_non_space = row_trimmed_end.trim_start();
        if leading_non_space.starts_with('\u{256d}') // ╭
            || leading_non_space.starts_with('\u{2570}')
        // ╰
        {
            continue;
        }

        // Detect code content rows: ` │ ` (space + │ + space) prefix.
        // If the user's selection spans the prefix cells, we read the
        // full row and strip it. If they dragged across only the
        // code portion, the prefix isn't in `row_trimmed_end` yet so
        // strip_prefix is a no-op.
        let stripped = if let Some(rest) = row_trimmed_end.strip_prefix(" \u{2502} ") {
            rest.to_string()
        } else {
            row_trimmed_end
        };

        // Now honor the user's actual column range for non-code rows.
        // For code rows (which we already trimmed to full width), we
        // preserve the whole content line because the user's intent
        // when dragging code is "give me the code, not a fragment".
        // For everything else, slice to the selected x range.
        if row.starts_with(" \u{2502} ") {
            rows.push(stripped);
        } else {
            let x_start = if y == start.1 { start.0 } else { left_edge };
            let x_end = if y == end.1 {
                (end.0.saturating_add(1)).min(right_edge)
            } else {
                right_edge
            };
            let mut partial = String::new();
            for x in x_start..x_end {
                partial.push_str(buf[(x, y)].symbol());
            }
            rows.push(partial.trim_end().to_string());
        }
    }

    rows.join("\n")
}

/// Apply a REVERSED style modifier to every cell in the selection
/// range. This is the standard "inverted" look users expect from
/// terminal selections. Purely visual — does not affect the symbols.
fn highlight_selection_cells(
    buf: &mut ratatui::buffer::Buffer,
    start: (u16, u16),
    end: (u16, u16),
) {
    use ratatui::style::Modifier;
    let left_edge = buf.area.left();
    let right_edge = buf.area.right();
    let bottom_edge = buf.area.bottom();

    for y in start.1..=end.1 {
        if y >= bottom_edge {
            break;
        }
        let x_start = if y == start.1 { start.0 } else { left_edge };
        let x_end = if y == end.1 {
            (end.0.saturating_add(1)).min(right_edge)
        } else {
            right_edge
        };
        for x in x_start..x_end {
            let cell = &mut buf[(x, y)];
            let current = cell.style();
            cell.set_style(current.add_modifier(Modifier::REVERSED));
        }
    }
}
