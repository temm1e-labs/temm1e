//! Main chat view — message list + input area + status bar + activity panel.

use ratatui::buffer::Buffer;
use ratatui::layout::{Constraint, Direction, Layout, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Widget};

use temm1e_agent::agent_task_status::AgentTaskPhase;

use crate::app::AppState;
use crate::widgets::input_area::InputArea;
use crate::widgets::status_bar::StatusBar;

/// Render the chat view.
pub fn render_chat(state: &AppState, area: Rect, buf: &mut Buffer) {
    let activity_height = state.activity_panel.height();
    let thinking_height = if state.is_agent_working && activity_height == 0 {
        1
    } else {
        activity_height
    };
    let input_height = (state.input.lines.len() as u16).clamp(1, 10);
    let status_height = 1u16;
    let hint_height = 1u16;

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),
            Constraint::Length(thinking_height),
            Constraint::Length(input_height + 1),
            Constraint::Length(hint_height),
            Constraint::Length(status_height),
        ])
        .split(area);

    // === Messages ===
    let msg_area = chunks[0];
    let view_height = msg_area.height as usize;

    // Render all message lines (already wrapped by markdown renderer)
    let mut all_lines: Vec<Line<'static>> = state.message_list.render_lines(
        state.theme.prompt,
        state.theme.text,
        state.theme.secondary,
        state.theme.secondary,
    );

    // Append in-progress streaming content if agent is working
    if state.is_agent_working {
        if let Some(ref renderer) = state.streaming_renderer {
            if !renderer.is_empty() {
                for rl in renderer.lines() {
                    all_lines.push(Line::from(rl.spans.clone()));
                }
            }
        }
    }

    let total = all_lines.len();

    // Scroll: offset 0 = bottom, higher = further back in history
    // Clamp to max_offset so the viewport is always full
    let max_offset = total.saturating_sub(view_height);
    let offset = state.message_list.scroll_offset.min(max_offset);
    let end = total.saturating_sub(offset);
    let start = end.saturating_sub(view_height);

    for (i, line) in all_lines[start..end].iter().enumerate() {
        let y = msg_area.top() + i as u16;
        if y < msg_area.bottom() {
            buf.set_line(msg_area.left(), y, line, msg_area.width);
        }
    }

    // Scroll indicator
    if offset > 0 && msg_area.width > 20 {
        let indicator = format!(" \u{2191} {} more lines ", offset);
        let ind_line = Line::from(Span::styled(
            indicator,
            state.theme.secondary.add_modifier(Modifier::DIM),
        ));
        buf.set_line(
            msg_area.right().saturating_sub(25),
            msg_area.bottom().saturating_sub(1),
            &ind_line,
            25,
        );
    }

    // === Activity/thinking ===
    if activity_height > 0 {
        let panel_lines = state.activity_panel.render_lines(
            state.theme.phase_done,
            state.theme.phase_active,
            state.theme.phase_pending,
            state.theme.tool_running,
            state.theme.info,
            state.theme.error,
        );
        for (i, line) in panel_lines.iter().enumerate() {
            let y = chunks[1].top() + i as u16;
            if y < chunks[1].bottom() {
                buf.set_line(chunks[1].left(), y, line, chunks[1].width);
            }
        }
    } else if state.is_agent_working && thinking_height > 0 {
        // B5 — richer collapsed thinking line with live tool info
        let elapsed = state.activity_panel.elapsed();
        let tools_count = state.activity_panel.tool_calls.len();
        let phase_display = match &state.activity_panel.phase {
            AgentTaskPhase::Preparing => format!("preparing · {:.1}s", elapsed.as_secs_f64()),
            AgentTaskPhase::Classifying => format!("classifying · {:.1}s", elapsed.as_secs_f64()),
            AgentTaskPhase::CallingProvider { round } => {
                if *round <= 1 {
                    format!("thinking · {:.0}s", elapsed.as_secs_f64())
                } else {
                    format!(
                        "thinking (round {}) · {} tools · {:.0}s total",
                        round,
                        tools_count,
                        elapsed.as_secs_f64()
                    )
                }
            }
            AgentTaskPhase::ExecutingTool {
                tool_name,
                started_at_ms,
                args_preview,
                ..
            } => {
                let tool_elapsed_ms = (elapsed.as_millis() as u64).saturating_sub(*started_at_ms);
                let preview = if args_preview.is_empty() {
                    String::new()
                } else {
                    format!(" {}", args_preview)
                };
                format!(
                    "{}{} · {:.1}s · {} tools · {:.0}s total",
                    tool_name,
                    preview,
                    tool_elapsed_ms as f64 / 1000.0,
                    tools_count,
                    elapsed.as_secs_f64()
                )
            }
            AgentTaskPhase::ToolCompleted {
                tool_name,
                duration_ms,
                ok,
                ..
            } => {
                let sym = if *ok { "✓" } else { "✗" };
                format!(
                    "{} {} ({}ms) · {} tools · {:.0}s total",
                    sym,
                    tool_name,
                    duration_ms,
                    tools_count,
                    elapsed.as_secs_f64()
                )
            }
            AgentTaskPhase::Finishing => format!("finishing · {:.1}s", elapsed.as_secs_f64()),
            AgentTaskPhase::Done => format!(
                "done · {} tools · {:.0}s",
                tools_count,
                elapsed.as_secs_f64()
            ),
            AgentTaskPhase::Interrupted { round } => format!(
                "cancelled at round {} · {} tools · {:.0}s",
                round,
                tools_count,
                elapsed.as_secs_f64()
            ),
        };

        let (symbol, sym_style) = match &state.activity_panel.phase {
            AgentTaskPhase::Preparing | AgentTaskPhase::Classifying | AgentTaskPhase::Finishing => {
                ("⧖", state.theme.phase_active)
            }
            AgentTaskPhase::CallingProvider { .. } => ("◐", state.theme.phase_active),
            AgentTaskPhase::ExecutingTool { .. } => ("▸", state.theme.tool_running),
            AgentTaskPhase::ToolCompleted { ok, .. } => {
                if *ok {
                    ("✓", state.theme.phase_done)
                } else {
                    ("✗", state.theme.error)
                }
            }
            AgentTaskPhase::Done => ("✓", state.theme.phase_done),
            AgentTaskPhase::Interrupted { .. } => ("⊗", state.theme.error),
        };

        let line = Line::from(vec![
            Span::styled(format!(" {} ", symbol), sym_style),
            Span::styled(
                phase_display,
                state.theme.phase_active.add_modifier(Modifier::DIM),
            ),
        ]);
        buf.set_line(chunks[1].left(), chunks[1].top(), &line, chunks[1].width);
    }

    // === Input ===
    let input_block = Block::default()
        .borders(Borders::TOP)
        .border_style(state.theme.border);
    let input_inner = input_block.inner(chunks[2]);
    input_block.render(chunks[2], buf);

    InputArea::new(&state.input)
        .prompt("tem> ")
        .prompt_style(state.theme.prompt)
        .text_style(state.theme.text)
        .cursor_style(state.theme.input_cursor)
        .render(input_inner, buf);

    // === Hint bar ===
    crate::widgets::hint_bar::render_hint_bar(state, chunks[3], buf);

    // === Status bar ===
    StatusBar::new(state).render(chunks[4], buf);
}
