//! Queen — task decomposition for the Hive.
//!
//! The Queen is NOT an always-on entity. It's a single LLM call that
//! decomposes a complex message into a task DAG. For simple messages,
//! it recommends single-agent mode (zero overhead).

use tracing::{debug, info};

use temm1e_core::types::error::Temm1eError;

use crate::config::HiveConfig;
use crate::dag;
use crate::types::DecompositionResult;

// ---------------------------------------------------------------------------
// Queen
// ---------------------------------------------------------------------------

/// The Queen: decomposes user requests into parallelizable task DAGs.
pub struct Queen {
    swarm_threshold_speedup: f64,
    queen_cost_ratio_max: f64,
}

impl Queen {
    pub fn new(config: &HiveConfig) -> Self {
        Self {
            swarm_threshold_speedup: config.swarm_threshold_speedup,
            queen_cost_ratio_max: config.queen_cost_ratio_max,
        }
    }

    /// Check whether a message should be decomposed into parallel subtasks.
    ///
    /// This is meant to be called AFTER the v2 LLM classifier has already
    /// categorized the message. The caller provides the classification result.
    ///
    /// Swarm decomposition candidates: `Order` + `Complex` difficulty.
    /// Chat, Stop, Simple, and Standard messages skip the swarm entirely.
    ///
    /// If the classifier is unavailable (v2 disabled), `is_complex_order`
    /// should be set based on a lightweight LLM call — never string matching.
    pub fn should_decompose(is_complex_order: bool) -> bool {
        is_complex_order
    }

    /// Lightweight LLM-based decomposition check. One-shot example enforces
    /// strict format. Returns the prompt to send — caller provides the LLM call.
    pub fn build_should_decompose_prompt(message: &str) -> String {
        format!(
            r#"Does this request contain 3 or more independent subtasks that could be worked on in parallel?

Example input: "Write a function to sort a list, another to reverse a string, and tests for both"
Example output: yes

Example input: "What's the weather like?"
Example output: no

Rules:
- Output ONLY the single word "yes" or "no". Nothing else. No explanation.
- "yes" means 3+ subtasks that don't depend on each other's output.
- "no" means it's a single task, a question, or tasks that must be done sequentially.

Input: "{message}"
Output:"#
        )
    }

    /// Parse the yes/no response from the decomposition check.
    pub fn parse_should_decompose(response: &str) -> bool {
        let lower = response.trim().to_lowercase();
        // Check if response starts with "yes" — handles "yes", "Yes.", "Yes, this can..."
        lower.starts_with("yes")
    }

    /// Build the decomposition prompt for the LLM.
    pub fn build_decomposition_prompt(message: &str) -> String {
        format!(
            r#"You are a task decomposer for an AI agent runtime. Break the user's request into atomic subtasks.

RULES:
1. Each task must be completable by a single agent worker in one tool-use loop
2. Minimize dependencies between tasks — maximize parallelism
3. If the request is simple (1-2 steps), set single_agent_recommended to true
4. Include context_tags for each task (e.g., ["rust", "api", "database"])
5. Estimated tokens should be conservative (overestimate by 20%)
6. Task IDs must be sequential: t1, t2, t3, ...
7. Dependencies reference task IDs: ["t1", "t2"] means this task needs t1 and t2 first

USER REQUEST:
{message}

Respond with ONLY valid JSON (no markdown, no explanation):
{{"tasks": [{{"id": "t1", "description": "...", "dependencies": [], "context_tags": ["..."], "estimated_tokens": 3000}}], "single_agent_recommended": false, "reasoning": "Brief explanation"}}"#
        )
    }

    /// Parse the LLM's JSON response into a DecompositionResult.
    pub fn parse_decomposition(response: &str) -> Result<DecompositionResult, Temm1eError> {
        // Try to extract JSON from the response (handle markdown wrapping)
        let json_str = extract_json(response);

        let result: DecompositionResult = serde_json::from_str(json_str).map_err(|e| {
            Temm1eError::Internal(format!(
                "Failed to parse decomposition JSON: {e}\nResponse: {response}"
            ))
        })?;

        // Validate the DAG
        dag::validate_dag(&result.tasks)?;

        if result.tasks.is_empty() {
            return Err(Temm1eError::Internal(
                "Decomposition produced zero tasks".into(),
            ));
        }

        info!(
            tasks = result.tasks.len(),
            single_agent = result.single_agent_recommended,
            "Parsed decomposition"
        );

        Ok(result)
    }

    /// Decide whether to activate swarm mode based on decomposition results.
    ///
    /// Returns true if swarm mode should be used.
    pub fn should_activate_swarm(
        &self,
        decomposition: &DecompositionResult,
        queen_tokens: u64,
        estimated_single_cost: u64,
    ) -> bool {
        // If the Queen recommends single agent, respect that
        if decomposition.single_agent_recommended {
            debug!("Queen recommends single agent");
            return false;
        }

        // Check minimum task count
        if decomposition.tasks.len() < 2 {
            debug!("Only 1 task — single agent");
            return false;
        }

        // Check theoretical speedup
        let s_max = dag::max_speedup(&decomposition.tasks);
        if s_max < self.swarm_threshold_speedup {
            debug!(
                s_max = s_max,
                threshold = self.swarm_threshold_speedup,
                "Speedup too low for pack"
            );
            return false;
        }

        // Check queen cost ratio
        if estimated_single_cost > 0 {
            let ratio = queen_tokens as f64 / estimated_single_cost as f64;
            if ratio > self.queen_cost_ratio_max {
                debug!(
                    ratio = ratio,
                    max = self.queen_cost_ratio_max,
                    "Queen decomposition too expensive"
                );
                return false;
            }
        }

        info!(
            tasks = decomposition.tasks.len(),
            s_max = s_max,
            "Pack mode activated"
        );
        true
    }
}

/// Extract JSON from a response that might be wrapped in markdown code blocks.
fn extract_json(response: &str) -> &str {
    let trimmed = response.trim();

    // Try to find JSON within ```json ... ``` blocks
    if let Some(start) = trimmed.find("```json") {
        let json_start = start + 7;
        if let Some(end) = trimmed[json_start..].find("```") {
            return trimmed[json_start..json_start + end].trim();
        }
    }

    // Try to find JSON within ``` ... ``` blocks
    if let Some(start) = trimmed.find("```") {
        let json_start = start + 3;
        if let Some(end) = trimmed[json_start..].find("```") {
            let candidate = trimmed[json_start..json_start + end].trim();
            if candidate.starts_with('{') {
                return candidate;
            }
        }
    }

    // Try to find raw JSON object
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
    }

    trimmed
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn not_complex_no_decompose() {
        assert!(!Queen::should_decompose(false));
    }

    #[test]
    fn complex_order_decomposes() {
        assert!(Queen::should_decompose(true));
    }

    #[test]
    fn parse_yes_response() {
        assert!(Queen::parse_should_decompose("yes"));
        assert!(Queen::parse_should_decompose(
            "Yes, this can be parallelized."
        ));
        assert!(Queen::parse_should_decompose("YES"));
    }

    #[test]
    fn parse_no_response() {
        assert!(!Queen::parse_should_decompose("no"));
        assert!(!Queen::parse_should_decompose("No, this is a single task."));
        assert!(!Queen::parse_should_decompose(""));
    }

    #[test]
    fn parse_valid_json() {
        let json = r#"{"tasks": [
            {"id": "t1", "description": "Create schema", "dependencies": [], "context_tags": ["db"], "estimated_tokens": 2000},
            {"id": "t2", "description": "Build API", "dependencies": ["t1"], "context_tags": ["api"], "estimated_tokens": 3000}
        ], "single_agent_recommended": false, "reasoning": "Two parallel-eligible tasks"}"#;

        let result = Queen::parse_decomposition(json).unwrap();
        assert_eq!(result.tasks.len(), 2);
        assert_eq!(result.tasks[0].id, "t1");
        assert_eq!(result.tasks[1].dependencies, vec!["t1"]);
        assert!(!result.single_agent_recommended);
    }

    #[test]
    fn parse_markdown_wrapped_json() {
        let response = "```json\n{\"tasks\": [{\"id\": \"t1\", \"description\": \"Test\", \
                        \"dependencies\": [], \"context_tags\": [], \"estimated_tokens\": 1000}], \
                        \"single_agent_recommended\": true, \"reasoning\": \"Simple\"}\n```";

        let result = Queen::parse_decomposition(response).unwrap();
        assert_eq!(result.tasks.len(), 1);
        assert!(result.single_agent_recommended);
    }

    #[test]
    fn parse_malformed_json_error() {
        assert!(Queen::parse_decomposition("not json at all").is_err());
    }

    #[test]
    fn parse_cyclic_dag_error() {
        let json = r#"{"tasks": [
            {"id": "t1", "description": "A", "dependencies": ["t2"], "context_tags": [], "estimated_tokens": 1000},
            {"id": "t2", "description": "B", "dependencies": ["t1"], "context_tags": [], "estimated_tokens": 1000}
        ], "single_agent_recommended": false, "reasoning": "cycle"}"#;

        assert!(Queen::parse_decomposition(json).is_err());
    }

    #[test]
    fn activation_threshold_low_speedup() {
        let queen = Queen::new(&HiveConfig::default());
        let decomp = DecompositionResult {
            tasks: vec![
                crate::types::DecomposedTask {
                    id: "t1".into(),
                    description: "A".into(),
                    dependencies: vec![],
                    context_tags: vec![],
                    estimated_tokens: 1000,
                },
                crate::types::DecomposedTask {
                    id: "t2".into(),
                    description: "B".into(),
                    dependencies: vec!["t1".into()],
                    context_tags: vec![],
                    estimated_tokens: 1000,
                },
            ],
            single_agent_recommended: false,
            reasoning: "serial".into(),
        };

        // Serial chain → speedup = 1.0 < 1.3 threshold
        assert!(!queen.should_activate_swarm(&decomp, 100, 5000));
    }

    #[test]
    fn activation_threshold_high_speedup() {
        let queen = Queen::new(&HiveConfig::default());
        let decomp = DecompositionResult {
            tasks: vec![
                crate::types::DecomposedTask {
                    id: "t1".into(),
                    description: "A".into(),
                    dependencies: vec![],
                    context_tags: vec![],
                    estimated_tokens: 1000,
                },
                crate::types::DecomposedTask {
                    id: "t2".into(),
                    description: "B".into(),
                    dependencies: vec![],
                    context_tags: vec![],
                    estimated_tokens: 1000,
                },
                crate::types::DecomposedTask {
                    id: "t3".into(),
                    description: "C".into(),
                    dependencies: vec![],
                    context_tags: vec![],
                    estimated_tokens: 1000,
                },
            ],
            single_agent_recommended: false,
            reasoning: "parallel".into(),
        };

        // 3 independent tasks → speedup = 3.0 > 1.3
        // queen cost 100 / single cost 10000 = 0.01 < 0.10
        assert!(queen.should_activate_swarm(&decomp, 100, 10000));
    }
}
