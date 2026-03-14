//! Vault URI resolver — parses `vault://temm1e/<key>` URIs and delegates
//! to a [`Vault`] implementation for retrieval.

use temm1e_core::types::error::Temm1eError;
use temm1e_core::Vault;

/// The URI scheme prefix.
const VAULT_SCHEME: &str = "vault://";

/// The expected authority for local vaults.
const VAULT_AUTHORITY: &str = "temm1e";

/// Parsed vault URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultUri {
    /// The authority portion (e.g. "temm1e").
    pub authority: String,
    /// The key path (everything after `vault://authority/`).
    pub key: String,
}

/// Parse a `vault://temm1e/key_name` URI.
///
/// Returns an error if the URI does not start with `vault://` or the
/// authority is not `temm1e`.
pub fn parse_vault_uri(uri: &str) -> Result<VaultUri, Temm1eError> {
    let rest = uri
        .strip_prefix(VAULT_SCHEME)
        .ok_or_else(|| Temm1eError::Vault(format!("not a vault URI: {uri}")))?;

    let (authority, key) = rest
        .split_once('/')
        .ok_or_else(|| Temm1eError::Vault(format!("vault URI missing key path: {uri}")))?;

    if authority != VAULT_AUTHORITY {
        return Err(Temm1eError::Vault(format!(
            "unsupported vault authority \"{authority}\", expected \"{VAULT_AUTHORITY}\""
        )));
    }

    if key.is_empty() {
        return Err(Temm1eError::Vault("vault URI has empty key".into()));
    }

    Ok(VaultUri {
        authority: authority.to_string(),
        key: key.to_string(),
    })
}

/// Returns `true` if `text` looks like a `vault://` URI.
pub fn is_vault_uri(text: &str) -> bool {
    text.starts_with(VAULT_SCHEME)
}

/// Resolve a vault URI to its plaintext value using the given vault backend.
pub async fn resolve(vault: &dyn Vault, uri: &str) -> Result<Option<Vec<u8>>, Temm1eError> {
    let parsed = parse_vault_uri(uri)?;
    vault.get_secret(&parsed.key).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid() {
        let uri = parse_vault_uri("vault://temm1e/my/secret/key").unwrap();
        assert_eq!(uri.authority, "temm1e");
        assert_eq!(uri.key, "my/secret/key");
    }

    #[test]
    fn parse_simple_key() {
        let uri = parse_vault_uri("vault://temm1e/api_key").unwrap();
        assert_eq!(uri.key, "api_key");
    }

    #[test]
    fn reject_wrong_scheme() {
        assert!(parse_vault_uri("http://temm1e/key").is_err());
    }

    #[test]
    fn reject_wrong_authority() {
        assert!(parse_vault_uri("vault://aws/key").is_err());
    }

    #[test]
    fn reject_empty_key() {
        assert!(parse_vault_uri("vault://temm1e/").is_err());
    }

    #[test]
    fn reject_missing_path() {
        assert!(parse_vault_uri("vault://temm1e").is_err());
    }

    #[test]
    fn is_vault_uri_check() {
        assert!(is_vault_uri("vault://temm1e/key"));
        assert!(!is_vault_uri("http://example.com"));
    }

    // ── T5b: New edge case tests ──────────────────────────────────────

    #[test]
    fn parse_nested_key_path() {
        let uri = parse_vault_uri("vault://temm1e/providers/anthropic/api_key").unwrap();
        assert_eq!(uri.key, "providers/anthropic/api_key");
    }

    #[test]
    fn is_vault_uri_empty_string() {
        assert!(!is_vault_uri(""));
    }

    #[test]
    fn is_vault_uri_partial_scheme() {
        assert!(!is_vault_uri("vault:/"));
        assert!(!is_vault_uri("vault:"));
    }

    #[test]
    fn reject_vault_uri_no_slash_after_authority() {
        // "vault://temm1e" has no trailing slash or key
        assert!(parse_vault_uri("vault://temm1e").is_err());
    }

    #[test]
    fn parse_vault_uri_with_special_chars_in_key() {
        let uri = parse_vault_uri("vault://temm1e/key-with_special.chars").unwrap();
        assert_eq!(uri.key, "key-with_special.chars");
    }

    #[test]
    fn is_vault_uri_case_sensitive() {
        // "Vault://" with capital V should not match
        assert!(!is_vault_uri("Vault://temm1e/key"));
    }
}
