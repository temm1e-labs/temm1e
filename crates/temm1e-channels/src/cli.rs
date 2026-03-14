//! CLI channel — interactive REPL over stdin/stdout.

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use std::path::PathBuf;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::mpsc;

use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::file::{FileData, FileMetadata, OutboundFile, ReceivedFile};
use temm1e_core::types::message::{AttachmentRef, InboundMessage, OutboundMessage};
use temm1e_core::{Channel, FileTransfer};

/// A channel that reads from stdin and writes to stdout, for local CLI usage.
pub struct CliChannel {
    /// Sender used by the stdin reader task to forward messages to the gateway.
    tx: mpsc::Sender<InboundMessage>,
    /// Receiver the gateway can drain to get inbound messages.
    rx: Option<mpsc::Receiver<InboundMessage>>,
    /// Handle to the background stdin reader task.
    reader_handle: Option<tokio::task::JoinHandle<()>>,
    /// Workspace directory for file operations.
    workspace: PathBuf,
}

impl CliChannel {
    /// Create a new CLI channel.
    ///
    /// `workspace` is the directory where received files are saved and from
    /// which files are read for sending.
    pub fn new(workspace: PathBuf) -> Self {
        let (tx, rx) = mpsc::channel(64);
        Self {
            tx,
            rx: Some(rx),
            reader_handle: None,
            workspace,
        }
    }

    /// Take the inbound message receiver. The gateway should call this once
    /// before calling `start()` to wire up the message pipeline.
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<InboundMessage>> {
        self.rx.take()
    }
}

#[async_trait]
impl Channel for CliChannel {
    fn name(&self) -> &str {
        "cli"
    }

    async fn start(&mut self) -> Result<(), Temm1eError> {
        let tx = self.tx.clone();

        let handle = tokio::spawn(async move {
            let stdin = tokio::io::stdin();
            let reader = BufReader::new(stdin);
            let mut lines = reader.lines();

            // Print a prompt before reading
            eprint!("temm1e> ");

            loop {
                match lines.next_line().await {
                    Ok(Some(line)) => {
                        let line = line.trim().to_string();
                        if line.is_empty() {
                            eprint!("temm1e> ");
                            continue;
                        }

                        // Check for exit commands
                        if line == "/quit" || line == "/exit" {
                            tracing::info!("CLI session ended by user");
                            break;
                        }

                        // Check if the user is sending a file path (prefixed with /file)
                        let (text, attachments) =
                            if let Some(path_str) = line.strip_prefix("/file ") {
                                let path = std::path::Path::new(path_str.trim());
                                if path.exists() {
                                    let att = AttachmentRef {
                                        file_id: path.to_string_lossy().to_string(),
                                        file_name: path
                                            .file_name()
                                            .map(|n| n.to_string_lossy().to_string()),
                                        mime_type: None,
                                        size: tokio::fs::metadata(path)
                                            .await
                                            .ok()
                                            .map(|m| m.len() as usize),
                                    };
                                    (Some(format!("[file: {}]", path.display())), vec![att])
                                } else {
                                    eprintln!("  [file not found: {}]", path.display());
                                    eprint!("temm1e> ");
                                    continue;
                                }
                            } else {
                                (Some(line.clone()), vec![])
                            };

                        let msg = InboundMessage {
                            id: uuid::Uuid::new_v4().to_string(),
                            channel: "cli".to_string(),
                            chat_id: "cli".to_string(),
                            user_id: "local".to_string(),
                            username: Some(whoami()),
                            text,
                            attachments,
                            reply_to: None,
                            timestamp: chrono::Utc::now(),
                        };

                        if tx.send(msg).await.is_err() {
                            tracing::warn!("CLI channel receiver dropped, stopping stdin reader");
                            break;
                        }
                    }
                    Ok(None) => {
                        // EOF
                        tracing::info!("stdin closed");
                        break;
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Error reading stdin");
                        break;
                    }
                }
            }
        });

        self.reader_handle = Some(handle);
        tracing::info!("CLI channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Temm1eError> {
        if let Some(handle) = self.reader_handle.take() {
            handle.abort();
        }
        tracing::info!("CLI channel stopped");
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
        // Print the response to stdout with a visual separator
        println!();
        println!("{}", msg.text);
        println!();
        eprint!("temm1e> ");
        Ok(())
    }

    fn file_transfer(&self) -> Option<&dyn FileTransfer> {
        Some(self)
    }

    fn is_allowed(&self, _user_id: &str) -> bool {
        // CLI is always local; no access control needed.
        true
    }
}

#[async_trait]
impl FileTransfer for CliChannel {
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>, Temm1eError> {
        let mut files = Vec::new();
        for att in &msg.attachments {
            // The file_id for CLI is the local file path
            let path = std::path::Path::new(&att.file_id);
            let data = tokio::fs::read(path).await.map_err(|e| {
                Temm1eError::FileTransfer(format!("Failed to read {}: {e}", path.display()))
            })?;
            let size = data.len();
            files.push(ReceivedFile {
                name: att.file_name.clone().unwrap_or_else(|| "file".to_string()),
                mime_type: att
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
                size,
                data: Bytes::from(data),
            });
        }
        Ok(files)
    }

    async fn send_file(&self, _chat_id: &str, file: OutboundFile) -> Result<(), Temm1eError> {
        let dest = self.workspace.join(&file.name);
        let data = match &file.data {
            FileData::Bytes(b) => b.clone(),
            FileData::Url(url) => {
                return Err(Temm1eError::FileTransfer(format!(
                    "CLI channel does not support URL file sending: {url}"
                )));
            }
        };
        tokio::fs::create_dir_all(&self.workspace)
            .await
            .map_err(|e| Temm1eError::FileTransfer(format!("Failed to create workspace: {e}")))?;
        tokio::fs::write(&dest, &data)
            .await
            .map_err(|e| Temm1eError::FileTransfer(format!("Failed to write file: {e}")))?;

        if let Some(caption) = &file.caption {
            println!("  [file saved: {} — {}]", dest.display(), caption);
        } else {
            println!("  [file saved: {}]", dest.display());
        }
        Ok(())
    }

    async fn send_file_stream(
        &self,
        _chat_id: &str,
        _stream: BoxStream<'_, Bytes>,
        _metadata: FileMetadata,
    ) -> Result<(), Temm1eError> {
        Err(Temm1eError::FileTransfer(
            "CLI channel does not support streaming file transfers".to_string(),
        ))
    }

    fn max_file_size(&self) -> usize {
        // 100 MB — local files, practically unlimited
        100 * 1024 * 1024
    }
}

/// Get the current OS username, best-effort.
fn whoami() -> String {
    std::env::var("USER")
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".to_string())
}
