//! Prompted tool calling — fallback for models that don't support native
//! function calling.  When the provider returns a 400 for a tool-bearing
//! request we inject the tool definitions into the system prompt and ask
//! the model to emit a JSON object instead.
//!
//! ## JSON contract
//!
//! The model is asked to produce:
//!
//! ```json
//! {"response": "text for the user", "tool_call": {"name": "...", "arguments": {...}}}
//! ```
//!
//! If the model doesn't want to call a tool it can respond with plain text
//! or `{"response": "..."}` (no `tool_call` field).
//!
//! ## Parsing strategy
//!
//! 1. Strip markdown code fences (` ```json ... ``` `).
//! 2. Locate the first `{` and last `}` to extract a JSON candidate.
//! 3. Try `serde_json::from_str`.
//! 4. If valid: look for `.tool_call.name` + `.tool_call.arguments`.
//! 5. If invalid JSON or no tool_call field: treat the entire original text
//!    as a plain-text response.

use serde_json::Value;
use temm1e_core::types::message::ToolDefinition;

/// Result of parsing a model response in prompted-tool-calling mode.
#[derive(Debug, Clone)]
pub enum PromptedToolResult {
    /// Model wants to call a tool.
    ToolCall {
        /// Text the model wanted to show the user (may be empty).
        response_text: String,
        /// Name of the tool to call.
        tool_name: String,
        /// Arguments as a JSON value.
        arguments: Value,
    },
    /// Plain text response — no tool call.
    TextOnly(String),
}

// ────────────────────────────────────────────────────────────────────────
// Prompt construction
// ────────────────────────────────────────────────────────────────────────

/// Build the system-prompt appendix that describes available tools and the
/// expected JSON output format.
pub fn format_tools_prompt(tools: &[ToolDefinition]) -> String {
    let mut prompt = String::from(
        "\n\n---\n\
         [TOOL CALLING]\n\
         You have access to the following tools. When you want to use a tool, \
         respond with ONLY a JSON object in this exact format (no markdown, no \
         extra text before or after):\n\
         {\"response\": \"brief explanation\", \"tool_call\": {\"name\": \"tool_name\", \"arguments\": {…}}}\n\n\
         If you don't need a tool, respond normally with plain text.\n\n\
         Available tools:\n",
    );

    for tool in tools {
        prompt.push_str(&format!(
            "- {}: {}\n  Parameters: {}\n",
            tool.name, tool.description, tool.parameters
        ));
    }

    prompt
}

/// Build a stricter retry prompt when the first JSON attempt was malformed.
pub fn format_strict_retry_prompt() -> &'static str {
    "[IMPORTANT] Your previous response was not valid JSON. \
     Respond with ONLY a raw JSON object — no markdown code fences, no \
     explanation text before or after. Example:\n\
     {\"response\": \"searching now\", \"tool_call\": {\"name\": \"search\", \"arguments\": {\"query\": \"test\"}}}\n\
     If you do not need a tool, respond with plain text only."
}

// ────────────────────────────────────────────────────────────────────────
// JSON parsing
// ────────────────────────────────────────────────────────────────────────

/// Parse the model's text output looking for a JSON tool call.
///
/// **Resilience notes**:
/// - Never panics — all paths return a valid `PromptedToolResult`.
/// - Handles markdown code fences, leading/trailing text, partial JSON.
/// - On any parse failure the original text is returned as `TextOnly`.
pub fn parse_tool_call_json(raw: &str) -> PromptedToolResult {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return PromptedToolResult::TextOnly(String::new());
    }

    // Step 1: strip markdown code fences
    let stripped = strip_code_fences(trimmed);

    // Step 2: locate the outermost { ... }
    let json_candidate = match extract_json_object(&stripped) {
        Some(s) => s,
        None => return PromptedToolResult::TextOnly(raw.to_string()),
    };

    // Step 3: parse as JSON
    let value: Value = match serde_json::from_str(json_candidate) {
        Ok(v) => v,
        Err(_) => return PromptedToolResult::TextOnly(raw.to_string()),
    };

    // Step 4: extract fields
    let response_text = value
        .get("response")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let tool_call = match value.get("tool_call") {
        Some(tc) if tc.is_object() => tc,
        _ => {
            // Valid JSON but no tool_call — use response field or raw text
            if response_text.is_empty() {
                return PromptedToolResult::TextOnly(raw.to_string());
            }
            return PromptedToolResult::TextOnly(response_text);
        }
    };

    let tool_name = match tool_call.get("name").and_then(|v| v.as_str()) {
        Some(name) if !name.is_empty() => name.to_string(),
        _ => return PromptedToolResult::TextOnly(response_text),
    };

    let arguments = tool_call
        .get("arguments")
        .cloned()
        .unwrap_or(Value::Object(serde_json::Map::new()));

    PromptedToolResult::ToolCall {
        response_text,
        tool_name,
        arguments,
    }
}

/// Strip ` ```json ... ``` ` or ` ``` ... ``` ` fences from the text.
fn strip_code_fences(text: &str) -> String {
    let mut result = text.to_string();

    // Handle ```json ... ``` and ``` ... ```
    if let Some(start) = result.find("```") {
        // Find the end of the opening fence line
        let fence_end = result[start + 3..]
            .find('\n')
            .map(|i| start + 3 + i + 1)
            .unwrap_or(start + 3);

        // Find the closing fence
        if let Some(close) = result[fence_end..].find("```") {
            result = result[fence_end..fence_end + close].to_string();
        } else {
            // No closing fence — take everything after opening
            result = result[fence_end..].to_string();
        }
    }

    result.trim().to_string()
}

/// Find the first balanced `{ ... }` object in the text.
///
/// Uses simple brace counting — handles nested objects but NOT
/// braces inside JSON strings.  Good enough for the tool-call format
/// where the JSON is relatively flat.  Deliberately conservative:
/// returns `None` if braces are unbalanced.
fn extract_json_object(text: &str) -> Option<&str> {
    let start = text.find('{')?;
    let mut depth = 0i32;
    let mut in_string = false;
    let mut escape_next = false;

    for (i, ch) in text[start..].char_indices() {
        if escape_next {
            escape_next = false;
            continue;
        }
        match ch {
            '\\' if in_string => {
                escape_next = true;
            }
            '"' => {
                in_string = !in_string;
            }
            '{' if !in_string => {
                depth += 1;
            }
            '}' if !in_string => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[start..start + i + 1]);
                }
            }
            _ => {}
        }
    }

    None // unbalanced
}

/// Build a user-friendly message when all prompted-tool-calling retries
/// are exhausted.  This message is shown to the end-user in chat.
pub fn tool_fallback_user_message(model: &str, tool_name: &str) -> String {
    format!(
        "I tried to use the \"{tool_name}\" tool but your current model (`{model}`) \
         couldn't process the request. This usually happens with models that don't \
         support function calling.\n\n\
         You can:\n\
         • Switch to a tool-capable model with /model (e.g. gpt-4o, claude-sonnet, \
         qwen-2.5-72b-instruct)\n\
         • Or ask me again — I'll answer from my knowledge without tools."
    )
}

// ────────────────────────────────────────────────────────────────────────
// Tests
// ────────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_tool_call() {
        let input = r#"{"response": "Let me search", "tool_call": {"name": "search", "arguments": {"query": "moltbook"}}}"#;
        match parse_tool_call_json(input) {
            PromptedToolResult::ToolCall {
                response_text,
                tool_name,
                arguments,
            } => {
                assert_eq!(response_text, "Let me search");
                assert_eq!(tool_name, "search");
                assert_eq!(arguments["query"], "moltbook");
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }
    }

    #[test]
    fn parse_json_without_tool_call() {
        let input = r#"{"response": "I don't need any tools for this."}"#;
        match parse_tool_call_json(input) {
            PromptedToolResult::TextOnly(text) => {
                assert_eq!(text, "I don't need any tools for this.");
            }
            other => panic!("Expected TextOnly, got {:?}", other),
        }
    }

    #[test]
    fn parse_plain_text() {
        let input = "Here is a plain text response with no JSON at all.";
        match parse_tool_call_json(input) {
            PromptedToolResult::TextOnly(text) => {
                assert_eq!(text, input);
            }
            other => panic!("Expected TextOnly, got {:?}", other),
        }
    }

    #[test]
    fn parse_markdown_fenced_json() {
        let input = "```json\n{\"response\": \"searching\", \"tool_call\": {\"name\": \"search\", \"arguments\": {\"query\": \"test\"}}}\n```";
        match parse_tool_call_json(input) {
            PromptedToolResult::ToolCall { tool_name, .. } => {
                assert_eq!(tool_name, "search");
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }
    }

    #[test]
    fn parse_text_before_json() {
        let input = "Sure, let me search for that.\n{\"response\": \"Searching\", \"tool_call\": {\"name\": \"web_search\", \"arguments\": {\"q\": \"rust\"}}}";
        match parse_tool_call_json(input) {
            PromptedToolResult::ToolCall { tool_name, .. } => {
                assert_eq!(tool_name, "web_search");
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }
    }

    #[test]
    fn parse_empty_string() {
        match parse_tool_call_json("") {
            PromptedToolResult::TextOnly(text) => {
                assert!(text.is_empty());
            }
            other => panic!("Expected TextOnly, got {:?}", other),
        }
    }

    #[test]
    fn parse_invalid_json() {
        let input = "{not valid json at all";
        match parse_tool_call_json(input) {
            PromptedToolResult::TextOnly(text) => {
                assert_eq!(text, input);
            }
            other => panic!("Expected TextOnly, got {:?}", other),
        }
    }

    #[test]
    fn parse_tool_call_missing_name() {
        let input = r#"{"response": "hmm", "tool_call": {"arguments": {"x": 1}}}"#;
        match parse_tool_call_json(input) {
            PromptedToolResult::TextOnly(text) => {
                assert_eq!(text, "hmm");
            }
            other => panic!("Expected TextOnly (missing name), got {:?}", other),
        }
    }

    #[test]
    fn parse_tool_call_empty_name() {
        let input = r#"{"response": "hmm", "tool_call": {"name": "", "arguments": {}}}"#;
        match parse_tool_call_json(input) {
            PromptedToolResult::TextOnly(text) => {
                assert_eq!(text, "hmm");
            }
            other => panic!("Expected TextOnly (empty name), got {:?}", other),
        }
    }

    #[test]
    fn parse_tool_call_no_arguments_field() {
        let input = r#"{"response": "ok", "tool_call": {"name": "ping"}}"#;
        match parse_tool_call_json(input) {
            PromptedToolResult::ToolCall {
                tool_name,
                arguments,
                ..
            } => {
                assert_eq!(tool_name, "ping");
                assert!(arguments.is_object());
                assert!(arguments.as_object().unwrap().is_empty());
            }
            other => panic!("Expected ToolCall with empty args, got {:?}", other),
        }
    }

    #[test]
    fn parse_nested_json_in_arguments() {
        let input = r#"{"response": "complex", "tool_call": {"name": "shell", "arguments": {"command": "echo {\"key\": \"value\"}"}}}"#;
        // This tests brace counting with strings
        let result = parse_tool_call_json(input);
        // Whether it parses or falls back, it should NOT panic
        match result {
            PromptedToolResult::ToolCall { tool_name, .. } => {
                assert_eq!(tool_name, "shell");
            }
            PromptedToolResult::TextOnly(_) => {
                // Also acceptable — escaped braces in strings are tricky
            }
        }
    }

    #[test]
    fn format_tools_prompt_includes_all_tools() {
        let tools = vec![
            ToolDefinition {
                name: "search".to_string(),
                description: "Search the web".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {"query": {"type": "string"}}}),
            },
            ToolDefinition {
                name: "shell".to_string(),
                description: "Run a command".to_string(),
                parameters: serde_json::json!({"type": "object", "properties": {"command": {"type": "string"}}}),
            },
        ];
        let prompt = format_tools_prompt(&tools);
        assert!(prompt.contains("search"));
        assert!(prompt.contains("shell"));
        assert!(prompt.contains("Search the web"));
        assert!(prompt.contains("Run a command"));
        assert!(prompt.contains("TOOL CALLING"));
    }

    #[test]
    fn tool_fallback_message_includes_model_and_tool() {
        let msg = tool_fallback_user_message("qwen/qwen3.5-9b", "search");
        assert!(msg.contains("qwen/qwen3.5-9b"));
        assert!(msg.contains("search"));
        assert!(msg.contains("/model"));
    }

    #[test]
    fn strip_code_fences_basic() {
        let input = "```json\n{\"a\": 1}\n```";
        assert_eq!(strip_code_fences(input), "{\"a\": 1}");
    }

    #[test]
    fn strip_code_fences_no_closing() {
        let input = "```json\n{\"a\": 1}";
        assert_eq!(strip_code_fences(input), "{\"a\": 1}");
    }

    #[test]
    fn strip_code_fences_no_fences() {
        let input = "{\"a\": 1}";
        assert_eq!(strip_code_fences(input), "{\"a\": 1}");
    }

    #[test]
    fn extract_json_object_basic() {
        let text = r#"some text {"key": "value"} more text"#;
        assert_eq!(extract_json_object(text), Some(r#"{"key": "value"}"#));
    }

    #[test]
    fn extract_json_object_nested() {
        let text = r#"{"outer": {"inner": 1}}"#;
        assert_eq!(
            extract_json_object(text),
            Some(r#"{"outer": {"inner": 1}}"#)
        );
    }

    #[test]
    fn extract_json_object_with_string_braces() {
        let text = r#"{"key": "value with { and }"}"#;
        assert_eq!(
            extract_json_object(text),
            Some(r#"{"key": "value with { and }"}"#)
        );
    }

    #[test]
    fn extract_json_object_unbalanced() {
        let text = r#"{"key": "value""#;
        assert_eq!(extract_json_object(text), None);
    }

    #[test]
    fn extract_json_object_no_braces() {
        let text = "just plain text";
        assert_eq!(extract_json_object(text), None);
    }

    #[test]
    fn unicode_safety_in_parse() {
        // Vietnamese text that historically caused panics
        let input = "Tôi đang tìm kiếm thông tin về moltbook cho bạn.";
        match parse_tool_call_json(input) {
            PromptedToolResult::TextOnly(text) => {
                assert_eq!(text, input);
            }
            other => panic!("Expected TextOnly, got {:?}", other),
        }
    }

    #[test]
    fn unicode_in_json_tool_call() {
        let input = r#"{"response": "Tìm kiếm cho bạn", "tool_call": {"name": "search", "arguments": {"query": "xin chào"}}}"#;
        match parse_tool_call_json(input) {
            PromptedToolResult::ToolCall {
                response_text,
                tool_name,
                arguments,
            } => {
                assert_eq!(response_text, "Tìm kiếm cho bạn");
                assert_eq!(tool_name, "search");
                assert_eq!(arguments["query"], "xin chào");
            }
            other => panic!("Expected ToolCall, got {:?}", other),
        }
    }
}
