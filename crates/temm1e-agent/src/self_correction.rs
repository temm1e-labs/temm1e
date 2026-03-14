//! Self-Correction Engine — tracks consecutive tool failures and injects
//! strategy rotation prompts after N failures on the same tool, preventing
//! the agent from retrying the same broken approach indefinitely.

use std::collections::HashMap;

/// Tracks consecutive failures per tool, storing the error messages so the
/// strategy rotation prompt can reference past attempts.
#[derive(Debug, Clone)]
pub struct FailureTracker {
    /// Map from tool name to a list of (tool_name, error_message) for
    /// consecutive failures. Cleared on success.
    failures: HashMap<String, Vec<(String, String)>>,
    /// Number of consecutive failures before triggering strategy rotation.
    pub max_failures: usize,
}

impl FailureTracker {
    /// Create a new `FailureTracker` with the given threshold.
    pub fn new(max_failures: usize) -> Self {
        Self {
            failures: HashMap::new(),
            max_failures,
        }
    }

    /// Record a tool failure. Appends the error to the consecutive failure list.
    pub fn record_failure(&mut self, tool_name: &str, error: &str) {
        let entry = self.failures.entry(tool_name.to_string()).or_default();
        entry.push((tool_name.to_string(), error.to_string()));
    }

    /// Record a tool success. Resets the failure counter for that tool.
    pub fn record_success(&mut self, tool_name: &str) {
        self.failures.remove(tool_name);
    }

    /// Check whether strategy rotation should be triggered for a tool.
    pub fn should_rotate_strategy(&self, tool_name: &str) -> bool {
        self.failures
            .get(tool_name)
            .is_some_and(|f| f.len() >= self.max_failures)
    }

    /// Get the consecutive failure count for a tool.
    pub fn failure_count(&self, tool_name: &str) -> usize {
        self.failures.get(tool_name).map_or(0, |f| f.len())
    }

    /// Get the list of recorded failures for a tool.
    pub fn get_failures(&self, tool_name: &str) -> &[(String, String)] {
        self.failures.get(tool_name).map_or(&[], |f| f.as_slice())
    }

    /// Format a strategy rotation prompt for a tool that has exceeded the
    /// failure threshold. Returns `None` if the threshold has not been reached.
    pub fn format_rotation_prompt(&self, tool_name: &str) -> Option<String> {
        let failures = self.failures.get(tool_name)?;
        if failures.len() < self.max_failures {
            return None;
        }

        let count = failures.len();
        let error_list: String = failures
            .iter()
            .enumerate()
            .map(|(i, (_, err))| {
                // Truncate very long error messages for the prompt
                let truncated = if err.len() > 500 {
                    let end = err
                        .char_indices()
                        .map(|(i, _)| i)
                        .take_while(|&i| i <= 500)
                        .last()
                        .unwrap_or(0);
                    format!("{}...", &err[..end])
                } else {
                    err.clone()
                };
                format!("  {}. {}", i + 1, truncated)
            })
            .collect::<Vec<_>>()
            .join("\n");

        Some(format!(
            "\n\n[STRATEGY ROTATION] This approach has failed {} times. Previous errors:\n\
             {}\n\
             \n\
             Do NOT retry the same approach. Instead:\n\
             1. Analyze WHY the approach fails\n\
             2. List 3 alternative approaches\n\
             3. Execute the most promising alternative\n\
             If no alternatives exist, ask the user for guidance.",
            count, error_list
        ))
    }
}

impl Default for FailureTracker {
    fn default() -> Self {
        Self::new(2)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn new_tracker_has_no_failures() {
        let tracker = FailureTracker::new(2);
        assert_eq!(tracker.failure_count("shell"), 0);
        assert!(!tracker.should_rotate_strategy("shell"));
    }

    #[test]
    fn record_failure_increments_count() {
        let mut tracker = FailureTracker::new(2);
        tracker.record_failure("shell", "command not found");
        assert_eq!(tracker.failure_count("shell"), 1);
        assert!(!tracker.should_rotate_strategy("shell"));

        tracker.record_failure("shell", "permission denied");
        assert_eq!(tracker.failure_count("shell"), 2);
        assert!(tracker.should_rotate_strategy("shell"));
    }

    #[test]
    fn record_success_resets_count() {
        let mut tracker = FailureTracker::new(2);
        tracker.record_failure("shell", "error 1");
        tracker.record_failure("shell", "error 2");
        assert!(tracker.should_rotate_strategy("shell"));

        tracker.record_success("shell");
        assert_eq!(tracker.failure_count("shell"), 0);
        assert!(!tracker.should_rotate_strategy("shell"));
    }

    #[test]
    fn failures_are_tracked_per_tool() {
        let mut tracker = FailureTracker::new(2);
        tracker.record_failure("shell", "error 1");
        tracker.record_failure("shell", "error 2");
        tracker.record_failure("file_read", "not found");

        assert!(tracker.should_rotate_strategy("shell"));
        assert!(!tracker.should_rotate_strategy("file_read"));
        assert_eq!(tracker.failure_count("file_read"), 1);
    }

    #[test]
    fn format_rotation_prompt_none_below_threshold() {
        let mut tracker = FailureTracker::new(3);
        tracker.record_failure("shell", "error");
        tracker.record_failure("shell", "error again");

        assert!(tracker.format_rotation_prompt("shell").is_none());
        assert!(tracker.format_rotation_prompt("nonexistent").is_none());
    }

    #[test]
    fn format_rotation_prompt_contains_errors() {
        let mut tracker = FailureTracker::new(2);
        tracker.record_failure("shell", "command not found: foo");
        tracker.record_failure("shell", "permission denied: /etc/shadow");

        let prompt = tracker.format_rotation_prompt("shell").unwrap();
        assert!(prompt.contains("STRATEGY ROTATION"));
        assert!(prompt.contains("failed 2 times"));
        assert!(prompt.contains("command not found: foo"));
        assert!(prompt.contains("permission denied: /etc/shadow"));
        assert!(prompt.contains("Do NOT retry the same approach"));
        assert!(prompt.contains("3 alternative approaches"));
    }

    #[test]
    fn format_rotation_prompt_truncates_long_errors() {
        let mut tracker = FailureTracker::new(1);
        let long_error = "x".repeat(1000);
        tracker.record_failure("shell", &long_error);

        let prompt = tracker.format_rotation_prompt("shell").unwrap();
        // The error in the prompt should be truncated at ~500 chars + "..."
        assert!(prompt.contains("..."));
        // Should not contain the full 1000-char error
        assert!(!prompt.contains(&"x".repeat(1000)));
    }

    #[test]
    fn get_failures_returns_empty_for_unknown_tool() {
        let tracker = FailureTracker::new(2);
        assert!(tracker.get_failures("unknown").is_empty());
    }

    #[test]
    fn get_failures_returns_recorded_errors() {
        let mut tracker = FailureTracker::new(2);
        tracker.record_failure("shell", "err1");
        tracker.record_failure("shell", "err2");

        let failures = tracker.get_failures("shell");
        assert_eq!(failures.len(), 2);
        assert_eq!(failures[0].1, "err1");
        assert_eq!(failures[1].1, "err2");
    }

    #[test]
    fn default_tracker_has_threshold_of_two() {
        let tracker = FailureTracker::default();
        assert_eq!(tracker.max_failures, 2);
    }
}
