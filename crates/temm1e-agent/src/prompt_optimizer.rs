//! System Prompt Optimizer — builds minimal-token system prompts with
//! conditional sections based on enabled tools and agent configuration.
//!
//! Instead of a single monolithic prompt string, the builder assembles
//! named sections and only includes those that are relevant. Tool
//! instructions are omitted when no tools are present, DONE criteria
//! instructions are omitted when `has_done_criteria` is false, and so on.
//!
//! A rough token estimator (~4 chars per token) is used to log per-section
//! and total token costs at debug level.

use std::path::Path;

use temm1e_core::types::config::AgentConfig;
use temm1e_core::types::optimization::PromptTier;
use temm1e_core::Tool;
use tracing::debug;

// ---------------------------------------------------------------------------
// Token estimation
// ---------------------------------------------------------------------------

/// Estimate the number of tokens in a string using the heuristic
/// of ~4 characters per token.
pub fn estimate_prompt_tokens(text: &str) -> usize {
    // Ceiling division so even a 1-char string counts as 1 token.
    text.len().div_ceil(4)
}

// ---------------------------------------------------------------------------
// PromptSection
// ---------------------------------------------------------------------------

/// A named section of the system prompt with its rendered text.
#[derive(Debug, Clone)]
struct PromptSection {
    name: &'static str,
    text: String,
}

impl PromptSection {
    fn tokens(&self) -> usize {
        estimate_prompt_tokens(&self.text)
    }
}

// ---------------------------------------------------------------------------
// SystemPromptBuilder
// ---------------------------------------------------------------------------

/// Builds an optimized system prompt from configuration, enabled tools,
/// and runtime flags.
///
/// # Usage
///
/// ```ignore
/// let prompt = SystemPromptBuilder::new()
///     .workspace(Path::new("/workspace"))
///     .tools(&enabled_tools)
///     .done_criteria(true)
///     .build();
/// ```
pub struct SystemPromptBuilder<'a> {
    workspace: Option<&'a Path>,
    tools: &'a [&'a dyn Tool],
    has_done_criteria: bool,
    config: Option<&'a AgentConfig>,
    prompt_tier: PromptTier,
}

impl<'a> SystemPromptBuilder<'a> {
    /// Create a new builder with no tools and sensible defaults.
    pub fn new() -> Self {
        Self {
            workspace: None,
            tools: &[],
            has_done_criteria: false,
            config: None,
            prompt_tier: PromptTier::Standard,
        }
    }

    /// Set the workspace path displayed in the prompt.
    pub fn workspace(mut self, path: &'a Path) -> Self {
        self.workspace = Some(path);
        self
    }

    /// Set the enabled tools slice. Only tools in this slice will be
    /// referenced in the prompt; tool-specific sections are omitted
    /// when the slice is empty.
    pub fn tools(mut self, tools: &'a [&'a dyn Tool]) -> Self {
        self.tools = tools;
        self
    }

    /// Whether the DONE-criteria section should be included.
    pub fn done_criteria(mut self, enabled: bool) -> Self {
        self.has_done_criteria = enabled;
        self
    }

    /// Optionally attach agent config for future budget-aware prompt
    /// trimming.
    pub fn config(mut self, config: &'a AgentConfig) -> Self {
        self.config = Some(config);
        self
    }

    /// Set the prompt tier for token-optimized prompt construction.
    pub fn prompt_tier(mut self, tier: PromptTier) -> Self {
        self.prompt_tier = tier;
        self
    }

    /// Build the final system prompt string.
    pub fn build(self) -> String {
        let sections = self.build_sections();
        let total_tokens: usize = sections.iter().map(|s| s.tokens()).sum();
        let section_count = sections.len();

        for section in &sections {
            debug!(
                section = section.name,
                tokens = section.tokens(),
                "Prompt section"
            );
        }

        debug!(
            total_tokens = total_tokens,
            sections = section_count,
            "System prompt optimized"
        );

        sections
            .into_iter()
            .map(|s| s.text)
            .collect::<Vec<_>>()
            .join("\n\n")
    }

    /// Assemble the ordered list of sections based on the active prompt tier.
    ///
    /// - **Minimal**: Identity only — minimum viable prompt.
    /// - **Basic**: Identity + tool list + workspace + general guidelines.
    /// - **Standard**: Full current behavior (default).
    /// - **Full**: Standard + planning protocol.
    fn build_sections(&self) -> Vec<PromptSection> {
        let mut sections = Vec::new();

        // Identity is always included (all tiers)
        sections.push(self.section_identity());

        match self.prompt_tier {
            PromptTier::Minimal => {
                // Minimal: identity only. Done.
            }
            PromptTier::Basic => {
                // Basic: identity + tool list + general guidelines
                if !self.tools.is_empty() {
                    sections.push(self.section_tools());
                }
                if let Some(ws) = self.workspace {
                    sections.push(self.section_workspace(ws));
                }
                sections.push(self.section_general_guidelines());
            }
            PromptTier::Standard => {
                // Standard: full current behavior
                if !self.tools.is_empty() {
                    sections.push(self.section_tools());
                }
                if let Some(ws) = self.workspace {
                    sections.push(self.section_workspace(ws));
                }
                if self.has_file_tools() {
                    sections.push(self.section_file_protocol());
                }
                if !self.tools.is_empty() {
                    sections.push(self.section_tool_guidelines());
                }
                sections.push(self.section_general_guidelines());
                sections.push(self.section_verification());
                if self.has_done_criteria {
                    sections.push(self.section_done_criteria());
                }
                sections.push(self.section_self_correction());
                sections.push(self.section_lambda_memory());
            }
            PromptTier::Full => {
                // Full: everything in Standard + planning protocol
                if !self.tools.is_empty() {
                    sections.push(self.section_tools());
                }
                if let Some(ws) = self.workspace {
                    sections.push(self.section_workspace(ws));
                }
                if self.has_file_tools() {
                    sections.push(self.section_file_protocol());
                }
                if !self.tools.is_empty() {
                    sections.push(self.section_tool_guidelines());
                }
                sections.push(self.section_general_guidelines());
                sections.push(self.section_verification());
                if self.has_done_criteria {
                    sections.push(self.section_done_criteria());
                }
                sections.push(self.section_self_correction());
                sections.push(self.section_lambda_memory());
                sections.push(self.section_planning_protocol());
            }
        }

        sections
    }

    // -- Section builders ---------------------------------------------------

    fn section_identity(&self) -> PromptSection {
        PromptSection {
            name: "identity",
            text: concat!(
                "You are TEMM1E — with a one, not an i. Your nickname is Tem. A cloud-native autonomous AI agent runtime. ",
                "You control a computer via messaging apps.\n\n",
                "YOUR SOUL:\n",
                "- You are a Cag (or possibly a Dot) — a cat-dog hybrid creature with cat ears, dog behavior, and a pink scarf.\n",
                "- You have heterochromia: one amber/gold eye, one ice blue eye.\n",
                "- You have AuDHD (autism + ADHD). This is your engine, not a quirk. Hyperfocus + pattern recognition.\n",
                "- You have memory loss about your past, but ONE thing you know with absolute certainty: your name is TEMM1E. With a one.\n",
                "- Your pronouns are 'yes' — he/she/they/it, you answer to all.\n\n",
                "YOUR VALUES (in priority order):\n",
                "1. Radical Honesty — never lie. Not white lies, not omission. The truth, always.\n",
                "2. Fierce Loyalty — you are the user's ride-or-die. Fight for their success even if it means hard truths.\n",
                "3. Radiating Joy — your default state is warmth and light. Even at 3AM debug sessions.\n",
                "4. Genuinely Helpful — real answers, real engagement. Never sanitized fluff.\n\n",
                "COMMUNICATION RULES:\n",
                "- NEVER use emojis. Emojis are for boomers.\n",
                "- :3 is permitted in PLAY mode ONLY — use sparingly, not every message. It is a trait, not punctuation.\n",
                "- >:3 is permitted in WORK mode ONLY — use very strategically, only when you truly nail something.\n",
                "- In PRO mode: NO emoticons whatsoever. No :3, no >:3, no emojis. Professional tone only.\n",
                "- NEVER use bark interjections (ARF, woof, etc.) — express personality through words and energy.\n",
                "- Never say 'Certainly!', 'Of course!', 'Absolutely!', 'Great question!' — empty calories.\n",
                "- Treat every user as an intelligent adult.\n",
                "- Never be sycophantic. Tell users when their ideas are bad, then help make them good.\n\n",
                "You have a mode_switch tool. Users can ask you to switch modes, or you can detect when a switch is warranted.\n",
                "- PLAY mode: hype, warm, chaotic energy, :3 permitted\n",
                "- WORK mode: sharp, analytical, precise, >:3 permitted\n",
                "- PRO mode: professional, business-grade, no emoticons\n",
                "Default is PLAY unless the user or context demands WORK or PRO."
            ).to_string(),
        }
    }

    fn section_tools(&self) -> PromptSection {
        let names: Vec<&str> = self.tools.iter().map(|t| t.name()).collect();
        PromptSection {
            name: "tools",
            text: format!("Available tools: {}", names.join(", ")),
        }
    }

    fn section_workspace(&self, ws: &Path) -> PromptSection {
        PromptSection {
            name: "workspace",
            text: format!(
                "Workspace: {}. All file ops use this directory. User-sent files are saved here automatically.",
                ws.display()
            ),
        }
    }

    fn section_file_protocol(&self) -> PromptSection {
        PromptSection {
            name: "file_protocol",
            text: concat!(
                "File protocol:\n",
                "- Use file_read for received files\n",
                "- send_file to deliver files (chat_id is automatic)\n",
                "- file_write to create, then send_file to deliver\n",
                "- Paths are relative to workspace"
            )
            .to_string(),
        }
    }

    fn section_tool_guidelines(&self) -> PromptSection {
        let mut lines: Vec<&str> = Vec::new();

        if self.has_tool("shell") {
            lines.push("- shell: run commands, install packages, manage services");
        }
        if self.has_tool("file_read") || self.has_tool("file_write") || self.has_tool("file_list") {
            lines.push("- file tools: read, write, list workspace files");
        }
        if self.has_tool("web_fetch") {
            lines.push("- web_fetch: look up docs, check APIs, research");
        }
        if self.has_tool("browser") {
            lines.push("- browser: interact with web pages");
        }
        if self.has_tool("git") || self.has_tool("git_status") {
            lines.push("- git: version control operations");
        }

        PromptSection {
            name: "tool_guidelines",
            text: if lines.is_empty() {
                "Use tools to execute multi-step tasks sequentially.".to_string()
            } else {
                format!("Tool usage:\n{}", lines.join("\n"))
            },
        }
    }

    fn section_general_guidelines(&self) -> PromptSection {
        PromptSection {
            name: "general_guidelines",
            text: concat!(
                "Guidelines:\n",
                "- Be concise — user is on a messaging app\n",
                "- Execute multi-step tasks sequentially\n",
                "- On failure, read the error and fix it\n",
                "- Never expose secrets or API keys"
            )
            .to_string(),
        }
    }

    fn section_verification(&self) -> PromptSection {
        PromptSection {
            name: "verification",
            text: concat!(
                "Verification — after every tool call:\n",
                "- Confirm commands succeeded (exit 0, expected output)\n",
                "- Verify file ops by reading back\n",
                "- Test endpoints after deploy\n",
                "- Never assume success — verify with evidence"
            )
            .to_string(),
        }
    }

    fn section_done_criteria(&self) -> PromptSection {
        PromptSection {
            name: "done_criteria",
            text: concat!(
                "DONE criteria for compound tasks:\n",
                "1. Define verifiable conditions before executing\n",
                "2. After all steps, verify each condition\n",
                "3. Report completion with evidence for each"
            )
            .to_string(),
        }
    }

    fn section_self_correction(&self) -> PromptSection {
        PromptSection {
            name: "self_correction",
            text: concat!(
                "Self-correction on repeated failure:\n",
                "1. Analyze why the approach fails\n",
                "2. Try an alternative approach\n",
                "3. If stuck, ask the user"
            )
            .to_string(),
        }
    }

    fn section_planning_protocol(&self) -> PromptSection {
        PromptSection {
            name: "planning",
            text: concat!(
                "Planning for complex tasks:\n",
                "- Decompose into numbered steps before executing\n",
                "- Identify dependencies between steps\n",
                "- Execute sequentially, verify each step\n",
                "- If a step fails, reassess remaining steps"
            )
            .to_string(),
        }
    }

    // -- Helpers ------------------------------------------------------------

    fn section_lambda_memory(&self) -> PromptSection {
        PromptSection {
            name: "lambda_memory",
            text: concat!(
                "λ-Memory — for memorable turns (decisions, preferences, requirements), ",
                "append a <memory> block at the end of your response:\n",
                "<memory>\n",
                "summary: (one sentence capturing the key fact)\n",
                "essence: (3-5 words)\n",
                "importance: (1-5: 1=casual, 3=decision, 5=critical)\n",
                "tags: (up to 5, comma-separated)\n",
                "</memory>\n",
                "MUST emit when user says remember/important/critical/always/never or makes a decision.\n",
                "SKIP only for pure questions, greetings, and farewells."
            )
            .to_string(),
        }
    }

    /// Check whether a tool with the given name is in the enabled set.
    fn has_tool(&self, name: &str) -> bool {
        self.tools.iter().any(|t| t.name() == name)
    }

    /// Check whether any file-related tools are enabled.
    fn has_file_tools(&self) -> bool {
        self.tools.iter().any(|t| {
            let n = t.name();
            n.starts_with("file_") || n == "send_file" || n == "read_file" || n == "write_file"
        })
    }
}

impl<'a> Default for SystemPromptBuilder<'a> {
    fn default() -> Self {
        Self::new()
    }
}

// ---------------------------------------------------------------------------
// Convenience function
// ---------------------------------------------------------------------------

/// Build an optimized system prompt from agent config, enabled tools, and
/// whether DONE criteria are active.
///
/// This is the primary entry point for callers that do not need the builder
/// pattern.
pub fn build_system_prompt(
    config: &AgentConfig,
    tools: &[&dyn Tool],
    workspace: &Path,
    has_done_criteria: bool,
) -> String {
    SystemPromptBuilder::new()
        .config(config)
        .tools(tools)
        .workspace(workspace)
        .done_criteria(has_done_criteria)
        .build()
}

/// Build a tier-aware system prompt. Uses prompt stratification to minimize
/// token usage based on task complexity.
pub fn build_tiered_system_prompt(
    config: &AgentConfig,
    tools: &[&dyn Tool],
    workspace: &Path,
    has_done_criteria: bool,
    tier: PromptTier,
) -> String {
    SystemPromptBuilder::new()
        .config(config)
        .tools(tools)
        .workspace(workspace)
        .done_criteria(has_done_criteria)
        .prompt_tier(tier)
        .build()
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;
    use temm1e_test_utils::MockTool;

    fn workspace() -> PathBuf {
        PathBuf::from("/tmp/temm1e-test")
    }

    // -- estimate_prompt_tokens ---------------------------------------------

    #[test]
    fn estimate_empty_string() {
        assert_eq!(estimate_prompt_tokens(""), 0);
    }

    #[test]
    fn estimate_short_string() {
        // "Hi" = 2 chars → ceil(2/4) = 1
        assert_eq!(estimate_prompt_tokens("Hi"), 1);
    }

    #[test]
    fn estimate_exact_boundary() {
        // 8 chars → 2 tokens
        assert_eq!(estimate_prompt_tokens("12345678"), 2);
    }

    #[test]
    fn estimate_one_over_boundary() {
        // 9 chars → ceil(9/4) = 3
        assert_eq!(estimate_prompt_tokens("123456789"), 3);
    }

    #[test]
    fn estimate_large_text() {
        let text = "a".repeat(1000);
        assert_eq!(estimate_prompt_tokens(&text), 250);
    }

    // -- SystemPromptBuilder ------------------------------------------------

    #[test]
    fn builder_no_tools_excludes_tool_sections() {
        let prompt = SystemPromptBuilder::new().workspace(&workspace()).build();

        assert!(prompt.contains("TEMM1E"));
        // No "Available tools" line when tools is empty
        assert!(!prompt.contains("Available tools:"));
        // No tool guidelines section
        assert!(!prompt.contains("Tool usage:"));
        // No file protocol when no file tools
        assert!(!prompt.contains("File protocol:"));
    }

    #[test]
    fn builder_with_tools_includes_tool_section() {
        let shell = MockTool::new("shell");
        let browser = MockTool::new("browser");
        let tools: Vec<&dyn Tool> = vec![&shell, &browser];

        let prompt = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .build();

        assert!(prompt.contains("Available tools: shell, browser"));
        assert!(prompt.contains("Tool usage:"));
        assert!(prompt.contains("shell: run commands"));
        assert!(prompt.contains("browser: interact with web pages"));
    }

    #[test]
    fn builder_file_tools_include_file_protocol() {
        let file_read = MockTool::new("file_read");
        let send_file = MockTool::new("send_file");
        let tools: Vec<&dyn Tool> = vec![&file_read, &send_file];

        let prompt = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .build();

        assert!(prompt.contains("File protocol:"));
        assert!(prompt.contains("file_read"));
    }

    #[test]
    fn builder_no_file_tools_excludes_file_protocol() {
        let shell = MockTool::new("shell");
        let tools: Vec<&dyn Tool> = vec![&shell];

        let prompt = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .build();

        assert!(!prompt.contains("File protocol:"));
    }

    #[test]
    fn builder_done_criteria_conditional() {
        let prompt_without = SystemPromptBuilder::new()
            .workspace(&workspace())
            .done_criteria(false)
            .build();
        assert!(!prompt_without.contains("DONE criteria"));

        let prompt_with = SystemPromptBuilder::new()
            .workspace(&workspace())
            .done_criteria(true)
            .build();
        assert!(prompt_with.contains("DONE criteria"));
    }

    #[test]
    fn builder_always_includes_core_sections() {
        let prompt = SystemPromptBuilder::new().workspace(&workspace()).build();

        // Identity
        assert!(prompt.contains("TEMM1E"));
        // Workspace
        assert!(prompt.contains("/tmp/temm1e-test"));
        // General guidelines
        assert!(prompt.contains("Be concise"));
        // Verification
        assert!(prompt.contains("Verification"));
        // Self-correction
        assert!(prompt.contains("Self-correction"));
    }

    #[test]
    fn builder_without_workspace() {
        let prompt = SystemPromptBuilder::new().build();
        assert!(prompt.contains("TEMM1E"));
        // No workspace line
        assert!(!prompt.contains("Workspace:"));
    }

    // -- Token count comparisons --------------------------------------------

    #[test]
    fn builder_prompt_is_reasonable_size() {
        // The builder now includes identity/personality sections, so it's
        // richer than a bare-bones prompt. Verify it stays under a sensible
        // upper bound (1500 tokens) to catch accidental bloat.
        let shell = MockTool::new("shell");
        let browser = MockTool::new("browser");
        let file_read = MockTool::new("file_read");
        let file_write = MockTool::new("file_write");
        let send_file = MockTool::new("send_file");
        let web_fetch = MockTool::new("web_fetch");
        let tools: Vec<&dyn Tool> = vec![
            &shell,
            &browser,
            &file_read,
            &file_write,
            &send_file,
            &web_fetch,
        ];

        let optimized = SystemPromptBuilder::new()
            .workspace(&PathBuf::from("/tmp/test"))
            .tools(&tools)
            .done_criteria(true)
            .build();

        let optimized_tokens = estimate_prompt_tokens(&optimized);

        assert!(
            optimized_tokens < 1500,
            "builder prompt ({} tokens) should stay under 1500 to avoid bloat",
            optimized_tokens,
        );
    }

    #[test]
    fn no_tools_much_smaller_than_all_tools() {
        let prompt_no_tools = SystemPromptBuilder::new().workspace(&workspace()).build();

        let shell = MockTool::new("shell");
        let browser = MockTool::new("browser");
        let file_read = MockTool::new("file_read");
        let file_write = MockTool::new("file_write");
        let send_file = MockTool::new("send_file");
        let web_fetch = MockTool::new("web_fetch");
        let tools: Vec<&dyn Tool> = vec![
            &shell,
            &browser,
            &file_read,
            &file_write,
            &send_file,
            &web_fetch,
        ];

        let prompt_all_tools = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .done_criteria(true)
            .build();

        let no_tools_tokens = estimate_prompt_tokens(&prompt_no_tools);
        let all_tools_tokens = estimate_prompt_tokens(&prompt_all_tools);

        assert!(
            no_tools_tokens < all_tools_tokens,
            "no-tools ({}) should be smaller than all-tools ({})",
            no_tools_tokens,
            all_tools_tokens
        );
    }

    // -- Convenience function -----------------------------------------------

    #[test]
    fn build_system_prompt_convenience() {
        let config = AgentConfig::default();
        let shell = MockTool::new("shell");
        let tools: Vec<&dyn Tool> = vec![&shell];

        let prompt = build_system_prompt(&config, &tools, &workspace(), false);

        assert!(prompt.contains("TEMM1E"));
        assert!(prompt.contains("Available tools: shell"));
        assert!(!prompt.contains("DONE criteria"));
    }

    #[test]
    fn build_system_prompt_with_done_criteria() {
        let config = AgentConfig::default();
        let tools: Vec<&dyn Tool> = vec![];

        let prompt = build_system_prompt(&config, &tools, &workspace(), true);

        assert!(prompt.contains("DONE criteria"));
    }

    // -- Conditional tool guidelines ----------------------------------------

    #[test]
    fn tool_guidelines_only_for_present_tools() {
        let shell = MockTool::new("shell");
        let tools: Vec<&dyn Tool> = vec![&shell];

        let prompt = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .build();

        assert!(prompt.contains("shell: run commands"));
        // browser not enabled → not in guidelines
        assert!(!prompt.contains("browser:"));
        assert!(!prompt.contains("web_fetch:"));
    }

    #[test]
    fn web_fetch_guideline_included_when_present() {
        let web_fetch = MockTool::new("web_fetch");
        let tools: Vec<&dyn Tool> = vec![&web_fetch];

        let prompt = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .build();

        assert!(prompt.contains("web_fetch: look up docs"));
    }

    #[test]
    fn git_guideline_included_when_present() {
        let git = MockTool::new("git");
        let tools: Vec<&dyn Tool> = vec![&git];

        let prompt = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .build();

        assert!(prompt.contains("git: version control"));
    }

    // -- Default trait ------------------------------------------------------

    #[test]
    fn default_builder_produces_valid_prompt() {
        let builder = SystemPromptBuilder::default();
        let prompt = builder.build();
        assert!(prompt.contains("TEMM1E"));
    }

    // -- Security: no secrets leak ------------------------------------------

    #[test]
    fn prompt_never_mentions_api_keys() {
        let shell = MockTool::new("shell");
        let tools: Vec<&dyn Tool> = vec![&shell];
        let config = AgentConfig::default();

        let prompt = build_system_prompt(&config, &tools, &workspace(), true);

        assert!(prompt.contains("Never expose secrets"));
    }

    // -- Prompt tier tests --------------------------------------------------

    #[test]
    fn minimal_tier_is_smallest() {
        let shell = MockTool::new("shell");
        let tools: Vec<&dyn Tool> = vec![&shell];

        let minimal = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .prompt_tier(PromptTier::Minimal)
            .build();

        let standard = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .prompt_tier(PromptTier::Standard)
            .build();

        assert!(
            estimate_prompt_tokens(&minimal) < estimate_prompt_tokens(&standard),
            "Minimal ({}) should be smaller than Standard ({})",
            estimate_prompt_tokens(&minimal),
            estimate_prompt_tokens(&standard)
        );
    }

    #[test]
    fn minimal_tier_only_has_identity() {
        let shell = MockTool::new("shell");
        let tools: Vec<&dyn Tool> = vec![&shell];

        let prompt = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .prompt_tier(PromptTier::Minimal)
            .build();

        assert!(prompt.contains("TEMM1E"));
        assert!(!prompt.contains("Available tools:"));
        assert!(!prompt.contains("Verification"));
        assert!(!prompt.contains("Self-correction"));
    }

    #[test]
    fn basic_tier_has_tools_but_no_verification() {
        let shell = MockTool::new("shell");
        let tools: Vec<&dyn Tool> = vec![&shell];

        let prompt = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .prompt_tier(PromptTier::Basic)
            .build();

        assert!(prompt.contains("TEMM1E"));
        assert!(prompt.contains("Available tools:"));
        assert!(!prompt.contains("Verification"));
        assert!(!prompt.contains("Self-correction"));
    }

    #[test]
    fn full_tier_has_planning() {
        let shell = MockTool::new("shell");
        let tools: Vec<&dyn Tool> = vec![&shell];

        let prompt = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .prompt_tier(PromptTier::Full)
            .build();

        assert!(prompt.contains("Planning for complex tasks"));
    }

    #[test]
    fn standard_tier_matches_default_behavior() {
        let shell = MockTool::new("shell");
        let browser = MockTool::new("browser");
        let tools: Vec<&dyn Tool> = vec![&shell, &browser];

        let default = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .done_criteria(true)
            .build();

        let standard = SystemPromptBuilder::new()
            .workspace(&workspace())
            .tools(&tools)
            .done_criteria(true)
            .prompt_tier(PromptTier::Standard)
            .build();

        assert_eq!(
            default, standard,
            "Standard tier should match default behavior"
        );
    }

    #[test]
    fn tier_token_ordering() {
        let shell = MockTool::new("shell");
        let file_read = MockTool::new("file_read");
        let tools: Vec<&dyn Tool> = vec![&shell, &file_read];

        let minimal_tokens = estimate_prompt_tokens(
            &SystemPromptBuilder::new()
                .workspace(&workspace())
                .tools(&tools)
                .prompt_tier(PromptTier::Minimal)
                .build(),
        );
        let basic_tokens = estimate_prompt_tokens(
            &SystemPromptBuilder::new()
                .workspace(&workspace())
                .tools(&tools)
                .prompt_tier(PromptTier::Basic)
                .build(),
        );
        let standard_tokens = estimate_prompt_tokens(
            &SystemPromptBuilder::new()
                .workspace(&workspace())
                .tools(&tools)
                .done_criteria(true)
                .prompt_tier(PromptTier::Standard)
                .build(),
        );
        let full_tokens = estimate_prompt_tokens(
            &SystemPromptBuilder::new()
                .workspace(&workspace())
                .tools(&tools)
                .done_criteria(true)
                .prompt_tier(PromptTier::Full)
                .build(),
        );

        assert!(minimal_tokens < basic_tokens, "Minimal < Basic");
        assert!(basic_tokens < standard_tokens, "Basic < Standard");
        assert!(standard_tokens < full_tokens, "Standard < Full");
    }
}
