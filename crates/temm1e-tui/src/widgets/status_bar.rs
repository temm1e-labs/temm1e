//! Bottom status bar — 3-section horizontal layout.
//!
//! Left   : session state indicator (idle / thinking / tool name / cancelled).
//! Center : model · provider · tokens · cost.
//! Right  : context-window usage meter · git repo/branch.
//!
//! Uses ratatui `Layout::horizontal` so the three sections physically
//! cannot overlap. Each section renders inside its own bounded `Rect`
//! and is independently truncated if it doesn't fit.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Widget;

use temm1e_agent::agent_task_status::AgentTaskPhase;

use crate::app::AppState;

/// Fixed width of the left (state indicator) section.
///
/// Enough for `◉ tool:browser_ag` (16 chars) plus a 2-char leading
/// separator. Truncated cleanly if the tool name is longer.
const LEFT_WIDTH: u16 = 20;

/// Fixed width of the right (context meter + git) section.
///
/// Enough for the 10-block meter (`▓▓▓▓▓▓▓▓▓▓`) + ` 100% ` + `▣ repo · branch`
/// with a reasonable repo/branch length. Terminals narrower than
/// `LEFT_WIDTH + RIGHT_WIDTH + 10` will crop the center.
const RIGHT_WIDTH: u16 = 48;

/// Renders the status bar at the bottom of the screen.
pub struct StatusBar<'a> {
    state: &'a AppState,
}

impl<'a> StatusBar<'a> {
    pub fn new(state: &'a AppState) -> Self {
        Self { state }
    }
}

impl<'a> Widget for StatusBar<'a> {
    fn render(self, area: Rect, buf: &mut Buffer) {
        if area.height == 0 || area.width == 0 {
            return;
        }

        let style = self.state.theme.status_bar;

        // Fill background across the whole bar
        for x in area.left()..area.right() {
            buf[(x, area.top())].set_style(style);
        }

        // Split horizontally into three sections — ratatui guarantees
        // they are non-overlapping, so a center overflow truncates
        // instead of writing over the right section.
        //
        // On terminals narrower than LEFT + RIGHT + 10, the center
        // gets squeezed to its Min but the left/right are preserved.
        // On very narrow terminals where left + right exceed width,
        // the layout engine gives all rects the same `area.y/height`
        // and shrinks widths proportionally.
        let chunks = Layout::horizontal([
            Constraint::Length(LEFT_WIDTH),
            Constraint::Min(10),
            Constraint::Length(RIGHT_WIDTH),
        ])
        .split(area);

        render_left(self.state, chunks[0], buf);
        render_center(self.state, chunks[1], buf);
        render_right(self.state, chunks[2], buf);
    }
}

// ── LEFT — session state indicator ─────────────────────────────

fn render_left(state: &AppState, area: Rect, buf: &mut Buffer) {
    if area.width == 0 {
        return;
    }
    let style = state.theme.status_bar;
    let accent = state.theme.accent;
    let tool_running = state.theme.tool_running;
    let secondary = state.theme.secondary;
    let error = state.theme.error;

    let (symbol, label, sym_style) = state_indicator(state, accent, tool_running, secondary, error);

    let line = Line::from(vec![
        Span::styled(" ", style),
        Span::styled(symbol.to_string(), sym_style),
        Span::styled(" ", style),
        Span::styled(label, sym_style),
    ]);
    buf.set_line(area.left(), area.top(), &line, area.width);
}

fn state_indicator(
    state: &AppState,
    accent: Style,
    tool_running: Style,
    secondary: Style,
    error: Style,
) -> (&'static str, String, Style) {
    if !state.is_agent_working {
        if matches!(
            state.activity_panel.phase,
            AgentTaskPhase::Interrupted { .. }
        ) {
            return ("⊗", "cancelled".to_string(), error);
        }
        return (
            "●",
            "idle".to_string(),
            secondary.add_modifier(Modifier::DIM),
        );
    }

    match &state.activity_panel.phase {
        AgentTaskPhase::Preparing | AgentTaskPhase::Classifying => {
            ("◐", "preparing".to_string(), accent)
        }
        AgentTaskPhase::CallingProvider { .. } => ("◐", "thinking".to_string(), accent),
        AgentTaskPhase::ExecutingTool { tool_name, .. } => {
            let truncated: String = tool_name.chars().take(12).collect();
            ("◉", format!("tool:{truncated}"), tool_running)
        }
        AgentTaskPhase::ToolCompleted { .. } => ("◐", "thinking".to_string(), accent),
        AgentTaskPhase::Finishing => ("⧖", "finishing".to_string(), accent),
        AgentTaskPhase::Done => (
            "●",
            "idle".to_string(),
            secondary.add_modifier(Modifier::DIM),
        ),
        AgentTaskPhase::Interrupted { .. } => ("⊗", "cancelled".to_string(), error),
    }
}

// ── CENTER — model · provider · tokens · cost ─────────────────

fn render_center(state: &AppState, area: Rect, buf: &mut Buffer) {
    if area.width == 0 {
        return;
    }
    let style = state.theme.status_bar;
    let accent = state.theme.accent;
    let info = state.theme.info;
    let secondary = state.theme.secondary;

    let mut spans: Vec<Span<'static>> = Vec::new();
    let model = state.current_model.clone().unwrap_or_default();
    let provider = state.current_provider.clone().unwrap_or_default();

    if !model.is_empty() {
        spans.push(Span::styled(model, accent));
    }
    if !provider.is_empty() {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ".to_string(), secondary));
        }
        spans.push(Span::styled(provider, style));
    }

    let ti = state.token_counter.total_input_tokens;
    let to = state.token_counter.total_output_tokens;
    if ti > 0 || to > 0 {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ".to_string(), secondary));
        }
        spans.push(Span::styled(
            format!("{}in/{}out", format_tokens_u32(ti), format_tokens_u32(to)),
            info,
        ));
    }

    let cost = state.token_counter.total_cost_usd;
    if cost > 0.0 {
        if !spans.is_empty() {
            spans.push(Span::styled(" · ".to_string(), secondary));
        }
        spans.push(Span::styled(format!("${:.4}", cost), info));
    }

    // Center the content within `area` horizontally.
    let text_width: u16 = spans.iter().map(|s| s.content.chars().count() as u16).sum();
    let x = if text_width >= area.width {
        area.left()
    } else {
        area.left() + (area.width - text_width) / 2
    };

    let line = Line::from(spans);
    // Clip to the remaining width so we never bleed past `area.right()`.
    let avail = area.width.saturating_sub(x.saturating_sub(area.left()));
    buf.set_line(x, area.top(), &line, avail);
}

// ── RIGHT — context meter + git repo/branch ────────────────────

fn render_right(state: &AppState, area: Rect, buf: &mut Buffer) {
    if area.width == 0 {
        return;
    }
    let accent = state.theme.accent;
    let secondary = state.theme.secondary;
    let error = state.theme.error;

    let mut spans: Vec<Span<'static>> = Vec::new();

    // Context window meter (D5).
    //
    // Uses `turn_input_tokens` — the size of the MOST RECENT request's
    // input context — not `total_input_tokens` (which accumulates
    // every request's size across the whole session and is the wrong
    // metric for "how full is my context window right now"). The
    // center section still shows the cumulative totals for billing.
    if let Some(ref model) = state.current_model {
        use temm1e_core::types::model_registry::model_limits;
        let (ctx_window, _) = model_limits(model);
        if ctx_window > 0 {
            let used = state.token_counter.turn_input_tokens as u64;
            let window = ctx_window as u64;
            let pct = ((used * 100) / window.max(1)).min(100);
            let filled = ((pct * 10) / 100) as usize;
            let meter: String = (0..10)
                .map(|i| if i < filled { '▓' } else { '░' })
                .collect();
            let meter_style = if pct >= 95 {
                error
            } else if pct >= 80 {
                error.add_modifier(Modifier::DIM)
            } else {
                secondary
            };
            spans.push(Span::styled(meter, meter_style));
            spans.push(Span::styled(format!(" {pct}% "), meter_style));
        }
    }

    // Git repo + branch (A3)
    if let Some(ref git) = state.git_info {
        spans.push(Span::styled("▣ ".to_string(), secondary));
        spans.push(Span::styled(git.repo_name.clone(), accent));
        spans.push(Span::styled(" · ".to_string(), secondary));
        spans.push(Span::styled(git.branch.clone(), secondary));
    }

    if spans.is_empty() {
        return;
    }

    // Right-align manually — place the content at the end of `area`
    // and let ratatui's set_line crop on the left if the content is
    // wider than the area.
    let text_width: u16 = spans.iter().map(|s| s.content.chars().count() as u16).sum();
    let x = if text_width >= area.width {
        area.left()
    } else {
        area.right() - text_width
    };

    let line = Line::from(spans);
    let avail = area.width.saturating_sub(x.saturating_sub(area.left()));
    buf.set_line(x, area.top(), &line, avail);
}

fn format_tokens_u32(n: u32) -> String {
    if n >= 1_000_000 {
        format!("{:.1}M", n as f64 / 1_000_000.0)
    } else if n >= 1_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else {
        n.to_string()
    }
}
