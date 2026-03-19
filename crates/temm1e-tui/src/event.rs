//! Event types for the TUI event loop (TEA architecture).

use temm1e_agent::agent_task_status::AgentTaskStatus;
use temm1e_core::types::message::OutboundMessage;

/// All events the TUI can process.
#[derive(Debug)]
pub enum Event {
    /// Keyboard/mouse/resize from crossterm.
    Terminal(crossterm::event::Event),
    /// Agent status changed (via watch channel).
    AgentStatus(AgentTaskStatus),
    /// Streaming text chunk from the agent.
    StreamChunk(StreamChunk),
    /// User submitted input (from input widget).
    UserSubmit(String),
    /// Agent finished processing — final response.
    AgentResponse(AgentResponseEvent),
    /// Tick for animations (spinner, elapsed time).
    Tick,
}

/// A chunk of streamed text from the agent.
#[derive(Debug, Clone)]
pub struct StreamChunk {
    /// The delta text to append.
    pub delta: String,
    /// Whether this is the final chunk.
    pub done: bool,
}

/// Agent response event with full message and usage info.
#[derive(Debug, Clone)]
pub struct AgentResponseEvent {
    pub message: OutboundMessage,
    pub input_tokens: u32,
    pub output_tokens: u32,
    pub cost_usd: f64,
}

/// Tool execution notification for the activity panel.
#[derive(Debug, Clone)]
pub enum ToolNotification {
    Started { name: String, args_summary: String },
    OutputLine { line: String },
    Completed { name: String, success: bool },
}
