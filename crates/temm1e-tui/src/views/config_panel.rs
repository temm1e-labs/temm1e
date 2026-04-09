//! Configuration overlay panels (model picker, keys, usage, status).
//!
//! Each overlay reads real data from `AppState` (and static registries
//! like `model_registry`) and renders it as a centered popup.

use ratatui::buffer::Buffer;
use ratatui::layout::{Alignment, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Widget, Wrap};

use temm1e_agent::agent_task_status::AgentTaskPhase;
use temm1e_core::types::model_registry;

use crate::app::AppState;
use crate::commands::registry::OverlayKind;

/// Render a config overlay for the given kind.
pub fn render_config_overlay(kind: &OverlayKind, state: &AppState, area: Rect, buf: &mut Buffer) {
    let (title, lines) = match kind {
        OverlayKind::Config => (" Configuration ", render_config_lines(state)),
        OverlayKind::Keys => (" API Keys ", render_keys_lines(state)),
        OverlayKind::Usage => (" Usage ", render_usage_lines(state)),
        OverlayKind::Status => (" Status ", render_status_lines(state)),
        OverlayKind::ModelPicker => (" Models ", render_model_lines(state)),
        OverlayKind::Tools => {
            // Delegate to dedicated tools overlay renderer
            crate::views::tools_overlay::render_tools_overlay(state, area, buf);
            return;
        }
        OverlayKind::Help => return, // /help is rendered by views/help.rs, not here
    };

    let popup_width = 70.min(area.width.saturating_sub(4));
    // Height: header + footer + content lines, clamped to terminal
    let content_rows = (lines.len() as u16).saturating_add(2);
    let popup_height = content_rows.clamp(6, 24).min(area.height.saturating_sub(4));

    let popup = centered_rect(popup_width, popup_height, area);

    Clear.render(popup, buf);

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

// ── Per-overlay content renderers ──────────────────────────────────

fn render_config_lines(state: &AppState) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    lines.push(Line::from(""));
    lines.push(row(
        "Provider",
        state.current_provider.as_deref().unwrap_or("(not set)"),
        state,
    ));
    lines.push(row(
        "Model",
        state.current_model.as_deref().unwrap_or("(not set)"),
        state,
    ));
    lines.push(row(
        "Mode",
        state.selected_mode.as_deref().unwrap_or("play"),
        state,
    ));
    lines.push(row(
        "Terminal",
        &format!("{}x{}", state.terminal_size.0, state.terminal_size.1),
        state,
    ));
    if let Some(ref git) = state.git_info {
        lines.push(row(
            "Repository",
            &format!("{} · {}", git.repo_name, git.branch),
            state,
        ));
    }
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
        lines.push(info("Paste an API key in chat to add one.", state));
    } else {
        for entry in &state.api_keys_cache {
            let marker = if entry.is_active { "● " } else { "  " };
            let style = if entry.is_active {
                state.theme.accent
            } else {
                state.theme.text
            };
            lines.push(Line::from(vec![
                Span::styled("  ", state.theme.secondary),
                Span::styled(marker.to_string(), style),
                Span::styled(format!("{:<14}", entry.provider), style),
                Span::styled(format!("…{}", entry.fingerprint), state.theme.secondary),
            ]));
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
    lines.push(section_header("Session totals", state));
    lines.push(row(
        "Input tokens",
        &format_number(tc.total_input_tokens as u64),
        state,
    ));
    lines.push(row(
        "Output tokens",
        &format_number(tc.total_output_tokens as u64),
        state,
    ));
    lines.push(row(
        "Total cost",
        &format!("${:.4}", tc.total_cost_usd),
        state,
    ));
    lines.push(Line::from(""));
    lines.push(section_header("Current turn", state));
    lines.push(row(
        "Input tokens",
        &format_number(tc.turn_input_tokens as u64),
        state,
    ));
    lines.push(row(
        "Output tokens",
        &format_number(tc.turn_output_tokens as u64),
        state,
    ));
    lines.push(row(
        "Turn cost",
        &format!("${:.4}", tc.turn_cost_usd),
        state,
    ));
    lines.push(Line::from(""));
    lines.push(hint("Press Esc to close", state));
    lines
}

fn render_status_lines(state: &AppState) -> Vec<Line<'static>> {
    let panel = &state.activity_panel;
    let elapsed = panel.elapsed();
    let phase = match &panel.phase {
        AgentTaskPhase::Preparing => "Preparing".to_string(),
        AgentTaskPhase::Classifying => "Classifying".to_string(),
        AgentTaskPhase::CallingProvider { round } => format!("Thinking (round {round})"),
        AgentTaskPhase::ExecutingTool { tool_name, .. } => format!("Running {tool_name}"),
        AgentTaskPhase::ToolCompleted {
            tool_name,
            duration_ms,
            ok,
            ..
        } => {
            let sym = if *ok { "✓" } else { "✗" };
            format!("{sym} {tool_name} ({duration_ms}ms)")
        }
        AgentTaskPhase::Finishing => "Finishing".to_string(),
        AgentTaskPhase::Done => "Idle".to_string(),
        AgentTaskPhase::Interrupted { round } => format!("Cancelled at round {round}"),
    };

    let mut lines = Vec::new();
    lines.push(Line::from(""));
    lines.push(row(
        "State",
        if state.is_agent_working {
            "working"
        } else {
            "idle"
        },
        state,
    ));
    lines.push(row("Phase", &phase, state));
    if state.is_agent_working {
        lines.push(row(
            "Elapsed",
            &format!("{:.1}s", elapsed.as_secs_f64()),
            state,
        ));
    }
    lines.push(row(
        "Tools (session)",
        &state.tool_call_history.len().to_string(),
        state,
    ));
    lines.push(row(
        "Total cost",
        &format!("${:.4}", state.token_counter.total_cost_usd),
        state,
    ));
    if let Some(ref git) = state.git_info {
        lines.push(row(
            "Repository",
            &format!("{} · {}", git.repo_name, git.branch),
            state,
        ));
    }
    lines.push(Line::from(""));
    lines.push(hint("Press Esc to close", state));
    lines
}

fn render_model_lines(state: &AppState) -> Vec<Line<'static>> {
    let provider = state.current_provider.as_deref().unwrap_or("");
    let models = model_registry::available_models_for_provider(provider);

    let mut lines = Vec::new();
    lines.push(Line::from(""));
    if provider.is_empty() {
        lines.push(info("No provider configured.", state));
    } else if models.is_empty() {
        lines.push(info(
            &format!("No models registered for provider '{provider}'."),
            state,
        ));
    } else {
        for model in models {
            let is_current = state.current_model.as_deref() == Some(model);
            let marker = if is_current { "● " } else { "  " };
            let style = if is_current {
                state.theme.accent
            } else {
                state.theme.text
            };
            let (ctx_window, _) = model_registry::model_limits(model);
            let ctx_display = format_tokens(ctx_window as u64);
            lines.push(Line::from(vec![
                Span::styled("  ", state.theme.secondary),
                Span::styled(marker.to_string(), style),
                Span::styled(format!("{:<32}", model), style),
                Span::styled(format!("ctx: {:>7}", ctx_display), state.theme.secondary),
            ]));
        }
    }
    lines.push(Line::from(""));
    lines.push(hint("Press Esc to close", state));
    lines
}

// ── Helpers ────────────────────────────────────────────────────────

fn row(label: &str, value: &str, state: &AppState) -> Line<'static> {
    Line::from(vec![
        Span::styled(format!("  {:<18}", label), state.theme.secondary),
        Span::styled(value.to_string(), state.theme.text),
    ])
}

fn section_header(text: &str, state: &AppState) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), state.theme.heading))
}

fn hint(text: &str, state: &AppState) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), state.theme.secondary))
}

fn info(text: &str, state: &AppState) -> Line<'static> {
    Line::from(Span::styled(format!("  {text}"), state.theme.info))
}

fn format_number(n: u64) -> String {
    let s = n.to_string();
    let mut out = String::with_capacity(s.len() + s.len() / 3);
    for (i, ch) in s.chars().rev().enumerate() {
        if i > 0 && i % 3 == 0 {
            out.push(',');
        }
        out.push(ch);
    }
    out.chars().rev().collect()
}

fn format_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        n.to_string()
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.left() + (area.width.saturating_sub(width)) / 2;
    let y = area.top() + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}
