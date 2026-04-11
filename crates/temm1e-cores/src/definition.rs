//! Core definition parsing — loads `.md` files with YAML frontmatter.

use serde::Deserialize;
use std::path::PathBuf;
use temm1e_core::types::error::Temm1eError;

/// YAML frontmatter schema for a core definition file.
#[derive(Debug, Clone, Deserialize)]
pub struct CoreFrontmatter {
    pub name: String,
    pub description: String,
    pub version: String,
    /// Optional temperature override. Defaults to 0.0 (deterministic).
    /// Creative cores use higher values (e.g., 0.7) for sampling variance.
    #[serde(default)]
    pub temperature: Option<f32>,
}

/// A parsed core definition — frontmatter metadata + system prompt body.
#[derive(Debug, Clone)]
pub struct CoreDefinition {
    /// Core name (unique identifier).
    pub name: String,
    /// Human-readable description.
    pub description: String,
    /// Semantic version string.
    pub version: String,
    /// LLM temperature override (None = 0.0 deterministic).
    pub temperature: Option<f32>,
    /// The system prompt body (everything after the YAML frontmatter).
    pub system_prompt: String,
    /// Filesystem path this definition was loaded from.
    pub source_path: PathBuf,
}

/// Parse a core definition from raw markdown content.
///
/// Format:
/// ```markdown
/// ---
/// name: architecture
/// description: "Analyzes repository structure"
/// version: "1.0.0"
/// ---
///
/// You are the Architecture Core...
/// ```
pub fn parse_core_content(
    content: &str,
    source_path: PathBuf,
) -> Result<CoreDefinition, Temm1eError> {
    // Find YAML frontmatter between --- delimiters
    let content = content.trim();
    if !content.starts_with("---") {
        return Err(Temm1eError::Config(
            "Core definition must start with '---' YAML frontmatter".to_string(),
        ));
    }

    let after_first = &content[3..];
    let end_idx = after_first.find("\n---").ok_or_else(|| {
        Temm1eError::Config("Core definition missing closing '---' for frontmatter".to_string())
    })?;

    let yaml_str = &after_first[..end_idx];
    let body_start = end_idx + 4; // skip "\n---"
    let body = after_first[body_start..].trim().to_string();

    let frontmatter: CoreFrontmatter = serde_yaml::from_str(yaml_str)
        .map_err(|e| Temm1eError::Config(format!("Failed to parse core frontmatter: {e}")))?;

    if frontmatter.name.is_empty() {
        return Err(Temm1eError::Config(
            "Core definition 'name' cannot be empty".to_string(),
        ));
    }

    // Warn if system prompt is over 800 tokens (~3200 chars)
    let non_ascii = body.as_bytes().iter().filter(|&&b| b > 127).count();
    let estimated_tokens = if non_ascii as f64 / body.len().max(1) as f64 > 0.3 {
        body.len() / 2
    } else {
        body.len() / 4
    };
    if estimated_tokens > 800 {
        tracing::warn!(
            core = %frontmatter.name,
            estimated_tokens,
            "Core system prompt exceeds recommended 800-token budget (W9)"
        );
    }

    Ok(CoreDefinition {
        name: frontmatter.name,
        description: frontmatter.description,
        version: frontmatter.version,
        temperature: frontmatter.temperature,
        system_prompt: body,
        source_path,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid_core_definition() {
        let content = r#"---
name: test-core
description: "A test core"
version: "1.0.0"
---

You are a test core.

## Protocol
1. Do the thing
2. Return the result"#;

        let def = parse_core_content(content, PathBuf::from("/tmp/test.md")).unwrap();
        assert_eq!(def.name, "test-core");
        assert_eq!(def.description, "A test core");
        assert_eq!(def.version, "1.0.0");
        assert!(def.temperature.is_none());
        assert!(def.system_prompt.contains("You are a test core"));
        assert!(def.system_prompt.contains("## Protocol"));
    }

    #[test]
    fn parse_core_with_temperature() {
        let content = r#"---
name: creative
description: "Creative core"
version: "1.0.0"
temperature: 0.7
---

Be creative."#;

        let def = parse_core_content(content, PathBuf::from("/tmp/creative.md")).unwrap();
        assert_eq!(def.temperature, Some(0.7));
    }

    #[test]
    fn parse_missing_frontmatter_start() {
        let content = "No frontmatter here";
        let result = parse_core_content(content, PathBuf::from("/tmp/bad.md"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_missing_frontmatter_end() {
        let content = "---\nname: broken\n";
        let result = parse_core_content(content, PathBuf::from("/tmp/bad.md"));
        assert!(result.is_err());
    }

    #[test]
    fn parse_empty_name_rejected() {
        let content = r#"---
name: ""
description: "Empty name"
version: "1.0.0"
---

Body"#;
        let result = parse_core_content(content, PathBuf::from("/tmp/bad.md"));
        assert!(result.is_err());
    }
}
