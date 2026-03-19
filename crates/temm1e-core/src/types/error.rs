use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Error, Debug)]
pub enum Temm1eError {
    #[error("Configuration error: {0}")]
    Config(String),

    #[error("Provider error: {0}")]
    Provider(String),

    #[error("Channel error: {0}")]
    Channel(String),

    #[error("Memory error: {0}")]
    Memory(String),

    #[error("Vault error: {0}")]
    Vault(String),

    #[error("Tool execution error: {0}")]
    Tool(String),

    #[error("File transfer error: {0}")]
    FileTransfer(String),

    #[error("Authentication error: {0}")]
    Auth(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),

    #[error("Sandbox violation: {0}")]
    SandboxViolation(String),

    #[error("Rate limited: {0}")]
    RateLimited(String),

    #[error("Not found: {0}")]
    NotFound(String),

    #[error("Skill error: {0}")]
    Skill(String),

    #[error("Serialization error: {0}")]
    Serialization(#[from] serde_json::Error),

    #[error("IO error: {0}")]
    Io(#[from] std::io::Error),

    #[error("Internal error: {0}")]
    Internal(String),

    /// Signal from process_message: classifier says Order+Complex,
    /// hive is enabled — caller should route to swarm instead.
    /// Contains the original message text for decomposition.
    #[error("Hive route: {0}")]
    HiveRoute(String),
}

// ── Structured Failure Types (Tem's Mind v2.0) ─────────────────────

/// Category of failure encountered during tool execution or verification.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum FailureKind {
    /// Tool returned an error (non-zero exit, HTTP error, etc.)
    ToolError,
    /// Tool succeeded but output doesn't match expected result.
    WrongOutput,
    /// Task partially done, more steps needed.
    Incomplete,
    /// External service unavailable.
    ServiceDown,
    /// Needs information from the user.
    NeedsInput,
    /// Operation timed out.
    Timeout,
    /// Permission or authentication issue.
    AuthError,
}

impl std::fmt::Display for FailureKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FailureKind::ToolError => write!(f, "ToolError"),
            FailureKind::WrongOutput => write!(f, "WrongOutput"),
            FailureKind::Incomplete => write!(f, "Incomplete"),
            FailureKind::ServiceDown => write!(f, "ServiceDown"),
            FailureKind::NeedsInput => write!(f, "NeedsInput"),
            FailureKind::Timeout => write!(f, "Timeout"),
            FailureKind::AuthError => write!(f, "AuthError"),
        }
    }
}

/// Whether a failure is retryable and how.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum Retryability {
    /// Same approach might work (transient error).
    RetryDirect,
    /// Need a different strategy.
    RetryDifferent,
    /// Can't be resolved without user input.
    NeedsHuman,
    /// Fundamentally impossible.
    Impossible,
}

impl std::fmt::Display for Retryability {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Retryability::RetryDirect => write!(f, "RetryDirect"),
            Retryability::RetryDifferent => write!(f, "RetryDifferent"),
            Retryability::NeedsHuman => write!(f, "NeedsHuman"),
            Retryability::Impossible => write!(f, "Impossible"),
        }
    }
}

/// Structured failure output from verification or tool execution.
/// Designed for minimal token footprint when fed back to THINK.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VerifyFailure {
    /// What category of failure occurred.
    pub kind: FailureKind,
    /// One-line description, max ~50 tokens.
    pub brief: String,
    /// Optional suggestion for retry, max ~30 tokens.
    pub suggestion: Option<String>,
    /// Which tool/step failed.
    pub failed_step: Option<String>,
    /// Is this retryable?
    pub retryable: Retryability,
}

impl VerifyFailure {
    /// Compact representation for injection into conversation history.
    /// Designed to be < 80 tokens in all cases.
    pub fn to_context_string(&self) -> String {
        let mut s = format!("[VERIFY FAILED] {}: {}", self.kind, self.brief);
        if let Some(ref suggestion) = self.suggestion {
            s.push_str(&format!(" | Suggestion: {}", suggestion));
        }
        s.push_str(&format!(" | Retry: {}", self.retryable));
        s
    }
}

/// Recovery action determined by failure classification.
/// Tells the runtime exactly what to do next.
///
/// Named `FailureRecovery` to avoid conflict with `RecoveryAction` in
/// `temm1e-agent::recovery` (which handles startup task recovery).
#[derive(Debug, Clone)]
pub enum FailureRecovery {
    /// Retry the same step with the same approach.
    RetryDirect { max_retries: u32, backoff_ms: u64 },
    /// Retry with a modified approach — hint fed to the LLM.
    RetryModified { hint: String },
    /// Ask the user for input.
    AskUser { question: String },
    /// Skip this step, continue with remaining work.
    SkipStep { reason: String },
    /// Abort entire task.
    Abort { explanation: String },
}

/// Classify a tool failure into a structured `VerifyFailure` from raw error info.
///
/// This is a rule-based classifier — no LLM call needed. Handles common
/// shell errors, HTTP status codes, and timeout patterns.
pub fn classify_tool_failure(
    tool_name: &str,
    exit_code: Option<i32>,
    output: &str,
) -> VerifyFailure {
    let lower = output.to_lowercase();

    // HTTP status code patterns
    if let Some(status) = extract_http_status(&lower) {
        return match status {
            401 | 403 => VerifyFailure {
                kind: FailureKind::AuthError,
                brief: format!("HTTP {}: authentication/permission denied", status),
                suggestion: Some("Check credentials or request access".to_string()),
                failed_step: Some(tool_name.to_string()),
                retryable: Retryability::NeedsHuman,
            },
            404 => VerifyFailure {
                kind: FailureKind::ToolError,
                brief: format!("HTTP {}: resource not found", status),
                suggestion: Some("Check URL or try alternative endpoint".to_string()),
                failed_step: Some(tool_name.to_string()),
                retryable: Retryability::RetryDifferent,
            },
            429 => VerifyFailure {
                kind: FailureKind::ToolError,
                brief: "HTTP 429: rate limited".to_string(),
                suggestion: Some("Wait and retry".to_string()),
                failed_step: Some(tool_name.to_string()),
                retryable: Retryability::RetryDirect,
            },
            500..=599 => VerifyFailure {
                kind: FailureKind::ServiceDown,
                brief: format!("HTTP {}: server error", status),
                suggestion: Some("Service may be temporarily down, retry".to_string()),
                failed_step: Some(tool_name.to_string()),
                retryable: Retryability::RetryDirect,
            },
            _ => VerifyFailure {
                kind: FailureKind::ToolError,
                brief: format!("HTTP {}", status),
                suggestion: None,
                failed_step: Some(tool_name.to_string()),
                retryable: Retryability::RetryDifferent,
            },
        };
    }

    // Shell error patterns
    if lower.contains("permission denied") || lower.contains("access denied") {
        return VerifyFailure {
            kind: FailureKind::AuthError,
            brief: extract_first_error_line(output),
            suggestion: Some("Try with elevated permissions or different path".to_string()),
            failed_step: Some(tool_name.to_string()),
            retryable: Retryability::RetryDifferent,
        };
    }

    if lower.contains("command not found") || lower.contains("not found") {
        return VerifyFailure {
            kind: FailureKind::ToolError,
            brief: extract_first_error_line(output),
            suggestion: Some("Install the command or use an alternative".to_string()),
            failed_step: Some(tool_name.to_string()),
            retryable: Retryability::RetryDifferent,
        };
    }

    if lower.contains("timed out") || lower.contains("timeout") {
        return VerifyFailure {
            kind: FailureKind::Timeout,
            brief: "Operation timed out".to_string(),
            suggestion: Some("Retry or increase timeout".to_string()),
            failed_step: Some(tool_name.to_string()),
            retryable: Retryability::RetryDirect,
        };
    }

    if lower.contains("connection refused") || lower.contains("connection reset") {
        return VerifyFailure {
            kind: FailureKind::ServiceDown,
            brief: extract_first_error_line(output),
            suggestion: Some("Service may be down, retry after a moment".to_string()),
            failed_step: Some(tool_name.to_string()),
            retryable: Retryability::RetryDirect,
        };
    }

    if lower.contains("no space left") || lower.contains("disk full") {
        return VerifyFailure {
            kind: FailureKind::ToolError,
            brief: "Disk full".to_string(),
            suggestion: Some("Free up disk space".to_string()),
            failed_step: Some(tool_name.to_string()),
            retryable: Retryability::NeedsHuman,
        };
    }

    // Default: generic tool error
    VerifyFailure {
        kind: FailureKind::ToolError,
        brief: extract_first_error_line(output),
        suggestion: None,
        failed_step: Some(tool_name.to_string()),
        retryable: if exit_code.unwrap_or(1) != 0 {
            Retryability::RetryDifferent
        } else {
            Retryability::RetryDirect
        },
    }
}

/// Determine the recovery action for a given failure.
pub fn determine_recovery(failure: &VerifyFailure) -> FailureRecovery {
    match failure.retryable {
        Retryability::RetryDirect => FailureRecovery::RetryDirect {
            max_retries: 2,
            backoff_ms: match failure.kind {
                FailureKind::Timeout => 15_000,
                FailureKind::ServiceDown => 10_000,
                _ => 1_000,
            },
        },
        Retryability::RetryDifferent => FailureRecovery::RetryModified {
            hint: failure
                .suggestion
                .clone()
                .unwrap_or_else(|| "Try a different approach".to_string()),
        },
        Retryability::NeedsHuman => FailureRecovery::AskUser {
            question: format!(
                "I need help: {}{}",
                failure.brief,
                failure
                    .suggestion
                    .as_ref()
                    .map(|s| format!(". {}", s))
                    .unwrap_or_default()
            ),
        },
        Retryability::Impossible => FailureRecovery::Abort {
            explanation: failure.brief.clone(),
        },
    }
}

/// Extract HTTP status code from output text.
fn extract_http_status(lower_output: &str) -> Option<u16> {
    // Match patterns like "HTTP/1.1 404", "status: 500", "status code: 429"
    for line in lower_output.lines() {
        if let Some(pos) = line.find("http/") {
            // "http/1.1 404 not found"
            let after = &line[pos..];
            let parts: Vec<&str> = after.split_whitespace().collect();
            if parts.len() >= 2 {
                if let Ok(code) = parts[1].parse::<u16>() {
                    if (100..600).contains(&code) {
                        return Some(code);
                    }
                }
            }
        }
        if line.contains("status") {
            // "status: 404" or "status code: 429"
            for word in line.split_whitespace() {
                if let Ok(code) = word
                    .trim_end_matches(|c: char| !c.is_ascii_digit())
                    .parse::<u16>()
                {
                    if (100..600).contains(&code) {
                        return Some(code);
                    }
                }
            }
        }
    }
    None
}

/// Extract the first error-like line from output, truncated to ~50 tokens.
fn extract_first_error_line(output: &str) -> String {
    let error_patterns = ["error", "fatal", "failed", "denied", "not found", "cannot"];

    for line in output.lines() {
        let lower = line.to_lowercase();
        if error_patterns.iter().any(|p| lower.contains(p)) {
            // Truncate to ~200 chars (~50 tokens)
            let truncated = if line.len() > 200 {
                format!(
                    "{}...",
                    &line[..line
                        .char_indices()
                        .take_while(|(i, _)| *i < 200)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(200)]
                )
            } else {
                line.to_string()
            };
            return truncated;
        }
    }

    // No error line found — take first line
    output
        .lines()
        .next()
        .map(|l| {
            if l.len() > 200 {
                format!(
                    "{}...",
                    &l[..l
                        .char_indices()
                        .take_while(|(i, _)| *i < 200)
                        .last()
                        .map(|(i, c)| i + c.len_utf8())
                        .unwrap_or(200)]
                )
            } else {
                l.to_string()
            }
        })
        .unwrap_or_else(|| "Unknown error".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classify_permission_denied() {
        let f = classify_tool_failure("shell", Some(1), "bash: /etc/hosts: Permission denied");
        assert_eq!(f.kind, FailureKind::AuthError);
        assert_eq!(f.retryable, Retryability::RetryDifferent);
    }

    #[test]
    fn classify_command_not_found() {
        let f = classify_tool_failure("shell", Some(127), "bash: foobar: command not found");
        assert_eq!(f.kind, FailureKind::ToolError);
        assert_eq!(f.retryable, Retryability::RetryDifferent);
    }

    #[test]
    fn classify_timeout() {
        let f = classify_tool_failure("shell", Some(124), "curl: operation timed out");
        assert_eq!(f.kind, FailureKind::Timeout);
        assert_eq!(f.retryable, Retryability::RetryDirect);
    }

    #[test]
    fn classify_connection_refused() {
        let f = classify_tool_failure("web_fetch", None, "connection refused to localhost:8080");
        assert_eq!(f.kind, FailureKind::ServiceDown);
        assert_eq!(f.retryable, Retryability::RetryDirect);
    }

    #[test]
    fn classify_http_404() {
        let f = classify_tool_failure("web_fetch", None, "HTTP/1.1 404 Not Found\nPage not found");
        assert_eq!(f.kind, FailureKind::ToolError);
        assert_eq!(f.retryable, Retryability::RetryDifferent);
    }

    #[test]
    fn classify_http_429() {
        let f = classify_tool_failure("web_fetch", None, "HTTP/1.1 429 Too Many Requests");
        assert_eq!(f.kind, FailureKind::ToolError);
        assert_eq!(f.retryable, Retryability::RetryDirect);
    }

    #[test]
    fn classify_http_500() {
        let f = classify_tool_failure("web_fetch", None, "HTTP/1.1 500 Internal Server Error");
        assert_eq!(f.kind, FailureKind::ServiceDown);
        assert_eq!(f.retryable, Retryability::RetryDirect);
    }

    #[test]
    fn classify_http_401() {
        let f = classify_tool_failure("web_fetch", None, "HTTP/1.1 401 Unauthorized");
        assert_eq!(f.kind, FailureKind::AuthError);
        assert_eq!(f.retryable, Retryability::NeedsHuman);
    }

    #[test]
    fn classify_disk_full() {
        let f = classify_tool_failure("shell", Some(1), "write error: No space left on device");
        assert_eq!(f.kind, FailureKind::ToolError);
        assert_eq!(f.retryable, Retryability::NeedsHuman);
    }

    #[test]
    fn classify_generic_error() {
        let f = classify_tool_failure("shell", Some(1), "something went wrong");
        assert_eq!(f.kind, FailureKind::ToolError);
        assert_eq!(f.retryable, Retryability::RetryDifferent);
    }

    #[test]
    fn verify_failure_compact_string() {
        let f = VerifyFailure {
            kind: FailureKind::AuthError,
            brief: "permission denied on /etc/crontab".to_string(),
            suggestion: Some("use user crontab instead".to_string()),
            failed_step: Some("shell".to_string()),
            retryable: Retryability::RetryDifferent,
        };
        let s = f.to_context_string();
        assert!(s.contains("[VERIFY FAILED]"));
        assert!(s.contains("AuthError"));
        assert!(s.contains("permission denied"));
        assert!(s.contains("Suggestion:"));
        assert!(s.contains("Retry: RetryDifferent"));
        // Should be compact — under 200 chars for this case
        assert!(s.len() < 200, "Context string too long: {} chars", s.len());
    }

    #[test]
    fn recovery_action_for_retry_direct() {
        let f = VerifyFailure {
            kind: FailureKind::Timeout,
            brief: "timed out".to_string(),
            suggestion: None,
            failed_step: None,
            retryable: Retryability::RetryDirect,
        };
        let action = determine_recovery(&f);
        assert!(matches!(
            action,
            FailureRecovery::RetryDirect { max_retries: 2, .. }
        ));
    }

    #[test]
    fn recovery_action_for_needs_human() {
        let f = VerifyFailure {
            kind: FailureKind::AuthError,
            brief: "needs credentials".to_string(),
            suggestion: Some("Provide API key".to_string()),
            failed_step: None,
            retryable: Retryability::NeedsHuman,
        };
        let action = determine_recovery(&f);
        assert!(matches!(action, FailureRecovery::AskUser { .. }));
    }

    #[test]
    fn recovery_action_for_impossible() {
        let f = VerifyFailure {
            kind: FailureKind::ToolError,
            brief: "API discontinued".to_string(),
            suggestion: None,
            failed_step: None,
            retryable: Retryability::Impossible,
        };
        let action = determine_recovery(&f);
        assert!(matches!(action, FailureRecovery::Abort { .. }));
    }

    #[test]
    fn extract_first_error_line_basic() {
        let output = "Building...\nCompiling...\nerror: cannot find module\nDone.";
        let line = extract_first_error_line(output);
        assert!(line.contains("error: cannot find module"));
    }

    #[test]
    fn extract_first_error_line_fallback() {
        let output = "some random output";
        let line = extract_first_error_line(output);
        assert_eq!(line, "some random output");
    }

    #[test]
    fn extract_http_status_from_response() {
        assert_eq!(extract_http_status("http/1.1 404 not found"), Some(404));
        assert_eq!(
            extract_http_status("http/2 500 internal server error"),
            Some(500)
        );
        assert_eq!(extract_http_status("status: 429"), Some(429));
        assert_eq!(extract_http_status("no status here"), None);
    }

    #[test]
    fn unicode_safe_error_extraction() {
        let output = "error: file '\u{30c6}\u{30b9}\u{30c8}.txt' not found";
        let line = extract_first_error_line(output);
        assert!(line.contains("\u{30c6}\u{30b9}\u{30c8}"));
    }

    #[test]
    fn long_error_truncated() {
        let long_error = format!("error: {}", "x".repeat(500));
        let line = extract_first_error_line(&long_error);
        assert!(line.len() <= 210); // 200 + "..."
    }
}
