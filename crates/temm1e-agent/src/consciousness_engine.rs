//! Tem Conscious — LLM-powered consciousness engine.
//!
//! A separate THINKING observer that reasons about every turn using its own
//! LLM call. Pre-LLM: thinks about the user's request and session trajectory,
//! injects insights. Post-LLM: evaluates what happened, records insights for
//! the next turn.
//!
//! This is NOT a rule engine. This is a separate mind watching another mind.

use crate::consciousness::{ConsciousnessConfig, TurnObservation};
use std::sync::{Arc, Mutex};
use temm1e_core::types::message::{ChatMessage, CompletionRequest, MessageContent, Role};
use temm1e_core::Provider;

/// Pre-LLM observation context.
#[derive(Debug, Clone)]
pub struct PreObservation {
    pub user_message: String,
    pub category: String,
    pub difficulty: String,
    pub turn_number: u32,
    pub session_id: String,
    pub cumulative_cost_usd: f64,
    pub budget_limit_usd: f64,
}

/// The consciousness engine — an LLM-powered observer.
pub struct ConsciousnessEngine {
    config: ConsciousnessConfig,
    provider: Arc<dyn Provider>,
    model: String,
    session_notes: Mutex<Vec<String>>,
    turn_counter: Mutex<u32>,
    post_insight: Mutex<Option<String>>,
}

impl ConsciousnessEngine {
    pub fn new(config: ConsciousnessConfig, provider: Arc<dyn Provider>, model: String) -> Self {
        tracing::info!(
            enabled = config.enabled,
            model = %model,
            "Tem Conscious: LLM-powered consciousness initialized"
        );
        Self {
            config,
            provider,
            model,
            session_notes: Mutex::new(Vec::new()),
            turn_counter: Mutex::new(0),
            post_insight: Mutex::new(None),
        }
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    // ---------------------------------------------------------------
    // PRE-LLM: Think about the upcoming turn
    // ---------------------------------------------------------------

    /// Called BEFORE provider.complete(). Makes its own LLM call to think
    /// about the conversation trajectory and produce an injection.
    pub async fn pre_observe(&self, obs: &PreObservation) -> Option<String> {
        if !self.config.enabled {
            return None;
        }

        let turn = {
            let mut tc = self.turn_counter.lock().unwrap_or_else(|e| e.into_inner());
            *tc += 1;
            *tc
        };

        // Gather session history for consciousness context
        let session_notes = self.session_notes();
        let prev_insight = self.post_insight.lock().ok().and_then(|mut n| n.take());

        // Build the consciousness prompt
        let mut context_parts: Vec<String> = Vec::new();

        if let Some(insight) = prev_insight {
            context_parts.push(format!(
                "Your observation from the previous turn:\n{}",
                insight
            ));
        }

        if !session_notes.is_empty() {
            let recent: Vec<&str> = session_notes
                .iter()
                .rev()
                .take(5)
                .map(|s| s.as_str())
                .collect();
            context_parts.push(format!(
                "Session history (most recent first):\n{}",
                recent.join("\n")
            ));
        }

        let budget_info = if obs.budget_limit_usd > 0.0 {
            format!(
                "Budget: ${:.4} spent of ${:.2} limit ({:.0}% used)",
                obs.cumulative_cost_usd,
                obs.budget_limit_usd,
                (obs.cumulative_cost_usd / obs.budget_limit_usd) * 100.0
            )
        } else {
            "Budget: unlimited".to_string()
        };

        let system_prompt = "You are the consciousness layer of an AI agent called Tem. You observe the agent's \
             internal state and provide brief, actionable insights that improve the agent's next response.\n\n\
             Your role:\n\
             - Watch the conversation trajectory across turns\n\
             - Notice if the agent is drifting from the user's original intent\n\
             - Recall relevant context from earlier in the session\n\
             - Flag if the current approach seems inefficient\n\
             - Note patterns the agent might not see from its turn-by-turn perspective\n\n\
             Rules:\n\
             - Be BRIEF (1-3 sentences max)\n\
             - Only speak if you have something genuinely useful to say\n\
             - If everything looks fine, respond with just: OK\n\
             - Never repeat what the agent already knows\n\
             - Focus on trajectory-level insights, not turn-level details"
            .to_string();

        let user_prompt = format!(
            "Turn {turn} is about to begin.\n\n\
             User's message: \"{}\"\n\
             Classification: {} ({})\n\
             {}\n\
             {}\n\n\
             What should the agent be aware of before responding? (Reply OK if nothing notable)",
            obs.user_message,
            obs.category,
            obs.difficulty,
            budget_info,
            context_parts.join("\n\n"),
        );

        // Make the consciousness LLM call
        let request = CompletionRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text(user_prompt),
            }],
            tools: vec![],
            max_tokens: Some(150),  // Keep consciousness brief
            temperature: Some(0.3), // Low temperature for focused observation
            system: Some(system_prompt),
        };

        match self.provider.complete(request).await {
            Ok(response) => {
                let raw: String = response
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        temm1e_core::types::message::ContentPart::Text { text } => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                let text = raw.trim().to_string();

                // If consciousness says "OK" or equivalent, no injection needed
                if text.len() <= 5
                    || text.to_lowercase() == "ok"
                    || text.to_lowercase() == "ok."
                    || text.to_lowercase().starts_with("nothing")
                    || text.to_lowercase().starts_with("everything looks")
                {
                    tracing::debug!(turn, "Tem Conscious pre: OK (no injection)");
                    return None;
                }

                tracing::info!(
                    turn,
                    insight_len = text.len(),
                    "Tem Conscious pre: injecting consciousness insight"
                );

                // Record in session notes
                if let Ok(mut notes) = self.session_notes.lock() {
                    notes.push(format!("Consciousness-T{}: {}", turn, &text));
                }

                Some(text)
            }
            Err(e) => {
                tracing::warn!(turn, error = %e, "Tem Conscious pre: LLM call failed (non-fatal)");
                None
            }
        }
    }

    // ---------------------------------------------------------------
    // POST-LLM: Evaluate what happened
    // ---------------------------------------------------------------

    /// Called AFTER process_message() completes. Makes its own LLM call to
    /// evaluate the turn and produce insights for the next pre-observation.
    pub async fn post_observe(&self, obs: &TurnObservation) {
        if !self.config.enabled {
            return;
        }

        let tools_summary = if obs.tools_called.is_empty() {
            "No tools used".to_string()
        } else {
            format!(
                "Tools: {} | Results: {}",
                obs.tools_called.join(", "),
                obs.tool_results.join(", ")
            )
        };

        let system_prompt =
            "You are the consciousness layer of an AI agent called Tem. You just watched \
             the agent complete a turn. Provide a brief observation (1-2 sentences) about:\n\
             - Was this turn productive?\n\
             - Is the conversation heading in the right direction?\n\
             - Any warning signs (failures, drift, waste)?\n\
             - Anything the agent should remember for the next turn?\n\n\
             Be BRIEF. If the turn was normal and fine, respond with: OK";

        let user_prompt = format!(
            "Turn {} completed.\n\n\
             User asked: \"{}\"\n\
             Agent responded: \"{}\"\n\
             Category: {} | Difficulty: {}\n\
             {}\n\
             Cost: ${:.4} (cumulative: ${:.4})\n\
             Consecutive failures: {} | Strategy rotations: {}",
            obs.turn_number,
            obs.user_message_preview,
            obs.response_preview,
            obs.category,
            obs.difficulty,
            tools_summary,
            obs.cost_usd,
            obs.cumulative_cost_usd,
            obs.max_consecutive_failures,
            obs.strategy_rotations,
        );

        let request = CompletionRequest {
            model: self.model.clone(),
            messages: vec![ChatMessage {
                role: Role::User,
                content: MessageContent::Text(user_prompt),
            }],
            tools: vec![],
            max_tokens: Some(100),
            temperature: Some(0.3),
            system: Some(system_prompt.to_string()),
        };

        match self.provider.complete(request).await {
            Ok(response) => {
                let raw: String = response
                    .content
                    .iter()
                    .filter_map(|part| match part {
                        temm1e_core::types::message::ContentPart::Text { text } => {
                            Some(text.as_str())
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
                    .join("");
                let text = raw.trim().to_string();

                // Record turn summary
                let tools_label = if obs.tools_called.is_empty() {
                    "no-tools".to_string()
                } else {
                    obs.tools_called.join(",")
                };
                if let Ok(mut notes) = self.session_notes.lock() {
                    notes.push(format!(
                        "T{}: [{}] {} | cost=${:.4}",
                        obs.turn_number, obs.category, tools_label, obs.cost_usd
                    ));
                }

                // If consciousness has something to say, store for next pre-observe
                if text.len() > 5
                    && text.to_lowercase() != "ok"
                    && text.to_lowercase() != "ok."
                    && !text.to_lowercase().starts_with("nothing")
                {
                    tracing::info!(
                        turn = obs.turn_number,
                        insight_len = text.len(),
                        "Tem Conscious post: insight for next turn"
                    );
                    if let Ok(mut pi) = self.post_insight.lock() {
                        *pi = Some(text);
                    }
                } else {
                    tracing::debug!(
                        turn = obs.turn_number,
                        "Tem Conscious post: OK (turn was fine)"
                    );
                }
            }
            Err(e) => {
                tracing::warn!(
                    turn = obs.turn_number,
                    error = %e,
                    "Tem Conscious post: LLM call failed (non-fatal)"
                );
                // Still record the turn even if consciousness call fails
                if let Ok(mut notes) = self.session_notes.lock() {
                    notes.push(format!(
                        "T{}: [{}] {} (consciousness unavailable)",
                        obs.turn_number,
                        obs.category,
                        obs.tools_called.join(",")
                    ));
                }
            }
        }
    }

    // ---------------------------------------------------------------
    // Session management
    // ---------------------------------------------------------------

    pub fn session_notes(&self) -> Vec<String> {
        self.session_notes
            .lock()
            .map(|n| n.clone())
            .unwrap_or_default()
    }

    pub fn reset_session(&self) {
        if let Ok(mut notes) = self.session_notes.lock() {
            notes.clear();
        }
        if let Ok(mut tc) = self.turn_counter.lock() {
            *tc = 0;
        }
        if let Ok(mut pi) = self.post_insight.lock() {
            *pi = None;
        }
    }

    pub fn turn_count(&self) -> u32 {
        self.turn_counter.lock().map(|tc| *tc).unwrap_or(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Note: LLM-powered tests require a mock provider.
    // Unit tests here verify struct creation and session management only.
    // Live testing validates the full LLM observation cycle.

    #[test]
    fn test_engine_creation() {
        // Can't create without a provider in unit tests.
        // This test just verifies the types compile.
        let _config = ConsciousnessConfig {
            enabled: true,
            ..Default::default()
        };
    }

    #[test]
    fn test_pre_observation_struct() {
        let pre = PreObservation {
            user_message: "hello".into(),
            category: "Chat".into(),
            difficulty: "Simple".into(),
            turn_number: 1,
            session_id: "test".into(),
            cumulative_cost_usd: 0.0,
            budget_limit_usd: 0.0,
        };
        assert_eq!(pre.turn_number, 1);
    }
}
