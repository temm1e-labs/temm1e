//! Integration tests for the vault — tests persistence, resolver integration,
//! and credential detection combined with vault operations.

use temm1e_core::Vault;
use temm1e_vault::local::LocalVault;
use temm1e_vault::{detect_credentials, is_vault_uri, parse_vault_uri, resolve};

#[tokio::test]
async fn vault_store_and_resolve_uri() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = LocalVault::with_dir(tmp.path().to_path_buf())
        .await
        .unwrap();

    vault
        .store_secret("api/key", b"secret-value-123")
        .await
        .unwrap();

    // Resolve through the resolver module
    let result = resolve(&vault, "vault://temm1e/api/key").await.unwrap();
    assert_eq!(result.unwrap(), b"secret-value-123");
}

#[tokio::test]
async fn vault_resolver_invalid_uri_errors() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = LocalVault::with_dir(tmp.path().to_path_buf())
        .await
        .unwrap();

    let result = resolve(&vault, "http://temm1e/key").await;
    assert!(result.is_err());
}

#[tokio::test]
async fn vault_resolver_missing_key_returns_none() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = LocalVault::with_dir(tmp.path().to_path_buf())
        .await
        .unwrap();

    let result = resolve(&vault, "vault://temm1e/nonexistent").await.unwrap();
    assert!(result.is_none());
}

#[tokio::test]
async fn vault_persistence_and_reload() {
    let tmp = tempfile::tempdir().unwrap();

    // Store multiple secrets
    {
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();
        vault.store_secret("key1", b"value1").await.unwrap();
        vault.store_secret("key2", b"value2").await.unwrap();
        vault
            .store_secret("nested/deep/key3", b"value3")
            .await
            .unwrap();
    }

    // Reload and verify all secrets survive
    {
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();

        let keys = vault.list_keys().await.unwrap();
        assert_eq!(keys.len(), 3);

        assert_eq!(vault.get_secret("key1").await.unwrap().unwrap(), b"value1");
        assert_eq!(vault.get_secret("key2").await.unwrap().unwrap(), b"value2");
        assert_eq!(
            vault.get_secret("nested/deep/key3").await.unwrap().unwrap(),
            b"value3"
        );
    }
}

#[tokio::test]
async fn vault_delete_and_reload() {
    let tmp = tempfile::tempdir().unwrap();

    {
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();
        vault.store_secret("to_delete", b"gone soon").await.unwrap();
        vault.store_secret("to_keep", b"stay").await.unwrap();
        vault.delete_secret("to_delete").await.unwrap();
    }

    // Reload and verify deletion persisted
    {
        let vault = LocalVault::with_dir(tmp.path().to_path_buf())
            .await
            .unwrap();
        assert!(!vault.has_key("to_delete").await.unwrap());
        assert!(vault.has_key("to_keep").await.unwrap());
    }
}

#[test]
fn credential_detection_and_vault_uri_integration() {
    // Detect a credential, then verify it could be stored in a vault URI
    let text = "api_key=super_secret_api_key_value_1234567890";
    let creds = detect_credentials(text);
    assert!(!creds.is_empty());

    // Construct a vault URI for the detected credential
    let uri = format!("vault://temm1e/{}", creds[0].key);
    assert!(is_vault_uri(&uri));

    let parsed = parse_vault_uri(&uri).unwrap();
    assert_eq!(parsed.key, creds[0].key);
}

#[tokio::test]
async fn vault_backend_name() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = LocalVault::with_dir(tmp.path().to_path_buf())
        .await
        .unwrap();
    assert_eq!(vault.backend_name(), "local-chacha20");
}

#[tokio::test]
async fn vault_update_secret_value() {
    let tmp = tempfile::tempdir().unwrap();
    let vault = LocalVault::with_dir(tmp.path().to_path_buf())
        .await
        .unwrap();

    vault.store_secret("mutable", b"v1").await.unwrap();
    assert_eq!(vault.get_secret("mutable").await.unwrap().unwrap(), b"v1");

    vault.store_secret("mutable", b"v2").await.unwrap();
    assert_eq!(vault.get_secret("mutable").await.unwrap().unwrap(), b"v2");

    // Only one key should exist
    let keys = vault.list_keys().await.unwrap();
    assert_eq!(keys.len(), 1);
}
