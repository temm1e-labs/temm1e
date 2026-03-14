use std::sync::LazyLock;

use regex::Regex;

static ENV_RE: LazyLock<Regex> =
    LazyLock::new(|| Regex::new(r"\$\{([^}]+)\}").expect("invalid env var regex"));

/// Expand environment variables in a string (${VAR_NAME} syntax)
pub fn expand_env_vars(input: &str) -> String {
    ENV_RE
        .replace_all(input, |caps: &regex::Captures| {
            let var_name = &caps[1];
            std::env::var(var_name).unwrap_or_default()
        })
        .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_expand_env_vars() {
        std::env::set_var("TEST_TEMM1E_VAR", "hello");
        assert_eq!(expand_env_vars("${TEST_TEMM1E_VAR}"), "hello");
        assert_eq!(
            expand_env_vars("prefix_${TEST_TEMM1E_VAR}_suffix"),
            "prefix_hello_suffix"
        );
        assert_eq!(expand_env_vars("no_vars_here"), "no_vars_here");
        assert_eq!(expand_env_vars("${NONEXISTENT_VAR}"), "");
        std::env::remove_var("TEST_TEMM1E_VAR");
    }

    // ── T5b: New edge case tests ──────────────────────────────────────

    #[test]
    fn test_missing_env_var_expands_to_empty() {
        let result = expand_env_vars("key=${THIS_VAR_DOES_NOT_EXIST_99999}");
        assert_eq!(result, "key=");
    }

    #[test]
    fn test_multiple_vars_in_one_string() {
        std::env::set_var("TEMM1E_A", "alpha");
        std::env::set_var("TEMM1E_B", "beta");
        let result = expand_env_vars("${TEMM1E_A}:${TEMM1E_B}");
        assert_eq!(result, "alpha:beta");
        std::env::remove_var("TEMM1E_A");
        std::env::remove_var("TEMM1E_B");
    }

    #[test]
    fn test_empty_input() {
        assert_eq!(expand_env_vars(""), "");
    }

    #[test]
    fn test_dollar_without_braces_not_expanded() {
        assert_eq!(expand_env_vars("$FOO"), "$FOO");
    }

    #[test]
    fn test_nested_braces_not_recursive() {
        // ${${INNER}} is not supported; the regex just finds ${${INNER}
        // and tries it as a var name, which won't match.
        std::env::set_var("TEMM1E_INNER", "value");
        let result = expand_env_vars("${${TEMM1E_INNER}}");
        // The regex matches ${${TEMM1E_INNER} with var name "${TEMM1E_INNER"
        // which doesn't exist, so it expands to "" followed by the trailing "}"
        assert_eq!(result, "}");
        std::env::remove_var("TEMM1E_INNER");
    }
}
