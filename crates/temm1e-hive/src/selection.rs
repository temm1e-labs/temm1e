//! Worker Task Selection — the heart of the swarm's self-organization.
//!
//! When a worker is idle, it evaluates all READY tasks and selects one
//! by maximizing a score function. **No LLM call is made.** This is pure
//! arithmetic over pheromone signals and tag similarity.

use std::collections::{HashMap, HashSet};

use rand::Rng;
use tracing::debug;

use crate::config::SelectionConfig;
use crate::pheromone::PheromoneField;
use crate::types::{HiveTask, SignalType, WorkerState};

// ---------------------------------------------------------------------------
// TaskSelector
// ---------------------------------------------------------------------------

/// Selects the best task for a worker based on the pheromone field and task attributes.
pub struct TaskSelector {
    alpha: f64,
    beta: f64,
    gamma: f64,
    delta: f64,
    zeta: f64,
    tie_threshold: f64,
}

impl TaskSelector {
    pub fn new(config: &SelectionConfig) -> Self {
        Self {
            alpha: config.alpha,
            beta: config.beta,
            gamma: config.gamma,
            delta: config.delta,
            zeta: config.zeta,
            tie_threshold: config.tie_threshold,
        }
    }

    /// Compute the selection score for a (worker, task) pair.
    ///
    /// `S = A^α · U^β · (1-D)^γ · (1-F)^δ · R^ζ`
    pub fn score(
        &self,
        affinity: f64,
        urgency: f64,
        difficulty: f64,
        failure: f64,
        reward: f64,
    ) -> f64 {
        // Clamp inputs to valid ranges
        let a = affinity.max(0.001); // avoid zero base with positive exponent
        let u = urgency.max(0.001);
        let d = difficulty.clamp(0.0, 0.999); // avoid (1-1)^γ = 0
        let f = failure.clamp(0.0, 0.999);
        let r = reward.max(0.001);

        a.powf(self.alpha)
            * u.powf(self.beta)
            * (1.0 - d).powf(self.gamma)
            * (1.0 - f).powf(self.delta)
            * r.powf(self.zeta)
    }

    /// Select the best task for a worker from a list of READY tasks.
    ///
    /// Returns the task ID, or None if no tasks are available.
    pub async fn select_task(
        &self,
        worker: &WorkerState,
        ready_tasks: &[HiveTask],
        pheromones: &PheromoneField,
        total_tasks: usize,
        dependent_counts: &HashMap<String, usize>,
    ) -> Option<String> {
        if ready_tasks.is_empty() {
            return None;
        }

        let mut scored: Vec<(String, f64)> = Vec::with_capacity(ready_tasks.len());

        for task in ready_tasks {
            // A: Affinity — tag overlap between worker and task
            let affinity = tag_affinity(&worker.recent_tags, &task.context_tags);

            // U: Urgency — from pheromone field
            let urgency = pheromones
                .read_total(SignalType::Urgency, &task.id)
                .await
                .unwrap_or(0.1)
                .max(0.1);

            // D: Difficulty — from pheromone field
            let difficulty = pheromones
                .read_total(SignalType::Difficulty, &task.id)
                .await
                .unwrap_or(0.0)
                .min(1.0);

            // F: Failure — from pheromone field
            let failure = pheromones
                .read_total(SignalType::Failure, &task.id)
                .await
                .unwrap_or(0.0)
                .min(1.0);

            // R: Downstream reward — how many tasks depend on this one
            let dependents = dependent_counts.get(&task.id).copied().unwrap_or(0);
            let reward = if total_tasks > 0 {
                1.0 + dependents as f64 / total_tasks as f64
            } else {
                1.0
            };

            let s = self.score(affinity, urgency, difficulty, failure, reward);
            scored.push((task.id.clone(), s));
        }

        // Find the max score
        let max_score = scored
            .iter()
            .map(|(_, s)| *s)
            .fold(f64::NEG_INFINITY, f64::max);

        if max_score <= 0.0 {
            // All scores zero or negative — just pick the first task
            return Some(scored[0].0.clone());
        }

        // Tie-breaking: scores within threshold of max → random pick
        let threshold = max_score * (1.0 - self.tie_threshold);
        let candidates: Vec<&(String, f64)> =
            scored.iter().filter(|(_, s)| *s >= threshold).collect();

        if candidates.is_empty() {
            return Some(scored[0].0.clone());
        }

        let idx = rand::thread_rng().gen_range(0..candidates.len());
        let selected = &candidates[idx].0;

        debug!(
            worker = %worker.id,
            task = %selected,
            score = candidates[idx].1,
            candidates = candidates.len(),
            "Selected task"
        );

        Some(selected.clone())
    }
}

/// Compute Jaccard similarity between worker tags and task tags.
///
/// Returns a value in [0.1, 1.0] (0.1 floor ensures nonzero affinity).
pub fn tag_affinity(worker_tags: &HashSet<String>, task_tags: &[String]) -> f64 {
    if worker_tags.is_empty() || task_tags.is_empty() {
        return 0.1; // floor
    }

    let task_set: HashSet<&str> = task_tags.iter().map(|s| s.as_str()).collect();
    let worker_set: HashSet<&str> = worker_tags.iter().map(|s| s.as_str()).collect();

    let intersection = worker_set.intersection(&task_set).count();
    let union = worker_set.union(&task_set).count();

    if union == 0 {
        return 0.1;
    }

    (intersection as f64 / union as f64).max(0.1)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_selector() -> TaskSelector {
        TaskSelector::new(&SelectionConfig::default())
    }

    #[test]
    fn score_increases_with_affinity() {
        let sel = make_selector();
        let s_low = sel.score(0.2, 1.0, 0.0, 0.0, 1.0);
        let s_high = sel.score(0.8, 1.0, 0.0, 0.0, 1.0);
        assert!(s_high > s_low, "high affinity={s_high} > low={s_low}");
    }

    #[test]
    fn score_increases_with_urgency() {
        let sel = make_selector();
        let s_low = sel.score(0.5, 0.2, 0.0, 0.0, 1.0);
        let s_high = sel.score(0.5, 3.0, 0.0, 0.0, 1.0);
        assert!(s_high > s_low, "high urgency={s_high} > low={s_low}");
    }

    #[test]
    fn score_decreases_with_difficulty() {
        let sel = make_selector();
        let s_easy = sel.score(0.5, 1.0, 0.1, 0.0, 1.0);
        let s_hard = sel.score(0.5, 1.0, 0.8, 0.0, 1.0);
        assert!(s_easy > s_hard, "easy={s_easy} > hard={s_hard}");
    }

    #[test]
    fn score_decreases_with_failure() {
        let sel = make_selector();
        let s_ok = sel.score(0.5, 1.0, 0.0, 0.1, 1.0);
        let s_fail = sel.score(0.5, 1.0, 0.0, 0.8, 1.0);
        assert!(s_ok > s_fail, "ok={s_ok} > fail={s_fail}");
    }

    #[test]
    fn score_increases_with_reward() {
        let sel = make_selector();
        let s_low = sel.score(0.5, 1.0, 0.0, 0.0, 1.0);
        let s_high = sel.score(0.5, 1.0, 0.0, 0.0, 2.0);
        assert!(s_high > s_low, "high reward={s_high} > low={s_low}");
    }

    #[test]
    fn tag_affinity_empty_is_floor() {
        let empty: HashSet<String> = HashSet::new();
        assert!((tag_affinity(&empty, &["rust".into()]) - 0.1).abs() < 1e-9);
        let worker: HashSet<String> = ["rust".into()].into();
        assert!((tag_affinity(&worker, &[]) - 0.1).abs() < 1e-9);
    }

    #[test]
    fn tag_affinity_exact_match() {
        let worker: HashSet<String> = ["rust".into(), "api".into()].into();
        let task = vec!["rust".into(), "api".into()];
        assert!((tag_affinity(&worker, &task) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn tag_affinity_partial_match() {
        let worker: HashSet<String> = ["rust".into(), "api".into()].into();
        let task = vec!["rust".into(), "db".into()];
        // intersection=1 (rust), union=3 (rust, api, db) → 1/3 ≈ 0.333
        let a = tag_affinity(&worker, &task);
        assert!((a - 1.0 / 3.0).abs() < 0.01, "got {a}");
    }

    #[test]
    fn tag_affinity_no_overlap() {
        let worker: HashSet<String> = ["python".into()].into();
        let task = vec!["rust".into()];
        // intersection=0, union=2 → 0/2 = 0 → clamped to 0.1
        assert!((tag_affinity(&worker, &task) - 0.1).abs() < 1e-9);
    }

    #[tokio::test]
    async fn select_returns_none_for_empty() {
        let sel = make_selector();
        let worker = WorkerState::new("w1".into());
        let field = crate::pheromone::PheromoneField::new(
            "sqlite::memory:",
            crate::config::PheromoneConfig::default(),
        )
        .await
        .unwrap();

        let result = sel
            .select_task(&worker, &[], &field, 0, &HashMap::new())
            .await;
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn select_single_task() {
        let sel = make_selector();
        let worker = WorkerState::new("w1".into());
        let field = crate::pheromone::PheromoneField::new(
            "sqlite::memory:",
            crate::config::PheromoneConfig::default(),
        )
        .await
        .unwrap();

        let task = HiveTask {
            id: "t1".into(),
            order_id: "o1".into(),
            description: "test".into(),
            status: crate::types::HiveTaskStatus::Ready,
            claimed_by: None,
            dependencies: vec![],
            context_tags: vec!["rust".into()],
            estimated_tokens: 1000,
            actual_tokens: 0,
            result_summary: None,
            artifacts: vec![],
            retry_count: 0,
            max_retries: 3,
            error_log: None,
            created_at: 0,
            started_at: None,
            completed_at: None,
        };

        let result = sel
            .select_task(&worker, &[task], &field, 1, &HashMap::new())
            .await;
        assert_eq!(result, Some("t1".to_string()));
    }
}
