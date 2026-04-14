//! Project language / framework auto-detection from filesystem markers.
//!
//! Used at Oath-creation time to determine which predicate sets are active
//! for the current project. Users can override via `[witness] language = "..."`
//! in config.

use std::path::Path;

/// Detect which predicate sets are active for a workspace based on file
/// markers. Multiple sets can be active (e.g., TypeScript + Python for
/// a full-stack project). `docs` and `shell` are always included as fallbacks.
pub fn detect_active_sets(workspace_root: &Path) -> Vec<String> {
    let mut sets = Vec::new();
    let check = |file: &str| workspace_root.join(file).exists();
    let check_glob = |pattern: &str| -> bool {
        let full = workspace_root.join(pattern);
        match glob::glob(full.to_string_lossy().as_ref()) {
            Ok(mut it) => it.next().is_some(),
            Err(_) => false,
        }
    };

    if check("Cargo.toml") {
        sets.push("rust".into());
    }
    if check("package.json") {
        sets.push("javascript".into());
        if check("tsconfig.json") {
            sets.push("typescript".into());
        }
    }
    if check("pyproject.toml") || check("setup.py") || check("requirements.txt") {
        sets.push("python".into());
    }
    if check("go.mod") {
        sets.push("go".into());
    }
    if check("composer.json") {
        sets.push("php".into());
    }
    if check("Gemfile") {
        sets.push("ruby".into());
    }
    if check("pom.xml") || check("build.gradle") || check("build.gradle.kts") {
        sets.push("java".into());
    }
    if check_glob("*.csproj") || check_glob("*.sln") {
        sets.push("csharp".into());
    }
    if check("mix.exs") {
        sets.push("elixir".into());
    }
    if check("Dockerfile") || check("docker-compose.yml") || check("docker-compose.yaml") {
        sets.push("config".into());
    }
    if check("main.tf") || check_glob("*.tf") {
        sets.push("terraform".into());
    }

    // Always include docs and shell as fallbacks. These are cheap no-ops
    // if no relevant files exist.
    sets.push("docs".into());
    sets.push("shell".into());

    sets
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn empty_workspace_has_docs_and_shell() {
        let dir = tempdir().unwrap();
        let sets = detect_active_sets(dir.path());
        assert!(sets.contains(&"docs".to_string()));
        assert!(sets.contains(&"shell".to_string()));
    }

    #[test]
    fn cargo_toml_detects_rust() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("Cargo.toml"), "[package]\nname = \"x\"").unwrap();
        let sets = detect_active_sets(dir.path());
        assert!(sets.contains(&"rust".to_string()));
    }

    #[test]
    fn package_json_detects_javascript() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        let sets = detect_active_sets(dir.path());
        assert!(sets.contains(&"javascript".to_string()));
        assert!(!sets.contains(&"typescript".to_string()));
    }

    #[test]
    fn tsconfig_adds_typescript() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        let sets = detect_active_sets(dir.path());
        assert!(sets.contains(&"javascript".to_string()));
        assert!(sets.contains(&"typescript".to_string()));
    }

    #[test]
    fn pyproject_detects_python() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]\n").unwrap();
        let sets = detect_active_sets(dir.path());
        assert!(sets.contains(&"python".to_string()));
    }

    #[test]
    fn cross_stack_detection() {
        let dir = tempdir().unwrap();
        std::fs::write(dir.path().join("package.json"), "{}").unwrap();
        std::fs::write(dir.path().join("tsconfig.json"), "{}").unwrap();
        std::fs::write(dir.path().join("pyproject.toml"), "[project]\n").unwrap();
        let sets = detect_active_sets(dir.path());
        assert!(sets.contains(&"javascript".to_string()));
        assert!(sets.contains(&"typescript".to_string()));
        assert!(sets.contains(&"python".to_string()));
    }
}
