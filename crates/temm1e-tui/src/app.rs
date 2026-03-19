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
            use crossterm::event::MouseEventKind;
            match mouse.kind {
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

    // Handle overlays first
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
            if state.is_agent_working {
                // First Ctrl+C while agent working: interrupt agent
                state.is_agent_working = false;
                state.message_list.push(DisplayMessage {
                    role: MessageRole::System,
                    content: vec![RenderedLine {
                        spans: vec![ratatui::text::Span::styled(
                            "Interrupted.".to_string(),
                            state.theme.secondary,
                        )],
                        indent: 0,
                    }],
                    timestamp: Utc::now(),
                    usage: None,
                });
                state.last_ctrl_c = None;
            } else if let Some(last) = state.last_ctrl_c {
                // Second Ctrl+C within 2 seconds: quit
                if last.elapsed() < std::time::Duration::from_secs(2) {
                    state.should_quit = true;
                } else {
                    // Expired — treat as first press
                    state.last_ctrl_c = Some(std::time::Instant::now());
                    state.message_list.push(DisplayMessage {
                        role: MessageRole::System,
                        content: vec![RenderedLine {
                            spans: vec![ratatui::text::Span::styled(
                                "Press Ctrl+C again to exit".to_string(),
                                state.theme.secondary,
                            )],
                            indent: 0,
                        }],
                        timestamp: Utc::now(),
                        usage: None,
                    });
                }
            } else {
                // First Ctrl+C while idle: show hint
                state.last_ctrl_c = Some(std::time::Instant::now());
                state.input.clear();
                state.message_list.push(DisplayMessage {
                    role: MessageRole::System,
                    content: vec![RenderedLine {
                        spans: vec![ratatui::text::Span::styled(
                            "Press Ctrl+C again to exit".to_string(),
                            state.theme.secondary,
                        )],
                        indent: 0,
                    }],
                    timestamp: Utc::now(),
                    usage: None,
                });
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
            state.overlay = Overlay::None;
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
