//! Local filesystem file store implementation.

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use futures::StreamExt;
use std::path::{Path, PathBuf};
use temm1e_core::error::Temm1eError;
use temm1e_core::file::FileMetadata;
use temm1e_core::FileStore;
use tokio::fs;
use tracing::{debug, info};

/// A file store backed by the local filesystem.
///
/// Files are stored under a configurable base directory. Storage keys are
/// relative paths within that directory.
pub struct LocalFileStore {
    base_dir: PathBuf,
}

impl LocalFileStore {
    /// Create a new `LocalFileStore` rooted at `base_dir`.
    ///
    /// The directory is created (recursively) if it does not already exist.
    pub async fn new(base_dir: impl Into<PathBuf>) -> Result<Self, Temm1eError> {
        let base_dir = base_dir.into();
        fs::create_dir_all(&base_dir).await.map_err(|e| {
            Temm1eError::FileTransfer(format!(
                "Failed to create base directory {}: {e}",
                base_dir.display()
            ))
        })?;
        info!(base_dir = %base_dir.display(), "Local file store initialised");
        Ok(Self { base_dir })
    }

    /// Resolve a relative storage path to an absolute filesystem path.
    ///
    /// Returns an error if the resolved path would escape the base directory
    /// (path traversal protection).
    fn resolve_path(&self, path: &str) -> Result<PathBuf, Temm1eError> {
        // Sanitise: strip leading slashes and reject ".." components.
        let sanitised = Path::new(path)
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .collect::<PathBuf>();

        if sanitised.as_os_str().is_empty() {
            return Err(Temm1eError::FileTransfer(
                "Empty or invalid storage path".to_string(),
            ));
        }

        let full = self.base_dir.join(&sanitised);

        // Double-check: the canonical prefix must be inside base_dir.
        // We can only canonicalise paths that already exist, so we check the
        // parent (which we will create) and the final component separately.
        // For simplicity, just ensure the joined path starts with base_dir.
        if !full.starts_with(&self.base_dir) {
            return Err(Temm1eError::FileTransfer(format!(
                "Path traversal detected: {path}"
            )));
        }

        Ok(full)
    }
}

#[async_trait]
impl FileStore for LocalFileStore {
    async fn store(
        &self,
        path: &str,
        data: Bytes,
        metadata: FileMetadata,
    ) -> Result<String, Temm1eError> {
        let full_path = self.resolve_path(path)?;

        // Ensure parent directory exists.
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                Temm1eError::FileTransfer(format!(
                    "Failed to create directory {}: {e}",
                    parent.display()
                ))
            })?;
        }

        fs::write(&full_path, &data).await.map_err(|e| {
            Temm1eError::FileTransfer(format!("Failed to write file {}: {e}", full_path.display()))
        })?;

        debug!(
            path = %full_path.display(),
            name = %metadata.name,
            mime_type = %metadata.mime_type,
            size = ?metadata.size,
            "Stored file locally"
        );

        Ok(path.to_string())
    }

    async fn store_stream(
        &self,
        path: &str,
        mut stream: BoxStream<'_, Bytes>,
        metadata: FileMetadata,
    ) -> Result<String, Temm1eError> {
        let full_path = self.resolve_path(path)?;

        // Ensure parent directory exists.
        if let Some(parent) = full_path.parent() {
            fs::create_dir_all(parent).await.map_err(|e| {
                Temm1eError::FileTransfer(format!(
                    "Failed to create directory {}: {e}",
                    parent.display()
                ))
            })?;
        }

        // Collect chunks into a single buffer, then write.
        let mut buf = Vec::new();
        while let Some(chunk) = stream.next().await {
            buf.extend_from_slice(&chunk);
        }

        fs::write(&full_path, &buf).await.map_err(|e| {
            Temm1eError::FileTransfer(format!(
                "Failed to write streamed file {}: {e}",
                full_path.display()
            ))
        })?;

        debug!(
            path = %full_path.display(),
            name = %metadata.name,
            mime_type = %metadata.mime_type,
            size = buf.len(),
            "Stored streamed file locally"
        );

        Ok(path.to_string())
    }

    async fn get(&self, key: &str) -> Result<Option<Bytes>, Temm1eError> {
        let full_path = self.resolve_path(key)?;

        if !full_path.exists() {
            debug!(key = %key, "File not found");
            return Ok(None);
        }

        let data = fs::read(&full_path).await.map_err(|e| {
            Temm1eError::FileTransfer(format!("Failed to read file {}: {e}", full_path.display()))
        })?;

        debug!(key = %key, size = data.len(), "Retrieved file");
        Ok(Some(Bytes::from(data)))
    }

    async fn presigned_url(
        &self,
        _key: &str,
        _expires_in_secs: u64,
    ) -> Result<Option<String>, Temm1eError> {
        // Local filesystem does not support presigned URLs.
        Ok(None)
    }

    async fn delete(&self, key: &str) -> Result<(), Temm1eError> {
        let full_path = self.resolve_path(key)?;

        if full_path.exists() {
            fs::remove_file(&full_path).await.map_err(|e| {
                Temm1eError::FileTransfer(format!(
                    "Failed to delete file {}: {e}",
                    full_path.display()
                ))
            })?;
            debug!(key = %key, "Deleted file");
        } else {
            debug!(key = %key, "File not found for deletion, ignoring");
        }

        Ok(())
    }

    async fn list(&self, prefix: &str) -> Result<Vec<String>, Temm1eError> {
        let search_dir = if prefix.is_empty() {
            self.base_dir.clone()
        } else {
            self.resolve_path(prefix)?
        };

        if !search_dir.exists() {
            return Ok(Vec::new());
        }

        let mut results = Vec::new();
        self.list_recursive(&search_dir, &mut results).await?;

        // Convert absolute paths back to relative keys.
        let keys: Vec<String> = results
            .into_iter()
            .filter_map(|p| {
                p.strip_prefix(&self.base_dir)
                    .ok()
                    .map(|rel| rel.to_string_lossy().to_string())
            })
            .collect();

        debug!(prefix = %prefix, count = keys.len(), "Listed files");
        Ok(keys)
    }

    fn backend_name(&self) -> &str {
        "local"
    }
}

impl LocalFileStore {
    /// Recursively list all files under `dir`.
    async fn list_recursive(
        &self,
        dir: &Path,
        results: &mut Vec<PathBuf>,
    ) -> Result<(), Temm1eError> {
        // If the path is a file rather than a directory, just return it.
        if dir.is_file() {
            results.push(dir.to_path_buf());
            return Ok(());
        }

        if !dir.is_dir() {
            return Ok(());
        }

        let mut entries = fs::read_dir(dir).await.map_err(|e| {
            Temm1eError::FileTransfer(format!("Failed to read directory {}: {e}", dir.display()))
        })?;

        while let Some(entry) = entries.next_entry().await.map_err(|e| {
            Temm1eError::FileTransfer(format!("Failed to read directory entry: {e}"))
        })? {
            let path = entry.path();
            if path.is_dir() {
                Box::pin(self.list_recursive(&path, results)).await?;
            } else {
                results.push(path);
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use futures::stream;
    use tempfile::tempdir;

    fn test_metadata(name: &str) -> FileMetadata {
        FileMetadata {
            name: name.to_string(),
            mime_type: "application/octet-stream".to_string(),
            size: None,
            content_hash: None,
        }
    }

    #[tokio::test]
    async fn test_backend_name() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();
        assert_eq!(store.backend_name(), "local");
    }

    #[tokio::test]
    async fn test_store_and_get() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        let data = Bytes::from("hello world");
        let key = store
            .store("test.txt", data.clone(), test_metadata("test.txt"))
            .await
            .unwrap();

        assert_eq!(key, "test.txt");

        let retrieved = store.get("test.txt").await.unwrap();
        assert_eq!(retrieved, Some(data));
    }

    #[tokio::test]
    async fn test_store_nested_path() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        let data = Bytes::from("nested content");
        let key = store
            .store(
                "subdir/deep/file.bin",
                data.clone(),
                test_metadata("file.bin"),
            )
            .await
            .unwrap();

        assert_eq!(key, "subdir/deep/file.bin");

        let retrieved = store.get("subdir/deep/file.bin").await.unwrap();
        assert_eq!(retrieved, Some(data));
    }

    #[tokio::test]
    async fn test_get_nonexistent() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        let result = store.get("does_not_exist.txt").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_delete() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        let data = Bytes::from("to be deleted");
        store
            .store("deleteme.txt", data, test_metadata("deleteme.txt"))
            .await
            .unwrap();

        // File exists.
        assert!(store.get("deleteme.txt").await.unwrap().is_some());

        // Delete it.
        store.delete("deleteme.txt").await.unwrap();

        // Gone.
        assert!(store.get("deleteme.txt").await.unwrap().is_none());
    }

    #[tokio::test]
    async fn test_delete_nonexistent_is_ok() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        // Deleting a file that doesn't exist should succeed silently.
        store.delete("nope.txt").await.unwrap();
    }

    #[tokio::test]
    async fn test_list_empty() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        let files = store.list("").await.unwrap();
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn test_list_files() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        store
            .store("a.txt", Bytes::from("a"), test_metadata("a.txt"))
            .await
            .unwrap();
        store
            .store("b.txt", Bytes::from("b"), test_metadata("b.txt"))
            .await
            .unwrap();
        store
            .store("sub/c.txt", Bytes::from("c"), test_metadata("c.txt"))
            .await
            .unwrap();

        let mut files = store.list("").await.unwrap();
        files.sort();
        assert_eq!(files.len(), 3);
        assert!(files.contains(&"a.txt".to_string()));
        assert!(files.contains(&"b.txt".to_string()));
        assert!(files.contains(&"sub/c.txt".to_string()));
    }

    #[tokio::test]
    async fn test_list_with_prefix() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        store
            .store("uploads/a.txt", Bytes::from("a"), test_metadata("a.txt"))
            .await
            .unwrap();
        store
            .store("uploads/b.txt", Bytes::from("b"), test_metadata("b.txt"))
            .await
            .unwrap();
        store
            .store("other/c.txt", Bytes::from("c"), test_metadata("c.txt"))
            .await
            .unwrap();

        let mut files = store.list("uploads").await.unwrap();
        files.sort();
        assert_eq!(files.len(), 2);
        assert!(files.contains(&"uploads/a.txt".to_string()));
        assert!(files.contains(&"uploads/b.txt".to_string()));
    }

    #[tokio::test]
    async fn test_list_nonexistent_prefix() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        let files = store.list("nonexistent").await.unwrap();
        assert!(files.is_empty());
    }

    #[tokio::test]
    async fn test_presigned_url_returns_none() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        store
            .store("file.txt", Bytes::from("data"), test_metadata("file.txt"))
            .await
            .unwrap();

        let url = store.presigned_url("file.txt", 3600).await.unwrap();
        assert!(url.is_none());
    }

    #[tokio::test]
    async fn test_store_stream() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        let chunks: Vec<Bytes> = vec![
            Bytes::from("chunk1"),
            Bytes::from("chunk2"),
            Bytes::from("chunk3"),
        ];
        let stream = stream::iter(chunks);
        let boxed: BoxStream<'_, Bytes> = Box::pin(stream);

        let key = store
            .store_stream("streamed.bin", boxed, test_metadata("streamed.bin"))
            .await
            .unwrap();

        assert_eq!(key, "streamed.bin");

        let retrieved = store.get("streamed.bin").await.unwrap().unwrap();
        assert_eq!(retrieved.as_ref(), b"chunk1chunk2chunk3");
    }

    #[tokio::test]
    async fn test_path_traversal_rejected() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        // Paths with ".." components should be stripped by the sanitiser,
        // resulting in a safe path or an error.
        let result = store
            .store(
                "../../../etc/passwd",
                Bytes::from("evil"),
                test_metadata("passwd"),
            )
            .await;

        // The ".." components are filtered out, so the path becomes "etc/passwd"
        // which is safe (inside base_dir). Verify it actually stored there.
        assert!(result.is_ok());

        // Verify that the file is inside the base directory.
        let full = dir.path().join("etc/passwd");
        assert!(full.exists());
    }

    #[tokio::test]
    async fn test_empty_path_rejected() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        let result = store
            .store("", Bytes::from("data"), test_metadata("empty"))
            .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_overwrite_existing_file() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        let original = Bytes::from("original");
        store
            .store("overwrite.txt", original, test_metadata("overwrite.txt"))
            .await
            .unwrap();

        let updated = Bytes::from("updated");
        store
            .store(
                "overwrite.txt",
                updated.clone(),
                test_metadata("overwrite.txt"),
            )
            .await
            .unwrap();

        let retrieved = store.get("overwrite.txt").await.unwrap().unwrap();
        assert_eq!(retrieved, updated);
    }

    #[tokio::test]
    async fn test_store_empty_file() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        let data = Bytes::new();
        store
            .store("empty.txt", data.clone(), test_metadata("empty.txt"))
            .await
            .unwrap();

        let retrieved = store.get("empty.txt").await.unwrap().unwrap();
        assert!(retrieved.is_empty());
    }

    #[tokio::test]
    async fn test_store_large_content() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        // 1 MB of data
        let data = Bytes::from(vec![0xABu8; 1_000_000]);
        store
            .store("large.bin", data.clone(), test_metadata("large.bin"))
            .await
            .unwrap();

        let retrieved = store.get("large.bin").await.unwrap().unwrap();
        assert_eq!(retrieved.len(), 1_000_000);
        assert_eq!(retrieved, data);
    }

    #[tokio::test]
    async fn test_store_stream_empty() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        let stream: BoxStream<'_, Bytes> = Box::pin(stream::empty());
        let key = store
            .store_stream(
                "empty_stream.bin",
                stream,
                test_metadata("empty_stream.bin"),
            )
            .await
            .unwrap();

        assert_eq!(key, "empty_stream.bin");

        let retrieved = store.get("empty_stream.bin").await.unwrap().unwrap();
        assert!(retrieved.is_empty());
    }

    #[tokio::test]
    async fn test_creates_base_dir_if_missing() {
        let dir = tempdir().unwrap();
        let nested = dir.path().join("a").join("b").join("c");

        assert!(!nested.exists());
        let _store = LocalFileStore::new(&nested).await.unwrap();
        assert!(nested.exists());
    }

    #[tokio::test]
    async fn test_full_crud_cycle() {
        let dir = tempdir().unwrap();
        let store = LocalFileStore::new(dir.path()).await.unwrap();

        // Create
        let data = Bytes::from("crud test data");
        store
            .store("crud/item.dat", data.clone(), test_metadata("item.dat"))
            .await
            .unwrap();

        // Read
        let read = store.get("crud/item.dat").await.unwrap().unwrap();
        assert_eq!(read, data);

        // List
        let files = store.list("crud").await.unwrap();
        assert_eq!(files, vec!["crud/item.dat".to_string()]);

        // Update (overwrite)
        let updated = Bytes::from("updated crud data");
        store
            .store("crud/item.dat", updated.clone(), test_metadata("item.dat"))
            .await
            .unwrap();
        let read2 = store.get("crud/item.dat").await.unwrap().unwrap();
        assert_eq!(read2, updated);

        // Delete
        store.delete("crud/item.dat").await.unwrap();
        assert!(store.get("crud/item.dat").await.unwrap().is_none());

        // List should be empty
        let files = store.list("crud").await.unwrap();
        assert!(files.is_empty());
    }
}
