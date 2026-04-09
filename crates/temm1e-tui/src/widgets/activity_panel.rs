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
    /// First-line result preview (populated on ToolCompleted).
    pub result_preview: String,
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
    /// Frozen elapsed duration — set when the turn reaches Done or
    /// Interrupted, cleared on `reset()`. While set, `elapsed()` returns
    /// this value instead of the live wall-clock. Prevents the counter
    /// from creeping up between turns when the user opens overlays or
    /// presses non-action keys.
    pub frozen_elapsed: Option<Duration>,
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
            frozen_elapsed: None,
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
        self.frozen_elapsed = None;
    }

    /// Return the elapsed time to display — frozen snapshot if the
    /// turn has reached a terminal phase (Done/Interrupted), otherwise
    /// live wall-clock since `started_at`.
    pub fn elapsed(&self) -> Duration {
        self.frozen_elapsed
            .unwrap_or_else(|| self.started_at.elapsed())
    }

    /// Update from an AgentTaskStatus snapshot.
    pub fn update_status(&mut self, status: &AgentTaskStatus) {
        self.phase = status.phase.clone();
        self.input_tokens = status.input_tokens;
        self.output_tokens = status.output_tokens;
        self.cost_usd = status.cost_usd;

        match &status.phase {
            // ── Tool started — push a new entry or reuse the current one ─
            AgentTaskPhase::ExecutingTool {
                tool_name,
                tool_index,
                args_preview,
                ..
            } => {
                let should_push = self.tool_calls.len() <= *tool_index as usize
                    || self
                        .tool_calls
                        .last()
                        .map(|t| &t.name)
                        .map(|n| n != tool_name)
                        .unwrap_or(true)
                    || self
                        .tool_calls
                        .last()
                        .map(|t| !matches!(t.status, ToolCallStatus::Running))
                        .unwrap_or(false);
                if should_push {
                    self.tool_calls.push(ToolCallEntry {
                        name: tool_name.clone(),
                        args_summary: args_preview.clone(),
                        status: ToolCallStatus::Running,
                        output_lines: Vec::new(),
                        expanded: true,
                        started_at: Instant::now(),
                        elapsed: Duration::ZERO,
                        result_preview: String::new(),
                    });
                } else if let Some(last) = self.tool_calls.last_mut() {
                    // Same in-flight entry — keep args up to date
                    last.args_summary = args_preview.clone();
                }
            }
            // ── Tool finished — mark the most recent matching entry ─────
            AgentTaskPhase::ToolCompleted {
                tool_name,
                duration_ms,
                ok,
                result_preview,
                ..
            } => {
                if let Some(entry) =
                    self.tool_calls.iter_mut().rev().find(|t| {
                        &t.name == tool_name && matches!(t.status, ToolCallStatus::Running)
                    })
                {
                    entry.status = if *ok {
                        ToolCallStatus::Success
                    } else {
                        ToolCallStatus::Failed(result_preview.clone())
                    };
                    entry.elapsed = Duration::from_millis(*duration_ms);
                    entry.result_preview = result_preview.clone();
                }
            }
            // ── Interrupted — mark all running entries as cancelled ─────
            AgentTaskPhase::Interrupted { .. } => {
                for entry in self.tool_calls.iter_mut() {
                    if matches!(entry.status, ToolCallStatus::Running) {
                        entry.status = ToolCallStatus::Failed("[cancelled]".to_string());
                        entry.result_preview = "[cancelled]".to_string();
                    }
                }
                // Freeze the displayed elapsed so it stops creeping.
                self.frozen_elapsed = Some(self.started_at.elapsed());
            }
            // ── Done — freeze the elapsed so the counter doesn't creep ─
            AgentTaskPhase::Done => {
                self.frozen_elapsed = Some(self.started_at.elapsed());
            }
            _ => {}
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
        let elapsed = self.elapsed();

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

        // Phase indicators.
        //
        // NOTE: the "Executing Tools" row also matches ToolCompleted —
        // a tool completion is a transient state between tool runs,
        // still visually "on" the Executing Tools step until the next
        // provider call. Without this, the stepper would silently
        // render every phase as "done" when ToolCompleted fires. (B2.)
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
                matches!(
                    self.phase,
                    AgentTaskPhase::ExecutingTool { .. } | AgentTaskPhase::ToolCompleted { .. }
                ),
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

        // Tool calls — streaming trace (last 5 visible)
        lines.push(Line::from(""));
        let visible_count = self.tool_calls.len().min(5);
        let start_idx = self.tool_calls.len().saturating_sub(5);
        for entry in &self.tool_calls[start_idx..start_idx + visible_count] {
            let (status_icon, status_style) = match &entry.status {
                ToolCallStatus::Running => ("\u{25b8}", tool_style), // ▸
                ToolCallStatus::Success => ("\u{2713}", phase_done),
                ToolCallStatus::Failed(_) => ("\u{2717}", error_style),
            };
            let duration_display = match &entry.status {
                ToolCallStatus::Running => format!("{:.1}s ⧖", entry.elapsed.as_secs_f64()),
                _ => {
                    let ms = entry.elapsed.as_millis();
                    if ms >= 1000 {
                        format!("{:.1}s", ms as f64 / 1000.0)
                    } else {
                        format!("{ms}ms")
                    }
                }
            };

            // Show args preview while running; result preview after
            let detail: String = match &entry.status {
                ToolCallStatus::Running => {
                    let ap: String = entry.args_summary.chars().take(56).collect();
                    if ap.is_empty() {
                        String::new()
                    } else {
                        format!(" {ap}")
                    }
                }
                _ => {
                    let rp: String = entry.result_preview.chars().take(56).collect();
                    if rp.is_empty() {
                        String::new()
                    } else {
                        format!(" → {rp}")
                    }
                }
            };

            lines.push(Line::from(vec![
                Span::styled(format!(" {} ", status_icon), status_style),
                Span::styled(entry.name.clone(), tool_style),
                Span::styled(detail, info_style.add_modifier(Modifier::DIM)),
                Span::styled(
                    format!("  {duration_display}"),
                    info_style.add_modifier(Modifier::DIM),
                ),
            ]));
        }
        if self.tool_calls.len() > 5 {
            lines.push(Line::from(Span::styled(
                format!(
                    "    · {} earlier tool call(s) hidden",
                    self.tool_calls.len() - 5
                ),
                info_style.add_modifier(Modifier::DIM),
            )));
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
