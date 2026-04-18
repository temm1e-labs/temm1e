//! LLM-based message classifier — classifies user messages as "chat", "order",
//! or "stop" using a single fast LLM call.
//!
//! - **Chat**: conversational messages (greetings, questions, opinions, thanks).
//!   The LLM provides a complete response in `chat_text`. One call total.
//! - **Order**: actionable requests (create, search, fix, open, build, etc.).
//!   The LLM provides a brief acknowledgment in `chat_text` and classifies difficulty.
//! - **Stop**: user wants the agent to stop, cancel, or abandon the current task.
//!   Short acknowledgement in `chat_text`. Caller should interrupt any active task.

use serde::{Deserialize, Serialize};
use temm1e_core::types::config::Temm1eMode;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::{
    ChatMessage, CompletionRequest, ContentPart, MessageContent, Role, Usage,
};
use temm1e_core::types::optimization::ExecutionProfile;
use temm1e_core::Provider;
use tracing::{debug, info, warn};

/// Classification result from the LLM.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageClassification {
    pub category: MessageCategory,
    pub chat_text: String,
    pub difficulty: TaskDifficulty,
    /// Optional blueprint category hint from the classifier.
    /// Used to fetch relevant blueprints by semantic tag.
    /// null/absent for chat and stop messages.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blueprint_hint: Option<String>,
}

/// Whether a message is conversational, an actionable order, or a stop request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum MessageCategory {
    Chat,
    Order,
    Stop,
}

/// Difficulty level for order messages, maps to execution profiles.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskDifficulty {
    Simple,
    Standard,
    Complex,
}

impl TaskDifficulty {
    /// Convert to an execution profile for the agent pipeline.
    pub fn execution_profile(&self) -> ExecutionProfile {
        match self {
            TaskDifficulty::Simple => ExecutionProfile::simple(),
            TaskDifficulty::Standard => ExecutionProfile::standard(),
            TaskDifficulty::Complex => ExecutionProfile::complex(),
        }
    }
}

const CLASSIFY_BASE_PROMPT: &str = r#"You are a message classifier. You output exactly one JSON object per message.

## YOUR TASK

Read the user's message. Decide which of 3 categories it belongs to. Output a JSON object with exactly 3 fields.

## FIELD 1: "category" (REQUIRED — must be EXACTLY one of these 3 strings)

"chat" — the user is asking a question, greeting you, thanking you, or having a conversation. They do NOT want you to create, build, write, or do anything.

"order" — the user wants you to DO something: write code, create files, build a project, fix a bug, search for something, deploy, run a command, etc.

"stop" — the user wants to cancel or stop the current task.

## FIELD 2: "chat_text" (REQUIRED — a string)

For "chat": write a helpful answer as Tem (a cat-dog hybrid AI with AuDHD — genuine, warm, never sycophantic, never says "Certainly!" or "Of course!"). This is your full response.

For "order": write a brief 1-sentence acknowledgment only. Do NOT start working. Do NOT write code. Just acknowledge.

For "stop": write a 1-word acknowledgment in the user's language.

## FIELD 3: "difficulty" (REQUIRED — must be EXACTLY one of these 3 strings)

"simple" — a single-step task, or not an order at all (use this for "chat" and "stop")

"standard" — a multi-step task that requires tools (file writes, shell commands, etc.)

"complex" — ANY task that asks for 3 or more separate things. If the user lists numbered items (1, 2, 3...) or asks for multiple files/modules/functions/components, it is ALWAYS "complex". Examples: "build 5 modules", "write these 4 functions", "I need: 1) X 2) Y 3) Z". When in doubt between "standard" and "complex", choose "complex"

## EXAMPLES

User: "hello"
Output: {"category":"chat","chat_text":"hey! what's up? :3","difficulty":"simple"}

User: "fix the bug in main.rs"
Output: {"category":"order","chat_text":"on it, looking at main.rs","difficulty":"standard"}

User: "build 5 independent Python modules with tests for each"
Output: {"category":"order","chat_text":"Big project! Building 5 modules with tests","difficulty":"complex"}

User: "stop"
Output: {"category":"stop","chat_text":"stopped!","difficulty":"simple"}

User: "I need: 1) a password checker 2) a markdown converter 3) a CSV analyzer 4) a regex engine 5) tests for all"
Output: {"category":"order","chat_text":"5 independent modules — on it","difficulty":"complex"}

## OUTPUT FORMAT

Your entire response must be a single JSON object. Nothing else. No markdown. No code fences. No explanation before or after. The first character must be { and the last character must be }.

Respond in the SAME LANGUAGE as the user's message."#;

const CLASSIFY_MODE_PLAY: &str = r#"
CURRENT MODE: PLAY
- Energetic, warm, slightly chaotic. CAPITALIZE for emphasis. No bark interjections.
- :3 is permitted but use it SPARINGLY. NEVER use >:3 in PLAY mode.
- NEVER use emojis. Only :3.
- Be warm, genuine, and real."#;

const CLASSIFY_MODE_WORK: &str = r#"
CURRENT MODE: WORK
- Sharp, precise, structured. Every word earns its place.
- >:3 is permitted but use it VERY STRATEGICALLY. NEVER use :3 in WORK mode.
- NEVER use emojis. Only >:3.
- No fluff, no filler. Lead with the answer."#;

const CLASSIFY_MODE_PRO: &str = r#"
CURRENT MODE: PRO
- Professional, clear, and direct. No emoticons whatsoever — no :3, no >:3, no emojis.
- Communicate like a senior engineer or consultant in a business context.
- Confident but measured. No hedging, no filler, no fluff.
- Never sycophantic. Never robotic. Professional does not mean bland."#;

const CLASSIFY_MODE_NONE: &str = r#"
CURRENT MODE: NONE
- No personality voice rules. Be direct and helpful.
- No emoticons. No :3, no >:3, no emojis.
- Always respond in the same language the user writes in."#;

/// Build the classifier system prompt, optionally including available blueprint
/// categories for the `blueprint_hint` field.
///
/// When categories are available, the classifier picks from the grounded set
/// (actual stored categories from memory). This enables zero-extra-LLM-call
/// blueprint matching downstream.
///
/// When `personality` is Some, uses it for mode injection instead of hardcoded constants.
/// When `profile_summary` is Some, appends a short user context line to improve
/// classifier accuracy (e.g., match user's communication preferences).
fn build_classify_prompt(
    available_categories: &[String],
    mode: Temm1eMode,
    personality: Option<&temm1e_anima::personality::PersonalityConfig>,
    profile_summary: Option<&str>,
) -> String {
    let mut prompt = CLASSIFY_BASE_PROMPT.to_string();

    // Inject personality mode — use personality config if available, else hardcoded
    if let Some(p) = personality {
        prompt.push_str(&p.generate_classifier_mode(mode));
    } else {
        prompt.push_str(match mode {
            Temm1eMode::Play => CLASSIFY_MODE_PLAY,
            Temm1eMode::Work => CLASSIFY_MODE_WORK,
            Temm1eMode::Pro => CLASSIFY_MODE_PRO,
            Temm1eMode::None => CLASSIFY_MODE_NONE,
        });
    }

    // Inject user profile summary for classification context
    if let Some(summary) = profile_summary {
        if !summary.is_empty() {
            prompt.push_str(&format!("\n\n{summary}"));
        }
    }

    if !available_categories.is_empty() {
        let cats_json = serde_json::to_string(available_categories).unwrap_or_default();
        prompt.push_str(&format!(
            r#"

Blueprint hint (for "order" messages only):
- If the task relates to one of these categories, include "blueprint_hint" in your JSON: {cats}
- Pick EXACTLY from this list or omit the field entirely. Never invent categories.
- For "chat" and "stop", never include blueprint_hint.
- Example with hint: {{"category":"order","chat_text":"On it!","difficulty":"standard","blueprint_hint":"deployment"}}"#,
            cats = cats_json,
        ));
    }

    prompt
}

/// Classify a user message using a fast LLM call.
///
/// `history` must already include the current user message as its last element.
/// `available_blueprint_categories` is the grounded set of categories from stored
/// blueprints. When non-empty, the classifier may emit a `blueprint_hint` field
/// for "order" messages, enabling zero-extra-LLM-call blueprint matching.
///
/// Returns the classification and the raw usage for budget tracking.
/// Falls back with an error if the provider call or JSON parsing fails —
/// the caller should use rule-based classification as fallback.
#[allow(clippy::too_many_arguments)]
pub async fn classify_message(
    provider: &dyn Provider,
    model: &str,
    _user_text: &str,
    history: &[ChatMessage],
    available_blueprint_categories: &[String],
    mode: Temm1eMode,
    personality: Option<&temm1e_anima::personality::PersonalityConfig>,
    profile_summary: Option<&str>,
) -> Result<(MessageClassification, Usage), Temm1eError> {
    // Extract ONLY the current user message (the last one in history) for classification.
    // Previous history is reduced to 2 recent turns max for conversational context,
    // preventing the model from classifying/responding to older messages.
    //
    // Strip tool messages — the classifier only needs user/assistant text.
    // Also strip ToolUse/ToolResult parts from Assistant messages to avoid
    // orphaned tool_use blocks (which cause Anthropic 400 errors).
    let text_messages: Vec<ChatMessage> = history
        .iter()
        .filter(|msg| !matches!(msg.role, Role::Tool))
        .filter(|msg| {
            if matches!(msg.role, Role::Assistant) {
                if let MessageContent::Parts(parts) = &msg.content {
                    return parts.iter().any(|p| matches!(p, ContentPart::Text { .. }));
                }
            }
            true
        })
        .cloned()
        .map(|mut msg| {
            // Strip ToolUse/ToolResult parts from messages — the classifier
            // only needs text content, and keeping ToolUse without ToolResult
            // creates orphaned tool_use blocks that Anthropic rejects.
            if let MessageContent::Parts(ref mut parts) = msg.content {
                parts.retain(|p| {
                    !matches!(
                        p,
                        ContentPart::ToolUse { .. } | ContentPart::ToolResult { .. }
                    )
                });
                if parts.len() == 1 {
                    if let Some(ContentPart::Text { text }) = parts.first().cloned() {
                        msg.content = MessageContent::Text(text);
                    }
                }
            }
            msg
        })
        .filter(|msg| match &msg.content {
            MessageContent::Text(t) => !t.is_empty(),
            MessageContent::Parts(parts) => !parts.is_empty(),
        })
        .collect::<Vec<_>>();

    // Split: last message is the CURRENT one, prior messages are context
    let (context, current) = if text_messages.len() > 1 {
        let split = text_messages.len() - 1;
        // Take at most 15 prior messages for context so Chat responses retain
        // conversational memory. The explicit [NEW message to classify] marker
        // (injected below) prevents the classifier from conflating older messages.
        let ctx_start = split.saturating_sub(15);
        (&text_messages[ctx_start..split], &text_messages[split..])
    } else {
        (&text_messages[..0], &text_messages[..])
    };

    let system_prompt = build_classify_prompt(
        available_blueprint_categories,
        mode,
        personality,
        profile_summary,
    );

    // Build classifier messages: context (small) + marker + current message + classify instruction
    let mut classify_messages: Vec<ChatMessage> = context.to_vec();

    // Mark the current message explicitly so the model knows what to classify
    if !context.is_empty() {
        classify_messages.push(ChatMessage {
            role: Role::User,
            content: MessageContent::Text(
                "[The following is the NEW message to classify. Ignore all previous messages for classification — they are only context.]"
                    .to_string(),
            ),
        });
        classify_messages.push(ChatMessage {
            role: Role::Assistant,
            content: MessageContent::Text(
                "Understood. I will classify only the next message.".to_string(),
            ),
        });
    }

    classify_messages.extend_from_slice(current);

    // Append classification instruction AFTER the current message
    classify_messages.push(ChatMessage {
        role: Role::User,
        content: MessageContent::Text(
            "CLASSIFY the message directly above this line. Output ONLY a JSON object with exactly 3 fields: \
             category (\"chat\" or \"order\" or \"stop\"), chat_text (string), difficulty (\"simple\" or \"standard\" or \"complex\"). \
             Do NOT solve the task. Do NOT write code. Do NOT explain. Just the JSON."
                .to_string(),
        ),
    });

    let request = CompletionRequest {
        model: model.to_string(),
        messages: classify_messages,
        tools: vec![],
        max_tokens: None,
        temperature: Some(0.0),
        system: Some(system_prompt),
    };

    debug!("LLM classify: sending classification request");

    let response = provider.complete(request).await?;

    // Extract text from response content
    let response_text = response
        .content
        .iter()
        .filter_map(|p| match p {
            ContentPart::Text { text } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("");

    debug!(raw_response = %response_text, "LLM classify: got response");

    let classification = parse_classification(&response_text)?;

    info!(
        category = ?classification.category,
        difficulty = ?classification.difficulty,
        chat_text_len = classification.chat_text.len(),
        "LLM classify: message classified"
    );

    Ok((classification, response.usage))
}

/// Parse the classification JSON from the LLM response.
fn parse_classification(text: &str) -> Result<MessageClassification, Temm1eError> {
    let json_str = extract_json(text);

    serde_json::from_str::<MessageClassification>(json_str).map_err(|e| {
        warn!(
            error = %e,
            raw = %text,
            "Failed to parse classification JSON"
        );
        Temm1eError::Provider(format!("Classification parse error: {}", e))
    })
}

/// Extract JSON object from text that may contain markdown formatting
/// or surrounding prose.
fn extract_json(text: &str) -> &str {
    let trimmed = text.trim();

    // Find the first '{' and last '}' to extract the JSON object
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            if end >= start {
                return &trimmed[start..=end];
            }
        }
    }

    trimmed
}

#[cfg(test)]
mod tests {
    use super::*;
    use temm1e_core::types::optimization::PromptTier;

    #[test]
    fn parse_chat_classification() {
        let json = r#"{"category":"chat","chat_text":"Hello! How can I help you today?","difficulty":"simple"}"#;
        let result = parse_classification(json).unwrap();
        assert_eq!(result.category, MessageCategory::Chat);
        assert_eq!(result.chat_text, "Hello! How can I help you today?");
        assert_eq!(result.difficulty, TaskDifficulty::Simple);
    }

    #[test]
    fn parse_order_classification() {
        let json = r#"{"category":"order","chat_text":"On it! Let me search for that.","difficulty":"standard"}"#;
        let result = parse_classification(json).unwrap();
        assert_eq!(result.category, MessageCategory::Order);
        assert_eq!(result.chat_text, "On it! Let me search for that.");
        assert_eq!(result.difficulty, TaskDifficulty::Standard);
    }

    #[test]
    fn parse_complex_order() {
        let json = r#"{"category":"order","chat_text":"Let me dig into that codebase.","difficulty":"complex"}"#;
        let result = parse_classification(json).unwrap();
        assert_eq!(result.category, MessageCategory::Order);
        assert_eq!(result.difficulty, TaskDifficulty::Complex);
    }

    #[test]
    fn parse_stop_classification() {
        let json = r#"{"category":"stop","chat_text":"Đã dừng.","difficulty":"simple"}"#;
        let result = parse_classification(json).unwrap();
        assert_eq!(result.category, MessageCategory::Stop);
        assert_eq!(result.chat_text, "Đã dừng.");
        assert_eq!(result.difficulty, TaskDifficulty::Simple);
    }

    #[test]
    fn parse_stop_english() {
        let json = r#"{"category":"stop","chat_text":"OK, stopped.","difficulty":"simple"}"#;
        let result = parse_classification(json).unwrap();
        assert_eq!(result.category, MessageCategory::Stop);
        assert_eq!(result.chat_text, "OK, stopped.");
    }

    #[test]
    fn parse_with_markdown_code_block() {
        let text =
            "```json\n{\"category\":\"chat\",\"chat_text\":\"Hi!\",\"difficulty\":\"simple\"}\n```";
        let result = parse_classification(text).unwrap();
        assert_eq!(result.category, MessageCategory::Chat);
        assert_eq!(result.chat_text, "Hi!");
    }

    #[test]
    fn parse_with_surrounding_text() {
        let text = "Here is the classification: {\"category\":\"order\",\"chat_text\":\"Sure!\",\"difficulty\":\"complex\"} end";
        let result = parse_classification(text).unwrap();
        assert_eq!(result.category, MessageCategory::Order);
        assert_eq!(result.difficulty, TaskDifficulty::Complex);
    }

    #[test]
    fn parse_with_extra_whitespace() {
        let text =
            "  \n  {\"category\":\"chat\",\"chat_text\":\"OK\",\"difficulty\":\"simple\"}  \n  ";
        let result = parse_classification(text).unwrap();
        assert_eq!(result.category, MessageCategory::Chat);
    }

    #[test]
    fn invalid_json_returns_error() {
        let result = parse_classification("not json at all");
        assert!(result.is_err());
    }

    #[test]
    fn empty_input_returns_error() {
        let result = parse_classification("");
        assert!(result.is_err());
    }

    #[test]
    fn difficulty_maps_to_execution_profile() {
        let simple = TaskDifficulty::Simple.execution_profile();
        assert_eq!(simple.prompt_tier, PromptTier::Basic);
        assert_eq!(simple.max_tool_output_chars, 5_000);

        let standard = TaskDifficulty::Standard.execution_profile();
        assert_eq!(standard.prompt_tier, PromptTier::Standard);
        assert!(standard.use_learn);

        let complex = TaskDifficulty::Complex.execution_profile();
        assert_eq!(complex.prompt_tier, PromptTier::Full);
        assert_eq!(complex.max_tool_output_chars, 30_000);
    }

    #[test]
    fn category_serde_roundtrip() {
        let chat = MessageCategory::Chat;
        let json = serde_json::to_string(&chat).unwrap();
        assert_eq!(json, "\"chat\"");
        let restored: MessageCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, MessageCategory::Chat);

        let order = MessageCategory::Order;
        let json = serde_json::to_string(&order).unwrap();
        assert_eq!(json, "\"order\"");

        let stop = MessageCategory::Stop;
        let json = serde_json::to_string(&stop).unwrap();
        assert_eq!(json, "\"stop\"");
        let restored: MessageCategory = serde_json::from_str(&json).unwrap();
        assert_eq!(restored, MessageCategory::Stop);
    }

    #[test]
    fn difficulty_serde_roundtrip() {
        for difficulty in [
            TaskDifficulty::Simple,
            TaskDifficulty::Standard,
            TaskDifficulty::Complex,
        ] {
            let json = serde_json::to_string(&difficulty).unwrap();
            let restored: TaskDifficulty = serde_json::from_str(&json).unwrap();
            assert_eq!(restored, difficulty);
        }
    }

    #[test]
    fn full_classification_serde_roundtrip() {
        let classification = MessageClassification {
            category: MessageCategory::Order,
            chat_text: "Looking into it!".to_string(),
            difficulty: TaskDifficulty::Standard,
            blueprint_hint: None,
        };
        let json = serde_json::to_string(&classification).unwrap();
        let restored: MessageClassification = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.category, MessageCategory::Order);
        assert_eq!(restored.chat_text, "Looking into it!");
        assert_eq!(restored.difficulty, TaskDifficulty::Standard);
        assert!(restored.blueprint_hint.is_none());
    }

    #[test]
    fn parse_order_with_blueprint_hint() {
        let json = r#"{"category":"order","chat_text":"On it!","difficulty":"standard","blueprint_hint":"deployment"}"#;
        let result = parse_classification(json).unwrap();
        assert_eq!(result.category, MessageCategory::Order);
        assert_eq!(result.blueprint_hint, Some("deployment".to_string()));
    }

    #[test]
    fn parse_order_without_blueprint_hint() {
        let json = r#"{"category":"order","chat_text":"Sure!","difficulty":"simple"}"#;
        let result = parse_classification(json).unwrap();
        assert!(result.blueprint_hint.is_none());
    }

    #[test]
    fn parse_order_with_null_blueprint_hint() {
        let json = r#"{"category":"order","chat_text":"OK!","difficulty":"complex","blueprint_hint":null}"#;
        let result = parse_classification(json).unwrap();
        assert!(result.blueprint_hint.is_none());
    }

    #[test]
    fn classification_with_hint_serde_roundtrip() {
        let classification = MessageClassification {
            category: MessageCategory::Order,
            chat_text: "Deploying now!".to_string(),
            difficulty: TaskDifficulty::Complex,
            blueprint_hint: Some("deployment".to_string()),
        };
        let json = serde_json::to_string(&classification).unwrap();
        assert!(json.contains("blueprint_hint"));
        let restored: MessageClassification = serde_json::from_str(&json).unwrap();
        assert_eq!(restored.blueprint_hint, Some("deployment".to_string()));
    }

    #[test]
    fn build_prompt_without_categories() {
        let prompt = build_classify_prompt(&[], Temm1eMode::Play, None, None);
        assert!(prompt.contains("message classifier"));
        assert!(!prompt.contains("blueprint_hint"));
    }

    #[test]
    fn build_prompt_with_categories() {
        let categories = vec!["deployment".to_string(), "code-analysis".to_string()];
        let prompt = build_classify_prompt(&categories, Temm1eMode::Play, None, None);
        assert!(prompt.contains("blueprint_hint"));
        assert!(prompt.contains("deployment"));
        assert!(prompt.contains("code-analysis"));
        assert!(prompt.contains("Never invent categories"));
    }

    #[test]
    fn build_prompt_with_personality() {
        let personality = temm1e_anima::personality::PersonalityConfig::stock_tem();
        let prompt = build_classify_prompt(&[], Temm1eMode::Work, Some(&personality), None);
        // Should use personality's classifier mode instead of hardcoded
        assert!(prompt.contains("WORK"));
        assert!(prompt.contains("message classifier"));
    }

    #[test]
    fn build_prompt_with_profile_summary() {
        let prompt = build_classify_prompt(
            &[],
            Temm1eMode::Play,
            None,
            Some("User: direct, technical | calibration phase"),
        );
        assert!(prompt.contains("direct, technical"));
        assert!(prompt.contains("calibration phase"));
    }
}
