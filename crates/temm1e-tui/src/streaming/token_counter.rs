//! Real-time token and cost tracking during streaming.

/// Tracks token usage and cost for the current session.
#[derive(Debug, Clone, Default)]
pub struct TokenCounter {
    pub total_input_tokens: u32,
    pub total_output_tokens: u32,
    pub total_cost_usd: f64,
    pub turn_input_tokens: u32,
    pub turn_output_tokens: u32,
    pub turn_cost_usd: f64,
}

impl TokenCounter {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record usage for a completed turn.
    pub fn record_turn(&mut self, input_tokens: u32, output_tokens: u32, cost_usd: f64) {
        self.turn_input_tokens = input_tokens;
        self.turn_output_tokens = output_tokens;
        self.turn_cost_usd = cost_usd;
        self.total_input_tokens += input_tokens;
        self.total_output_tokens += output_tokens;
        self.total_cost_usd += cost_usd;
    }

    /// Reset per-turn counters.
    pub fn reset_turn(&mut self) {
        self.turn_input_tokens = 0;
        self.turn_output_tokens = 0;
        self.turn_cost_usd = 0.0;
    }
}
