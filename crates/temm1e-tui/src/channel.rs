//! TuiChannel — Channel trait implementation for the TUI.
//!
//! Routes agent output through the TUI event loop instead of println.

use std::path::PathBuf;

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use tokio::sync::{mpsc, watch};

use temm1e_agent::agent_task_status::AgentTaskStatus;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::file::{FileData, FileMetadata, OutboundFile, ReceivedFile};
use temm1e_core::types::message::{InboundMessage, OutboundMessage};
use temm1e_core::{Channel, FileTransfer};

use crate::event::{AgentResponseEvent, Event, StreamChunk};

/// Channel implementation for the TUI.
///
/// Instead of printing to stdout, routes messages through the TUI event loop
/// so they can be rendered with markdown, syntax highlighting, and styling.
pub struct TuiChannel {
    /// Send user input to the agent processing loop.
    inbound_tx: mpsc::Sender<InboundMessage>,
    /// Receive user input (taken by the processing loop).
    inbound_rx: Option<mpsc::Receiver<InboundMessage>>,
    /// Send events to the TUI event loop.
    event_tx: mpsc::UnboundedSender<Event>,
    /// Agent task status watch channel.
    pub status_tx: watch::Sender<AgentTaskStatus>,
    pub status_rx: watch::Receiver<AgentTaskStatus>,
    /// Stream chunk sender for streaming responses.
    pub stream_tx: mpsc::UnboundedSender<StreamChunk>,
    /// Workspace directory for file operations.
    workspace: PathBuf,
}

impl TuiChannel {
    /// Create a new TUI channel.
    pub fn new(event_tx: mpsc::UnboundedSender<Event>, workspace: PathBuf) -> Self {
        let (inbound_tx, inbound_rx) = mpsc::channel(64);
        let (status_tx, status_rx) = watch::channel(AgentTaskStatus::default());
        let (stream_tx, _stream_rx) = mpsc::unbounded_channel();

        Self {
            inbound_tx,
            inbound_rx: Some(inbound_rx),
            event_tx,
            status_tx,
            status_rx,
            stream_tx,
            workspace,
        }
    }

    /// Take the inbound message receiver for the agent processing loop.
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<InboundMessage>> {
        self.inbound_rx.take()
    }

    /// Get a sender for submitting user input from the TUI.
    pub fn inbound_sender(&self) -> mpsc::Sender<InboundMessage> {
        self.inbound_tx.clone()
    }

    /// Get the status watch sender for passing to AgentRuntime::process_message().
    pub fn status_sender(&self) -> watch::Sender<AgentTaskStatus> {
        self.status_tx.clone()
    }

    /// Get a clone of the status watch receiver for the TUI event loop.
    pub fn status_receiver(&self) -> watch::Receiver<AgentTaskStatus> {
        self.status_rx.clone()
    }

    /// Get the stream chunk sender for streaming responses.
    pub fn stream_sender(&self) -> mpsc::UnboundedSender<StreamChunk> {
        self.stream_tx.clone()
    }
}

#[async_trait]
impl Channel for TuiChannel {
    fn name(&self) -> &str {
        "tui"
    }

    async fn start(&mut self) -> Result<(), Temm1eError> {
        // TUI manages its own lifecycle — no-op
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Temm1eError> {
        // TUI manages its own lifecycle — no-op
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
        // Route through the TUI event loop
        let _ = self.event_tx.send(Event::AgentResponse(AgentResponseEvent {
            message: msg,
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
        }));
        Ok(())
    }

    fn file_transfer(&self) -> Option<&dyn FileTransfer> {
        Some(self)
    }

    fn is_allowed(&self, _user_id: &str) -> bool {
        true // Local CLI — no access control
    }
}

#[async_trait]
impl FileTransfer for TuiChannel {
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>, Temm1eError> {
        let mut files = Vec::new();
        for att in &msg.attachments {
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
                    "TUI channel does not support URL file sending: {url}"
                )));
            }
        };
        tokio::fs::create_dir_all(&self.workspace)
            .await
            .map_err(|e| Temm1eError::FileTransfer(format!("Failed to create workspace: {e}")))?;
        tokio::fs::write(&dest, &data)
            .await
            .map_err(|e| Temm1eError::FileTransfer(format!("Failed to write file: {e}")))?;

        // Notify TUI about the saved file
        let msg = format!("[File saved: {}]", dest.display());
        let _ = self.event_tx.send(Event::AgentResponse(AgentResponseEvent {
            message: OutboundMessage {
                chat_id: "tui".to_string(),
                text: msg,
                reply_to: None,
                parse_mode: None,
            },
            input_tokens: 0,
            output_tokens: 0,
            cost_usd: 0.0,
        }));
        Ok(())
    }

    async fn send_file_stream(
        &self,
        _chat_id: &str,
        _stream: BoxStream<'_, Bytes>,
        _metadata: FileMetadata,
    ) -> Result<(), Temm1eError> {
        Err(Temm1eError::FileTransfer(
            "TUI channel does not support streaming file transfers".to_string(),
        ))
    }

    fn max_file_size(&self) -> usize {
        100 * 1024 * 1024 // 100 MB
    }
}
