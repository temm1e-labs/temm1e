//! Collapsible agent activity/observability panel.

use std::time::{Duration, Instant};

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

use temm1e_agent::agent_task_status::{AgentTaskPhase, AgentTaskStatus};

use super::spinner::Spinner;

/// Status of a tracked tool call.
#[derive(Debug, Clone)]
pub enum ToolCallStatus {
    Running,
    Success,
    Failed(String),
}

/// A tracked tool call in the activity panel.
#[derive(Debug, Clone)]
pub struct ToolCallEntry {
    pub name: String,
    pub args_summary: String,
    pub status: ToolCallStatus,
    pub output_lines: Vec<String>,
    pub expanded: bool,
    pub started_at: Instant,
    pub elapsed: Duration,
}

/// The activity/observability panel state.
#[derive(Debug)]
pub struct ActivityPanel {
    pub phase: AgentTaskPhase,
    pub started_at: Instant,
    pub tool_calls: Vec<ToolCallEntry>,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: f64,
    pub expanded: bool,
    pub spinner: Spinner,
}

impl Default for ActivityPanel {
    fn default() -> Self {
        Self {
            phase: AgentTaskPhase::Preparing,
            started_at: Instant::now(),
            tool_calls: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
            expanded: false,
            spinner: Spinner::default(),
        }
    }
}

impl ActivityPanel {
    pub fn new() -> Self {
        Self::default()
    }

    /// Reset for a new agent task.
    pub fn reset(&mut self) {
        self.phase = AgentTaskPhase::Preparing;
        self.started_at = Instant::now();
        self.tool_calls.clear();
        self.input_tokens = 0;
        self.output_tokens = 0;
        self.cost_usd = 0.0;
    }

    /// Update from an AgentTaskStatus snapshot.
    pub fn update_status(&mut self, status: &AgentTaskStatus) {
        self.phase = status.phase.clone();
        self.input_tokens = status.input_tokens;
        self.output_tokens = status.output_tokens;
        self.cost_usd = status.cost_usd;

        // Track tool calls
        if let AgentTaskPhase::ExecutingTool {
            tool_name,
            tool_index,
            ..
        } = &status.phase
        {
            if self.tool_calls.len() <= *tool_index as usize
                || self.tool_calls.last().map(|t| &t.name) != Some(tool_name)
            {
                self.tool_calls.push(ToolCallEntry {
                    name: tool_name.clone(),
                    args_summary: String::new(),
                    status: ToolCallStatus::Running,
                    output_lines: Vec::new(),
                    expanded: true,
                    started_at: Instant::now(),
                    elapsed: Duration::ZERO,
                });
            }
        }
    }

    pub fn toggle(&mut self) {
        self.expanded = !self.expanded;
    }

    pub fn tick(&mut self) {
        self.spinner.tick();
        // Update elapsed on running tool
        if let Some(entry) = self.tool_calls.last_mut() {
            if matches!(entry.status, ToolCallStatus::Running) {
                entry.elapsed = entry.started_at.elapsed();
            }
        }
    }

    /// Render the panel to lines (caller handles placement).
    pub fn render_lines(
        &self,
        phase_done: Style,
        phase_active: Style,
        phase_pending: Style,
        tool_style: Style,
        info_style: Style,
        error_style: Style,
    ) -> Vec<Line<'static>> {
        if !self.expanded {
            return Vec::new();
        }

        let mut lines = Vec::new();
        let elapsed = self.started_at.elapsed();

        // Header
        lines.push(Line::from(vec![
            Span::styled(
                format!(
                    "\u{2500}\u{2500} Agent Activity \u{2500}\u{2500} {:.1}s \u{2500}\u{2500} {} tokens ",
                    elapsed.as_secs_f64(),
                    self.input_tokens + self.output_tokens,
                ),
                info_style,
            ),
        ]));

        // Phase indicators
        let phases = [
            ("Preparing", matches!(self.phase, AgentTaskPhase::Preparing)),
            (
                "Classifying",
                matches!(self.phase, AgentTaskPhase::Classifying),
            ),
            (
                "Calling Provider",
                matches!(self.phase, AgentTaskPhase::CallingProvider { .. }),
            ),
            (
                "Executing Tools",
                matches!(self.phase, AgentTaskPhase::ExecutingTool { .. }),
            ),
            ("Finishing", matches!(self.phase, AgentTaskPhase::Finishing)),
        ];

        let mut found_active = false;
        for (name, is_current) in &phases {
            let (icon, style) = if *is_current {
                found_active = true;
                ("\u{25c9}", phase_active)
            } else if found_active {
                ("\u{25cb}", phase_pending)
            } else {
                ("\u{25cf}", phase_done)
            };
            lines.push(Line::from(Span::styled(
                format!(" {} {}", icon, name),
                style,
            )));
        }

        // Tool calls
        for entry in &self.tool_calls {
            let status_icon = match &entry.status {
                ToolCallStatus::Running => "\u{25b6}",
                ToolCallStatus::Success => "\u{2713}",
                ToolCallStatus::Failed(_) => "\u{2717}",
            };
            let status_style = match &entry.status {
                ToolCallStatus::Running => tool_style,
                ToolCallStatus::Success => phase_done,
                ToolCallStatus::Failed(_) => error_style,
            };
            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", status_icon), status_style),
                Span::styled(entry.name.clone(), tool_style),
                Span::styled(
                    format!(" ({:.1}s)", entry.elapsed.as_secs_f64()),
                    info_style.add_modifier(Modifier::DIM),
                ),
            ]));

            // Tool output lines (if expanded)
            if entry.expanded {
                for output_line in entry.output_lines.iter().take(10) {
                    lines.push(Line::from(Span::styled(
                        format!("   \u{2502} {}", output_line),
                        info_style.add_modifier(Modifier::DIM),
                    )));
                }
            }
        }

        lines
    }

    /// How many rows the panel needs when expanded.
    pub fn height(&self) -> u16 {
        if !self.expanded {
            return 0;
        }
        let base = 7; // header + 5 phases + spacing
        let tools: u16 = self
            .tool_calls
            .iter()
            .map(|t| {
                1 + if t.expanded {
                    t.output_lines.len().min(10) as u16
                } else {
                    0
                }
            })
            .sum();
        base + tools
    }
}
