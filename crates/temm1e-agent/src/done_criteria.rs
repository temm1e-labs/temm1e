//! DONE Definition Engine — detects compound tasks and generates DONE criteria
//! prompts so the LLM articulates verifiable completion conditions before
//! executing multi-step work.

/// DONE criteria for a compound task. Tracks the list of verifiable conditions
/// that must all be true when the task is complete.
#[derive(Debug, Clone)]
pub struct DoneCriteria {
    /// The verifiable conditions that define DONE.
    pub criteria: Vec<String>,
    /// Whether each criterion has been verified as met.
    pub verified: Vec<bool>,
}

impl DoneCriteria {
    /// Create a new empty `DoneCriteria`.
    pub fn new() -> Self {
        Self {
            criteria: Vec::new(),
            verified: Vec::new(),
        }
    }

    /// Add a criterion. Returns the index.
    pub fn add(&mut self, criterion: String) -> usize {
        let idx = self.criteria.len();
        self.criteria.push(criterion);
        self.verified.push(false);
        idx
    }

    /// Mark a criterion as verified.
    pub fn mark_verified(&mut self, index: usize) {
        if index < self.verified.len() {
            self.verified[index] = true;
        }
    }

    /// Check whether all criteria are verified.
    pub fn all_verified(&self) -> bool {
        !self.verified.is_empty() && self.verified.iter().all(|v| *v)
    }

    /// Return the number of criteria.
    pub fn len(&self) -> usize {
        self.criteria.len()
    }

    /// Return whether there are no criteria.
    pub fn is_empty(&self) -> bool {
        self.criteria.is_empty()
    }
}

impl Default for DoneCriteria {
    fn default() -> Self {
        Self::new()
    }
}

/// Heuristic detection of compound (multi-step) tasks.
///
/// Returns `true` if the user's message appears to request multiple distinct
/// actions — e.g. contains "and" joining action verbs, numbered lists, or
/// multiple imperative sentences.
pub fn is_compound_task(text: &str) -> bool {
    let trimmed = text.trim();

    // Very short messages are rarely compound
    if trimmed.len() < 20 {
        return false;
    }

    // Check for numbered or bulleted lists (e.g. "1. do X  2. do Y")
    let has_numbered_list = {
        let mut count = 0u32;
        for line in trimmed.lines() {
            let l = line.trim();
            if l.starts_with("1.")
                || l.starts_with("2.")
                || l.starts_with("3.")
                || l.starts_with("- ")
                || l.starts_with("* ")
            {
                count += 1;
            }
        }
        count >= 2
    };
    if has_numbered_list {
        return true;
    }

    // Check for "and" or "then" connecting action verbs
    let connectors = [" and ", " then ", ", then ", " after that ", " afterwards "];
    let action_verbs = [
        "deploy",
        "run",
        "create",
        "build",
        "test",
        "install",
        "update",
        "delete",
        "remove",
        "send",
        "check",
        "verify",
        "configure",
        "set up",
        "write",
        "read",
        "copy",
        "move",
        "fix",
        "restart",
        "stop",
        "start",
        "download",
        "upload",
        "push",
        "pull",
        "commit",
        "migrate",
        "fetch",
        "compile",
        "execute",
        "open",
        "close",
        "save",
        "edit",
        "modify",
        "scan",
        "count",
        "find",
        "search",
        "analyze",
        "generate",
        "list",
    ];

    let lower = trimmed.to_lowercase();
    for connector in &connectors {
        if lower.contains(connector) {
            // Check if there are action verbs on both sides of the connector
            if let Some(pos) = lower.find(connector) {
                let before = &lower[..pos];
                let after = &lower[pos + connector.len()..];
                let has_verb_before = action_verbs.iter().any(|v| before.contains(v));
                let has_verb_after = action_verbs.iter().any(|v| after.contains(v));
                if has_verb_before && has_verb_after {
                    return true;
                }
            }
        }
    }

    // Check for multiple imperative sentences (sentences starting with action verbs)
    let sentences: Vec<&str> = trimmed
        .split(['.', '!', ';'])
        .filter(|s| !s.trim().is_empty())
        .collect();

    if sentences.len() >= 2 {
        let imperative_count = sentences
            .iter()
            .filter(|s| {
                let first_word = s.split_whitespace().next().unwrap_or("").to_lowercase();
                action_verbs.iter().any(|v| first_word == *v)
            })
            .count();
        if imperative_count >= 2 {
            return true;
        }
    }

    false
}

/// Generate a prompt asking the LLM to define DONE criteria for a compound task.
pub fn format_done_prompt(user_text: &str) -> String {
    format!(
        "The user has given a compound task. Before executing, define what DONE looks like.\n\
         \n\
         User request: \"{}\"\n\
         \n\
         List specific, verifiable conditions that must ALL be true when the task is complete.\n\
         Format your response as:\n\
         DONE WHEN:\n\
         1. [specific verifiable condition]\n\
         2. [specific verifiable condition]\n\
         ...\n\
         \n\
         Then proceed to execute the task. After completing all steps, verify each condition \
         before declaring the task done.",
        user_text
    )
}

/// Generate a verification prompt to append when the agent produces a final
/// text response for a compound task.
pub fn format_verification_prompt(criteria: &DoneCriteria) -> String {
    if criteria.is_empty() {
        return String::new();
    }

    let criteria_list: String = criteria
        .criteria
        .iter()
        .enumerate()
        .map(|(i, c)| format!("  {}. {}", i + 1, c))
        .collect::<Vec<_>>()
        .join("\n");

    format!(
        "\n\n[VERIFY DONE CRITERIA] Before responding to the user, verify each condition:\n\
         {}\n\
         \n\
         If any condition is NOT met, continue working. Do NOT declare the task complete \
         until all conditions are verified with evidence.",
        criteria_list
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn done_criteria_new_is_empty() {
        let dc = DoneCriteria::new();
        assert!(dc.is_empty());
        assert_eq!(dc.len(), 0);
        assert!(!dc.all_verified());
    }

    #[test]
    fn done_criteria_add_and_verify() {
        let mut dc = DoneCriteria::new();
        let idx0 = dc.add("File exists".to_string());
        let idx1 = dc.add("Tests pass".to_string());

        assert_eq!(dc.len(), 2);
        assert!(!dc.is_empty());
        assert!(!dc.all_verified());

        dc.mark_verified(idx0);
        assert!(!dc.all_verified());

        dc.mark_verified(idx1);
        assert!(dc.all_verified());
    }

    #[test]
    fn done_criteria_mark_out_of_bounds_is_safe() {
        let mut dc = DoneCriteria::new();
        dc.add("something".to_string());
        dc.mark_verified(999); // should not panic
        assert!(!dc.all_verified());
    }

    #[test]
    fn is_compound_short_text_is_not_compound() {
        assert!(!is_compound_task("read the file"));
        assert!(!is_compound_task("hello"));
        assert!(!is_compound_task(""));
    }

    #[test]
    fn is_compound_numbered_list() {
        let text =
            "Please do the following:\n1. Create a new file\n2. Write some content\n3. Save it";
        assert!(is_compound_task(text));
    }

    #[test]
    fn is_compound_bulleted_list() {
        let text = "I need you to:\n- deploy the app\n- run the tests\n- send me the logs";
        assert!(is_compound_task(text));
    }

    #[test]
    fn is_compound_action_verbs_with_and() {
        let text = "Deploy the application and run the migrations and verify the health check";
        assert!(is_compound_task(text));
    }

    #[test]
    fn is_compound_action_verbs_with_then() {
        let text = "Build the project then deploy it to production";
        assert!(is_compound_task(text));
    }

    #[test]
    fn is_compound_multiple_imperative_sentences() {
        let text = "Deploy the app. Run the tests. Send me the results.";
        assert!(is_compound_task(text));
    }

    #[test]
    fn is_compound_single_action_is_not_compound() {
        let text = "Deploy the application to the production server";
        assert!(!is_compound_task(text));
    }

    #[test]
    fn format_done_prompt_contains_user_text() {
        let prompt = format_done_prompt("deploy and test");
        assert!(prompt.contains("deploy and test"));
        assert!(prompt.contains("DONE WHEN"));
        assert!(prompt.contains("verifiable condition"));
    }

    #[test]
    fn format_verification_prompt_empty_criteria() {
        let dc = DoneCriteria::new();
        let prompt = format_verification_prompt(&dc);
        assert!(prompt.is_empty());
    }

    #[test]
    fn format_verification_prompt_lists_criteria() {
        let mut dc = DoneCriteria::new();
        dc.add("File created".to_string());
        dc.add("Tests pass".to_string());

        let prompt = format_verification_prompt(&dc);
        assert!(prompt.contains("File created"));
        assert!(prompt.contains("Tests pass"));
        assert!(prompt.contains("VERIFY DONE CRITERIA"));
    }
}
