//! Vault URI resolver — parses `vault://skyclaw/<key>` URIs and delegates
//! to a [`Vault`] implementation for retrieval.

use skyclaw_core::Vault;
use skyclaw_core::types::error::SkyclawError;

/// The URI scheme prefix.
const VAULT_SCHEME: &str = "vault://";

/// The expected authority for local vaults.
const VAULT_AUTHORITY: &str = "skyclaw";

/// Parsed vault URI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VaultUri {
    /// The authority portion (e.g. "skyclaw").
    pub authority: String,
    /// The key path (everything after `vault://authority/`).
    pub key: String,
}

/// Parse a `vault://skyclaw/key_name` URI.
///
/// Returns an error if the URI does not start with `vault://` or the
/// authority is not `skyclaw`.
pub fn parse_vault_uri(uri: &str) -> Result<VaultUri, SkyclawError> {
    let rest = uri
        .strip_prefix(VAULT_SCHEME)
        .ok_or_else(|| SkyclawError::Vault(format!("not a vault URI: {uri}")))?;

    let (authority, key) = rest
        .split_once('/')
        .ok_or_else(|| SkyclawError::Vault(format!("vault URI missing key path: {uri}")))?;

    if authority != VAULT_AUTHORITY {
        return Err(SkyclawError::Vault(format!(
            "unsupported vault authority \"{authority}\", expected \"{VAULT_AUTHORITY}\""
        )));
    }

    if key.is_empty() {
        return Err(SkyclawError::Vault("vault URI has empty key".into()));
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
pub async fn resolve(
    vault: &dyn Vault,
    uri: &str,
) -> Result<Option<Vec<u8>>, SkyclawError> {
    let parsed = parse_vault_uri(uri)?;
    vault.get_secret(&parsed.key).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_valid() {
        let uri = parse_vault_uri("vault://skyclaw/my/secret/key").unwrap();
        assert_eq!(uri.authority, "skyclaw");
        assert_eq!(uri.key, "my/secret/key");
    }

    #[test]
    fn parse_simple_key() {
        let uri = parse_vault_uri("vault://skyclaw/api_key").unwrap();
        assert_eq!(uri.key, "api_key");
    }

    #[test]
    fn reject_wrong_scheme() {
        assert!(parse_vault_uri("http://skyclaw/key").is_err());
    }

    #[test]
    fn reject_wrong_authority() {
        assert!(parse_vault_uri("vault://aws/key").is_err());
    }

    #[test]
    fn reject_empty_key() {
        assert!(parse_vault_uri("vault://skyclaw/").is_err());
    }

    #[test]
    fn reject_missing_path() {
        assert!(parse_vault_uri("vault://skyclaw").is_err());
    }

    #[test]
    fn is_vault_uri_check() {
        assert!(is_vault_uri("vault://skyclaw/key"));
        assert!(!is_vault_uri("http://example.com"));
    }
}
