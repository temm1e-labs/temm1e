//! TEMM1E Skills crate
//!
//! Provides skill discovery, parsing, and registry for the TemHub v1 skill system.
//! Skills are Markdown files with YAML frontmatter that contain instructions for
//! the agent runtime.

use std::path::{Path, PathBuf};

use serde::Deserialize;
use temm1e_core::error::Temm1eError;
use tokio::fs;

/// A parsed skill loaded from a `.md` file.
#[derive(Debug, Clone)]
pub struct Skill {
    /// Unique skill name (from frontmatter).
    pub name: String,
    /// Human-readable description (from frontmatter).
    pub description: String,
    /// List of capability keywords used for relevance matching.
    pub capabilities: Vec<String>,
    /// Semantic version string.
    pub version: String,
    /// The instruction body (everything after the YAML frontmatter).
    pub instructions: String,
    /// Filesystem path the skill was loaded from.
    pub source_path: PathBuf,
}

/// YAML frontmatter schema for deserialization.
#[derive(Debug, Deserialize)]
struct SkillFrontmatter {
    name: String,
    description: String,
    capabilities: Vec<String>,
    version: String,
}

/// Registry that discovers, parses, and indexes skills from the filesystem.
pub struct SkillRegistry {
    /// Workspace root used to locate the `skills/` subdirectory.
    workspace_path: PathBuf,
    /// All successfully loaded skills.
    skills: Vec<Skill>,
}

impl SkillRegistry {
    /// Create a new registry bound to the given workspace path.
    ///
    /// Skills are not loaded until [`load_skills`] is called.
    pub fn new(workspace_path: PathBuf) -> Self {
        Self {
            workspace_path,
            skills: Vec::new(),
        }
    }

    /// Scan the global (`~/.temm1e/skills/`) and workspace (`<workspace>/skills/`)
    /// directories for `.md` skill files, parse them, and populate the registry.
    ///
    /// Previously loaded skills are cleared before rescanning.
    pub async fn load_skills(&mut self) -> Result<(), Temm1eError> {
        self.skills.clear();

        let mut dirs_to_scan: Vec<PathBuf> = Vec::new();

        // Global skills directory
        if let Some(home) = dirs::home_dir() {
            let global_dir = home.join(".temm1e").join("skills");
            dirs_to_scan.push(global_dir);
        }

        // Workspace skills directory
        let workspace_dir = self.workspace_path.join("skills");
        dirs_to_scan.push(workspace_dir);

        for dir in &dirs_to_scan {
            if dir.is_dir() {
                let found = scan_directory(dir).await?;
                self.skills.extend(found);
            } else {
                tracing::debug!(path = %dir.display(), "Skills directory does not exist, skipping");
            }
        }

        tracing::info!(count = self.skills.len(), "Loaded skills");
        Ok(())
    }

    /// Look up a skill by exact name.
    pub fn get_skill(&self, name: &str) -> Option<&Skill> {
        self.skills.iter().find(|s| s.name == name)
    }

    /// Return a slice of all loaded skills.
    pub fn list_skills(&self) -> &[Skill] {
        &self.skills
    }

    /// Find skills whose capabilities match keywords in the task description.
    ///
    /// Matching is case-insensitive: each capability is checked for substring
    /// presence in the lowercased task description.
    pub fn find_relevant_skills(&self, task_description: &str) -> Vec<&Skill> {
        let task_lower = task_description.to_lowercase();
        self.skills
            .iter()
            .filter(|skill| {
                skill
                    .capabilities
                    .iter()
                    .any(|cap| task_lower.contains(&cap.to_lowercase()))
            })
            .collect()
    }

    /// Format a set of skills into a text block suitable for injection into a
    /// system prompt.
    pub fn format_skill_context(&self, skills: &[&Skill]) -> String {
        if skills.is_empty() {
            return String::new();
        }

        let mut output = String::from("# Available Skills\n\n");
        for skill in skills {
            output.push_str(&format!("## {} (v{})\n", skill.name, skill.version));
            output.push_str(&format!("{}\n\n", skill.description));
            output.push_str(&skill.instructions);
            output.push_str("\n\n");
        }
        output.trim_end().to_string()
    }
}

/// Scan a single directory for `.md` files and parse each one.
async fn scan_directory(dir: &Path) -> Result<Vec<Skill>, Temm1eError> {
    let mut loaded = Vec::new();
    let mut entries = fs::read_dir(dir).await.map_err(|e| {
        Temm1eError::Skill(format!("Failed to read directory {}: {}", dir.display(), e))
    })?;

    while let Some(entry) = entries.next_entry().await.map_err(|e| {
        Temm1eError::Skill(format!("Failed to read entry in {}: {}", dir.display(), e))
    })? {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) == Some("md") {
            match parse_skill_file(&path).await {
                Ok(skill) => {
                    tracing::debug!(name = %skill.name, path = %path.display(), "Loaded skill");
                    loaded.push(skill);
                }
                Err(e) => {
                    tracing::warn!(path = %path.display(), error = %e, "Failed to parse skill file, skipping");
                }
            }
        }
    }

    Ok(loaded)
}

/// Parse a single `.md` skill file into a [`Skill`].
///
/// The file must start with YAML frontmatter delimited by `---` lines.
/// Everything after the closing `---` is treated as the instruction body.
async fn parse_skill_file(path: &Path) -> Result<Skill, Temm1eError> {
    let content = fs::read_to_string(path)
        .await
        .map_err(|e| Temm1eError::Skill(format!("Failed to read {}: {}", path.display(), e)))?;

    parse_skill_content(&content, path)
}

/// Parse skill content (frontmatter + body) from a string.
fn parse_skill_content(content: &str, source_path: &Path) -> Result<Skill, Temm1eError> {
    let trimmed = content.trim_start();

    if !trimmed.starts_with("---") {
        return Err(Temm1eError::Skill(format!(
            "Skill file {} does not start with YAML frontmatter delimiter '---'",
            source_path.display()
        )));
    }

    // Skip the opening "---" and any immediately following newline characters.
    let after_opening = &trimmed[3..];
    let after_opening = after_opening.trim_start_matches(['\r', '\n']);

    // Locate the closing "---" delimiter (must be on its own line).
    let closing_pos = after_opening.find("\n---").ok_or_else(|| {
        Temm1eError::Skill(format!(
            "Skill file {} has no closing YAML frontmatter delimiter '---'",
            source_path.display()
        ))
    })?;

    let yaml_str = &after_opening[..closing_pos];

    // Body starts after the closing "\n---" (4 bytes).
    let body_start = closing_pos + 4;
    let instructions = if body_start < after_opening.len() {
        after_opening[body_start..].trim().to_string()
    } else {
        String::new()
    };

    let frontmatter: SkillFrontmatter = serde_yaml::from_str(yaml_str).map_err(|e| {
        Temm1eError::Skill(format!(
            "Failed to parse YAML frontmatter in {}: {}",
            source_path.display(),
            e
        ))
    })?;

    Ok(Skill {
        name: frontmatter.name,
        description: frontmatter.description,
        capabilities: frontmatter.capabilities,
        version: frontmatter.version,
        instructions,
        source_path: source_path.to_path_buf(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs as std_fs;
    use tempfile::tempdir;

    /// Helper: create a valid skill file in the given directory.
    fn write_skill_file(
        dir: &Path,
        filename: &str,
        name: &str,
        desc: &str,
        caps: &[&str],
        version: &str,
        body: &str,
    ) {
        let caps_yaml: String = caps.iter().map(|c| format!("  - {c}\n")).collect();
        let content = format!(
            "---\nname: {name}\ndescription: {desc}\ncapabilities:\n{caps_yaml}version: {version}\n---\n{body}"
        );
        std_fs::write(dir.join(filename), content).unwrap();
    }

    // ---------------------------------------------------------------
    // load_skills tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_load_skills_from_workspace_dir() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        write_skill_file(
            &skills_dir,
            "deploy.md",
            "deploy",
            "Deploy to cloud",
            &["deploy", "cloud"],
            "1.0.0",
            "Run deploy steps.",
        );

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        assert_eq!(registry.list_skills().len(), 1);
        assert_eq!(registry.list_skills()[0].name, "deploy");
    }

    #[tokio::test]
    async fn test_load_skills_empty_directory() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        assert!(registry.list_skills().is_empty());
    }

    #[tokio::test]
    async fn test_load_skills_no_directory() {
        let tmp = tempdir().unwrap();
        // No skills/ subdirectory created at all.

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        assert!(registry.list_skills().is_empty());
    }

    #[tokio::test]
    async fn test_load_skills_ignores_non_md_files() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        std_fs::write(skills_dir.join("notes.txt"), "not a skill").unwrap();
        std_fs::write(skills_dir.join("data.json"), "{}").unwrap();

        write_skill_file(
            &skills_dir,
            "real.md",
            "real",
            "A real skill",
            &["test"],
            "1.0.0",
            "Instructions.",
        );

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        assert_eq!(registry.list_skills().len(), 1);
        assert_eq!(registry.list_skills()[0].name, "real");
    }

    #[tokio::test]
    async fn test_load_skills_skips_malformed_frontmatter() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        // Malformed: missing required fields
        std_fs::write(
            skills_dir.join("bad.md"),
            "---\ntitle: oops\n---\nBody text.",
        )
        .unwrap();

        // Good one
        write_skill_file(
            &skills_dir,
            "good.md",
            "good",
            "Good skill",
            &["test"],
            "1.0.0",
            "Do things.",
        );

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        assert_eq!(registry.list_skills().len(), 1);
        assert_eq!(registry.list_skills()[0].name, "good");
    }

    #[tokio::test]
    async fn test_load_skills_skips_no_frontmatter() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        std_fs::write(
            skills_dir.join("plain.md"),
            "# Just a markdown file\nNo frontmatter here.",
        )
        .unwrap();

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        assert!(registry.list_skills().is_empty());
    }

    #[tokio::test]
    async fn test_load_skills_skips_unclosed_frontmatter() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        std_fs::write(
            skills_dir.join("unclosed.md"),
            "---\nname: broken\ndescription: no closing\n",
        )
        .unwrap();

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        assert!(registry.list_skills().is_empty());
    }

    #[tokio::test]
    async fn test_load_skills_clears_previous() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        write_skill_file(
            &skills_dir,
            "first.md",
            "first",
            "First",
            &["a"],
            "1.0.0",
            "Body.",
        );

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();
        assert_eq!(registry.list_skills().len(), 1);

        // Remove the file and add a different one.
        std_fs::remove_file(skills_dir.join("first.md")).unwrap();
        write_skill_file(
            &skills_dir,
            "second.md",
            "second",
            "Second",
            &["b"],
            "1.0.0",
            "Body.",
        );

        registry.load_skills().await.unwrap();
        assert_eq!(registry.list_skills().len(), 1);
        assert_eq!(registry.list_skills()[0].name, "second");
    }

    #[tokio::test]
    async fn test_multiple_skills_loaded_from_single_dir() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        for i in 0..5 {
            write_skill_file(
                &skills_dir,
                &format!("skill{i}.md"),
                &format!("skill-{i}"),
                &format!("Skill number {i}"),
                &[&format!("cap{i}")],
                "1.0.0",
                &format!("Instructions for skill {i}."),
            );
        }

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        assert_eq!(registry.list_skills().len(), 5);
    }

    // ---------------------------------------------------------------
    // parse_skill_content tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_parse_skill_content_full() {
        let content = "\
---
name: test-skill
description: A test skill
capabilities:
  - testing
  - debugging
version: 2.1.0
---
Step 1: do something.
Step 2: do another thing.";

        let skill = parse_skill_content(content, Path::new("test.md")).unwrap();

        assert_eq!(skill.name, "test-skill");
        assert_eq!(skill.description, "A test skill");
        assert_eq!(skill.capabilities, vec!["testing", "debugging"]);
        assert_eq!(skill.version, "2.1.0");
        assert_eq!(
            skill.instructions,
            "Step 1: do something.\nStep 2: do another thing."
        );
        assert_eq!(skill.source_path, Path::new("test.md"));
    }

    #[tokio::test]
    async fn test_parse_skill_content_empty_body() {
        let content = "\
---
name: empty
description: No body
capabilities:
  - misc
version: 0.1.0
---
";
        let skill = parse_skill_content(content, Path::new("empty.md")).unwrap();

        assert_eq!(skill.name, "empty");
        assert!(skill.instructions.is_empty());
    }

    #[tokio::test]
    async fn test_parse_skill_content_missing_name() {
        let content = "\
---
description: Missing name
capabilities:
  - test
version: 1.0.0
---
Body.";
        let result = parse_skill_content(content, Path::new("bad.md"));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_parse_skill_content_no_opening_delimiter() {
        let content = "name: nope\n---\nBody.";
        let result = parse_skill_content(content, Path::new("bad.md"));
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_parse_skill_content_whitespace_before_frontmatter() {
        let content = "\n\n---\nname: spaced\ndescription: Has whitespace\ncapabilities:\n  - test\nversion: 1.0.0\n---\n\nBody with leading newline.";
        let skill = parse_skill_content(content, Path::new("spaced.md")).unwrap();

        assert_eq!(skill.name, "spaced");
        assert_eq!(skill.instructions, "Body with leading newline.");
    }

    // ---------------------------------------------------------------
    // get_skill tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_get_skill_found() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        write_skill_file(
            &skills_dir,
            "alpha.md",
            "alpha",
            "Alpha skill",
            &["a"],
            "1.0.0",
            "Alpha instructions.",
        );
        write_skill_file(
            &skills_dir,
            "beta.md",
            "beta",
            "Beta skill",
            &["b"],
            "2.0.0",
            "Beta instructions.",
        );

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        let skill = registry.get_skill("alpha");
        assert!(skill.is_some());
        assert_eq!(skill.unwrap().description, "Alpha skill");

        let skill = registry.get_skill("beta");
        assert!(skill.is_some());
        assert_eq!(skill.unwrap().version, "2.0.0");
    }

    #[tokio::test]
    async fn test_get_skill_not_found() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        assert!(registry.get_skill("nonexistent").is_none());
    }

    // ---------------------------------------------------------------
    // find_relevant_skills tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_find_relevant_skills_matches() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        write_skill_file(
            &skills_dir,
            "deploy.md",
            "deploy",
            "Deploy to cloud",
            &["deploy", "cloud", "infrastructure"],
            "1.0.0",
            "Deploy steps.",
        );
        write_skill_file(
            &skills_dir,
            "test.md",
            "test",
            "Run tests",
            &["test", "ci", "quality"],
            "1.0.0",
            "Test steps.",
        );
        write_skill_file(
            &skills_dir,
            "docs.md",
            "docs",
            "Generate docs",
            &["documentation", "api"],
            "1.0.0",
            "Doc steps.",
        );

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        let results = registry.find_relevant_skills("I need to deploy to the cloud");
        let names: Vec<&str> = results.iter().map(|s| s.name.as_str()).collect();
        assert!(names.contains(&"deploy"));
        // "test" and "docs" should not match
        assert!(!names.contains(&"docs"));
    }

    #[tokio::test]
    async fn test_find_relevant_skills_case_insensitive() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        write_skill_file(
            &skills_dir,
            "deploy.md",
            "deploy",
            "Deploy",
            &["Deploy", "CLOUD"],
            "1.0.0",
            "Steps.",
        );

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        let results = registry.find_relevant_skills("deploy to cloud");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "deploy");
    }

    #[tokio::test]
    async fn test_find_relevant_skills_no_match() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        write_skill_file(
            &skills_dir,
            "deploy.md",
            "deploy",
            "Deploy",
            &["deploy", "cloud"],
            "1.0.0",
            "Steps.",
        );

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        let results = registry.find_relevant_skills("write unit tests");
        assert!(results.is_empty());
    }

    #[tokio::test]
    async fn test_find_relevant_skills_partial_capability_match() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        write_skill_file(
            &skills_dir,
            "db.md",
            "database",
            "Database ops",
            &["database", "migration"],
            "1.0.0",
            "DB steps.",
        );

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        // "database" is a substring of the task description
        let results = registry.find_relevant_skills("run database migration scripts");
        assert_eq!(results.len(), 1);
        assert_eq!(results[0].name, "database");
    }

    // ---------------------------------------------------------------
    // format_skill_context tests
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_format_skill_context_empty() {
        let registry = SkillRegistry::new(PathBuf::from("/tmp"));
        let result = registry.format_skill_context(&[]);
        assert!(result.is_empty());
    }

    #[tokio::test]
    async fn test_format_skill_context_single() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        write_skill_file(
            &skills_dir,
            "deploy.md",
            "deploy",
            "Deploy to cloud",
            &["deploy"],
            "1.0.0",
            "Step 1: configure.\nStep 2: ship.",
        );

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        let skills: Vec<&Skill> = registry.list_skills().iter().collect();
        let context = registry.format_skill_context(&skills);

        assert!(context.contains("# Available Skills"));
        assert!(context.contains("## deploy (v1.0.0)"));
        assert!(context.contains("Deploy to cloud"));
        assert!(context.contains("Step 1: configure."));
        assert!(context.contains("Step 2: ship."));
    }

    #[tokio::test]
    async fn test_format_skill_context_multiple() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        write_skill_file(
            &skills_dir,
            "a.md",
            "alpha",
            "Alpha skill",
            &["a"],
            "1.0.0",
            "Alpha body.",
        );
        write_skill_file(
            &skills_dir,
            "b.md",
            "beta",
            "Beta skill",
            &["b"],
            "2.0.0",
            "Beta body.",
        );

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        let skills: Vec<&Skill> = registry.list_skills().iter().collect();
        let context = registry.format_skill_context(&skills);

        assert!(context.contains("## alpha (v1.0.0)"));
        assert!(context.contains("## beta (v2.0.0)"));
        assert!(context.contains("Alpha body."));
        assert!(context.contains("Beta body."));
    }

    // ---------------------------------------------------------------
    // source_path preserved correctly
    // ---------------------------------------------------------------

    #[tokio::test]
    async fn test_skill_source_path_preserved() {
        let tmp = tempdir().unwrap();
        let skills_dir = tmp.path().join("skills");
        std_fs::create_dir_all(&skills_dir).unwrap();

        write_skill_file(
            &skills_dir,
            "myskill.md",
            "myskill",
            "My skill",
            &["test"],
            "1.0.0",
            "Body.",
        );

        let mut registry = SkillRegistry::new(tmp.path().to_path_buf());
        registry.load_skills().await.unwrap();

        let skill = registry.get_skill("myskill").unwrap();
        assert_eq!(skill.source_path, skills_dir.join("myskill.md"));
    }
}
