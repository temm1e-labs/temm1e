//! Tool output compression — intelligently compresses large tool outputs
//! to fit within token budgets while preserving error information.

/// Threshold below which output is returned as-is (roughly 500 tokens).
const SMALL_OUTPUT_THRESHOLD: usize = 2048;

/// Maximum number of key lines to extract from output.
const MAX_KEY_LINES: usize = 50;

/// Patterns indicating errors or warnings in shell output.
const ERROR_PATTERNS: &[&str] = &[
    "error", "Error", "ERROR", "warning", "Warning", "WARNING", "failed", "Failed", "FAILED",
    "fatal", "Fatal", "FATAL", "panic", "PANIC",
];

/// Compress tool output based on the tool type and a rough token budget.
///
/// If the output is small (<2KB), it is returned as-is. For larger outputs,
/// heuristic compression is applied based on the tool name:
/// - Shell outputs: extract errors, warnings, and tail lines
/// - File reads: keep head + tail with omission marker
/// - Web fetch: extract status, content-type, and truncated body
/// - Git: extract status summary and changed files
/// - Default: keep head + tail with truncation marker
///
/// The `max_tokens` parameter sets the rough token budget (1 token ~ 4 chars).
/// Error information is always preserved.
pub fn compress_tool_output(tool_name: &str, output: &str, max_tokens: usize) -> String {
    // Small outputs pass through unchanged
    if output.len() < SMALL_OUTPUT_THRESHOLD {
        return output.to_string();
    }

    let max_chars = max_tokens * 4;

    match tool_name {
        "shell" | "bash" | "command" | "exec" => summarize_shell_output(output, max_chars),
        "file_read" | "read_file" | "cat" => summarize_large_text(output, 50, 20),
        "web_fetch" | "http" | "fetch" | "curl" => summarize_web_output(output, max_chars),
        "git" | "git_status" | "git_diff" | "git_log" => summarize_git_output(output, max_chars),
        _ => default_compress(output, max_chars),
    }
}

/// Scan output for lines matching any of the given patterns (case-insensitive
/// substring match). Returns up to [`MAX_KEY_LINES`] matched lines.
pub fn extract_key_lines(output: &str, patterns: &[&str]) -> Vec<String> {
    let mut matches = Vec::new();

    for line in output.lines() {
        let lower = line.to_lowercase();
        if patterns.iter().any(|p| lower.contains(&p.to_lowercase())) {
            matches.push(line.to_string());
            if matches.len() >= MAX_KEY_LINES {
                break;
            }
        }
    }

    matches
}

/// Summarize shell output by extracting exit code, error/warning lines,
/// and trailing output lines.
///
/// Produces a summary in the format:
/// ```text
/// [Shell output: N lines total, M errors/warnings]
/// {error/warning lines}
/// ...
/// {last 20 lines}
/// ```
pub fn summarize_shell_output(output: &str, max_chars: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let total_lines = lines.len();

    // Extract exit code line if present
    let exit_code_line = lines.iter().find(|l| {
        let lower = l.to_lowercase();
        lower.contains("exit code") || lower.contains("exit status") || lower.starts_with("exit ")
    });

    // Collect error/warning lines
    let key_lines = extract_key_lines(output, ERROR_PATTERNS);
    let error_count = key_lines.len();

    // Keep last 20 lines of output
    let tail_count = 20.min(total_lines);
    let tail_lines: Vec<&str> = lines[total_lines.saturating_sub(tail_count)..].to_vec();

    // Build the summary
    let mut result = format!(
        "[Shell output: {} lines total, {} errors/warnings]\n",
        total_lines, error_count
    );

    if let Some(exit_line) = exit_code_line {
        result.push_str(exit_line);
        result.push('\n');
    }

    if !key_lines.is_empty() {
        result.push_str("--- Key lines ---\n");
        for line in &key_lines {
            result.push_str(line);
            result.push('\n');
        }
    }

    if !tail_lines.is_empty() {
        result.push_str("--- Last lines ---\n");
        for line in &tail_lines {
            result.push_str(line);
            result.push('\n');
        }
    }

    // If still over budget, truncate to max_chars while preserving structure
    if result.len() > max_chars && max_chars > 0 {
        truncate_preserving_lines(&result, max_chars)
    } else {
        result
    }
}

/// Summarize large text by keeping the first `head_lines` and last
/// `tail_lines`, replacing the middle with an omission marker showing
/// how many lines were skipped.
pub fn summarize_large_text(output: &str, head_lines: usize, tail_lines: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let total = lines.len();

    // If small enough, return as-is
    if total <= head_lines + tail_lines {
        return output.to_string();
    }

    let omitted = total - head_lines - tail_lines;

    let mut result = String::new();

    // Head
    for line in &lines[..head_lines] {
        result.push_str(line);
        result.push('\n');
    }

    // Omission marker
    result.push_str(&format!("[...{} lines omitted...]\n", omitted));

    // Tail
    for line in &lines[total - tail_lines..] {
        result.push_str(line);
        result.push('\n');
    }

    result
}

/// Summarize web fetch output by extracting status code, content-type header,
/// and truncating the body.
fn summarize_web_output(output: &str, max_chars: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();

    // Try to extract status code
    let status_line = lines.iter().find(|l| {
        let lower = l.to_lowercase();
        lower.contains("status") || lower.starts_with("http/")
    });

    // Try to extract content-type
    let content_type = lines.iter().find(|l| {
        let lower = l.to_lowercase();
        lower.contains("content-type")
    });

    let mut result = String::from("[Web fetch summary]\n");

    if let Some(status) = status_line {
        result.push_str(status);
        result.push('\n');
    }

    if let Some(ct) = content_type {
        result.push_str(ct);
        result.push('\n');
    }

    // Keep first portion of body within budget
    let remaining_budget = max_chars.saturating_sub(result.len());
    let body_budget = remaining_budget.min(2048);

    if output.len() > body_budget {
        // Take body content up to budget
        let truncated = safe_truncate(output, body_budget);
        result.push_str("--- Body (truncated) ---\n");
        result.push_str(truncated);
        result.push_str("\n[...truncated...]\n");
    } else {
        result.push_str(output);
    }

    result
}

/// Summarize git output by extracting status summaries, file lists,
/// and error messages.
fn summarize_git_output(output: &str, max_chars: usize) -> String {
    let lines: Vec<&str> = output.lines().collect();
    let total = lines.len();

    // Extract status/change lines (lines starting with common git status markers)
    let mut status_lines = Vec::new();
    let mut error_lines = Vec::new();

    for line in &lines {
        let trimmed = line.trim();

        // Git status markers: M, A, D, ??, R, C, U, etc.
        if trimmed.starts_with("M ")
            || trimmed.starts_with("A ")
            || trimmed.starts_with("D ")
            || trimmed.starts_with("?? ")
            || trimmed.starts_with("R ")
            || trimmed.starts_with("C ")
            || trimmed.starts_with("U ")
            || trimmed.starts_with("modified:")
            || trimmed.starts_with("new file:")
            || trimmed.starts_with("deleted:")
            || trimmed.starts_with("renamed:")
            || trimmed.starts_with("diff --git")
            || trimmed.starts_with("+++")
            || trimmed.starts_with("---")
        {
            status_lines.push(*line);
        }

        // Error lines
        let lower = line.to_lowercase();
        if lower.contains("error")
            || lower.contains("fatal")
            || lower.contains("conflict")
            || lower.contains("failed")
        {
            error_lines.push(*line);
        }
    }

    let mut result = format!(
        "[Git output: {} lines, {} changed files, {} errors]\n",
        total,
        status_lines.len(),
        error_lines.len()
    );

    if !error_lines.is_empty() {
        result.push_str("--- Errors ---\n");
        for line in &error_lines {
            result.push_str(line);
            result.push('\n');
        }
    }

    if !status_lines.is_empty() {
        result.push_str("--- Changed files ---\n");
        for line in &status_lines {
            result.push_str(line);
            result.push('\n');
        }
    }

    // If still over budget, truncate
    if result.len() > max_chars && max_chars > 0 {
        truncate_preserving_lines(&result, max_chars)
    } else {
        result
    }
}

/// Default compression: keep first 1KB + last 500 bytes with a truncation marker.
fn default_compress(output: &str, max_chars: usize) -> String {
    let head_budget = (max_chars * 2) / 3; // ~66% for head
    let tail_budget = max_chars / 3; // ~33% for tail

    let head_bytes = head_budget.min(1024);
    let tail_bytes = tail_budget.min(512);

    if output.len() <= head_bytes + tail_bytes {
        return output.to_string();
    }

    let head = safe_truncate(output, head_bytes);
    let tail = safe_truncate_tail(output, tail_bytes);

    let omitted = output.len() - head.len() - tail.len();

    format!("{}\n[...truncated {} bytes...]\n{}", head, omitted, tail)
}

/// Truncate a string at a char boundary, never splitting a multi-byte character.
fn safe_truncate(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    // Walk backwards from max_bytes to find a char boundary
    let mut end = max_bytes;
    while end > 0 && !s.is_char_boundary(end) {
        end -= 1;
    }
    &s[..end]
}

/// Get the last `max_bytes` of a string, aligned to a char boundary.
fn safe_truncate_tail(s: &str, max_bytes: usize) -> &str {
    if s.len() <= max_bytes {
        return s;
    }
    let start = s.len() - max_bytes;
    let mut aligned = start;
    while aligned < s.len() && !s.is_char_boundary(aligned) {
        aligned += 1;
    }
    &s[aligned..]
}

/// Truncate to `max_chars` while trying to preserve complete lines.
fn truncate_preserving_lines(s: &str, max_chars: usize) -> String {
    if s.len() <= max_chars {
        return s.to_string();
    }

    let mut result = String::new();
    for line in s.lines() {
        if result.len() + line.len() + 1 > max_chars {
            result.push_str("[...truncated...]\n");
            break;
        }
        result.push_str(line);
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn small_output_returned_as_is() {
        let small = "Hello, world!";
        let result = compress_tool_output("shell", small, 1000);
        assert_eq!(result, small);
    }

    #[test]
    fn small_output_just_under_threshold() {
        let small = "x".repeat(SMALL_OUTPUT_THRESHOLD - 1);
        let result = compress_tool_output("shell", &small, 1000);
        assert_eq!(result, small);
    }

    #[test]
    fn empty_output_handled() {
        let result = compress_tool_output("shell", "", 1000);
        assert_eq!(result, "");
    }

    #[test]
    fn large_shell_output_compressed_with_errors_preserved() {
        let mut output = String::new();
        for i in 0..200 {
            if i == 50 {
                output.push_str("ERROR: compilation failed at line 42\n");
            } else if i == 100 {
                output.push_str("warning: unused variable `x`\n");
            } else if i == 150 {
                output.push_str("Error: cannot find module 'foo'\n");
            } else {
                output.push_str(&format!("Building module {} ... ok\n", i));
            }
        }

        let result = compress_tool_output("shell", &output, 2000);

        // Must contain the error/warning lines
        assert!(result.contains("ERROR: compilation failed at line 42"));
        assert!(result.contains("warning: unused variable `x`"));
        assert!(result.contains("Error: cannot find module 'foo'"));

        // Must contain summary header
        assert!(result.contains("[Shell output:"));
        assert!(result.contains("errors/warnings]"));

        // Should be shorter than original
        assert!(result.len() < output.len());
    }

    #[test]
    fn shell_output_preserves_exit_code() {
        let mut output = String::new();
        for i in 0..100 {
            output.push_str(&format!("line {}\n", i));
        }
        output.push_str("exit code: 1\n");
        // Pad to exceed threshold
        for i in 100..200 {
            output.push_str(&format!("more output {}\n", i));
        }

        let result = compress_tool_output("shell", &output, 2000);
        assert!(result.contains("exit code: 1"));
    }

    #[test]
    fn large_text_output_head_tail_preserved() {
        let mut lines = Vec::new();
        for i in 0..200 {
            lines.push(format!("Line {}: content here", i));
        }
        let output = lines.join("\n");

        let result = summarize_large_text(&output, 50, 20);

        // First line preserved
        assert!(result.contains("Line 0: content here"));
        // Line 49 preserved (last of head)
        assert!(result.contains("Line 49: content here"));
        // Line 50 should be omitted
        assert!(!result.contains("Line 50: content here"));
        // Last line preserved
        assert!(result.contains("Line 199: content here"));
        // Omission marker present
        assert!(result.contains("[...130 lines omitted...]"));
    }

    #[test]
    fn large_text_small_enough_returned_as_is() {
        let mut lines = Vec::new();
        for i in 0..60 {
            lines.push(format!("Line {}", i));
        }
        let output = lines.join("\n");

        // 50 head + 20 tail = 70, which is > 60 lines total
        let result = summarize_large_text(&output, 50, 20);
        assert_eq!(result, output);
    }

    #[test]
    fn extract_key_lines_finds_errors() {
        let output = "line 1 ok\nerror: bad thing\nline 3 ok\nWarning: careful\nline 5\n";
        let result = extract_key_lines(output, ERROR_PATTERNS);

        assert_eq!(result.len(), 2);
        assert!(result[0].contains("error: bad thing"));
        assert!(result[1].contains("Warning: careful"));
    }

    #[test]
    fn extract_key_lines_case_insensitive() {
        let output = "FATAL crash\nsome normal line\nfailed to connect\n";
        let patterns = &["fatal", "failed"];
        let result = extract_key_lines(output, patterns);

        assert_eq!(result.len(), 2);
        assert!(result[0].contains("FATAL crash"));
        assert!(result[1].contains("failed to connect"));
    }

    #[test]
    fn extract_key_lines_respects_max() {
        let mut output = String::new();
        for i in 0..100 {
            output.push_str(&format!("error line {}\n", i));
        }

        let result = extract_key_lines(&output, &["error"]);
        assert_eq!(result.len(), MAX_KEY_LINES);
    }

    #[test]
    fn extract_key_lines_no_matches() {
        let output = "everything is fine\nall good\nno problems\n";
        let result = extract_key_lines(output, ERROR_PATTERNS);
        assert!(result.is_empty());
    }

    #[test]
    fn error_lines_always_kept_in_shell_summary() {
        let mut output = String::new();
        // Create large output with errors scattered throughout
        for i in 0..500 {
            if i % 100 == 0 {
                output.push_str(&format!("FAILED: step {} did not complete\n", i));
            } else {
                output.push_str(&format!("Processing step {} of 500 ...\n", i));
            }
        }

        let result = summarize_shell_output(&output, 4000);

        // All error lines should be preserved
        assert!(result.contains("FAILED: step 0 did not complete"));
        assert!(result.contains("FAILED: step 100 did not complete"));
        assert!(result.contains("FAILED: step 200 did not complete"));
        assert!(result.contains("FAILED: step 300 did not complete"));
        assert!(result.contains("FAILED: step 400 did not complete"));
    }

    #[test]
    fn unicode_content_does_not_panic() {
        let mut output = String::new();
        // Mix of ASCII and multi-byte characters
        for _ in 0..500 {
            output.push_str("日本語テスト Hello 世界 🦀 Rust\n");
        }

        // None of these should panic
        let _ = compress_tool_output("shell", &output, 500);
        let _ = compress_tool_output("file_read", &output, 500);
        let _ = compress_tool_output("web_fetch", &output, 500);
        let _ = compress_tool_output("git", &output, 500);
        let _ = compress_tool_output("unknown_tool", &output, 500);
    }

    #[test]
    fn unicode_safe_truncate() {
        // 4-byte character: 🦀
        let s = "ab🦀cd";
        // Truncating at byte 3 would split the emoji
        let result = safe_truncate(s, 3);
        assert_eq!(result, "ab");

        // Truncating at byte 6 includes the full emoji
        let result = safe_truncate(s, 6);
        assert_eq!(result, "ab🦀");
    }

    #[test]
    fn unicode_safe_truncate_tail() {
        let s = "ab🦀cd";
        // Get last 2 bytes: "cd"
        let result = safe_truncate_tail(s, 2);
        assert_eq!(result, "cd");

        // Get last 6 bytes: "🦀cd" (4 bytes emoji + 2 bytes)
        let result = safe_truncate_tail(s, 6);
        assert_eq!(result, "🦀cd");
    }

    #[test]
    fn default_compress_large_output() {
        let output = "A".repeat(5000);
        let result = default_compress(&output, 4000);

        assert!(result.contains("[...truncated"));
        assert!(result.len() < output.len());
    }

    #[test]
    fn default_compress_small_output() {
        let output = "small text";
        let result = default_compress(output, 4000);
        assert_eq!(result, output);
    }

    #[test]
    fn file_read_compression() {
        let mut lines = Vec::new();
        for i in 0..200 {
            lines.push(format!("Line {}: some file content here", i));
        }
        let output = lines.join("\n");

        // Pad to exceed 2KB threshold
        let padded = format!("{}{}", output, " ".repeat(2048));

        let result = compress_tool_output("file_read", &padded, 2000);

        // Head lines preserved
        assert!(result.contains("Line 0:"));
        assert!(result.contains("Line 49:"));
        // Omission marker present
        assert!(result.contains("lines omitted"));
    }

    #[test]
    fn web_fetch_compression() {
        let mut output = String::new();
        output.push_str("HTTP/1.1 200 OK\n");
        output.push_str("Content-Type: application/json\n");
        output.push('\n');
        for i in 0..500 {
            output.push_str(&format!("{{\"item\": {}}}\n", i));
        }

        let result = compress_tool_output("web_fetch", &output, 1000);

        assert!(result.contains("[Web fetch summary]"));
        assert!(result.contains("HTTP/1.1 200 OK"));
        assert!(result.contains("Content-Type: application/json"));
        assert!(result.len() < output.len());
    }

    #[test]
    fn git_output_compression() {
        let mut output = String::new();
        output.push_str("M  src/main.rs\n");
        output.push_str("A  src/new_file.rs\n");
        output.push_str("D  src/old_file.rs\n");
        output.push_str("?? untracked.txt\n");
        for i in 0..300 {
            output.push_str(&format!("diff line {}\n", i));
        }

        let result = compress_tool_output("git", &output, 2000);

        assert!(result.contains("[Git output:"));
        assert!(result.contains("M  src/main.rs"));
        assert!(result.contains("A  src/new_file.rs"));
        assert!(result.contains("D  src/old_file.rs"));
        assert!(result.contains("?? untracked.txt"));
        assert!(result.len() < output.len());
    }

    #[test]
    fn git_output_preserves_errors() {
        let mut output = String::new();
        output.push_str("fatal: not a git repository\n");
        for i in 0..300 {
            output.push_str(&format!("some line {}\n", i));
        }

        let result = compress_tool_output("git", &output, 2000);

        assert!(result.contains("fatal: not a git repository"));
        assert!(result.contains("1 errors"));
    }

    #[test]
    fn summarize_shell_output_with_no_errors() {
        let mut output = String::new();
        for i in 0..100 {
            output.push_str(&format!("ok line {}\n", i));
        }

        let result = summarize_shell_output(&output, 8000);

        assert!(result.contains("[Shell output: 100 lines total, 0 errors/warnings]"));
        assert!(result.contains("--- Last lines ---"));
        // Last line should be present
        assert!(result.contains("ok line 99"));
    }

    #[test]
    fn summarize_large_text_exact_boundary() {
        // Exactly head + tail lines
        let mut lines = Vec::new();
        for i in 0..70 {
            lines.push(format!("Line {}", i));
        }
        let output = lines.join("\n");

        let result = summarize_large_text(&output, 50, 20);

        // 70 == 50 + 20, so nothing omitted
        assert_eq!(result, output);
    }

    #[test]
    fn compress_tool_output_unknown_tool_uses_default() {
        let output = "x".repeat(5000);
        let result = compress_tool_output("some_random_tool", &output, 1000);

        assert!(result.contains("[...truncated"));
        assert!(result.len() < output.len());
    }

    #[test]
    fn shell_output_last_lines_preserved() {
        let mut output = String::new();
        for i in 0..100 {
            output.push_str(&format!("build step {}\n", i));
        }
        // Pad to exceed threshold
        let padded = format!("{}{}", output, " ".repeat(2048));

        let result = compress_tool_output("shell", &padded, 2000);

        // Last lines of the actual output should be present
        assert!(result.contains("build step 99"));
    }
}
