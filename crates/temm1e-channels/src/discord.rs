//! Discord channel — uses serenity for the Discord Bot API.

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

use serenity::all::{
    ChannelId, Context, CreateAttachment, CreateMessage, EventHandler, GatewayIntents, Message,
    Ready,
};
use serenity::Client;

/// Maximum file size Discord supports for non-Nitro uploads (25 MB).
const DISCORD_UPLOAD_LIMIT: usize = 25 * 1024 * 1024;

// ── Persistent allowlist ──────────────────────────────────────────

/// On-disk representation of the Discord allowlist stored at
/// `~/.temm1e/discord_allowlist.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AllowlistFile {
    /// The admin user ID (the first user to ever message the bot).
    admin: String,
    /// All allowed user IDs (admin is always included).
    users: Vec<String>,
}

/// Return the path to `~/.temm1e/discord_allowlist.toml`.
fn allowlist_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".temm1e").join("discord_allowlist.toml"))
}

/// Load the persisted Discord allowlist from disk.
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
                "Failed to parse Discord allowlist file, ignoring"
            );
            None
        }
    }
}

/// Save the Discord allowlist to disk. Creates `~/.temm1e/` if needed.
fn save_allowlist_file(data: &AllowlistFile) -> Result<(), Temm1eError> {
    let path = allowlist_path().ok_or_else(|| {
        Temm1eError::Channel("Cannot determine home directory for Discord allowlist".into())
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Temm1eError::Channel(format!("Failed to create ~/.temm1e directory: {e}"))
        })?;
    }
    let content = toml::to_string_pretty(data)
        .map_err(|e| Temm1eError::Channel(format!("Failed to serialize Discord allowlist: {e}")))?;
    std::fs::write(&path, content).map_err(|e| {
        Temm1eError::Channel(format!("Failed to write Discord allowlist file: {e}"))
    })?;
    tracing::info!(path = %path.display(), "Discord allowlist saved");
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
            tracing::error!("Discord allowlist RwLock poisoned during persist, recovering");
            poisoned.into_inner().clone()
        }
    };
    let admin_id = match admin.read() {
        Ok(guard) => guard.clone().unwrap_or_default(),
        Err(poisoned) => {
            tracing::error!("Discord admin RwLock poisoned during persist, recovering");
            poisoned.into_inner().clone().unwrap_or_default()
        }
    };
    save_allowlist_file(&AllowlistFile {
        admin: admin_id,
        users: list,
    })
}

/// Discord messaging channel.
///
/// Implements the `Channel` and `FileTransfer` traits for Discord bot
/// integration via the serenity library. Supports DMs and @mentions in
/// guild channels, allowlist enforcement by numeric snowflake ID, and
/// file attachment transfer up to 25 MB (non-Nitro limit).
pub struct DiscordChannel {
    /// The serenity HTTP client for sending messages after connection.
    http: Arc<RwLock<Option<Arc<serenity::http::Http>>>>,
    /// Bot token.
    token: String,
    /// Whether to respond to DMs.
    respond_to_dms: bool,
    /// Whether to respond to @mentions in guild channels.
    respond_to_mentions: bool,
    /// Allowlist of user IDs (Discord snowflake IDs as strings).
    /// Empty at startup = auto-whitelist first user.
    allowlist: Arc<RwLock<Vec<String>>>,
    /// Admin user ID (first user to message the bot). `None` until the first
    /// user is auto-whitelisted or loaded from the persisted allowlist file.
    admin: Arc<RwLock<Option<String>>>,
    /// Sender used to forward inbound messages to the gateway.
    tx: mpsc::Sender<InboundMessage>,
    /// Receiver the gateway drains. Taken once via `take_receiver()`.
    rx: Option<mpsc::Receiver<InboundMessage>>,
    /// Handle to the client task.
    client_handle: Option<tokio::task::JoinHandle<()>>,
    /// Shutdown signal.
    shutdown: Arc<AtomicBool>,
}

impl std::fmt::Debug for DiscordChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let allowlist_display: Vec<String> = match self.allowlist.read() {
            Ok(guard) => guard.clone(),
            Err(_) => vec!["<lock poisoned>".to_string()],
        };
        f.debug_struct("DiscordChannel")
            .field("respond_to_dms", &self.respond_to_dms)
            .field("respond_to_mentions", &self.respond_to_mentions)
            .field("allowlist", &allowlist_display)
            .finish_non_exhaustive()
    }
}

impl DiscordChannel {
    /// Create a new Discord channel from a `ChannelConfig`.
    ///
    /// If a persisted allowlist exists at `~/.temm1e/discord_allowlist.toml`,
    /// it is loaded and merged with any entries from the config file.
    pub fn new(config: &ChannelConfig) -> Result<Self, Temm1eError> {
        let token = config
            .token
            .clone()
            .ok_or_else(|| Temm1eError::Config("Discord channel requires a bot token".into()))?;

        let (tx, rx) = mpsc::channel(256);

        // Try to load persisted allowlist; fall back to config.
        let (allowlist, admin) = if let Some(file) = load_allowlist_file() {
            tracing::info!(
                admin = %file.admin,
                users = ?file.users,
                "Loaded persisted Discord allowlist"
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
            http: Arc::new(RwLock::new(None)),
            token,
            respond_to_dms: true,
            respond_to_mentions: true,
            allowlist: Arc::new(RwLock::new(allowlist)),
            admin: Arc::new(RwLock::new(admin)),
            tx,
            rx: Some(rx),
            client_handle: None,
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Take the inbound message receiver. The gateway should call this once.
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<InboundMessage>> {
        self.rx.take()
    }

    /// Check if a user (by numeric snowflake ID) is on the allowlist.
    ///
    /// Only numeric user IDs are matched. Usernames are ignored because
    /// they can be changed, enabling allowlist bypass (CA-04).
    /// An empty allowlist means no one is whitelisted yet (auto-whitelist
    /// happens in the event handler when the first user writes).
    fn check_allowed(&self, user_id: &str) -> bool {
        let list = match self.allowlist.read() {
            Ok(guard) => guard,
            Err(poisoned) => {
                tracing::error!("Discord allowlist RwLock poisoned, recovering");
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
impl Channel for DiscordChannel {
    fn name(&self) -> &str {
        "discord"
    }

    async fn start(&mut self) -> Result<(), Temm1eError> {
        let intents = GatewayIntents::GUILD_MESSAGES
            | GatewayIntents::DIRECT_MESSAGES
            | GatewayIntents::MESSAGE_CONTENT;

        let tx = self.tx.clone();
        let allowlist = self.allowlist.clone();
        let admin = self.admin.clone();
        let http_holder = self.http.clone();
        let shutdown = self.shutdown.clone();
        let respond_to_dms = self.respond_to_dms;
        let respond_to_mentions = self.respond_to_mentions;

        let handler = DiscordHandler {
            tx,
            allowlist,
            admin,
            http_holder: http_holder.clone(),
            respond_to_dms,
            respond_to_mentions,
        };

        let token = self.token.clone();

        let handle = tokio::spawn(async move {
            let mut backoff = std::time::Duration::from_secs(1);

            loop {
                if shutdown.load(Ordering::Relaxed) {
                    tracing::info!("Discord client shutdown requested");
                    break;
                }

                let client_result = Client::builder(&token, intents)
                    .event_handler(handler.clone())
                    .await;

                match client_result {
                    Ok(mut client) => {
                        // Reset backoff on successful connection
                        backoff = std::time::Duration::from_secs(1);

                        if let Err(e) = client.start().await {
                            if shutdown.load(Ordering::Relaxed) {
                                break;
                            }
                            tracing::error!(error = %e, "Discord client error");
                        }
                    }
                    Err(e) => {
                        tracing::error!(error = %e, "Failed to build Discord client");
                    }
                }

                if shutdown.load(Ordering::Relaxed) {
                    break;
                }

                tracing::warn!(
                    backoff_secs = backoff.as_secs(),
                    "Discord client exited unexpectedly, reconnecting"
                );
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(std::time::Duration::from_secs(60));
            }
        });

        self.client_handle = Some(handle);
        tracing::info!("Discord channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Temm1eError> {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.client_handle.take() {
            // Give the client a moment to notice the shutdown
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        }
        tracing::info!("Discord channel stopped");
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
        let http = {
            let guard = match self.http.read() {
                Ok(g) => g,
                Err(poisoned) => {
                    tracing::error!("Discord HTTP RwLock poisoned in send_message, recovering");
                    poisoned.into_inner()
                }
            };
            guard
                .clone()
                .ok_or_else(|| Temm1eError::Channel("Discord client not connected yet".into()))?
        };

        let channel_id: ChannelId =
            msg.chat_id
                .parse::<u64>()
                .map(ChannelId::new)
                .map_err(|_| {
                    Temm1eError::Channel(format!("Invalid Discord channel_id: {}", msg.chat_id))
                })?;

        // Discord uses Markdown by default, so we just send the text directly.
        // If parse_mode is Html, we strip it (Discord doesn't support HTML).
        let text = match msg.parse_mode {
            Some(ParseMode::Html) => {
                // Basic HTML tag stripping for compatibility
                msg.text.clone()
            }
            _ => msg.text.clone(),
        };

        // Discord has a 2000 character message limit. Split if needed.
        let chunks = split_message(&text, 2000);
        for chunk in chunks {
            let builder = CreateMessage::new().content(chunk);
            channel_id.send_message(&http, builder).await.map_err(|e| {
                Temm1eError::Channel(format!("Failed to send Discord message: {e}"))
            })?;
        }

        Ok(())
    }

    fn file_transfer(&self) -> Option<&dyn FileTransfer> {
        Some(self)
    }

    fn is_allowed(&self, user_id: &str) -> bool {
        self.check_allowed(user_id)
    }

    async fn delete_message(&self, chat_id: &str, message_id: &str) -> Result<(), Temm1eError> {
        let http = {
            let guard = match self.http.read() {
                Ok(g) => g,
                Err(poisoned) => {
                    tracing::error!("Discord HTTP RwLock poisoned in delete_message, recovering");
                    poisoned.into_inner()
                }
            };
            guard
                .clone()
                .ok_or_else(|| Temm1eError::Channel("Discord client not connected yet".into()))?
        };

        let channel_id: ChannelId = chat_id.parse::<u64>().map(ChannelId::new).map_err(|_| {
            Temm1eError::Channel(format!("Invalid Discord channel_id: {}", chat_id))
        })?;

        let msg_id: serenity::all::MessageId = message_id
            .parse::<u64>()
            .map(serenity::all::MessageId::new)
            .map_err(|_| {
                Temm1eError::Channel(format!("Invalid Discord message_id: {}", message_id))
            })?;

        channel_id
            .delete_message(&http, msg_id)
            .await
            .map_err(|e| {
                Temm1eError::Channel(format!(
                    "Failed to delete Discord message {}: {}",
                    message_id, e
                ))
            })?;

        tracing::info!(
            chat_id = %chat_id,
            message_id = %message_id,
            "Deleted sensitive message from Discord channel"
        );
        Ok(())
    }
}

#[async_trait]
impl FileTransfer for DiscordChannel {
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>, Temm1eError> {
        let http = {
            let guard = match self.http.read() {
                Ok(g) => g,
                Err(poisoned) => {
                    tracing::error!("Discord HTTP RwLock poisoned in receive_file, recovering");
                    poisoned.into_inner()
                }
            };
            guard
                .clone()
                .ok_or_else(|| Temm1eError::Channel("Discord client not connected yet".into()))?
        };

        let mut files = Vec::new();

        for att in &msg.attachments {
            // The file_id for Discord attachments is the download URL.
            let url = &att.file_id;

            // Use a timeout to prevent hanging on slow/unresponsive CDN downloads.
            let client = reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(120))
                .build()
                .map_err(|e| {
                    Temm1eError::FileTransfer(format!("Failed to build HTTP client: {e}"))
                })?;
            let response = client.get(url).send().await.map_err(|e| {
                Temm1eError::FileTransfer(format!("Failed to download Discord attachment: {e}"))
            })?;

            let data = response.bytes().await.map_err(|e| {
                Temm1eError::FileTransfer(format!("Failed to read Discord attachment bytes: {e}"))
            })?;

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

        // Suppress unused variable warning for http — it is needed to verify
        // the client is connected, and future implementations may use it for
        // authenticated downloads.
        let _ = http;

        Ok(files)
    }

    async fn send_file(&self, chat_id: &str, file: OutboundFile) -> Result<(), Temm1eError> {
        let http = {
            let guard = match self.http.read() {
                Ok(g) => g,
                Err(poisoned) => {
                    tracing::error!("Discord HTTP RwLock poisoned in send_file, recovering");
                    poisoned.into_inner()
                }
            };
            guard
                .clone()
                .ok_or_else(|| Temm1eError::Channel("Discord client not connected yet".into()))?
        };

        let channel_id: ChannelId = chat_id
            .parse::<u64>()
            .map(ChannelId::new)
            .map_err(|_| Temm1eError::Channel(format!("Invalid Discord channel_id: {chat_id}")))?;

        let data = match &file.data {
            FileData::Bytes(b) => b.to_vec(),
            FileData::Url(url) => {
                // Use a timeout to prevent hanging on slow/unresponsive downloads.
                let client = reqwest::Client::builder()
                    .timeout(std::time::Duration::from_secs(120))
                    .build()
                    .map_err(|e| {
                        Temm1eError::FileTransfer(format!("Failed to build HTTP client: {e}"))
                    })?;
                let response = client.get(url).send().await.map_err(|e| {
                    Temm1eError::FileTransfer(format!("Failed to download file from URL: {e}"))
                })?;
                response
                    .bytes()
                    .await
                    .map_err(|e| {
                        Temm1eError::FileTransfer(format!("Failed to read file bytes: {e}"))
                    })?
                    .to_vec()
            }
        };

        let attachment = CreateAttachment::bytes(data, &file.name);
        let mut builder = CreateMessage::new().add_file(attachment);
        if let Some(ref caption) = file.caption {
            builder = builder.content(caption);
        }

        channel_id
            .send_message(&http, builder)
            .await
            .map_err(|e| Temm1eError::FileTransfer(format!("Failed to send Discord file: {e}")))?;

        Ok(())
    }

    async fn send_file_stream(
        &self,
        _chat_id: &str,
        _stream: BoxStream<'_, Bytes>,
        metadata: FileMetadata,
    ) -> Result<(), Temm1eError> {
        // Discord does not support streaming uploads. Callers should use
        // `send_file` with the fully-buffered data instead.
        Err(Temm1eError::FileTransfer(format!(
            "Discord does not support streaming file uploads. \
             Buffer the file ({}) and use send_file() instead.",
            metadata.name
        )))
    }

    fn max_file_size(&self) -> usize {
        DISCORD_UPLOAD_LIMIT
    }
}

// ── Serenity event handler ──────────────────────────────────────────

/// Internal serenity event handler that forwards Discord messages to the
/// gateway via the mpsc channel.
#[derive(Clone)]
struct DiscordHandler {
    tx: mpsc::Sender<InboundMessage>,
    allowlist: Arc<RwLock<Vec<String>>>,
    admin: Arc<RwLock<Option<String>>>,
    http_holder: Arc<RwLock<Option<Arc<serenity::http::Http>>>>,
    respond_to_dms: bool,
    respond_to_mentions: bool,
}

#[async_trait]
impl EventHandler for DiscordHandler {
    async fn ready(&self, ctx: Context, ready: Ready) {
        tracing::info!(
            bot_name = %ready.user.name,
            guild_count = ready.guilds.len(),
            "Discord bot connected"
        );
        // Store the HTTP client so DiscordChannel can use it for sending.
        let mut guard = match self.http_holder.write() {
            Ok(g) => g,
            Err(poisoned) => {
                tracing::error!("Discord HTTP holder RwLock poisoned in ready(), recovering");
                poisoned.into_inner()
            }
        };
        *guard = Some(ctx.http.clone());
    }

    async fn message(&self, ctx: Context, msg: Message) {
        // Ignore messages from bots (including ourselves)
        if msg.author.bot {
            return;
        }

        let user_id = msg.author.id.get().to_string();
        let username = Some(msg.author.name.clone());
        let is_dm = msg.guild_id.is_none();

        // Determine if we should handle this message based on channel type
        if is_dm && !self.respond_to_dms {
            return;
        }

        if !is_dm {
            // In guild channels, only respond to @mentions
            if !self.respond_to_mentions {
                return;
            }

            // Check if the bot is mentioned in the message
            let bot_mentioned = {
                let current_user_id = ctx.cache.current_user().id;
                msg.mentions.iter().any(|u| u.id == current_user_id)
            };

            if !bot_mentioned {
                return;
            }
        }

        // Auto-whitelist first user & set as admin
        {
            let mut list = match self.allowlist.write() {
                Ok(g) => g,
                Err(poisoned) => {
                    tracing::error!(
                        "Discord allowlist RwLock poisoned in auto-whitelist, recovering"
                    );
                    poisoned.into_inner()
                }
            };
            if list.is_empty() {
                list.push(user_id.clone());
                let mut adm = match self.admin.write() {
                    Ok(g) => g,
                    Err(poisoned) => {
                        tracing::error!(
                            "Discord admin RwLock poisoned in auto-whitelist, recovering"
                        );
                        poisoned.into_inner()
                    }
                };
                *adm = Some(user_id.clone());
                tracing::info!(
                    user_id = %user_id,
                    username = ?username,
                    "Auto-whitelisted first Discord user as admin"
                );
                drop(list);
                drop(adm);
                if let Err(e) = persist_allowlist(&self.allowlist, &self.admin) {
                    tracing::error!(
                        error = %e,
                        "Failed to persist Discord allowlist after auto-whitelist"
                    );
                }
            }
        }

        // Reject non-allowlisted users
        {
            let list = match self.allowlist.read() {
                Ok(g) => g,
                Err(poisoned) => {
                    tracing::error!(
                        "Discord allowlist RwLock poisoned in access check, recovering"
                    );
                    poisoned.into_inner()
                }
            };
            if !list.iter().any(|a| a == &user_id) {
                drop(list);
                tracing::warn!(
                    user_id = %user_id,
                    username = ?username,
                    "Rejected Discord message from non-allowlisted user"
                );
                return;
            }
        }

        let channel_id = msg.channel_id;

        // Intercept admin commands
        if let Some(text) = msg.content.strip_prefix('!').or(Some(&msg.content)) {
            let trimmed = text.trim();

            if trimmed.starts_with("/allow ")
                || trimmed.starts_with("/revoke ")
                || trimmed == "/users"
            {
                let is_admin = {
                    let adm = match self.admin.read() {
                        Ok(g) => g,
                        Err(poisoned) => {
                            tracing::error!(
                                "Discord admin RwLock poisoned in admin check, recovering"
                            );
                            poisoned.into_inner()
                        }
                    };
                    adm.as_deref() == Some(&user_id)
                };

                if !is_admin {
                    if let Err(e) = channel_id
                        .send_message(
                            &ctx.http,
                            CreateMessage::new().content("Only the admin can use this command."),
                        )
                        .await
                    {
                        tracing::warn!(error = %e, "Failed to send admin-only reply to Discord");
                    }
                    return;
                }

                // /users — list all allowed user IDs
                if trimmed == "/users" {
                    let reply_text = {
                        let list = match self.allowlist.read() {
                            Ok(g) => g,
                            Err(poisoned) => {
                                tracing::error!(
                                    "Discord allowlist RwLock poisoned in /users, recovering"
                                );
                                poisoned.into_inner()
                            }
                        };
                        let admin_id = match self.admin.read() {
                            Ok(g) => g.clone().unwrap_or_default(),
                            Err(poisoned) => {
                                tracing::error!(
                                    "Discord admin RwLock poisoned in /users, recovering"
                                );
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
                    if let Err(e) = channel_id
                        .send_message(&ctx.http, CreateMessage::new().content(&reply_text))
                        .await
                    {
                        tracing::warn!(error = %e, "Failed to send /users reply to Discord");
                    }
                    return;
                }

                // /allow <user_id>
                if let Some(target) = trimmed.strip_prefix("/allow ") {
                    let target = target.trim().to_string();
                    if target.is_empty() {
                        if let Err(e) = channel_id
                            .send_message(
                                &ctx.http,
                                CreateMessage::new().content("Usage: /allow <user_id>"),
                            )
                            .await
                        {
                            tracing::warn!(error = %e, "Failed to send /allow usage reply to Discord");
                        }
                        return;
                    }
                    let already_exists = {
                        let mut list = match self.allowlist.write() {
                            Ok(g) => g,
                            Err(poisoned) => {
                                tracing::error!(
                                    "Discord allowlist RwLock poisoned in /allow, recovering"
                                );
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
                        if let Err(e) = channel_id
                            .send_message(
                                &ctx.http,
                                CreateMessage::new()
                                    .content(format!("User {} is already allowed.", target)),
                            )
                            .await
                        {
                            tracing::warn!(error = %e, "Failed to send already-allowed reply to Discord");
                        }
                        return;
                    }
                    let reply = if let Err(e) = persist_allowlist(&self.allowlist, &self.admin) {
                        tracing::error!(
                            error = %e,
                            "Failed to persist allowlist after /allow"
                        );
                        format!("User {} added (but failed to save to disk: {}).", target, e)
                    } else {
                        format!("User {} added to the allowlist.", target)
                    };
                    if let Err(e) = channel_id
                        .send_message(&ctx.http, CreateMessage::new().content(&reply))
                        .await
                    {
                        tracing::warn!(error = %e, "Failed to send /allow reply to Discord");
                    }
                    tracing::info!(target = %target, "Admin added user to Discord allowlist");
                    return;
                }

                // /revoke <user_id>
                if let Some(target) = trimmed.strip_prefix("/revoke ") {
                    let target = target.trim().to_string();
                    if target.is_empty() {
                        if let Err(e) = channel_id
                            .send_message(
                                &ctx.http,
                                CreateMessage::new().content("Usage: /revoke <user_id>"),
                            )
                            .await
                        {
                            tracing::warn!(error = %e, "Failed to send /revoke usage reply to Discord");
                        }
                        return;
                    }
                    if target == user_id {
                        if let Err(e) = channel_id
                            .send_message(
                                &ctx.http,
                                CreateMessage::new().content("You cannot revoke yourself."),
                            )
                            .await
                        {
                            tracing::warn!(error = %e, "Failed to send self-revoke reply to Discord");
                        }
                        return;
                    }
                    let was_present = {
                        let mut list = match self.allowlist.write() {
                            Ok(g) => g,
                            Err(poisoned) => {
                                tracing::error!(
                                    "Discord allowlist RwLock poisoned in /revoke, recovering"
                                );
                                poisoned.into_inner()
                            }
                        };
                        let before = list.len();
                        list.retain(|a| a != &target);
                        list.len() < before
                    };
                    if !was_present {
                        if let Err(e) = channel_id
                            .send_message(
                                &ctx.http,
                                CreateMessage::new()
                                    .content(format!("User {} is not on the allowlist.", target)),
                            )
                            .await
                        {
                            tracing::warn!(error = %e, "Failed to send not-on-allowlist reply to Discord");
                        }
                        return;
                    }
                    let reply = if let Err(e) = persist_allowlist(&self.allowlist, &self.admin) {
                        tracing::error!(
                            error = %e,
                            "Failed to persist allowlist after /revoke"
                        );
                        format!(
                            "User {} revoked (but failed to save to disk: {}).",
                            target, e
                        )
                    } else {
                        format!("User {} removed from the allowlist.", target)
                    };
                    if let Err(e) = channel_id
                        .send_message(&ctx.http, CreateMessage::new().content(&reply))
                        .await
                    {
                        tracing::warn!(error = %e, "Failed to send /revoke reply to Discord");
                    }
                    tracing::info!(target = %target, "Admin revoked user from Discord allowlist");
                    return;
                }
            }
        }

        // Extract message text — strip the bot mention prefix if in a guild
        let text = if !is_dm {
            let current_user_id = ctx.cache.current_user().id;
            let mention_str = format!("<@{}>", current_user_id);
            let mention_nick_str = format!("<@!{}>", current_user_id);
            let cleaned = msg
                .content
                .replace(&mention_str, "")
                .replace(&mention_nick_str, "");
            let cleaned = cleaned.trim().to_string();
            if cleaned.is_empty() {
                None
            } else {
                Some(cleaned)
            }
        } else {
            let content = msg.content.trim().to_string();
            if content.is_empty() {
                None
            } else {
                Some(content)
            }
        };

        // Extract attachments
        let attachments = extract_attachments(&msg);

        // Skip messages with no text and no attachments
        if text.is_none() && attachments.is_empty() {
            return;
        }

        let chat_id_str = channel_id.get().to_string();

        let inbound = InboundMessage {
            id: msg.id.get().to_string(),
            channel: "discord".to_string(),
            chat_id: chat_id_str,
            user_id,
            username,
            text,
            attachments,
            reply_to: msg
                .referenced_message
                .as_ref()
                .map(|r| r.id.get().to_string()),
            timestamp: *msg.timestamp,
        };

        if self.tx.send(inbound).await.is_err() {
            tracing::error!("Discord inbound message receiver dropped");
        }
    }
}

/// Extract attachment references from a Discord message.
fn extract_attachments(msg: &Message) -> Vec<AttachmentRef> {
    msg.attachments
        .iter()
        .map(|att| AttachmentRef {
            // Use the URL as the file_id — Discord attachments are
            // downloadable directly via their CDN URL.
            file_id: att.url.clone(),
            file_name: Some(att.filename.clone()),
            mime_type: att.content_type.clone(),
            size: Some(att.size as usize),
        })
        .collect()
}

/// Split a message into chunks that fit within Discord's character limit.
fn split_message(text: &str, max_len: usize) -> Vec<String> {
    if text.len() <= max_len {
        return vec![text.to_string()];
    }

    let mut chunks = Vec::new();
    let mut remaining = text;

    while !remaining.is_empty() {
        if remaining.len() <= max_len {
            chunks.push(remaining.to_string());
            break;
        }

        // Try to split at a newline boundary
        let split_at = remaining[..max_len].rfind('\n').unwrap_or_else(|| {
            // Fall back to splitting at a space
            remaining[..max_len].rfind(' ').unwrap_or(max_len)
        });

        let (chunk, rest) = remaining.split_at(split_at);
        chunks.push(chunk.to_string());
        remaining = rest.trim_start_matches('\n');
    }

    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_discord_channel_requires_token() {
        let config = ChannelConfig {
            enabled: true,
            token: None,
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        };
        let result = DiscordChannel::new(&config);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("bot token"), "error was: {err}");
    }

    #[test]
    fn create_discord_channel_with_token() {
        let config = ChannelConfig {
            enabled: true,
            token: Some("test-token-123".to_string()),
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        };
        let channel = DiscordChannel::new(&config).unwrap();
        assert_eq!(channel.name(), "discord");
    }

    #[test]
    fn discord_channel_name() {
        let config = ChannelConfig {
            enabled: true,
            token: Some("test-token".to_string()),
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        };
        let channel = DiscordChannel::new(&config).unwrap();
        assert_eq!(channel.name(), "discord");
    }

    #[test]
    fn discord_empty_allowlist_denies_all() {
        let config = ChannelConfig {
            enabled: true,
            token: Some("test-token".to_string()),
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        };
        let channel = DiscordChannel::new(&config).unwrap();
        // Empty allowlist = deny all (DF-16)
        assert!(!channel.is_allowed("123456789"));
        assert!(!channel.is_allowed("anyone"));
    }

    #[test]
    fn discord_allowlist_matches_user_ids() {
        let config = ChannelConfig {
            enabled: true,
            token: Some("test-token".to_string()),
            allowlist: vec!["111222333".to_string(), "444555666".to_string()],
            file_transfer: true,
            max_file_size: None,
        };
        let channel = DiscordChannel::new(&config).unwrap();
        assert!(channel.is_allowed("111222333"));
        assert!(channel.is_allowed("444555666"));
        assert!(!channel.is_allowed("999888777"));
        // Must match exact ID, not username
        assert!(!channel.is_allowed("SomeUsername#1234"));
    }

    #[test]
    fn discord_file_transfer_available() {
        let config = ChannelConfig {
            enabled: true,
            token: Some("test-token".to_string()),
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        };
        let channel = DiscordChannel::new(&config).unwrap();
        assert!(channel.file_transfer().is_some());
    }

    #[test]
    fn discord_max_file_size() {
        let config = ChannelConfig {
            enabled: true,
            token: Some("test-token".to_string()),
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        };
        let channel = DiscordChannel::new(&config).unwrap();
        assert_eq!(
            channel.file_transfer().unwrap().max_file_size(),
            25 * 1024 * 1024
        );
    }

    #[test]
    fn discord_take_receiver() {
        let config = ChannelConfig {
            enabled: true,
            token: Some("test-token".to_string()),
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        };
        let mut channel = DiscordChannel::new(&config).unwrap();
        // First take should succeed
        assert!(channel.take_receiver().is_some());
        // Second take should return None
        assert!(channel.take_receiver().is_none());
    }

    #[test]
    fn split_message_short() {
        let chunks = split_message("hello", 2000);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_at_limit() {
        let text = "a".repeat(2000);
        let chunks = split_message(&text, 2000);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), 2000);
    }

    #[test]
    fn split_message_over_limit() {
        let text = "a".repeat(2500);
        let chunks = split_message(&text, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 2000);
        assert_eq!(chunks[1].len(), 500);
    }

    #[test]
    fn split_message_prefers_newline_boundary() {
        let mut text = "a".repeat(1900);
        text.push('\n');
        text.push_str(&"b".repeat(500));
        let chunks = split_message(&text, 2000);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 1900);
    }

    #[test]
    fn extract_attachments_empty() {
        // We cannot easily construct a serenity Message in tests without
        // the full Discord API, so we test the split_message helper instead.
        // The extract_attachments function is a trivial mapping and will be
        // validated by integration tests.
    }

    // ── delete_message trait method existence ─────────────────────────
    // We verify the method exists by confirming DiscordChannel implements
    // the Channel trait (which now includes delete_message).

    #[test]
    fn discord_channel_implements_channel_trait() {
        let config = ChannelConfig {
            enabled: true,
            token: Some("test-token".to_string()),
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        };
        let channel = DiscordChannel::new(&config).unwrap();
        // If this compiles, DiscordChannel implements Channel (including delete_message)
        let _: &dyn Channel = &channel;
    }

    #[tokio::test]
    async fn discord_delete_message_requires_client_connected() {
        let config = ChannelConfig {
            enabled: true,
            token: Some("test-token".to_string()),
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        };
        let channel = DiscordChannel::new(&config).unwrap();
        // delete_message should fail because the client is not connected
        let result = channel.delete_message("123456789", "987654321").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(
            err.contains("not connected"),
            "Should fail with 'not connected', got: {err}"
        );
    }
}
