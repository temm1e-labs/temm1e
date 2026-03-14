use bytes::Bytes;
use serde::{Deserialize, Serialize};

/// A file received from a user via a messaging channel
#[derive(Debug, Clone)]
pub struct ReceivedFile {
    pub name: String,
    pub mime_type: String,
    pub size: usize,
    pub data: Bytes,
}

/// A file to send to a user via a messaging channel
#[derive(Debug, Clone)]
pub struct OutboundFile {
    pub name: String,
    pub mime_type: String,
    pub data: FileData,
    pub caption: Option<String>,
}

/// File data: either raw bytes or a URL to download from
#[derive(Debug, Clone)]
pub enum FileData {
    Bytes(Bytes),
    Url(String),
}

/// Metadata about a file (for storage and transfer)
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FileMetadata {
    pub name: String,
    pub mime_type: String,
    pub size: Option<usize>,
    pub content_hash: Option<String>,
}
