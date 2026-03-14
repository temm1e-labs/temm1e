//! Telegram channel — uses teloxide for the Telegram Bot API.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use temm1e_core::types::config::ChannelConfig;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::file::{FileData, FileMetadata, OutboundFile, ReceivedFile};
use temm1e_core::types::message::{AttachmentRef, InboundMessage, OutboundMessage, ParseMode};
use temm1e_core::{Channel, FileTransfer};

use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{InputFile, MediaKind, MessageKind};

/// Maximum file size the Telegram Bot API supports for uploads (50 MB).
const TELEGRAM_UPLOAD_LIMIT: usize = 50 * 1024 * 1024;

// ── Persistent allowlist ──────────────────────────────────────────

/// On-disk representation of the allowlist stored at `~/.temm1e/allowlist.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AllowlistFile {
    /// The admin user ID (the first user to ever message the bot).
    admin: String,
    /// All allowed user IDs (admin is always included).
    users: Vec<String>,
}

/// Return the path to `~/.temm1e/allowlist.toml`.
fn allowlist_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".temm1e").join("allowlist.toml"))
}

/// Load the persisted allowlist from disk.
/// Returns `None` if the file does not exist or cannot be parsed.
fn load_allowlist_file() -> Option<AllowlistFile> {
    let path = allowlist_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    match toml::from_str(&content) {
        Ok(parsed) => Some(parsed),
        Err(e) => {
            tracing::warn!(
                path = %path.display(),
                error = %e,
                "Failed to parse allowlist file, ignoring"
            );
            None
        }
    }
}

/// Save the allowlist to disk. Creates `~/.temm1e/` if needed.
fn save_allowlist_file(data: &AllowlistFile) -> Result<(), Temm1eError> {
    let path = allowlist_path().ok_or_else(|| {
        Temm1eError::Channel("Cannot determine home directory for allowlist".into())
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Temm1eError::Channel(format!("Failed to create ~/.temm1e directory: {e}"))
        })?;
    }
    let content = toml::to_string_pretty(data)
        .map_err(|e| Temm1eError::Channel(format!("Failed to serialize allowlist: {e}")))?;
    std::fs::write(&path, content)
        .map_err(|e| Temm1eError::Channel(format!("Failed to write allowlist file: {e}")))?;
    tracing::info!(path = %path.display(), "Allowlist saved");
    Ok(())
}

/// Telegram messaging channel.
pub struct TelegramChannel {
    /// The teloxide Bot handle.
    bot: Option<Bot>,
    /// Bot token.
    token: String,
    /// Allowlist of user IDs. Empty at startup = auto-whitelist first user.
    allowlist: Arc<RwLock<Vec<String>>>,
    /// Admin user ID (first user to message the bot). `None` until the first
    /// user is auto-whitelisted or loaded from the persisted allowlist file.
    admin: Arc<RwLock<Option<String>>>,
    /// Sender used to forward inbound messages to the gateway.
    tx: mpsc::Sender<InboundMessage>,
    /// Receiver the gateway drains. Taken once via `take_receiver()`.
    rx: Option<mpsc::Receiver<InboundMessage>>,
    /// Handle to the polling dispatcher task.
    dispatcher_handle: Option<tokio::task::JoinHandle<()>>,
    /// Shutdown signal for the reconnection loop.
    shutdown: Arc<AtomicBool>,
}

impl TelegramChannel {
    /// Create a new Telegram channel from a `ChannelConfig`.
    ///
    /// If a persisted allowlist exists at `~/.temm1e/allowlist.toml`, it is
    /// loaded and merged with any entries from the config file. The admin is
    /// always the user recorded in the persisted file (or the first entry in
    /// the config allowlist, if no file exists yet).
    pub fn new(config: &ChannelConfig) -> Result<Self, Temm1eError> {
        let token = config
            .token
            .clone()
            .ok_or_else(|| Temm1eError::Config("Telegram channel requires a bot token".into()))?;

        let (tx, rx) = mpsc::channel(256);

        // Try to load persisted allowlist; fall back to config.
        let (allowlist, admin) = if let Some(file) = load_allowlist_file() {
            tracing::info!(
                admin = %file.admin,
                users = ?file.users,
                "Loaded persisted allowlist"
            );
            (file.users.clone(), Some(file.admin.clone()))
        } else if !config.allowlist.is_empty() {
            // Legacy: first entry in the config allowlist becomes admin.
            let admin = config.allowlist[0].clone();
            (config.allowlist.clone(), Some(admin))
        } else {
            (Vec::new(), None)
        };

        Ok(Self {
            bot: None,
            token,
            allowlist: Arc::new(RwLock::new(allowlist)),
            admin: Arc::new(RwLock::new(admin)),
            tx,
            rx: Some(rx),
            dispatcher_handle: None,
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Take the inbound message receiver. The gateway should call this once.
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<InboundMessage>> {
        self.rx.take()
    }

    /// Check if a user (by numeric ID) is on the allowlist.
    ///
    /// Only numeric user IDs are matched. Usernames are ignored because
    /// they can be changed, enabling allowlist bypass (CA-04).
    /// An empty allowlist means no one is whitelisted yet (auto-whitelist
    /// happens in `handle_telegram_message` when the first user writes).
    fn check_allowed(&self, user_id: &str, _username: Option<&str>) -> bool {
        let list = match self.allowlist.read() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::error!("Allowlist RwLock poisoned, recovering");
                poisoned.into_inner()
            }
        };
        if list.is_empty() {
            return false; // No one whitelisted yet
        }
        list.iter().any(|a| a == user_id)
    }
}

#[async_trait]
impl Channel for TelegramChannel {
    fn name(&self) -> &str {
        "telegram"
    }

    async fn start(&mut self) -> Result<(), Temm1eError> {
        let bot = Bot::new(&self.token);

        // Validate token by calling getMe before starting the dispatcher.
        // teloxide panics on invalid tokens during dispatch — catch it here.
        bot.get_me().await.map_err(|e| {
            Temm1eError::Channel(format!("Invalid Telegram bot token — getMe failed: {e}"))
        })?;

        self.bot = Some(bot.clone());

        let tx = self.tx.clone();
        let allowlist = self.allowlist.clone();
        let admin = self.admin.clone();
        let shutdown = self.shutdown.clone();

        let handle = tokio::spawn(async move {
            let mut backoff = std::time::Duration::from_secs(1);

            loop {
                if shutdown.load(Ordering::Relaxed) {
                    tracing::info!("Telegram dispatcher shutdown requested");
                    break;
                }

                // Rebuild handler each iteration (dispatcher takes ownership)
                let tx = tx.clone();
                let allowlist = allowlist.clone();
                let admin = admin.clone();
                let handler = Update::filter_message().endpoint(
                    move |bot: Bot, msg: teloxide::types::Message| {
                        let tx = tx.clone();
                        let allowlist = allowlist.clone();
                        let admin = admin.clone();
                        async move {
                            if let Err(e) =
                                handle_telegram_message(&bot, msg, &tx, allowlist, admin).await
                            {
                                tracing::error!(error = %e, "Failed to handle Telegram message");
                            }
                            respond(())
                        }
                    },
                );

                let mut dispatcher = Dispatcher::builder(bot.clone(), handler).build();

                dispatcher.dispatch().await;

                // Dispatcher exited — network error, API throttle, etc.
                if shutdown.load(Ordering::Relaxed) {
                    break;
                }

                tracing::warn!(
                    backoff_secs = backoff.as_secs(),
                    "Telegram dispatcher exited unexpectedly, reconnecting"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
            }
        });

        self.dispatcher_handle = Some(handle);
        tracing::info!("Telegram channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Temm1eError> {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.dispatcher_handle.take() {
            // Give the dispatcher a moment to notice the shutdown
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        }
        tracing::info!("Telegram channel stopped");
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
        let bot = self
            .bot
            .as_ref()
            .ok_or_else(|| Temm1eError::Channel("Telegram bot not started".into()))?;

        let chat_id: ChatId = msg
            .chat_id
            .parse::<i64>()
            .map(ChatId)
            .map_err(|_| Temm1eError::Channel(format!("Invalid chat_id: {}", msg.chat_id)))?;

        let mut request = bot.send_message(chat_id, &msg.text);

        if let Some(ref mode) = msg.parse_mode {
            request = match mode {
                ParseMode::Markdown => request.parse_mode(teloxide::types::ParseMode::MarkdownV2),
                ParseMode::Html => request.parse_mode(teloxide::types::ParseMode::Html),
                ParseMode::Plain => request,
            };
        }

        request
            .await
            .map_err(|e| Temm1eError::Channel(format!("Failed to send Telegram message: {e}")))?;

        Ok(())
    }

    fn file_transfer(&self) -> Option<&dyn FileTransfer> {
        Some(self)
    }

    fn is_allowed(&self, user_id: &str) -> bool {
        self.check_allowed(user_id, None)
    }

    async fn delete_message(&self, chat_id: &str, message_id: &str) -> Result<(), Temm1eError> {
        let bot = self
            .bot
            .as_ref()
            .ok_or_else(|| Temm1eError::Channel("Telegram bot not started".into()))?;

        let tg_chat_id: ChatId = chat_id
            .parse::<i64>()
            .map(ChatId)
            .map_err(|_| Temm1eError::Channel(format!("Invalid chat_id: {}", chat_id)))?;

        let msg_id: teloxide::types::MessageId =
            teloxide::types::MessageId(message_id.parse::<i32>().map_err(|_| {
                Temm1eError::Channel(format!("Invalid message_id: {}", message_id))
            })?);

        bot.delete_message(tg_chat_id, msg_id).await.map_err(|e| {
            Temm1eError::Channel(format!(
                "Failed to delete Telegram message {}: {}",
                message_id, e
            ))
        })?;

        tracing::info!(
            chat_id = %chat_id,
            message_id = %message_id,
            "Deleted sensitive message from Telegram chat"
        );
        Ok(())
    }
}

#[async_trait]
impl FileTransfer for TelegramChannel {
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>, Temm1eError> {
        let bot = self
            .bot
            .as_ref()
            .ok_or_else(|| Temm1eError::Channel("Telegram bot not started".into()))?;

        let mut files = Vec::new();

        for att in &msg.attachments {
            let file_id = teloxide::types::FileId(att.file_id.clone());
            let tg_file = bot
                .get_file(file_id)
                .await
                .map_err(|e| Temm1eError::FileTransfer(format!("Failed to get file info: {e}")))?;

            // Use teloxide's built-in download to avoid exposing the bot
            // token in a manually-constructed URL (CA-03).
            let mut buf = Vec::new();
            bot.download_file(&tg_file.path, &mut buf)
                .await
                .map_err(|e| Temm1eError::FileTransfer(format!("Failed to download file: {e}")))?;

            let data = bytes::Bytes::from(buf);

            let name = att
                .file_name
                .clone()
                .unwrap_or_else(|| format!("file_{}", att.file_id));

            files.push(ReceivedFile {
                name,
                mime_type: att
                    .mime_type
                    .clone()
                    .unwrap_or_else(|| "application/octet-stream".to_string()),
                size: data.len(),
                data,
            });
        }

        Ok(files)
    }

    async fn send_file(&self, chat_id: &str, file: OutboundFile) -> Result<(), Temm1eError> {
        let bot = self
            .bot
            .as_ref()
            .ok_or_else(|| Temm1eError::Channel("Telegram bot not started".into()))?;

        let tg_chat_id: ChatId = chat_id
            .parse::<i64>()
            .map(ChatId)
            .map_err(|_| Temm1eError::Channel(format!("Invalid chat_id: {chat_id}")))?;

        let input_file = match &file.data {
            FileData::Bytes(b) => InputFile::memory(b.to_vec()).file_name(file.name.clone()),
            FileData::Url(url) => {
                let parsed = url
                    .parse::<url::Url>()
                    .map_err(|e| Temm1eError::FileTransfer(format!("Invalid file URL: {e}")))?;
                InputFile::url(parsed).file_name(file.name.clone())
            }
        };

        let mut request = bot.send_document(tg_chat_id, input_file);
        if let Some(ref caption) = file.caption {
            request = request.caption(caption);
        }

        request
            .await
            .map_err(|e| Temm1eError::FileTransfer(format!("Failed to send document: {e}")))?;

        Ok(())
    }

    async fn send_file_stream(
        &self,
        _chat_id: &str,
        _stream: BoxStream<'_, Bytes>,
        metadata: FileMetadata,
    ) -> Result<(), Temm1eError> {
        // Telegram does not support streaming uploads. We could buffer the
        // entire stream, but for now we return an error — callers should use
        // `send_file` with the fully-buffered data instead.
        Err(Temm1eError::FileTransfer(format!(
            "Telegram does not support streaming file uploads. \
                 Buffer the file ({}) and use send_file() instead.",
            metadata.name
        )))
    }

    fn max_file_size(&self) -> usize {
        TELEGRAM_UPLOAD_LIMIT
    }
}

/// Send a plain-text reply to the originating chat via the bot.
async fn bot_reply(bot: &Bot, chat_id: ChatId, text: &str) -> Result<(), Temm1eError> {
    bot.send_message(chat_id, text)
        .await
        .map_err(|e| Temm1eError::Channel(format!("Failed to send admin-command reply: {e}")))?;
    Ok(())
}

/// Persist the current in-memory allowlist + admin to disk.
fn persist_allowlist(
    allowlist: &Arc<RwLock<Vec<String>>>,
    admin: &Arc<RwLock<Option<String>>>,
) -> Result<(), Temm1eError> {
    let list = match allowlist.read() {
        Ok(guard) => guard.clone(),
        Err(poisoned) => {
            tracing::error!("Allowlist RwLock poisoned during persist, recovering");
            poisoned.into_inner().clone()
        }
    };
    let admin_id = match admin.read() {
        Ok(guard) => guard.clone().unwrap_or_default(),
        Err(poisoned) => {
            tracing::error!("Admin RwLock poisoned during persist, recovering");
            poisoned.into_inner().clone().unwrap_or_default()
        }
    };
    save_allowlist_file(&AllowlistFile {
        admin: admin_id,
        users: list,
    })
}

/// Convert and forward a teloxide Message to the gateway via the mpsc sender.
///
/// Admin commands (`/allow`, `/revoke`, `/users`) are intercepted here and
/// handled directly — they never reach the agent.
async fn handle_telegram_message(
    bot: &Bot,
    msg: teloxide::types::Message,
    tx: &mpsc::Sender<InboundMessage>,
    allowlist: Arc<RwLock<Vec<String>>>,
    admin: Arc<RwLock<Option<String>>>,
) -> Result<(), Temm1eError> {
    let user = msg.from.as_ref();

    let user_id = user
        .map(|u| u.id.0.to_string())
        .unwrap_or_else(|| "unknown".to_string());

    let username = user.and_then(|u| u.username.clone());

    let chat_id = msg.chat.id;

    // ── Auto-whitelist first user & set as admin ──────────────────
    {
        let mut list = match allowlist.write() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::error!("Allowlist RwLock poisoned in auto-whitelist, recovering");
                poisoned.into_inner()
            }
        };
        if list.is_empty() {
            list.push(user_id.clone());
            let mut adm = match admin.write() {
                Ok(guard) => guard,
                Err(poisoned) => {
                    tracing::error!("Admin RwLock poisoned in auto-whitelist, recovering");
                    poisoned.into_inner()
                }
            };
            *adm = Some(user_id.clone());
            tracing::info!(user_id = %user_id, username = ?username, "Auto-whitelisted first user as admin");
            // Persist immediately so the admin survives a restart.
            drop(list);
            drop(adm);
            if let Err(e) = persist_allowlist(&allowlist, &admin) {
                tracing::error!(error = %e, "Failed to persist allowlist after auto-whitelist");
            }
        }
    }

    // ── Reject non-allowlisted users ─────────────────────────────
    {
        let list = match allowlist.read() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::error!("Allowlist RwLock poisoned in access check, recovering");
                poisoned.into_inner()
            }
        };
        if !list.iter().any(|a| a == &user_id) {
            drop(list);
            tracing::warn!(user_id = %user_id, username = ?username, "Rejected message from non-allowlisted user");
            return Ok(());
        }
    }

    // ── Intercept admin commands ─────────────────────────────────
    if let Some(text) = msg.text() {
        let trimmed = text.trim();

        if trimmed.starts_with("/allow ") || trimmed.starts_with("/revoke ") || trimmed == "/users"
        {
            let is_admin = {
                let adm = match admin.read() {
                    Ok(guard) => guard,
                    Err(poisoned) => {
                        tracing::error!("Admin RwLock poisoned in admin check, recovering");
                        poisoned.into_inner()
                    }
                };
                adm.as_deref() == Some(&user_id)
            };

            if !is_admin {
                bot_reply(bot, chat_id, "Only the admin can use this command.").await?;
                return Ok(());
            }

            // /users — list all allowed user IDs
            if trimmed == "/users" {
                let reply_text = {
                    let list = match allowlist.read() {
                        Ok(guard) => guard,
                        Err(poisoned) => {
                            tracing::error!("Allowlist RwLock poisoned in /users, recovering");
                            poisoned.into_inner()
                        }
                    };
                    let admin_id = match admin.read() {
                        Ok(guard) => guard.clone().unwrap_or_default(),
                        Err(poisoned) => {
                            tracing::error!("Admin RwLock poisoned in /users, recovering");
                            poisoned.into_inner().clone().unwrap_or_default()
                        }
                    };
                    if list.is_empty() {
                        "Allowlist is empty.".to_string()
                    } else {
                        let mut lines = Vec::with_capacity(list.len());
                        for uid in list.iter() {
                            if uid == &admin_id {
                                lines.push(format!("{} (admin)", uid));
                            } else {
                                lines.push(uid.clone());
                            }
                        }
                        format!("Allowed users:\n{}", lines.join("\n"))
                    }
                };
                bot_reply(bot, chat_id, &reply_text).await?;
                return Ok(());
            }

            // /allow <user_id>
            if let Some(target) = trimmed.strip_prefix("/allow ") {
                let target = target.trim().to_string();
                if target.is_empty() {
                    bot_reply(bot, chat_id, "Usage: /allow <user_id>").await?;
                    return Ok(());
                }
                let already_exists = {
                    let mut list = match allowlist.write() {
                        Ok(guard) => guard,
                        Err(poisoned) => {
                            tracing::error!("Allowlist RwLock poisoned in /allow, recovering");
                            poisoned.into_inner()
                        }
                    };
                    if list.iter().any(|a| a == &target) {
                        true
                    } else {
                        list.push(target.clone());
                        false
                    }
                };
                if already_exists {
                    bot_reply(
                        bot,
                        chat_id,
                        &format!("User {} is already allowed.", target),
                    )
                    .await?;
                    return Ok(());
                }
                if let Err(e) = persist_allowlist(&allowlist, &admin) {
                    tracing::error!(error = %e, "Failed to persist allowlist after /allow");
                    bot_reply(
                        bot,
                        chat_id,
                        &format!("User {} added (but failed to save to disk: {}).", target, e),
                    )
                    .await?;
                } else {
                    bot_reply(
                        bot,
                        chat_id,
                        &format!("User {} added to the allowlist.", target),
                    )
                    .await?;
                }
                tracing::info!(target = %target, "Admin added user to allowlist");
                return Ok(());
            }

            // /revoke <user_id>
            if let Some(target) = trimmed.strip_prefix("/revoke ") {
                let target = target.trim().to_string();
                if target.is_empty() {
                    bot_reply(bot, chat_id, "Usage: /revoke <user_id>").await?;
                    return Ok(());
                }
                // Cannot revoke self (the admin).
                if target == user_id {
                    bot_reply(bot, chat_id, "You cannot revoke yourself.").await?;
                    return Ok(());
                }
                let was_present = {
                    let mut list = match allowlist.write() {
                        Ok(guard) => guard,
                        Err(poisoned) => {
                            tracing::error!("Allowlist RwLock poisoned in /revoke, recovering");
                            poisoned.into_inner()
                        }
                    };
                    let before = list.len();
                    list.retain(|a| a != &target);
                    list.len() < before
                };
                if !was_present {
                    bot_reply(
                        bot,
                        chat_id,
                        &format!("User {} is not on the allowlist.", target),
                    )
                    .await?;
                    return Ok(());
                }
                if let Err(e) = persist_allowlist(&allowlist, &admin) {
                    tracing::error!(error = %e, "Failed to persist allowlist after /revoke");
                    bot_reply(
                        bot,
                        chat_id,
                        &format!(
                            "User {} revoked (but failed to save to disk: {}).",
                            target, e
                        ),
                    )
                    .await?;
                } else {
                    bot_reply(
                        bot,
                        chat_id,
                        &format!("User {} removed from the allowlist.", target),
                    )
                    .await?;
                }
                tracing::info!(target = %target, "Admin revoked user from allowlist");
                return Ok(());
            }
        }
    }

    // ── Forward to gateway as usual ──────────────────────────────
    let chat_id_str = chat_id.0.to_string();

    let text = msg.text().map(|t| t.to_string());
    let attachments = extract_attachments(&msg);
    let reply_to = msg.reply_to_message().map(|r| r.id.0.to_string());

    let inbound = InboundMessage {
        id: msg.id.0.to_string(),
        channel: "telegram".to_string(),
        chat_id: chat_id_str,
        user_id,
        username,
        text,
        attachments,
        reply_to,
        timestamp: msg.date,
    };

    tx.send(inbound)
        .await
        .map_err(|_| Temm1eError::Channel("Inbound message receiver dropped".into()))?;

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

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(token: Option<&str>, allowlist: Vec<String>) -> ChannelConfig {
        ChannelConfig {
            enabled: true,
            token: token.map(|t| t.to_string()),
            allowlist,
            file_transfer: true,
            max_file_size: None,
        }
    }

    #[test]
    fn create_telegram_channel_requires_token() {
        let config = test_config(None, Vec::new());
        let result = TelegramChannel::new(&config);
        assert!(result.is_err());
        let err = match result {
            Err(e) => e.to_string(),
            Ok(_) => unreachable!(),
        };
        assert!(err.contains("bot token"), "error was: {err}");
    }

    #[test]
    fn create_telegram_channel_with_token() {
        let config = test_config(Some("123456:ABC-DEF1234"), Vec::new());
        let channel = TelegramChannel::new(&config).unwrap();
        assert_eq!(channel.name(), "telegram");
    }

    #[test]
    fn telegram_channel_name() {
        let config = test_config(Some("test-token"), Vec::new());
        let channel = TelegramChannel::new(&config).unwrap();
        assert_eq!(channel.name(), "telegram");
    }

    #[test]
    fn telegram_empty_allowlist_denies_all() {
        let config = test_config(Some("test-token"), Vec::new());
        let channel = TelegramChannel::new(&config).unwrap();
        // Force empty allowlist to avoid interference from persisted file
        {
            let mut list = channel.allowlist.write().unwrap();
            list.clear();
        }
        // Empty allowlist = deny all (DF-16)
        assert!(!channel.is_allowed("123456"));
        assert!(!channel.is_allowed("anyone"));
    }

    #[test]
    fn telegram_allowlist_matches_user_ids() {
        let config = test_config(Some("test-token"), Vec::new());
        let channel = TelegramChannel::new(&config).unwrap();
        // Manually set the allowlist to avoid interference from persisted
        // allowlist file at ~/.temm1e/allowlist.toml
        {
            let mut list = channel.allowlist.write().unwrap();
            *list = vec!["111222333".to_string(), "444555666".to_string()];
        }
        assert!(channel.is_allowed("111222333"));
        assert!(channel.is_allowed("444555666"));
        assert!(!channel.is_allowed("999888777"));
        // Must match exact ID, not username
        assert!(!channel.is_allowed("SomeUsername"));
    }

    #[test]
    fn telegram_file_transfer_available() {
        let config = test_config(Some("test-token"), Vec::new());
        let channel = TelegramChannel::new(&config).unwrap();
        assert!(channel.file_transfer().is_some());
    }

    #[test]
    fn telegram_max_file_size() {
        let config = test_config(Some("test-token"), Vec::new());
        let channel = TelegramChannel::new(&config).unwrap();
        assert_eq!(
            channel.file_transfer().unwrap().max_file_size(),
            50 * 1024 * 1024
        );
    }

    #[test]
    fn telegram_take_receiver() {
        let config = test_config(Some("test-token"), Vec::new());
        let mut channel = TelegramChannel::new(&config).unwrap();
        // First take should succeed
        assert!(channel.take_receiver().is_some());
        // Second take should return None
        assert!(channel.take_receiver().is_none());
    }

    // ── delete_message trait method existence ─────────────────────────
    // We cannot call the actual Telegram API, but we verify the method
    // exists and has the correct signature by checking that the struct
    // implements Channel (which includes delete_message).

    #[test]
    fn telegram_channel_implements_channel_trait() {
        let config = test_config(Some("test-token"), Vec::new());
        let channel = TelegramChannel::new(&config).unwrap();
        // If this compiles, TelegramChannel implements Channel (including delete_message)
        let _: &dyn Channel = &channel;
    }

    #[tokio::test]
    async fn telegram_delete_message_requires_bot_started() {
        let config = test_config(Some("test-token"), Vec::new());
        let channel = TelegramChannel::new(&config).unwrap();
        // delete_message should fail because the bot is not started
        let result = channel.delete_message("123", "456").await;
        assert!(result.is_err());
        let err = match result {
            Err(e) => e.to_string(),
            Ok(_) => unreachable!(),
        };
        assert!(
            err.contains("not started"),
            "Should fail with 'not started', got: {err}"
        );
    }
}
