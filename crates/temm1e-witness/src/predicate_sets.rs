//! Predicate set loader and template interpolation.
//!
//! Predicate sets are declared in `witness.toml` as named templates that
//! users and the Planner can reference when building an Oath. Each template
//! is a string of the form `PredicateKind(field1='${var}', field2=N)` that
//! interpolates variables and compiles to a concrete `Predicate`.
//!
//! Phase 1 ships with the default sets baked in (Rust, Python, JavaScript,
//! Go, shell, docs, config, data). Users override via a local `.witness.toml`.

use crate::error::WitnessError;
use std::collections::BTreeMap;

/// A named predicate set — e.g., "rust" or "python" — mapping template names
/// like `test_passes` or `no_stubs` to their string templates.
#[derive(Debug, Clone, Default)]
pub struct PredicateSet {
    pub name: String,
    pub templates: BTreeMap<String, String>,
}

impl PredicateSet {
    pub fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            templates: BTreeMap::new(),
        }
    }

    pub fn with_template(mut self, name: impl Into<String>, template: impl Into<String>) -> Self {
        self.templates.insert(name.into(), template.into());
        self
    }

    pub fn get(&self, template_name: &str) -> Option<&str> {
        self.templates.get(template_name).map(|s| s.as_str())
    }
}

/// Catalog of available predicate sets keyed by name ("rust", "python", ...).
#[derive(Debug, Clone, Default)]
pub struct PredicateSetCatalog {
    pub sets: BTreeMap<String, PredicateSet>,
}

impl PredicateSetCatalog {
    pub fn new() -> Self {
        Self::default()
    }

    /// Build the default catalog shipped with TEMM1E. Users override entries
    /// by loading their own catalog afterward and merging.
    pub fn defaults() -> Self {
        let mut c = Self::new();
        c.insert(default_rust());
        c.insert(default_python());
        c.insert(default_javascript());
        c.insert(default_typescript());
        c.insert(default_go());
        c.insert(default_shell());
        c.insert(default_docs());
        c.insert(default_config());
        c.insert(default_data());
        c
    }

    pub fn insert(&mut self, set: PredicateSet) {
        self.sets.insert(set.name.clone(), set);
    }

    pub fn get(&self, set_name: &str) -> Option<&PredicateSet> {
        self.sets.get(set_name)
    }

    /// Merge another catalog into this one. Templates in `other` override
    /// matching templates in `self`.
    pub fn merge(&mut self, other: PredicateSetCatalog) {
        for (name, set) in other.sets {
            self.sets
                .entry(name.clone())
                .and_modify(|existing| {
                    for (k, v) in &set.templates {
                        existing.templates.insert(k.clone(), v.clone());
                    }
                })
                .or_insert(set);
        }
    }
}

/// Interpolate `${var}` placeholders in a template with values from `vars`.
/// Returns an error if any referenced variable is missing — no silent defaults.
pub fn interpolate(
    template: &str,
    vars: &BTreeMap<String, String>,
) -> Result<String, WitnessError> {
    let re = regex::Regex::new(r"\$\{([a-zA-Z_][a-zA-Z0-9_]*)\}").unwrap();
    let mut out = String::with_capacity(template.len());
    let mut last_end = 0;
    for caps in re.captures_iter(template) {
        let whole = caps.get(0).unwrap();
        out.push_str(&template[last_end..whole.start()]);
        let var_name = &caps[1];
        let value = vars
            .get(var_name)
            .ok_or_else(|| WitnessError::MissingTemplateVar(var_name.to_string()))?;
        out.push_str(value);
        last_end = whole.end();
    }
    out.push_str(&template[last_end..]);
    Ok(out)
}

// ---------------------------------------------------------------------------
// Default predicate sets
// ---------------------------------------------------------------------------

pub fn default_rust() -> PredicateSet {
    PredicateSet::new("rust")
        .with_template(
            "test_passes",
            "CommandExits(cmd='cargo', args=['test', '${test_name}'], exit=0, timeout_ms=300000)",
        )
        .with_template(
            "lint_clean",
            "CommandExits(cmd='cargo', args=['clippy', '--', '-D', 'warnings'], exit=0, timeout_ms=300000)",
        )
        .with_template(
            "fmt_clean",
            "CommandExits(cmd='cargo', args=['fmt', '--check'], exit=0, timeout_ms=30000)",
        )
        .with_template(
            "no_stubs",
            "GrepAbsent(pattern='todo!\\\\(|unimplemented!\\\\(|panic!\\\\(\"(stub|unimplemented)', path_glob='${target_files}')",
        )
        .with_template(
            "symbol_wired",
            "GrepCountAtLeast(pattern='${symbol}', path_glob='${crate_dir}', n=2)",
        )
}

pub fn default_python() -> PredicateSet {
    PredicateSet::new("python")
        .with_template(
            "test_passes",
            "CommandExits(cmd='pytest', args=['${test_name}'], exit=0, timeout_ms=300000)",
        )
        .with_template(
            "lint_clean",
            "CommandExits(cmd='ruff', args=['check', '.'], exit=0, timeout_ms=60000)",
        )
        .with_template(
            "type_check",
            "CommandExits(cmd='mypy', args=['.'], exit=0, timeout_ms=180000)",
        )
        .with_template(
            "no_stubs",
            "GrepAbsent(pattern='pass\\\\s*#.*TODO|raise NotImplementedError|\\\\.\\\\.\\\\.\\\\s*$', path_glob='${target_files}')",
        )
        .with_template(
            "symbol_wired",
            "GrepCountAtLeast(pattern='${symbol}', path_glob='**/*.py', n=2)",
        )
}

pub fn default_javascript() -> PredicateSet {
    PredicateSet::new("javascript")
        .with_template(
            "test_passes",
            "CommandExits(cmd='npm', args=['test', '--', '${test_name}'], exit=0, timeout_ms=300000)",
        )
        .with_template(
            "lint_clean",
            "CommandExits(cmd='npm', args=['run', 'lint'], exit=0, timeout_ms=120000)",
        )
        .with_template(
            "build_clean",
            "CommandExits(cmd='npm', args=['run', 'build'], exit=0, timeout_ms=300000)",
        )
        .with_template(
            "no_stubs",
            "GrepAbsent(pattern='throw new Error\\\\(.unimplemented|// TODO:|// FIXME:', path_glob='${target_files}')",
        )
        .with_template(
            "symbol_wired",
            "GrepCountAtLeast(pattern='${symbol}', path_glob='src/**/*.{ts,tsx,js,jsx}', n=2)",
        )
}

pub fn default_typescript() -> PredicateSet {
    let mut s = default_javascript();
    s.name = "typescript".into();
    s.templates.insert(
        "type_check".into(),
        "CommandExits(cmd='npx', args=['tsc', '--noEmit'], exit=0, timeout_ms=300000)".into(),
    );
    s
}

pub fn default_go() -> PredicateSet {
    PredicateSet::new("go")
        .with_template(
            "test_passes",
            "CommandExits(cmd='go', args=['test', './...'], exit=0, timeout_ms=300000)",
        )
        .with_template(
            "vet_clean",
            "CommandExits(cmd='go', args=['vet', './...'], exit=0, timeout_ms=60000)",
        )
        .with_template(
            "build_clean",
            "CommandExits(cmd='go', args=['build', './...'], exit=0, timeout_ms=300000)",
        )
        .with_template(
            "no_stubs",
            "GrepAbsent(pattern='panic\\\\([\"\\\\'](not implemented|todo|stub)|// TODO:', path_glob='${target_files}')",
        )
        .with_template(
            "symbol_wired",
            "GrepCountAtLeast(pattern='${symbol}', path_glob='**/*.go', n=2)",
        )
}

pub fn default_shell() -> PredicateSet {
    PredicateSet::new("shell")
        .with_template(
            "syntax_ok",
            "CommandExits(cmd='bash', args=['-n', '${script}'], exit=0, timeout_ms=10000)",
        )
        .with_template(
            "no_user_paths",
            "GrepAbsent(pattern='/home/[a-z]+|/Users/[a-z]+', path_glob='${script}')",
        )
}

pub fn default_docs() -> PredicateSet {
    PredicateSet::new("docs")
        .with_template("file_exists", "FileExists(path='${doc_path}')")
        .with_template(
            "mentions_feature",
            "FileContains(path='${doc_path}', regex='${feature_name}')",
        )
        .with_template(
            "no_todo",
            "GrepAbsent(pattern='TODO|FIXME|XXX', path_glob='${doc_path}')",
        )
}

pub fn default_config() -> PredicateSet {
    PredicateSet::new("config")
        .with_template(
            "syntax_valid",
            "CommandExits(cmd='${validator_cmd}', args=[], exit=0, timeout_ms=30000)",
        )
        .with_template(
            "service_responds",
            "HttpStatus(url='${service_url}', method='GET', expected_status=200)",
        )
        .with_template(
            "has_entry",
            "FileContains(path='${config_path}', regex='${expected_entry}')",
        )
}

pub fn default_data() -> PredicateSet {
    PredicateSet::new("data")
        .with_template(
            "script_runs",
            "CommandExits(cmd='python', args=['${script}'], exit=0, timeout_ms=600000)",
        )
        .with_template("output_exists", "FileExists(path='${output_path}')")
        .with_template(
            "output_reasonable",
            "FileSizeInRange(path='${output_path}', min_bytes=1024, max_bytes=10737418240)",
        )
        .with_template(
            "report_has_metric",
            "FileContains(path='${report_path}', regex='${metric_name}:\\\\s*[0-9.]+')",
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn interpolate_replaces_single_var() {
        let mut vars = BTreeMap::new();
        vars.insert("name".into(), "foo".into());
        let out = interpolate("hello ${name}", &vars).unwrap();
        assert_eq!(out, "hello foo");
    }

    #[test]
    fn interpolate_replaces_multiple() {
        let mut vars = BTreeMap::new();
        vars.insert("a".into(), "alpha".into());
        vars.insert("b".into(), "beta".into());
        let out = interpolate("${a} and ${b}", &vars).unwrap();
        assert_eq!(out, "alpha and beta");
    }

    #[test]
    fn interpolate_missing_var_errors() {
        let vars = BTreeMap::new();
        let err = interpolate("hi ${missing}", &vars);
        assert!(matches!(err, Err(WitnessError::MissingTemplateVar(_))));
    }

    #[test]
    fn defaults_contain_core_sets() {
        let c = PredicateSetCatalog::defaults();
        assert!(c.get("rust").is_some());
        assert!(c.get("python").is_some());
        assert!(c.get("javascript").is_some());
        assert!(c.get("typescript").is_some());
        assert!(c.get("go").is_some());
        assert!(c.get("shell").is_some());
        assert!(c.get("docs").is_some());
    }

    #[test]
    fn rust_set_has_required_templates() {
        let c = PredicateSetCatalog::defaults();
        let rust = c.get("rust").unwrap();
        assert!(rust.get("test_passes").is_some());
        assert!(rust.get("lint_clean").is_some());
        assert!(rust.get("no_stubs").is_some());
        assert!(rust.get("symbol_wired").is_some());
    }

    #[test]
    fn merge_overrides_templates() {
        let mut base = PredicateSetCatalog::defaults();
        let mut override_set = PredicateSet::new("rust");
        override_set.templates.insert(
            "test_passes".into(),
            "CommandExits(cmd='my-custom-test', args=[], exit=0, timeout_ms=1000)".into(),
        );
        let mut override_cat = PredicateSetCatalog::new();
        override_cat.insert(override_set);

        base.merge(override_cat);
        let rust = base.get("rust").unwrap();
        assert!(rust.get("test_passes").unwrap().contains("my-custom-test"));
        // Other templates preserved.
        assert!(rust.get("lint_clean").is_some());
    }
}
