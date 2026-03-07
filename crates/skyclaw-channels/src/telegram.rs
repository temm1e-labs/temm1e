//! Telegram channel — uses teloxide for the Telegram Bot API.

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use tokio::sync::mpsc;

use skyclaw_core::types::config::ChannelConfig;
use skyclaw_core::types::error::SkyclawError;
use skyclaw_core::types::file::{FileData, FileMetadata, OutboundFile, ReceivedFile};
use skyclaw_core::types::message::{AttachmentRef, InboundMessage, OutboundMessage, ParseMode};
use skyclaw_core::{Channel, FileTransfer};

use teloxide::prelude::*;
use teloxide::types::{InputFile, MessageKind, MediaKind};

/// Maximum file size the Telegram Bot API supports for uploads (50 MB).
const TELEGRAM_UPLOAD_LIMIT: usize = 50 * 1024 * 1024;

/// Telegram messaging channel.
pub struct TelegramChannel {
    /// The teloxide Bot handle.
    bot: Option<Bot>,
    /// Bot token.
    token: String,
    /// Allowlist of user IDs / usernames that may interact with the bot.
    /// Empty means everyone is allowed.
    allowlist: Vec<String>,
    /// Sender used to forward inbound messages to the gateway.
    tx: mpsc::Sender<InboundMessage>,
    /// Receiver the gateway drains. Taken once via `take_receiver()`.
    rx: Option<mpsc::Receiver<InboundMessage>>,
    /// Handle to the polling dispatcher task.
    dispatcher_handle: Option<tokio::task::JoinHandle<()>>,
    /// Shutdown token for the dispatcher.
    shutdown_token: Option<teloxide::dispatching::ShutdownToken>,
}

impl TelegramChannel {
    /// Create a new Telegram channel from a `ChannelConfig`.
    pub fn new(config: &ChannelConfig) -> Result<Self, SkyclawError> {
        let token = config
            .token
            .clone()
            .ok_or_else(|| SkyclawError::Config("Telegram channel requires a bot token".into()))?;

        let (tx, rx) = mpsc::channel(256);

        Ok(Self {
            bot: None,
            token,
            allowlist: config.allowlist.clone(),
            tx,
            rx: Some(rx),
            dispatcher_handle: None,
            shutdown_token: None,
        })
    }

    /// Take the inbound message receiver. The gateway should call this once.
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<InboundMessage>> {
        self.rx.take()
    }

    /// Check if a user (by numeric ID or username) is on the allowlist.
    fn check_allowed(&self, user_id: &str, username: Option<&str>) -> bool {
        if self.allowlist.is_empty() {
            return true;
        }
        if self.allowlist.iter().any(|a| a == user_id) {
            return true;
        }
        if let Some(uname) = username {
            if self.allowlist.iter().any(|a| a == uname || a.trim_start_matches('@') == uname) {
                return true;
            }
        }
        false
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn start(&mut self) -> Result<(), SkyclawError> {
        let bot = Bot::new(&self.token);
        self.bot = Some(bot.clone());

        let tx = self.tx.clone();
        let allowlist = self.allowlist.clone();

        // Build the dispatcher
        let handler = Update::filter_message().endpoint(
            move |bot: Bot, msg: teloxide::types::Message| {
                let tx = tx.clone();
                let allowlist = allowlist.clone();
                async move {
                    if let Err(e) = handle_telegram_message(&bot, msg, &tx, &allowlist).await {
                        tracing::error!(error = %e, "Failed to handle Telegram message");
                    }
                    respond(())
                }
            },
        );

        let mut dispatcher = Dispatcher::builder(bot, handler)
            .enable_ctrlc_handler()
            .build();

        self.shutdown_token = Some(dispatcher.shutdown_token());

        let handle = tokio::spawn(async move {
            dispatcher.dispatch().await;
        });

        self.dispatcher_handle = Some(handle);
        tracing::info!("Telegram channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), SkyclawError> {
        if let Some(token) = self.shutdown_token.take() {
            let fut = token.shutdown().map_err(|_| {
                SkyclawError::Channel("Failed to send shutdown signal to Telegram dispatcher".into())
            })?;
            fut.await;
        }
        if let Some(handle) = self.dispatcher_handle.take() {
            let _ = handle.await;
        }
        tracing::info!("Telegram channel stopped");
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<(), SkyclawError> {
        let bot = self.bot.as_ref().ok_or_else(|| {
            SkyclawError::Channel("Telegram bot not started".into())
        })?;

        let chat_id: ChatId = msg.chat_id.parse::<i64>()
            .map(ChatId)
            .map_err(|_| SkyclawError::Channel(format!("Invalid chat_id: {}", msg.chat_id)))?;

        let mut request = bot.send_message(chat_id, &msg.text);

        if let Some(ref mode) = msg.parse_mode {
            request = match mode {
                ParseMode::Markdown => request.parse_mode(teloxide::types::ParseMode::MarkdownV2),
                ParseMode::Html => request.parse_mode(teloxide::types::ParseMode::Html),
                ParseMode::Plain => request,
            };
        }

        request.await.map_err(|e| {
            SkyclawError::Channel(format!("Failed to send Telegram message: {e}"))
        })?;

        Ok(())
    }

    fn file_transfer(&self) -> Option<&dyn FileTransfer> {
        Some(self)
    }

    fn is_allowed(&self, user_id: &str) -> bool {
        self.check_allowed(user_id, None)
    }
}

#[async_trait]
impl FileTransfer for TelegramChannel {
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>, SkyclawError> {
        let bot = self.bot.as_ref().ok_or_else(|| {
            SkyclawError::Channel("Telegram bot not started".into())
        })?;

        let mut files = Vec::new();

        for att in &msg.attachments {
            let file_id = teloxide::types::FileId(att.file_id.clone());
            let tg_file = bot.get_file(file_id).await.map_err(|e| {
                SkyclawError::FileTransfer(format!("Failed to get file info: {e}"))
            })?;

            // Build the download URL
            let url = format!(
                "https://api.telegram.org/file/bot{}/{}",
                self.token, tg_file.path
            );

            let response = reqwest::get(&url).await.map_err(|e| {
                SkyclawError::FileTransfer(format!("Failed to download file: {e}"))
            })?;

            let data = response.bytes().await.map_err(|e| {
                SkyclawError::FileTransfer(format!("Failed to read file bytes: {e}"))
            })?;

            let name = att
                .file_name
                .clone()
                .unwrap_or_else(|| format!("file_{}", att.file_id));

            files.push(ReceivedFile {
                name,
                mime_type: att.mime_type.clone().unwrap_or_else(|| "application/octet-stream".to_string()),
                size: data.len(),
                data,
            });
        }

        Ok(files)
    }

    async fn send_file(&self, chat_id: &str, file: OutboundFile) -> Result<(), SkyclawError> {
        let bot = self.bot.as_ref().ok_or_else(|| {
            SkyclawError::Channel("Telegram bot not started".into())
        })?;

        let tg_chat_id: ChatId = chat_id.parse::<i64>()
            .map(ChatId)
            .map_err(|_| SkyclawError::Channel(format!("Invalid chat_id: {chat_id}")))?;

        let input_file = match &file.data {
            FileData::Bytes(b) => InputFile::memory(b.to_vec()).file_name(file.name.clone()),
            FileData::Url(url) => {
                let parsed = url.parse::<url::Url>().map_err(|e| {
                    SkyclawError::FileTransfer(format!("Invalid file URL: {e}"))
                })?;
                InputFile::url(parsed).file_name(file.name.clone())
            }
        };

        let mut request = bot.send_document(tg_chat_id, input_file);
        if let Some(ref caption) = file.caption {
            request = request.caption(caption);
        }

        request.await.map_err(|e| {
            SkyclawError::FileTransfer(format!("Failed to send document: {e}"))
        })?;

        Ok(())
    }

    async fn send_file_stream(
        &self,
        _chat_id: &str,
        _stream: BoxStream<'_, Bytes>,
        metadata: FileMetadata,
    ) -> Result<(), SkyclawError> {
        // Telegram does not support streaming uploads. We could buffer the
        // entire stream, but for now we return an error — callers should use
        // `send_file` with the fully-buffered data instead.
        Err(SkyclawError::FileTransfer(
            format!(
                "Telegram does not support streaming file uploads. \
                 Buffer the file ({}) and use send_file() instead.",
                metadata.name
            ),
        ))
    }

    fn max_file_size(&self) -> usize {
        TELEGRAM_UPLOAD_LIMIT
    }
}

/// Convert and forward a teloxide Message to the gateway via the mpsc sender.
async fn handle_telegram_message(
    _bot: &Bot,
    msg: teloxide::types::Message,
    tx: &mpsc::Sender<InboundMessage>,
    allowlist: &[String],
) -> Result<(), SkyclawError> {
    let user = msg.from.as_ref();

    let user_id = user
        .map(|u| u.id.0.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let username = user.and_then(|u| u.username.clone());

    // Allowlist check
    if !allowlist.is_empty() {
        let allowed = allowlist.iter().any(|a| a == &user_id)
            || username.as_ref().map_or(false, |uname| {
                allowlist.iter().any(|a| a == uname || a.trim_start_matches('@') == uname)
            });
        if !allowed {
            tracing::warn!(user_id = %user_id, username = ?username, "Rejected message from non-allowlisted user");
            return Ok(());
        }
    }

    let chat_id = msg.chat.id.0.to_string();

    // Extract text
    let text = msg.text().map(|t| t.to_string());

    // Extract file attachments
    let attachments = extract_attachments(&msg);

    let reply_to = msg.reply_to_message().map(|r| r.id.0.to_string());

    let inbound = InboundMessage {
        id: msg.id.0.to_string(),
        channel: "telegram".to_string(),
        chat_id,
        user_id,
        username,
        text,
        attachments,
        reply_to,
        timestamp: msg.date,
    };

    tx.send(inbound).await.map_err(|_| {
        SkyclawError::Channel("Inbound message receiver dropped".into())
    })?;

    Ok(())
}

/// Extract attachment references from a Telegram message (documents, photos, etc.).
fn extract_attachments(msg: &teloxide::types::Message) -> Vec<AttachmentRef> {
    let mut attachments = Vec::new();

    if let MessageKind::Common(common) = &msg.kind {
        match &common.media_kind {
            MediaKind::Document(doc) => {
                let d = &doc.document;
                attachments.push(AttachmentRef {
                    file_id: d.file.id.0.clone(),
                    file_name: d.file_name.clone(),
                    mime_type: d.mime_type.as_ref().map(|m| m.to_string()),
                    size: Some(d.file.size as usize),
                });
            }
            MediaKind::Photo(photo) => {
                // Use the largest photo size
                if let Some(largest) = photo.photo.iter().max_by_key(|p| p.width * p.height) {
                    attachments.push(AttachmentRef {
                        file_id: largest.file.id.0.clone(),
                        file_name: Some("photo.jpg".to_string()),
                        mime_type: Some("image/jpeg".to_string()),
                        size: Some(largest.file.size as usize),
                    });
                }
            }
            MediaKind::Audio(audio) => {
                let a = &audio.audio;
                attachments.push(AttachmentRef {
                    file_id: a.file.id.0.clone(),
                    file_name: a.file_name.clone(),
                    mime_type: a.mime_type.as_ref().map(|m| m.to_string()),
                    size: Some(a.file.size as usize),
                });
            }
            MediaKind::Voice(voice) => {
                let v = &voice.voice;
                attachments.push(AttachmentRef {
                    file_id: v.file.id.0.clone(),
                    file_name: Some("voice.ogg".to_string()),
                    mime_type: v.mime_type.as_ref().map(|m| m.to_string()),
                    size: Some(v.file.size as usize),
                });
            }
            MediaKind::Video(video) => {
                let v = &video.video;
                attachments.push(AttachmentRef {
                    file_id: v.file.id.0.clone(),
                    file_name: v.file_name.clone(),
                    mime_type: v.mime_type.as_ref().map(|m| m.to_string()),
                    size: Some(v.file.size as usize),
                });
            }
            MediaKind::VideoNote(vn) => {
                let v = &vn.video_note;
                attachments.push(AttachmentRef {
                    file_id: v.file.id.0.clone(),
                    file_name: Some("video_note.mp4".to_string()),
                    mime_type: Some("video/mp4".to_string()),
                    size: Some(v.file.size as usize),
                });
            }
            _ => {}
        }
    }

    attachments
}
