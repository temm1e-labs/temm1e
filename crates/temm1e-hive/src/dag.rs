//! DAG validation and critical path computation.
//!
//! Before the Hive executes a decomposed order, the dependency graph must be
//! validated (no cycles) and the critical path computed to determine whether
//! swarm mode is worthwhile.

use std::collections::{HashMap, HashSet, VecDeque};

use temm1e_core::types::error::Temm1eError;

use crate::types::DecomposedTask;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Validate that the dependency graph is a DAG (no cycles).
///
/// Uses Kahn's algorithm: O(|T| + |E|).
/// Returns `Err` if cycles are detected.
pub fn validate_dag(tasks: &[DecomposedTask]) -> Result<(), Temm1eError> {
    let ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();

    // Check all dependencies reference valid task IDs
    for task in tasks {
        for dep in &task.dependencies {
            if !ids.contains(dep.as_str()) {
                return Err(Temm1eError::Internal(format!(
                    "Task '{}' depends on unknown task '{dep}'",
                    task.id
                )));
            }
            if dep == &task.id {
                return Err(Temm1eError::Internal(format!(
                    "Task '{}' depends on itself",
                    task.id
                )));
            }
        }
    }

    // Kahn's algorithm
    let sorted = topological_sort(tasks)?;
    if sorted.len() != tasks.len() {
        return Err(Temm1eError::Internal(
            "Dependency graph contains a cycle".into(),
        ));
    }

    Ok(())
}

/// Topological sort of tasks. Returns task IDs in dependency order.
///
/// Uses Kahn's algorithm. Returns `Err` if cycles are detected.
pub fn topological_sort(tasks: &[DecomposedTask]) -> Result<Vec<String>, Temm1eError> {
    let n = tasks.len();

    // Compute in-degree for each task
    let mut in_degree: HashMap<&str, usize> = HashMap::new();
    let mut dependents: HashMap<&str, Vec<&str>> = HashMap::new();

    for task in tasks {
        in_degree.entry(task.id.as_str()).or_insert(0);
        for dep in &task.dependencies {
            *in_degree.entry(task.id.as_str()).or_insert(0) += 1;
            dependents
                .entry(dep.as_str())
                .or_default()
                .push(task.id.as_str());
        }
    }

    // Start with all tasks that have no dependencies
    let mut queue: VecDeque<&str> = in_degree
        .iter()
        .filter(|(_, &deg)| deg == 0)
        .map(|(&id, _)| id)
        .collect();

    let mut sorted = Vec::with_capacity(n);

    while let Some(id) = queue.pop_front() {
        sorted.push(id.to_string());

        if let Some(deps) = dependents.get(id) {
            for &dep_id in deps {
                if let Some(deg) = in_degree.get_mut(dep_id) {
                    *deg -= 1;
                    if *deg == 0 {
                        queue.push_back(dep_id);
                    }
                }
            }
        }
    }

    if sorted.len() != n {
        return Err(Temm1eError::Internal(format!(
            "Cycle detected: only {}/{n} tasks could be sorted",
            sorted.len()
        )));
    }

    Ok(sorted)
}

/// Compute the critical path length (sum of estimated_tokens on the longest path).
///
/// Returns the total estimated tokens along the critical path.
pub fn critical_path_tokens(tasks: &[DecomposedTask]) -> u32 {
    if tasks.is_empty() {
        return 0;
    }

    let task_map: HashMap<&str, &DecomposedTask> =
        tasks.iter().map(|t| (t.id.as_str(), t)).collect();

    // longest_path[id] = maximum tokens from any source to id (inclusive)
    let mut longest: HashMap<String, u32> = HashMap::new();

    // Process in topological order
    if let Ok(sorted) = topological_sort(tasks) {
        for id in &sorted {
            let task = task_map[id.as_str()];
            let max_dep = task
                .dependencies
                .iter()
                .filter_map(|d| longest.get(d.as_str()))
                .copied()
                .max()
                .unwrap_or(0);
            longest.insert(id.clone(), max_dep + task.estimated_tokens);
        }
    }

    longest.values().copied().max().unwrap_or(0)
}

/// Compute the theoretical maximum speedup from parallelism.
///
/// `S_max = total_tokens / critical_path_tokens`
///
/// Returns 1.0 if the critical path equals total work (fully serial).
pub fn max_speedup(tasks: &[DecomposedTask]) -> f64 {
    if tasks.is_empty() {
        return 1.0;
    }

    let total: u32 = tasks.iter().map(|t| t.estimated_tokens).sum();
    let cp = critical_path_tokens(tasks);

    if cp == 0 {
        return 1.0;
    }

    total as f64 / cp as f64
}

/// Count how many dependents each task has (how many other tasks depend on it).
pub fn dependent_counts(tasks: &[DecomposedTask]) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for task in tasks {
        counts.entry(task.id.clone()).or_insert(0);
        for dep in &task.dependencies {
            *counts.entry(dep.clone()).or_insert(0) += 1;
        }
    }
    counts
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task(id: &str, deps: &[&str], tokens: u32) -> DecomposedTask {
        DecomposedTask {
            id: id.into(),
            description: format!("Task {id}"),
            dependencies: deps.iter().map(|d| d.to_string()).collect(),
            context_tags: vec![],
            estimated_tokens: tokens,
        }
    }

    #[test]
    fn valid_dag() {
        let tasks = vec![
            make_task("t1", &[], 1000),
            make_task("t2", &["t1"], 2000),
            make_task("t3", &["t1"], 1500),
            make_task("t4", &["t2", "t3"], 1000),
        ];
        assert!(validate_dag(&tasks).is_ok());
    }

    #[test]
    fn cyclic_graph_rejected() {
        let tasks = vec![
            make_task("t1", &["t3"], 1000),
            make_task("t2", &["t1"], 1000),
            make_task("t3", &["t2"], 1000),
        ];
        assert!(validate_dag(&tasks).is_err());
    }

    #[test]
    fn self_reference_rejected() {
        let tasks = vec![make_task("t1", &["t1"], 1000)];
        assert!(validate_dag(&tasks).is_err());
    }

    #[test]
    fn unknown_dependency_rejected() {
        let tasks = vec![make_task("t1", &["t99"], 1000)];
        assert!(validate_dag(&tasks).is_err());
    }

    #[test]
    fn single_task_speedup() {
        let tasks = vec![make_task("t1", &[], 3000)];
        let s = max_speedup(&tasks);
        assert!((s - 1.0).abs() < 1e-9);
    }

    #[test]
    fn fully_parallel_speedup() {
        // 4 independent tasks of equal size
        let tasks = vec![
            make_task("t1", &[], 1000),
            make_task("t2", &[], 1000),
            make_task("t3", &[], 1000),
            make_task("t4", &[], 1000),
        ];
        let s = max_speedup(&tasks);
        assert!((s - 4.0).abs() < 1e-9);
    }

    #[test]
    fn serial_chain_speedup() {
        // t1 → t2 → t3
        let tasks = vec![
            make_task("t1", &[], 1000),
            make_task("t2", &["t1"], 1000),
            make_task("t3", &["t2"], 1000),
        ];
        let s = max_speedup(&tasks);
        assert!((s - 1.0).abs() < 1e-9);
    }

    #[test]
    fn diamond_dag_critical_path() {
        //     t1(1000)
        //    /       \
        // t2(2000)  t3(1500)
        //    \       /
        //     t4(1000)
        let tasks = vec![
            make_task("t1", &[], 1000),
            make_task("t2", &["t1"], 2000),
            make_task("t3", &["t1"], 1500),
            make_task("t4", &["t2", "t3"], 1000),
        ];

        // Critical path: t1(1000) → t2(2000) → t4(1000) = 4000
        let cp = critical_path_tokens(&tasks);
        assert_eq!(cp, 4000);

        // Total = 5500, CP = 4000, speedup = 1.375
        let s = max_speedup(&tasks);
        assert!((s - 1.375).abs() < 0.01);
    }

    #[test]
    fn topological_sort_order() {
        let tasks = vec![
            make_task("t3", &["t1", "t2"], 1000),
            make_task("t1", &[], 1000),
            make_task("t2", &["t1"], 1000),
        ];
        let sorted = topological_sort(&tasks).unwrap();
        assert_eq!(sorted.len(), 3);
        // t1 must come before t2 and t3
        let pos = |id: &str| sorted.iter().position(|s| s == id).unwrap();
        assert!(pos("t1") < pos("t2"));
        assert!(pos("t1") < pos("t3"));
        assert!(pos("t2") < pos("t3"));
    }

    #[test]
    fn dependent_counts_correct() {
        let tasks = vec![
            make_task("t1", &[], 1000),
            make_task("t2", &["t1"], 1000),
            make_task("t3", &["t1"], 1000),
            make_task("t4", &["t2", "t3"], 1000),
        ];
        let counts = dependent_counts(&tasks);
        // t1 is depended on by t2 and t3
        assert_eq!(counts["t1"], 2);
        // t2 and t3 are depended on by t4
        assert_eq!(counts["t2"], 1);
        assert_eq!(counts["t3"], 1);
        // t4 has no dependents
        assert_eq!(counts["t4"], 0);
    }

    #[test]
    fn empty_tasks() {
        assert!(validate_dag(&[]).is_ok());
        assert_eq!(critical_path_tokens(&[]), 0);
        assert!((max_speedup(&[]) - 1.0).abs() < 1e-9);
    }
}
