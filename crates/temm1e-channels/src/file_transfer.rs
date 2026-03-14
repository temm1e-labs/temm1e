//! Shared file transfer utilities for all channels.

use bytes::Bytes;
use std::path::{Path, PathBuf};
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::file::{FileData, OutboundFile, ReceivedFile};

/// Save a received file to the workspace directory.
///
/// Creates the workspace directory if it does not exist. Returns the
/// full path where the file was written.
pub async fn save_received_file(
    file: &ReceivedFile,
    workspace: &Path,
) -> Result<PathBuf, Temm1eError> {
    // Ensure workspace directory exists
    tokio::fs::create_dir_all(workspace)
        .await
        .map_err(|e| Temm1eError::FileTransfer(format!("Failed to create workspace dir: {e}")))?;

    // Sanitize filename: strip directory components to prevent path traversal
    let safe_name = Path::new(&file.name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed_file".to_string());

    let dest = workspace.join(&safe_name);

    tokio::fs::write(&dest, &file.data)
        .await
        .map_err(|e| Temm1eError::FileTransfer(format!("Failed to write file {safe_name}: {e}")))?;

    tracing::info!(path = %dest.display(), size = file.size, "Saved received file");
    Ok(dest)
}

/// Read a local file and prepare it for sending via a channel.
pub async fn read_file_for_sending(path: &Path) -> Result<OutboundFile, Temm1eError> {
    let data = tokio::fs::read(path).await.map_err(|e| {
        Temm1eError::FileTransfer(format!("Failed to read file {}: {e}", path.display()))
    })?;

    let name = path
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "file".to_string());

    let mime_type = mime_from_extension(path);

    Ok(OutboundFile {
        name,
        mime_type,
        data: FileData::Bytes(Bytes::from(data)),
        caption: None,
    })
}

/// Best-effort MIME type detection from file extension.
fn mime_from_extension(path: &Path) -> String {
    match path.extension().and_then(|e| e.to_str()) {
        Some("txt") => "text/plain".to_string(),
        Some("md") => "text/markdown".to_string(),
        Some("json") => "application/json".to_string(),
        Some("toml") => "application/toml".to_string(),
        Some("yaml" | "yml") => "application/yaml".to_string(),
        Some("rs") => "text/x-rust".to_string(),
        Some("py") => "text/x-python".to_string(),
        Some("js") => "text/javascript".to_string(),
        Some("ts") => "text/typescript".to_string(),
        Some("html" | "htm") => "text/html".to_string(),
        Some("css") => "text/css".to_string(),
        Some("png") => "image/png".to_string(),
        Some("jpg" | "jpeg") => "image/jpeg".to_string(),
        Some("gif") => "image/gif".to_string(),
        Some("pdf") => "application/pdf".to_string(),
        Some("zip") => "application/zip".to_string(),
        Some("tar") => "application/x-tar".to_string(),
        Some("gz") => "application/gzip".to_string(),
        _ => "application/octet-stream".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mime_detection_known_extensions() {
        assert_eq!(mime_from_extension(Path::new("file.txt")), "text/plain");
        assert_eq!(mime_from_extension(Path::new("doc.pdf")), "application/pdf");
        assert_eq!(mime_from_extension(Path::new("photo.png")), "image/png");
        assert_eq!(mime_from_extension(Path::new("photo.jpg")), "image/jpeg");
        assert_eq!(mime_from_extension(Path::new("photo.jpeg")), "image/jpeg");
        assert_eq!(mime_from_extension(Path::new("code.rs")), "text/x-rust");
        assert_eq!(
            mime_from_extension(Path::new("data.json")),
            "application/json"
        );
        assert_eq!(
            mime_from_extension(Path::new("config.yaml")),
            "application/yaml"
        );
        assert_eq!(
            mime_from_extension(Path::new("config.yml")),
            "application/yaml"
        );
    }

    #[test]
    fn mime_detection_unknown_extension() {
        assert_eq!(
            mime_from_extension(Path::new("file.xyz")),
            "application/octet-stream"
        );
        assert_eq!(
            mime_from_extension(Path::new("noext")),
            "application/octet-stream"
        );
    }

    #[tokio::test]
    async fn save_received_file_sanitizes_path_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();

        // A malicious filename with path traversal
        let file = ReceivedFile {
            name: "../../../etc/passwd".to_string(),
            mime_type: "text/plain".to_string(),
            size: 4,
            data: Bytes::from("test"),
        };

        let saved_path = save_received_file(&file, workspace).await.unwrap();

        // The file should be saved inside the workspace, not at /etc/passwd
        assert!(saved_path.starts_with(workspace));
        // The filename should be just "passwd" (stripped of directory components)
        assert_eq!(saved_path.file_name().unwrap().to_str().unwrap(), "passwd");
    }

    #[tokio::test]
    async fn save_received_file_creates_workspace_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path().join("new_dir").join("nested");

        let file = ReceivedFile {
            name: "test.txt".to_string(),
            mime_type: "text/plain".to_string(),
            size: 5,
            data: Bytes::from("hello"),
        };

        let saved_path = save_received_file(&file, &workspace).await.unwrap();
        assert!(saved_path.exists());

        let content = tokio::fs::read_to_string(&saved_path).await.unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn read_file_for_sending_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("test.json");
        tokio::fs::write(&file_path, r#"{"key": "value"}"#)
            .await
            .unwrap();

        let outbound = read_file_for_sending(&file_path).await.unwrap();
        assert_eq!(outbound.name, "test.json");
        assert_eq!(outbound.mime_type, "application/json");
        assert!(outbound.caption.is_none());

        match outbound.data {
            FileData::Bytes(b) => {
                assert_eq!(String::from_utf8_lossy(&b), r#"{"key": "value"}"#);
            }
            _ => panic!("expected Bytes"),
        }
    }

    #[tokio::test]
    async fn read_file_for_sending_nonexistent_fails() {
        let result = read_file_for_sending(Path::new("/tmp/nonexistent_file_12345.txt")).await;
        assert!(result.is_err());
    }

    // ── T5b: New edge case tests ──────────────────────────────────────

    #[tokio::test]
    async fn save_zero_byte_file() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();

        let file = ReceivedFile {
            name: "empty.txt".to_string(),
            mime_type: "text/plain".to_string(),
            size: 0,
            data: Bytes::new(),
        };

        let saved_path = save_received_file(&file, workspace).await.unwrap();
        assert!(saved_path.exists());
        let content = tokio::fs::read(&saved_path).await.unwrap();
        assert!(content.is_empty());
    }

    #[tokio::test]
    async fn save_file_with_unicode_name() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();

        let file = ReceivedFile {
            name: "\u{6D4B}\u{8BD5}\u{6587}\u{4EF6}.txt".to_string(),
            mime_type: "text/plain".to_string(),
            size: 5,
            data: Bytes::from("hello"),
        };

        let saved_path = save_received_file(&file, workspace).await.unwrap();
        assert!(saved_path.exists());
        let content = tokio::fs::read_to_string(&saved_path).await.unwrap();
        assert_eq!(content, "hello");
    }

    #[tokio::test]
    async fn save_duplicate_file_overwrites() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();

        let file1 = ReceivedFile {
            name: "dup.txt".to_string(),
            mime_type: "text/plain".to_string(),
            size: 6,
            data: Bytes::from("first!"),
        };
        save_received_file(&file1, workspace).await.unwrap();

        let file2 = ReceivedFile {
            name: "dup.txt".to_string(),
            mime_type: "text/plain".to_string(),
            size: 7,
            data: Bytes::from("second!"),
        };
        let saved_path = save_received_file(&file2, workspace).await.unwrap();

        let content = tokio::fs::read_to_string(&saved_path).await.unwrap();
        assert_eq!(content, "second!");
    }

    #[tokio::test]
    async fn save_file_strips_backslash_traversal() {
        let tmp = tempfile::tempdir().unwrap();
        let workspace = tmp.path();

        // Backslash-based path traversal
        let file = ReceivedFile {
            name: "..\\..\\etc\\shadow".to_string(),
            mime_type: "text/plain".to_string(),
            size: 4,
            data: Bytes::from("test"),
        };

        let saved_path = save_received_file(&file, workspace).await.unwrap();
        assert!(saved_path.starts_with(workspace));
    }

    #[tokio::test]
    async fn read_file_for_sending_zero_byte() {
        let tmp = tempfile::tempdir().unwrap();
        let file_path = tmp.path().join("empty.txt");
        tokio::fs::write(&file_path, b"").await.unwrap();

        let outbound = read_file_for_sending(&file_path).await.unwrap();
        assert_eq!(outbound.name, "empty.txt");
        match outbound.data {
            FileData::Bytes(b) => assert!(b.is_empty()),
            _ => panic!("expected Bytes"),
        }
    }

    #[test]
    fn mime_detection_all_extensions() {
        // Cover all branches of mime_from_extension
        assert_eq!(mime_from_extension(Path::new("f.md")), "text/markdown");
        assert_eq!(mime_from_extension(Path::new("f.toml")), "application/toml");
        assert_eq!(mime_from_extension(Path::new("f.py")), "text/x-python");
        assert_eq!(mime_from_extension(Path::new("f.js")), "text/javascript");
        assert_eq!(mime_from_extension(Path::new("f.ts")), "text/typescript");
        assert_eq!(mime_from_extension(Path::new("f.html")), "text/html");
        assert_eq!(mime_from_extension(Path::new("f.htm")), "text/html");
        assert_eq!(mime_from_extension(Path::new("f.css")), "text/css");
        assert_eq!(mime_from_extension(Path::new("f.gif")), "image/gif");
        assert_eq!(mime_from_extension(Path::new("f.zip")), "application/zip");
        assert_eq!(mime_from_extension(Path::new("f.tar")), "application/x-tar");
        assert_eq!(mime_from_extension(Path::new("f.gz")), "application/gzip");
    }
}
