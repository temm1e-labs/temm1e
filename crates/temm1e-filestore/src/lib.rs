//! TEMM1E Filestore crate
//!
//! Provides file storage backends for TEMM1E. Two backends are available:
//!
//! - [`LocalFileStore`] — stores files on the local filesystem under a base
//!   directory. Always available.
//! - [`S3FileStore`] — stores files in an S3-compatible bucket (AWS S3,
//!   Cloudflare R2, MinIO). Requires the `s3` feature flag.
//!
//! Use [`create_filestore`] to construct a backend from configuration.

pub mod local;

#[cfg(feature = "s3")]
pub mod s3;

pub use local::LocalFileStore;

#[cfg(feature = "s3")]
pub use s3::S3FileStore;

use temm1e_core::error::Temm1eError;
use temm1e_core::types::config::FileStoreConfig;
use temm1e_core::FileStore;

/// Factory function: create a file store backend by configuration.
///
/// Supported backends:
/// - `"local"` — uses `config.path` as the base directory (defaults to `"./files"`).
/// - `"s3"` — requires the `s3` feature and a `config.bucket`.
pub async fn create_filestore(config: &FileStoreConfig) -> Result<Box<dyn FileStore>, Temm1eError> {
    match config.backend.as_str() {
        "local" => {
            let path = config.path.as_deref().unwrap_or("./files");
            let store = LocalFileStore::new(path).await?;
            Ok(Box::new(store))
        }
        #[cfg(feature = "s3")]
        "s3" => {
            let store = S3FileStore::new(config).await?;
            Ok(Box::new(store))
        }
        #[cfg(not(feature = "s3"))]
        "s3" => Err(Temm1eError::Config(
            "S3 file store requires the 's3' feature flag".to_string(),
        )),
        other => Err(Temm1eError::Config(format!(
            "Unknown filestore backend: {other}"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_create_local_filestore() {
        let dir = tempfile::tempdir().unwrap();
        let config = FileStoreConfig {
            backend: "local".to_string(),
            bucket: None,
            region: None,
            endpoint: None,
            path: Some(dir.path().to_string_lossy().to_string()),
        };

        let store = create_filestore(&config).await.unwrap();
        assert_eq!(store.backend_name(), "local");
    }

    #[tokio::test]
    async fn test_create_local_filestore_default_path() {
        let config = FileStoreConfig {
            backend: "local".to_string(),
            bucket: None,
            region: None,
            endpoint: None,
            path: None,
        };

        let store = create_filestore(&config).await.unwrap();
        assert_eq!(store.backend_name(), "local");

        // Clean up the default directory created.
        let _ = std::fs::remove_dir("./files");
    }

    #[tokio::test]
    async fn test_create_unknown_backend() {
        let config = FileStoreConfig {
            backend: "redis".to_string(),
            bucket: None,
            region: None,
            endpoint: None,
            path: None,
        };

        let result = create_filestore(&config).await;
        let err = result.err().expect("should fail for unknown backend");
        let msg = err.to_string();
        assert!(
            msg.contains("redis"),
            "Error should mention the unknown backend: {msg}"
        );
    }

    #[cfg(not(feature = "s3"))]
    #[tokio::test]
    async fn test_create_s3_without_feature() {
        let config = FileStoreConfig {
            backend: "s3".to_string(),
            bucket: Some("test".to_string()),
            region: None,
            endpoint: None,
            path: None,
        };

        let result = create_filestore(&config).await;
        let err = result.err().expect("should fail without s3 feature");
        let msg = err.to_string();
        assert!(
            msg.contains("feature"),
            "Error should mention feature flag: {msg}"
        );
    }
}
