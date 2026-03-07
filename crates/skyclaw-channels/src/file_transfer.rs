//! Shared file transfer utilities for all channels.

use bytes::Bytes;
use std::path::{Path, PathBuf};
use skyclaw_core::types::error::SkyclawError;
use skyclaw_core::types::file::{ReceivedFile, OutboundFile, FileData};

/// Save a received file to the workspace directory.
///
/// Creates the workspace directory if it does not exist. Returns the
/// full path where the file was written.
pub async fn save_received_file(file: &ReceivedFile, workspace: &Path) -> Result<PathBuf, SkyclawError> {
    // Ensure workspace directory exists
    tokio::fs::create_dir_all(workspace).await.map_err(|e| {
        SkyclawError::FileTransfer(format!("Failed to create workspace dir: {e}"))
    })?;

    // Sanitize filename: strip directory components to prevent path traversal
    let safe_name = Path::new(&file.name)
        .file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed_file".to_string());

    let dest = workspace.join(&safe_name);

    tokio::fs::write(&dest, &file.data).await.map_err(|e| {
        SkyclawError::FileTransfer(format!("Failed to write file {safe_name}: {e}"))
    })?;

    tracing::info!(path = %dest.display(), size = file.size, "Saved received file");
    Ok(dest)
}

/// Read a local file and prepare it for sending via a channel.
pub async fn read_file_for_sending(path: &Path) -> Result<OutboundFile, SkyclawError> {
    let data = tokio::fs::read(path).await.map_err(|e| {
        SkyclawError::FileTransfer(format!("Failed to read file {}: {e}", path.display()))
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
