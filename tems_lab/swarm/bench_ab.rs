//! TEMM1E Hive A/B Benchmark — Single Agent vs Swarm
//!
//! This benchmark compares single-agent execution against swarm execution
//! for tasks of varying complexity. It uses Gemini 3.1 Flash Lite to
//! minimize costs ($30 total budget).
//!
//! ## Usage
//!
//! This is a reference script — integrate into the TEMM1E binary or run
//! as a standalone test with:
//!
//! ```bash
//! cargo test -p temm1e-hive --test bench_ab -- --nocapture
//! ```
//!
//! ## Budget Tracking
//!
//! Gemini 3.1 Flash Lite estimated pricing:
//!   Input:  $0.075 / 1M tokens
//!   Output: $0.30  / 1M tokens
//!
//! Per benchmark run (~150K tokens avg): ~$0.045
//! 36 runs (12 scenarios × 3 each): ~$1.62
//! Total budget: $30 (massive headroom)

use std::collections::HashMap;
use std::time::Instant;

// ---------------------------------------------------------------------------
// Benchmark Result
// ---------------------------------------------------------------------------

/// Metrics collected for a single benchmark run.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct BenchmarkResult {
    pub task_name: String,
    pub mode: String, // "single" or "swarm"
    pub run_number: u32,
    pub tokens_input: u64,
    pub tokens_output: u64,
    pub tokens_total: u64,
    pub cost_usd: f64,
    pub wall_clock_ms: u64,
    pub tasks_decomposed: usize,
    pub tasks_parallel: usize,
    pub quality_score: f64,
    pub swarm_activated: bool,
    pub error: Option<String>,
}

// ---------------------------------------------------------------------------
// Budget Tracker
// ---------------------------------------------------------------------------

/// Tracks cumulative spend across all benchmark runs.
#[derive(Debug)]
pub struct BudgetTracker {
    pub total_spent: f64,
    pub budget_limit: f64,
    pub runs: Vec<BenchmarkResult>,
}

impl BudgetTracker {
    pub fn new(budget_limit: f64) -> Self {
        Self {
            total_spent: 0.0,
            budget_limit,
            runs: Vec::new(),
        }
    }

    pub fn record(&mut self, result: BenchmarkResult) {
        self.total_spent += result.cost_usd;
        self.runs.push(result);
    }

    pub fn can_afford(&self, estimated_cost: f64) -> bool {
        self.total_spent + estimated_cost < self.budget_limit
    }

    pub fn summary(&self) -> BenchmarkSummary {
        let mut by_task: HashMap<String, Vec<&BenchmarkResult>> = HashMap::new();
        for run in &self.runs {
            by_task
                .entry(format!("{}:{}", run.task_name, run.mode))
                .or_default()
                .push(run);
        }

        let mut comparisons = Vec::new();
        let task_names: Vec<String> = self
            .runs
            .iter()
            .map(|r| r.task_name.clone())
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        for task in &task_names {
            let single_key = format!("{task}:single");
            let swarm_key = format!("{task}:swarm");

            let single_runs = by_task.get(&single_key);
            let swarm_runs = by_task.get(&swarm_key);

            if let (Some(singles), Some(swarms)) = (single_runs, swarm_runs) {
                let avg = |runs: &[&BenchmarkResult], f: fn(&BenchmarkResult) -> f64| -> f64 {
                    let sum: f64 = runs.iter().map(|r| f(r)).sum();
                    sum / runs.len() as f64
                };

                let single_tokens = avg(singles, |r| r.tokens_total as f64);
                let swarm_tokens = avg(swarms, |r| r.tokens_total as f64);
                let single_ms = avg(singles, |r| r.wall_clock_ms as f64);
                let swarm_ms = avg(swarms, |r| r.wall_clock_ms as f64);
                let single_cost = avg(singles, |r| r.cost_usd);
                let swarm_cost = avg(swarms, |r| r.cost_usd);

                comparisons.push(TaskComparison {
                    task_name: task.clone(),
                    single_avg_tokens: single_tokens as u64,
                    swarm_avg_tokens: swarm_tokens as u64,
                    token_ratio: swarm_tokens / single_tokens.max(1.0),
                    single_avg_ms: single_ms as u64,
                    swarm_avg_ms: swarm_ms as u64,
                    speedup: single_ms / swarm_ms.max(1.0),
                    single_avg_cost: single_cost,
                    swarm_avg_cost: swarm_cost,
                    runs_per_mode: singles.len(),
                });
            }
        }

        BenchmarkSummary {
            total_runs: self.runs.len(),
            total_spent: self.total_spent,
            budget_remaining: self.budget_limit - self.total_spent,
            comparisons,
        }
    }
}

#[derive(Debug, serde::Serialize)]
pub struct BenchmarkSummary {
    pub total_runs: usize,
    pub total_spent: f64,
    pub budget_remaining: f64,
    pub comparisons: Vec<TaskComparison>,
}

#[derive(Debug, serde::Serialize)]
pub struct TaskComparison {
    pub task_name: String,
    pub single_avg_tokens: u64,
    pub swarm_avg_tokens: u64,
    pub token_ratio: f64,
    pub single_avg_ms: u64,
    pub swarm_avg_ms: u64,
    pub speedup: f64,
    pub single_avg_cost: f64,
    pub swarm_avg_cost: f64,
    pub runs_per_mode: usize,
}

// ---------------------------------------------------------------------------
// Benchmark Tasks
// ---------------------------------------------------------------------------

/// The tasks used for A/B comparison.
pub fn benchmark_tasks() -> Vec<(&'static str, &'static str)> {
    vec![
        (
            "simple_chat",
            "What is the capital of France?",
        ),
        (
            "3_step",
            "Create a Rust function that: 1. Reads a CSV file from disk \
             2. Counts the number of unique words across all rows \
             3. Writes the word frequencies to a new JSON file sorted by count",
        ),
        (
            "7_step",
            "Build a complete REST API microservice: \
             1. Define the database schema with users and posts tables \
             2. Set up the SQLite connection pool with migrations \
             3. Implement CRUD endpoints for users (create, read, update, delete) \
             4. Implement CRUD endpoints for posts with user foreign key \
             5. Add request validation middleware for all endpoints \
             6. Write unit tests for each endpoint handler \
             7. Create API documentation with example requests and responses",
        ),
        (
            "10_step",
            "Refactor the authentication module completely: \
             1. Extract the auth trait interface from the current monolithic implementation \
             2. Split the module into separate files: jwt.rs, oauth.rs, session.rs, middleware.rs \
             3. Define proper error types with thiserror for each auth failure mode \
             4. Implement the auth trait for JWT token validation \
             5. Implement the auth trait for OAuth2 code exchange flow \
             6. Add structured logging with tracing to every auth operation \
             7. Write unit tests for JWT token creation and validation \
             8. Write unit tests for OAuth2 state management \
             9. Write integration tests for the full auth middleware chain \
             10. Update all import paths across the codebase to use the new module structure",
        ),
    ]
}

// ---------------------------------------------------------------------------
// Mock Benchmark (for validation without API calls)
// ---------------------------------------------------------------------------

/// Run a mock benchmark to validate the framework.
/// Uses synthetic data to verify metrics collection and reporting.
pub fn run_mock_benchmark() -> BudgetTracker {
    let mut tracker = BudgetTracker::new(30.0);

    let tasks = benchmark_tasks();

    for (task_name, task_desc) in &tasks {
        for run in 1..=3 {
            // Simulate single-agent execution
            let single_tokens = match *task_name {
                "simple_chat" => 500,
                "3_step" => 8_000,
                "7_step" => 70_000,
                "10_step" => 150_000,
                _ => 10_000,
            };

            tracker.record(BenchmarkResult {
                task_name: task_name.to_string(),
                mode: "single".into(),
                run_number: run,
                tokens_input: (single_tokens as f64 * 0.6) as u64,
                tokens_output: (single_tokens as f64 * 0.4) as u64,
                tokens_total: single_tokens,
                cost_usd: estimate_cost(single_tokens),
                wall_clock_ms: single_tokens as u64 * 10, // rough: 10ms per token
                tasks_decomposed: 0,
                tasks_parallel: 0,
                quality_score: 0.95,
                swarm_activated: false,
                error: None,
            });

            // Simulate swarm execution
            let (swarm_tokens, speedup, swarm_activated) = match *task_name {
                "simple_chat" => (515, 1.0, false), // 3% overhead, no swarm
                "3_step" => (7_200, 1.1, false),    // marginal, no swarm
                "7_step" => (25_100, 1.75, true),   // big savings
                "10_step" => (48_000, 2.1, true),   // bigger savings
                _ => (10_000, 1.0, false),
            };

            tracker.record(BenchmarkResult {
                task_name: task_name.to_string(),
                mode: "swarm".into(),
                run_number: run,
                tokens_input: (swarm_tokens as f64 * 0.6) as u64,
                tokens_output: (swarm_tokens as f64 * 0.4) as u64,
                tokens_total: swarm_tokens,
                cost_usd: estimate_cost(swarm_tokens),
                wall_clock_ms: (single_tokens as f64 * 10.0 / speedup) as u64,
                tasks_decomposed: if swarm_activated {
                    task_name.chars().next().unwrap().to_digit(10).unwrap_or(3) as usize
                } else {
                    0
                },
                tasks_parallel: if swarm_activated { 3 } else { 0 },
                quality_score: 0.94,
                swarm_activated,
                error: None,
            });
        }
    }

    tracker
}

/// Estimate cost in USD for Gemini 3.1 Flash Lite.
fn estimate_cost(total_tokens: u64) -> f64 {
    let input = total_tokens as f64 * 0.6; // 60% input
    let output = total_tokens as f64 * 0.4; // 40% output
    (input * 0.075 / 1_000_000.0) + (output * 0.30 / 1_000_000.0)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mock_benchmark_runs() {
        let tracker = run_mock_benchmark();
        assert_eq!(tracker.runs.len(), 24); // 4 tasks × 2 modes × 3 runs

        let summary = tracker.summary();
        assert_eq!(summary.total_runs, 24);
        assert!(summary.total_spent < 1.0, "mock should be cheap: {}", summary.total_spent);
        assert!(summary.budget_remaining > 29.0);
    }

    #[test]
    fn simple_task_no_swarm() {
        let tracker = run_mock_benchmark();
        let simple_swarm: Vec<_> = tracker
            .runs
            .iter()
            .filter(|r| r.task_name == "simple_chat" && r.mode == "swarm")
            .collect();

        for run in &simple_swarm {
            assert!(!run.swarm_activated, "simple tasks should NOT activate swarm");
        }
    }

    #[test]
    fn complex_task_swarm_saves_tokens() {
        let tracker = run_mock_benchmark();
        let summary = tracker.summary();

        for comp in &summary.comparisons {
            if comp.task_name == "7_step" {
                assert!(
                    comp.token_ratio < 0.5,
                    "7-step swarm should use <50% of single-agent tokens, got {:.2}",
                    comp.token_ratio
                );
                assert!(
                    comp.speedup > 1.3,
                    "7-step should have speedup >1.3x, got {:.2}",
                    comp.speedup
                );
            }
        }
    }

    #[test]
    fn axiom_a5_cost_dominance() {
        let tracker = run_mock_benchmark();
        let summary = tracker.summary();

        for comp in &summary.comparisons {
            // Swarm cost should not exceed 1.15× single-agent cost
            if comp.single_avg_cost > 0.0 {
                let ratio = comp.swarm_avg_cost / comp.single_avg_cost;
                assert!(
                    ratio <= 1.15,
                    "Axiom A5 violated for {}: swarm/single cost ratio = {:.3}",
                    comp.task_name,
                    ratio
                );
            }
        }
    }

    #[test]
    fn budget_tracking() {
        let mut tracker = BudgetTracker::new(30.0);
        assert!(tracker.can_afford(1.0));

        tracker.record(BenchmarkResult {
            task_name: "test".into(),
            mode: "single".into(),
            run_number: 1,
            tokens_input: 1000,
            tokens_output: 500,
            tokens_total: 1500,
            cost_usd: 29.5,
            wall_clock_ms: 100,
            tasks_decomposed: 0,
            tasks_parallel: 0,
            quality_score: 1.0,
            swarm_activated: false,
            error: None,
        });

        assert!(!tracker.can_afford(1.0), "should exceed budget");
        assert!(tracker.can_afford(0.4), "should have $0.50 left");
    }
}
