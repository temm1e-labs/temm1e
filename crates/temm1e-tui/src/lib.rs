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
    load_active_provider_keys, load_saved_credentials, save_credentials,
};
use temm1e_core::types::config::Temm1eConfig;
use temm1e_core::types::model_registry::default_model;

use agent_bridge::{validate_provider_key, AgentHandle, AgentSetup};
use app::{update, AppState, Overlay, Screen};
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
    terminal.draw(|frame| view(&state, frame))?;
    state.needs_redraw = false;

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

        // Render if needed
        if state.needs_redraw {
            terminal.draw(|frame| view(&state, frame))?;
            state.needs_redraw = false;
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

/// TEA view function — renders AppState to a ratatui frame.
fn view(state: &AppState, frame: &mut ratatui::Frame) {
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
                        &state.theme,
                        area,
                        frame.buffer_mut(),
                    );
                }
                Overlay::None => {}
            }
        }
    }
}
