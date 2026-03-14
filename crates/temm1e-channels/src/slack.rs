//! Slack channel — uses the Slack Web API with poll-based message retrieval.
//!
//! This channel polls Slack conversations for new messages and sends responses
//! via the `chat.postMessage` API. File transfer is supported via `files.upload`
//! (sending) and authenticated downloads from `url_private` (receiving).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use bytes::Bytes;
use chrono::{DateTime, Utc};
use futures::stream::BoxStream;
use serde::{Deserialize, Serialize};
use tokio::sync::mpsc;

use temm1e_core::types::config::ChannelConfig;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::file::{FileData, FileMetadata, OutboundFile, ReceivedFile};
use temm1e_core::types::message::{AttachmentRef, InboundMessage, OutboundMessage};
use temm1e_core::{Channel, FileTransfer};

/// Maximum message length Slack supports (approximately 4000 characters for
/// `chat.postMessage`). We use a conservative limit to account for formatting.
const SLACK_MESSAGE_LIMIT: usize = 4000;

/// Maximum file size Slack supports for uploads (1 GB for paid plans, but we
/// use a conservative 100 MB default).
const SLACK_UPLOAD_LIMIT: usize = 100 * 1024 * 1024;

/// Default polling interval for checking new messages (in milliseconds).
const DEFAULT_POLL_INTERVAL_MS: u64 = 2000;

/// Slack Web API base URL.
const SLACK_API_BASE: &str = "https://slack.com/api";

// ── Slack API response types ──────────────────────────────────────

/// Wrapper for Slack API responses.
#[derive(Debug, Deserialize)]
struct SlackApiResponse<T> {
    ok: bool,
    #[serde(default)]
    error: Option<String>,
    #[serde(flatten)]
    data: Option<T>,
}

/// Response from `conversations.list`.
#[derive(Debug, Deserialize)]
struct ConversationsListData {
    #[serde(default)]
    channels: Vec<SlackConversation>,
}

/// A Slack conversation (channel or DM).
#[derive(Debug, Deserialize)]
struct SlackConversation {
    id: String,
    #[serde(default)]
    is_im: bool,
    #[serde(default)]
    is_member: bool,
}

/// Response from `conversations.history`.
#[derive(Debug, Deserialize)]
struct ConversationsHistoryData {
    #[serde(default)]
    messages: Vec<SlackMessage>,
}

/// A Slack message from the API.
#[derive(Debug, Clone, Deserialize)]
struct SlackMessage {
    /// Unique message timestamp (also serves as an ID).
    ts: String,
    /// User ID who sent the message (absent for bot messages).
    user: Option<String>,
    /// Message text.
    #[serde(default)]
    text: String,
    /// The channel the message was posted in (populated by us, not the API).
    #[serde(skip)]
    channel: String,
    /// File attachments.
    #[serde(default)]
    files: Vec<SlackFile>,
    /// Thread timestamp (present if this is a threaded reply).
    thread_ts: Option<String>,
    /// Bot ID (present if from a bot).
    bot_id: Option<String>,
}

/// A Slack file attachment.
#[derive(Debug, Clone, Deserialize)]
#[allow(dead_code)]
struct SlackFile {
    id: String,
    name: Option<String>,
    mimetype: Option<String>,
    size: Option<usize>,
    url_private: Option<String>,
}

/// Response from `auth.test` for validating tokens.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct AuthTestData {
    user_id: Option<String>,
    bot_id: Option<String>,
}

/// Response from `chat.postMessage`.
#[derive(Debug, Deserialize)]
#[allow(dead_code)]
struct ChatPostMessageData {
    ts: Option<String>,
}

// ── Persistent allowlist ──────────────────────────────────────────

/// On-disk representation of the Slack allowlist stored at
/// `~/.temm1e/slack_allowlist.toml`.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct AllowlistFile {
    /// The admin user ID (the first user to ever message the bot).
    admin: String,
    /// All allowed user IDs (admin is always included).
    users: Vec<String>,
}

/// Return the path to `~/.temm1e/slack_allowlist.toml`.
fn allowlist_path() -> Option<std::path::PathBuf> {
    dirs::home_dir().map(|h| h.join(".temm1e").join("slack_allowlist.toml"))
}

/// Load the persisted Slack allowlist from disk.
/// Returns `None` if the file does not exist or cannot be parsed.
fn load_allowlist_file() -> Option<AllowlistFile> {
    let path = allowlist_path()?;
    let content = std::fs::read_to_string(&path).ok()?;
    toml::from_str(&content).ok()
}

/// Save the Slack allowlist to disk. Creates `~/.temm1e/` if needed.
fn save_allowlist_file(data: &AllowlistFile) -> Result<(), Temm1eError> {
    let path = allowlist_path().ok_or_else(|| {
        Temm1eError::Channel("Cannot determine home directory for Slack allowlist".into())
    })?;
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).map_err(|e| {
            Temm1eError::Channel(format!("Failed to create ~/.temm1e directory: {e}"))
        })?;
    }
    let content = toml::to_string_pretty(data)
        .map_err(|e| Temm1eError::Channel(format!("Failed to serialize Slack allowlist: {e}")))?;
    std::fs::write(&path, content)
        .map_err(|e| Temm1eError::Channel(format!("Failed to write Slack allowlist file: {e}")))?;
    tracing::info!(path = %path.display(), "Slack allowlist saved");
    Ok(())
}

/// Persist the current in-memory allowlist + admin to disk.
fn persist_allowlist(
    allowlist: &Arc<RwLock<Vec<String>>>,
    admin: &Arc<RwLock<Option<String>>>,
) -> Result<(), Temm1eError> {
    let list = allowlist
        .read()
        .unwrap_or_else(|e| {
            tracing::error!("RwLock poisoned, recovering");
            e.into_inner()
        })
        .clone();
    let admin_id = admin
        .read()
        .unwrap_or_else(|e| {
            tracing::error!("RwLock poisoned, recovering");
            e.into_inner()
        })
        .clone()
        .unwrap_or_default();
    save_allowlist_file(&AllowlistFile {
        admin: admin_id,
        users: list,
    })
}

// ── SlackChannel ──────────────────────────────────────────────────

/// Slack messaging channel.
///
/// Implements the `Channel` and `FileTransfer` traits for Slack bot
/// integration via the Slack Web API. Uses poll-based message retrieval
/// with `conversations.history`, sending via `chat.postMessage`, and
/// file transfer via `files.upload` / authenticated `url_private` downloads.
pub struct SlackChannel {
    /// Bot OAuth token for API calls.
    token: String,
    /// HTTP client for Slack API requests.
    client: reqwest::Client,
    /// Allowlist of user IDs (Slack user IDs like U12345678).
    /// Empty at startup = auto-whitelist first user.
    allowlist: Arc<RwLock<Vec<String>>>,
    /// Admin user ID (first user to message the bot). `None` until the first
    /// user is auto-whitelisted or loaded from the persisted allowlist file.
    admin: Arc<RwLock<Option<String>>>,
    /// Our own bot user ID (populated after `auth.test`).
    bot_user_id: Arc<RwLock<Option<String>>>,
    /// Sender used to forward inbound messages to the gateway.
    tx: mpsc::Sender<InboundMessage>,
    /// Receiver the gateway drains. Taken once via `take_receiver()`.
    rx: Option<mpsc::Receiver<InboundMessage>>,
    /// Handle to the polling task.
    poll_handle: Option<tokio::task::JoinHandle<()>>,
    /// Shutdown signal.
    shutdown: Arc<AtomicBool>,
}

impl std::fmt::Debug for SlackChannel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SlackChannel")
            .field(
                "allowlist",
                &*self.allowlist.read().unwrap_or_else(|e| {
                    tracing::error!("RwLock poisoned, recovering");
                    e.into_inner()
                }),
            )
            .field(
                "bot_user_id",
                &*self.bot_user_id.read().unwrap_or_else(|e| {
                    tracing::error!("RwLock poisoned, recovering");
                    e.into_inner()
                }),
            )
            .finish_non_exhaustive()
    }
}

impl SlackChannel {
    /// Create a new Slack channel from a `ChannelConfig`.
    ///
    /// If a persisted allowlist exists at `~/.temm1e/slack_allowlist.toml`,
    /// it is loaded and merged with any entries from the config file.
    pub fn new(config: &ChannelConfig) -> Result<Self, Temm1eError> {
        let token = config
            .token
            .clone()
            .ok_or_else(|| Temm1eError::Config("Slack channel requires a bot token".into()))?;

        let (tx, rx) = mpsc::channel(256);

        // Try to load persisted allowlist; fall back to config.
        let (allowlist, admin) = if let Some(file) = load_allowlist_file() {
            tracing::info!(
                admin = %file.admin,
                users = ?file.users,
                "Loaded persisted Slack allowlist"
            );
            (file.users.clone(), Some(file.admin.clone()))
        } else if !config.allowlist.is_empty() {
            // Legacy: first entry in the config allowlist becomes admin.
            let admin = config.allowlist[0].clone();
            (config.allowlist.clone(), Some(admin))
        } else {
            (Vec::new(), None)
        };

        let client = reqwest::Client::new();

        Ok(Self {
            token,
            client,
            allowlist: Arc::new(RwLock::new(allowlist)),
            admin: Arc::new(RwLock::new(admin)),
            bot_user_id: Arc::new(RwLock::new(None)),
            tx,
            rx: Some(rx),
            poll_handle: None,
            shutdown: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Take the inbound message receiver. The gateway should call this once.
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<InboundMessage>> {
        self.rx.take()
    }

    /// Check if a user (by Slack user ID) is on the allowlist.
    ///
    /// Only Slack user IDs (e.g., U12345678) are matched. Display names
    /// and usernames are ignored because they can be changed, enabling
    /// allowlist bypass (CA-04).
    /// An empty allowlist means no one is whitelisted yet (auto-whitelist
    /// happens in the polling loop when the first user writes).
    fn check_allowed(&self, user_id: &str) -> bool {
        let list = self.allowlist.read().unwrap_or_else(|e| {
            tracing::error!("RwLock poisoned, recovering");
            e.into_inner()
        });
        if list.is_empty() {
            return false; // No one whitelisted yet
        }
        list.iter().any(|a| a == user_id)
    }
}

#[async_trait]
impl Channel for SlackChannel {
    fn name(&self) -> &str {
        "slack"
    }

    async fn start(&mut self) -> Result<(), Temm1eError> {
        // Validate the bot token by calling auth.test.
        let auth_response = self
            .client
            .post(format!("{SLACK_API_BASE}/auth.test"))
            .bearer_auth(&self.token)
            .send()
            .await
            .map_err(|e| Temm1eError::Channel(format!("Failed to call Slack auth.test: {e}")))?;

        let auth_result: SlackApiResponse<AuthTestData> =
            auth_response.json().await.map_err(|e| {
                Temm1eError::Channel(format!("Failed to parse Slack auth.test response: {e}"))
            })?;

        if !auth_result.ok {
            return Err(Temm1eError::Channel(format!(
                "Slack auth.test failed: {}",
                auth_result.error.unwrap_or_else(|| "unknown error".into())
            )));
        }

        if let Some(data) = &auth_result.data {
            if let Some(ref uid) = data.user_id {
                let mut guard = self.bot_user_id.write().unwrap_or_else(|e| {
                    tracing::error!("RwLock poisoned, recovering");
                    e.into_inner()
                });
                *guard = Some(uid.clone());
                tracing::info!(bot_user_id = %uid, "Slack bot authenticated");
            }
        }

        let token = self.token.clone();
        let client = self.client.clone();
        let tx = self.tx.clone();
        let allowlist = self.allowlist.clone();
        let admin = self.admin.clone();
        let bot_user_id = self.bot_user_id.clone();
        let shutdown = self.shutdown.clone();

        let handle = tokio::spawn(async move {
            poll_slack_messages(token, client, tx, allowlist, admin, bot_user_id, shutdown).await;
        });

        self.poll_handle = Some(handle);
        tracing::info!("Slack channel started");
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Temm1eError> {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Some(handle) = self.poll_handle.take() {
            // Give the polling loop a moment to notice the shutdown.
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        }
        tracing::info!("Slack channel stopped");
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
        let chunks = split_message(&msg.text, SLACK_MESSAGE_LIMIT);

        for chunk in chunks {
            let mut body = serde_json::json!({
                "channel": msg.chat_id,
                "text": chunk,
            });

            if let Some(ref thread_ts) = msg.reply_to {
                body["thread_ts"] = serde_json::Value::String(thread_ts.clone());
            }

            let response = self
                .client
                .post(format!("{SLACK_API_BASE}/chat.postMessage"))
                .bearer_auth(&self.token)
                .json(&body)
                .send()
                .await
                .map_err(|e| Temm1eError::Channel(format!("Failed to send Slack message: {e}")))?;

            let result: SlackApiResponse<ChatPostMessageData> =
                response.json().await.map_err(|e| {
                    Temm1eError::Channel(format!(
                        "Failed to parse Slack chat.postMessage response: {e}"
                    ))
                })?;

            if !result.ok {
                return Err(Temm1eError::Channel(format!(
                    "Slack chat.postMessage failed: {}",
                    result.error.unwrap_or_else(|| "unknown error".into())
                )));
            }

            // Rate limiting: wait ~1 second between messages to avoid
            // hitting Slack's rate limits (~1 msg/sec per channel).
            tokio::time::sleep(std::time::Duration::from_millis(100)).await;
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
        let response: serde_json::Value = self
            .client
            .post(format!("{}/chat.delete", SLACK_API_BASE))
            .bearer_auth(&self.token)
            .json(&serde_json::json!({
                "channel": chat_id,
                "ts": message_id
            }))
            .send()
            .await
            .map_err(|e| Temm1eError::Channel(format!("Failed to call Slack chat.delete: {}", e)))?
            .json()
            .await
            .map_err(|e| {
                Temm1eError::Channel(format!("Failed to parse Slack chat.delete response: {}", e))
            })?;

        let ok = response
            .get("ok")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);
        if !ok {
            let error = response
                .get("error")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");
            return Err(Temm1eError::Channel(format!(
                "Slack chat.delete failed: {}",
                error
            )));
        }

        tracing::info!(
            chat_id = %chat_id,
            message_id = %message_id,
            "Deleted sensitive message from Slack channel"
        );
        Ok(())
    }
}

#[async_trait]
impl FileTransfer for SlackChannel {
    async fn receive_file(&self, msg: &InboundMessage) -> Result<Vec<ReceivedFile>, Temm1eError> {
        let mut files = Vec::new();

        for att in &msg.attachments {
            // The file_id for Slack attachments contains the url_private URL.
            let url = &att.file_id;

            let response = self
                .client
                .get(url)
                .bearer_auth(&self.token)
                .send()
                .await
                .map_err(|e| {
                    Temm1eError::FileTransfer(format!("Failed to download Slack attachment: {e}"))
                })?;

            let data = response.bytes().await.map_err(|e| {
                Temm1eError::FileTransfer(format!("Failed to read Slack attachment bytes: {e}"))
            })?;

            let name = att
                .file_name
                .clone()
                .unwrap_or_else(|| format!("file_{}", att.file_id));

            // Sanitize file name to prevent path traversal.
            let name = sanitize_filename(&name);

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
        let data = match &file.data {
            FileData::Bytes(b) => b.to_vec(),
            FileData::Url(url) => {
                let response = reqwest::get(url).await.map_err(|e| {
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

        // Use files.upload API (v1) for simplicity.
        let mut form = reqwest::multipart::Form::new()
            .text("channels", chat_id.to_string())
            .text("filename", file.name.clone());

        if let Some(ref caption) = file.caption {
            form = form.text("initial_comment", caption.clone());
        }

        let part = reqwest::multipart::Part::bytes(data)
            .file_name(file.name.clone())
            .mime_str(&file.mime_type)
            .map_err(|e| Temm1eError::FileTransfer(format!("Invalid MIME type: {e}")))?;

        form = form.part("file", part);

        let response = self
            .client
            .post(format!("{SLACK_API_BASE}/files.upload"))
            .bearer_auth(&self.token)
            .multipart(form)
            .send()
            .await
            .map_err(|e| {
                Temm1eError::FileTransfer(format!("Failed to upload file to Slack: {e}"))
            })?;

        let result: SlackApiResponse<serde_json::Value> = response.json().await.map_err(|e| {
            Temm1eError::FileTransfer(format!("Failed to parse Slack files.upload response: {e}"))
        })?;

        if !result.ok {
            return Err(Temm1eError::FileTransfer(format!(
                "Slack files.upload failed: {}",
                result.error.unwrap_or_else(|| "unknown error".into())
            )));
        }

        Ok(())
    }

    async fn send_file_stream(
        &self,
        _chat_id: &str,
        _stream: BoxStream<'_, Bytes>,
        metadata: FileMetadata,
    ) -> Result<(), Temm1eError> {
        // Slack does not support streaming uploads. Callers should use
        // `send_file` with the fully-buffered data instead.
        Err(Temm1eError::FileTransfer(format!(
            "Slack does not support streaming file uploads. \
             Buffer the file ({}) and use send_file() instead.",
            metadata.name
        )))
    }

    fn max_file_size(&self) -> usize {
        SLACK_UPLOAD_LIMIT
    }
}

// ── Polling loop ──────────────────────────────────────────────────

/// Background task that polls Slack for new messages across all conversations
/// the bot is a member of.
async fn poll_slack_messages(
    token: String,
    client: reqwest::Client,
    tx: mpsc::Sender<InboundMessage>,
    allowlist: Arc<RwLock<Vec<String>>>,
    admin: Arc<RwLock<Option<String>>>,
    bot_user_id: Arc<RwLock<Option<String>>>,
    shutdown: Arc<AtomicBool>,
) {
    let poll_interval = std::time::Duration::from_millis(DEFAULT_POLL_INTERVAL_MS);
    // Track the latest timestamp we've seen per channel to avoid re-processing.
    let mut latest_ts: std::collections::HashMap<String, String> = std::collections::HashMap::new();

    // Initialize with the current time so we don't process old messages on startup.
    let startup_ts = format!("{}.000000", Utc::now().timestamp());

    loop {
        if shutdown.load(Ordering::Relaxed) {
            tracing::info!("Slack polling loop shutdown requested");
            break;
        }

        // Fetch conversations the bot is a member of.
        let conversations = match fetch_bot_conversations(&client, &token).await {
            Ok(convos) => convos,
            Err(e) => {
                tracing::error!(error = %e, "Failed to fetch Slack conversations");
                tokio::time::sleep(poll_interval).await;
                continue;
            }
        };

        for convo in &conversations {
            if shutdown.load(Ordering::Relaxed) {
                break;
            }

            let oldest = latest_ts
                .get(&convo.id)
                .cloned()
                .unwrap_or_else(|| startup_ts.clone());

            let messages =
                match fetch_conversation_history(&client, &token, &convo.id, &oldest).await {
                    Ok(msgs) => msgs,
                    Err(e) => {
                        tracing::warn!(
                            channel_id = %convo.id,
                            error = %e,
                            "Failed to fetch Slack conversation history"
                        );
                        continue;
                    }
                };

            for mut msg in messages {
                msg.channel = convo.id.clone();

                // Skip bot messages (including our own).
                if msg.bot_id.is_some() {
                    // Update latest_ts even for bot messages so we don't re-fetch.
                    update_latest_ts(&mut latest_ts, &convo.id, &msg.ts);
                    continue;
                }

                let user_id = match &msg.user {
                    Some(uid) => uid.clone(),
                    None => {
                        update_latest_ts(&mut latest_ts, &convo.id, &msg.ts);
                        continue;
                    }
                };

                // Skip our own messages.
                {
                    let our_id = bot_user_id.read().unwrap_or_else(|e| {
                        tracing::error!("RwLock poisoned, recovering");
                        e.into_inner()
                    });
                    if our_id.as_deref() == Some(&user_id) {
                        update_latest_ts(&mut latest_ts, &convo.id, &msg.ts);
                        continue;
                    }
                }

                // Auto-whitelist first user & set as admin.
                {
                    let mut list = allowlist.write().unwrap_or_else(|e| {
                        tracing::error!("RwLock poisoned, recovering");
                        e.into_inner()
                    });
                    if list.is_empty() {
                        list.push(user_id.clone());
                        let mut adm = admin.write().unwrap_or_else(|e| {
                            tracing::error!("RwLock poisoned, recovering");
                            e.into_inner()
                        });
                        *adm = Some(user_id.clone());
                        tracing::info!(
                            user_id = %user_id,
                            "Auto-whitelisted first Slack user as admin"
                        );
                        drop(list);
                        drop(adm);
                        if let Err(e) = persist_allowlist(&allowlist, &admin) {
                            tracing::error!(
                                error = %e,
                                "Failed to persist Slack allowlist after auto-whitelist"
                            );
                        }
                    }
                }

                // Reject non-allowlisted users.
                {
                    let list = allowlist.read().unwrap_or_else(|e| {
                        tracing::error!("RwLock poisoned, recovering");
                        e.into_inner()
                    });
                    if !list.iter().any(|a| a == &user_id) {
                        drop(list);
                        tracing::warn!(
                            user_id = %user_id,
                            "Rejected Slack message from non-allowlisted user"
                        );
                        update_latest_ts(&mut latest_ts, &convo.id, &msg.ts);
                        continue;
                    }
                }

                // Skip empty messages with no files.
                let text = if msg.text.trim().is_empty() {
                    None
                } else {
                    Some(msg.text.clone())
                };
                let attachments = extract_slack_attachments(&msg);

                if text.is_none() && attachments.is_empty() {
                    update_latest_ts(&mut latest_ts, &convo.id, &msg.ts);
                    continue;
                }

                let timestamp = parse_slack_ts(&msg.ts).unwrap_or_else(Utc::now);

                let inbound = InboundMessage {
                    id: msg.ts.clone(),
                    channel: "slack".to_string(),
                    chat_id: convo.id.clone(),
                    user_id,
                    username: None, // Slack API doesn't include username in history
                    text,
                    attachments,
                    reply_to: msg.thread_ts.clone(),
                    timestamp,
                };

                if tx.send(inbound).await.is_err() {
                    tracing::error!("Slack inbound message receiver dropped");
                    return;
                }

                update_latest_ts(&mut latest_ts, &convo.id, &msg.ts);
            }
        }

        tokio::time::sleep(poll_interval).await;
    }
}

/// Fetch all conversations (channels + DMs) the bot is a member of.
async fn fetch_bot_conversations(
    client: &reqwest::Client,
    token: &str,
) -> Result<Vec<SlackConversation>, Temm1eError> {
    let response = client
        .get(format!("{SLACK_API_BASE}/conversations.list"))
        .bearer_auth(token)
        .query(&[
            ("types", "public_channel,private_channel,im,mpim"),
            ("exclude_archived", "true"),
            ("limit", "200"),
        ])
        .send()
        .await
        .map_err(|e| {
            Temm1eError::Channel(format!("Failed to call Slack conversations.list: {e}"))
        })?;

    let result: SlackApiResponse<ConversationsListData> = response.json().await.map_err(|e| {
        Temm1eError::Channel(format!(
            "Failed to parse Slack conversations.list response: {e}"
        ))
    })?;

    if !result.ok {
        return Err(Temm1eError::Channel(format!(
            "Slack conversations.list failed: {}",
            result.error.unwrap_or_else(|| "unknown error".into())
        )));
    }

    let data = result.data.unwrap_or(ConversationsListData {
        channels: Vec::new(),
    });

    // Return only conversations the bot is a member of (or DMs).
    Ok(data
        .channels
        .into_iter()
        .filter(|c| c.is_member || c.is_im)
        .collect())
}

/// Fetch new messages from a conversation since the given timestamp.
async fn fetch_conversation_history(
    client: &reqwest::Client,
    token: &str,
    channel_id: &str,
    oldest: &str,
) -> Result<Vec<SlackMessage>, Temm1eError> {
    let response = client
        .get(format!("{SLACK_API_BASE}/conversations.history"))
        .bearer_auth(token)
        .query(&[
            ("channel", channel_id),
            ("oldest", oldest),
            ("limit", "100"),
            ("inclusive", "false"),
        ])
        .send()
        .await
        .map_err(|e| {
            Temm1eError::Channel(format!("Failed to call Slack conversations.history: {e}"))
        })?;

    let result: SlackApiResponse<ConversationsHistoryData> =
        response.json().await.map_err(|e| {
            Temm1eError::Channel(format!(
                "Failed to parse Slack conversations.history response: {e}"
            ))
        })?;

    if !result.ok {
        return Err(Temm1eError::Channel(format!(
            "Slack conversations.history failed: {}",
            result.error.unwrap_or_else(|| "unknown error".into())
        )));
    }

    let data = result.data.unwrap_or(ConversationsHistoryData {
        messages: Vec::new(),
    });

    // Messages come newest-first from the API; reverse to process chronologically.
    let mut messages = data.messages;
    messages.reverse();
    Ok(messages)
}

/// Update the latest-seen timestamp for a channel.
fn update_latest_ts(
    latest_ts: &mut std::collections::HashMap<String, String>,
    channel_id: &str,
    ts: &str,
) {
    let current = latest_ts.get(channel_id);
    if current.is_none_or(|c| ts > c.as_str()) {
        latest_ts.insert(channel_id.to_string(), ts.to_string());
    }
}

/// Parse a Slack timestamp (e.g., "1234567890.123456") into a `DateTime<Utc>`.
fn parse_slack_ts(ts: &str) -> Option<DateTime<Utc>> {
    let parts: Vec<&str> = ts.split('.').collect();
    if parts.is_empty() {
        return None;
    }
    let secs: i64 = parts[0].parse().ok()?;
    let nanos: u32 = if parts.len() > 1 {
        // Slack uses 6-digit microseconds; convert to nanoseconds.
        let micros: u32 = parts[1].parse().ok()?;
        micros * 1000
    } else {
        0
    };
    DateTime::from_timestamp(secs, nanos)
}

/// Extract attachment references from a Slack message.
fn extract_slack_attachments(msg: &SlackMessage) -> Vec<AttachmentRef> {
    msg.files
        .iter()
        .filter_map(|f| {
            let url = f.url_private.as_ref()?;
            Some(AttachmentRef {
                file_id: url.clone(),
                file_name: f.name.clone(),
                mime_type: f.mimetype.clone(),
                size: f.size,
            })
        })
        .collect()
}

/// Split a message into chunks that fit within Slack's character limit.
/// Tries to split at newline boundaries first, then at spaces, then at
/// the hard limit.
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

        // Try to split at a newline boundary.
        let split_at = remaining[..max_len].rfind('\n').unwrap_or_else(|| {
            // Fall back to splitting at a space.
            remaining[..max_len].rfind(' ').unwrap_or(max_len)
        });

        let (chunk, rest) = remaining.split_at(split_at);
        chunks.push(chunk.to_string());
        remaining = rest.trim_start_matches('\n');
    }

    chunks
}

/// Sanitize a file name to prevent path traversal.
/// Strips all directory components and ensures the name is safe.
fn sanitize_filename(name: &str) -> String {
    let path = std::path::Path::new(name);
    path.file_name()
        .map(|n| n.to_string_lossy().to_string())
        .unwrap_or_else(|| "unnamed_file".to_string())
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

    // ── Construction tests ─────────────────────────────────────────

    #[test]
    fn create_slack_channel_requires_token() {
        let config = test_config(None, Vec::new());
        let result = SlackChannel::new(&config);
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("bot token"), "error was: {err}");
    }

    #[test]
    fn create_slack_channel_with_token() {
        let config = test_config(Some("xoxb-test-token-123"), Vec::new());
        let channel = SlackChannel::new(&config).unwrap();
        assert_eq!(channel.name(), "slack");
    }

    #[test]
    fn slack_channel_name() {
        let config = test_config(Some("xoxb-test"), Vec::new());
        let channel = SlackChannel::new(&config).unwrap();
        assert_eq!(channel.name(), "slack");
    }

    // ── Allowlist tests ────────────────────────────────────────────

    #[test]
    fn slack_empty_allowlist_denies_all() {
        let config = test_config(Some("xoxb-test"), Vec::new());
        let channel = SlackChannel::new(&config).unwrap();
        // Empty allowlist = deny all (DF-16).
        assert!(!channel.is_allowed("U12345678"));
        assert!(!channel.is_allowed("anyone"));
    }

    #[test]
    fn slack_allowlist_matches_user_ids() {
        let config = test_config(
            Some("xoxb-test"),
            vec!["U111222333".to_string(), "U444555666".to_string()],
        );
        let channel = SlackChannel::new(&config).unwrap();
        assert!(channel.is_allowed("U111222333"));
        assert!(channel.is_allowed("U444555666"));
        assert!(!channel.is_allowed("U999888777"));
        // Must match exact ID, not display name.
        assert!(!channel.is_allowed("SomeDisplayName"));
    }

    #[test]
    fn slack_allowlist_no_partial_match() {
        let config = test_config(Some("xoxb-test"), vec!["U12345678".to_string()]);
        let channel = SlackChannel::new(&config).unwrap();
        assert!(channel.is_allowed("U12345678"));
        // Partial match should not work.
        assert!(!channel.is_allowed("U1234567"));
        assert!(!channel.is_allowed("U123456789"));
    }

    // ── File transfer tests ────────────────────────────────────────

    #[test]
    fn slack_file_transfer_available() {
        let config = test_config(Some("xoxb-test"), Vec::new());
        let channel = SlackChannel::new(&config).unwrap();
        assert!(channel.file_transfer().is_some());
    }

    #[test]
    fn slack_max_file_size() {
        let config = test_config(Some("xoxb-test"), Vec::new());
        let channel = SlackChannel::new(&config).unwrap();
        assert_eq!(
            channel.file_transfer().unwrap().max_file_size(),
            100 * 1024 * 1024
        );
    }

    // ── Receiver tests ─────────────────────────────────────────────

    #[test]
    fn slack_take_receiver() {
        let config = test_config(Some("xoxb-test"), Vec::new());
        let mut channel = SlackChannel::new(&config).unwrap();
        // First take should succeed.
        assert!(channel.take_receiver().is_some());
        // Second take should return None.
        assert!(channel.take_receiver().is_none());
    }

    // ── Message splitting tests ────────────────────────────────────

    #[test]
    fn split_message_short() {
        let chunks = split_message("hello", SLACK_MESSAGE_LIMIT);
        assert_eq!(chunks, vec!["hello"]);
    }

    #[test]
    fn split_message_at_limit() {
        let text = "a".repeat(SLACK_MESSAGE_LIMIT);
        let chunks = split_message(&text, SLACK_MESSAGE_LIMIT);
        assert_eq!(chunks.len(), 1);
        assert_eq!(chunks[0].len(), SLACK_MESSAGE_LIMIT);
    }

    #[test]
    fn split_message_over_limit() {
        let text = "a".repeat(5000);
        let chunks = split_message(&text, SLACK_MESSAGE_LIMIT);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), SLACK_MESSAGE_LIMIT);
        assert_eq!(chunks[1].len(), 1000);
    }

    #[test]
    fn split_message_prefers_newline_boundary() {
        let mut text = "a".repeat(3900);
        text.push('\n');
        text.push_str(&"b".repeat(500));
        let chunks = split_message(&text, SLACK_MESSAGE_LIMIT);
        assert_eq!(chunks.len(), 2);
        assert_eq!(chunks[0].len(), 3900);
    }

    #[test]
    fn split_message_empty() {
        let chunks = split_message("", SLACK_MESSAGE_LIMIT);
        assert_eq!(chunks, vec![""]);
    }

    #[test]
    fn split_message_multiple_chunks() {
        // Create a message that will split into 3 chunks.
        let text = "a".repeat(SLACK_MESSAGE_LIMIT * 2 + 500);
        let chunks = split_message(&text, SLACK_MESSAGE_LIMIT);
        assert_eq!(chunks.len(), 3);
        assert_eq!(chunks[0].len(), SLACK_MESSAGE_LIMIT);
        assert_eq!(chunks[1].len(), SLACK_MESSAGE_LIMIT);
        assert_eq!(chunks[2].len(), 500);
    }

    // ── Timestamp parsing tests ────────────────────────────────────

    #[test]
    fn parse_slack_ts_valid() {
        let ts = parse_slack_ts("1234567890.123456");
        assert!(ts.is_some());
        let dt = ts.unwrap();
        assert_eq!(dt.timestamp(), 1234567890);
    }

    #[test]
    fn parse_slack_ts_no_micros() {
        let ts = parse_slack_ts("1234567890");
        assert!(ts.is_some());
        let dt = ts.unwrap();
        assert_eq!(dt.timestamp(), 1234567890);
    }

    #[test]
    fn parse_slack_ts_invalid() {
        assert!(parse_slack_ts("").is_none());
        assert!(parse_slack_ts("not-a-timestamp").is_none());
    }

    // ── Attachment extraction tests ────────────────────────────────

    #[test]
    fn extract_attachments_from_message_with_files() {
        let msg = SlackMessage {
            ts: "1234567890.123456".to_string(),
            user: Some("U12345678".to_string()),
            text: "Here is a file".to_string(),
            channel: "C12345678".to_string(),
            files: vec![
                SlackFile {
                    id: "F001".to_string(),
                    name: Some("report.pdf".to_string()),
                    mimetype: Some("application/pdf".to_string()),
                    size: Some(1024),
                    url_private: Some(
                        "https://files.slack.com/files-pri/T123/report.pdf".to_string(),
                    ),
                },
                SlackFile {
                    id: "F002".to_string(),
                    name: Some("photo.jpg".to_string()),
                    mimetype: Some("image/jpeg".to_string()),
                    size: Some(2048),
                    url_private: Some(
                        "https://files.slack.com/files-pri/T123/photo.jpg".to_string(),
                    ),
                },
            ],
            thread_ts: None,
            bot_id: None,
        };

        let attachments = extract_slack_attachments(&msg);
        assert_eq!(attachments.len(), 2);
        assert_eq!(attachments[0].file_name.as_deref(), Some("report.pdf"));
        assert_eq!(attachments[0].mime_type.as_deref(), Some("application/pdf"));
        assert_eq!(attachments[0].size, Some(1024));
        assert_eq!(attachments[1].file_name.as_deref(), Some("photo.jpg"));
    }

    #[test]
    fn extract_attachments_skips_files_without_url() {
        let msg = SlackMessage {
            ts: "1234567890.123456".to_string(),
            user: Some("U12345678".to_string()),
            text: "".to_string(),
            channel: "C12345678".to_string(),
            files: vec![SlackFile {
                id: "F001".to_string(),
                name: Some("no_url.txt".to_string()),
                mimetype: None,
                size: None,
                url_private: None,
            }],
            thread_ts: None,
            bot_id: None,
        };

        let attachments = extract_slack_attachments(&msg);
        assert!(attachments.is_empty());
    }

    #[test]
    fn extract_attachments_empty() {
        let msg = SlackMessage {
            ts: "1234567890.123456".to_string(),
            user: Some("U12345678".to_string()),
            text: "Hello".to_string(),
            channel: "C12345678".to_string(),
            files: vec![],
            thread_ts: None,
            bot_id: None,
        };

        let attachments = extract_slack_attachments(&msg);
        assert!(attachments.is_empty());
    }

    // ── Filename sanitization tests ────────────────────────────────

    #[test]
    fn sanitize_filename_strips_directory() {
        assert_eq!(sanitize_filename("../../etc/passwd"), "passwd");
        assert_eq!(sanitize_filename("/tmp/secret.txt"), "secret.txt");
        assert_eq!(sanitize_filename("normal.txt"), "normal.txt");
    }

    #[test]
    fn sanitize_filename_handles_empty() {
        // Path::new("").file_name() returns None.
        assert_eq!(sanitize_filename(""), "unnamed_file");
    }

    // ── Update latest_ts tests ─────────────────────────────────────

    #[test]
    fn update_latest_ts_inserts_new() {
        let mut map = std::collections::HashMap::new();
        update_latest_ts(&mut map, "C123", "1234567890.000001");
        assert_eq!(map.get("C123").unwrap(), "1234567890.000001");
    }

    #[test]
    fn update_latest_ts_updates_newer() {
        let mut map = std::collections::HashMap::new();
        map.insert("C123".to_string(), "1234567890.000001".to_string());
        update_latest_ts(&mut map, "C123", "1234567891.000001");
        assert_eq!(map.get("C123").unwrap(), "1234567891.000001");
    }

    #[test]
    fn update_latest_ts_ignores_older() {
        let mut map = std::collections::HashMap::new();
        map.insert("C123".to_string(), "1234567891.000001".to_string());
        update_latest_ts(&mut map, "C123", "1234567890.000001");
        assert_eq!(map.get("C123").unwrap(), "1234567891.000001");
    }

    // ── Slack API response deserialization tests ───────────────────

    #[test]
    fn deserialize_auth_test_success() {
        let json = r#"{
            "ok": true,
            "user_id": "U12345678",
            "bot_id": "B12345678"
        }"#;
        let result: SlackApiResponse<AuthTestData> = serde_json::from_str(json).unwrap();
        assert!(result.ok);
        assert!(result.error.is_none());
        let data = result.data.unwrap();
        assert_eq!(data.user_id.as_deref(), Some("U12345678"));
        assert_eq!(data.bot_id.as_deref(), Some("B12345678"));
    }

    #[test]
    fn deserialize_auth_test_failure() {
        let json = r#"{
            "ok": false,
            "error": "invalid_auth"
        }"#;
        let result: SlackApiResponse<AuthTestData> = serde_json::from_str(json).unwrap();
        assert!(!result.ok);
        assert_eq!(result.error.as_deref(), Some("invalid_auth"));
    }

    #[test]
    fn deserialize_conversations_list() {
        let json = r#"{
            "ok": true,
            "channels": [
                {
                    "id": "C12345678",
                    "is_im": false,
                    "is_member": true
                },
                {
                    "id": "D87654321",
                    "is_im": true,
                    "is_member": false
                }
            ]
        }"#;
        let result: SlackApiResponse<ConversationsListData> = serde_json::from_str(json).unwrap();
        assert!(result.ok);
        let data = result.data.unwrap();
        assert_eq!(data.channels.len(), 2);
        assert_eq!(data.channels[0].id, "C12345678");
        assert!(!data.channels[0].is_im);
        assert!(data.channels[0].is_member);
        assert_eq!(data.channels[1].id, "D87654321");
        assert!(data.channels[1].is_im);
    }

    #[test]
    fn deserialize_conversations_history() {
        let json = r#"{
            "ok": true,
            "messages": [
                {
                    "ts": "1234567890.123456",
                    "user": "U12345678",
                    "text": "Hello from Slack!",
                    "files": []
                },
                {
                    "ts": "1234567891.123456",
                    "text": "Bot message",
                    "bot_id": "B12345678",
                    "files": []
                }
            ]
        }"#;
        let result: SlackApiResponse<ConversationsHistoryData> =
            serde_json::from_str(json).unwrap();
        assert!(result.ok);
        let data = result.data.unwrap();
        assert_eq!(data.messages.len(), 2);
        assert_eq!(data.messages[0].user.as_deref(), Some("U12345678"));
        assert_eq!(data.messages[0].text, "Hello from Slack!");
        assert!(data.messages[1].bot_id.is_some());
    }

    #[test]
    fn deserialize_message_with_files() {
        let json = r#"{
            "ts": "1234567890.123456",
            "user": "U12345678",
            "text": "Here's a file",
            "files": [
                {
                    "id": "F12345678",
                    "name": "document.pdf",
                    "mimetype": "application/pdf",
                    "size": 4096,
                    "url_private": "https://files.slack.com/files-pri/T123-F123/document.pdf"
                }
            ]
        }"#;
        let msg: SlackMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.files.len(), 1);
        assert_eq!(msg.files[0].id, "F12345678");
        assert_eq!(msg.files[0].name.as_deref(), Some("document.pdf"));
        assert_eq!(msg.files[0].size, Some(4096));
        assert!(msg.files[0].url_private.is_some());
    }

    #[test]
    fn deserialize_chat_post_message_response() {
        let json = r#"{
            "ok": true,
            "ts": "1234567890.123456"
        }"#;
        let result: SlackApiResponse<ChatPostMessageData> = serde_json::from_str(json).unwrap();
        assert!(result.ok);
        let data = result.data.unwrap();
        assert_eq!(data.ts.as_deref(), Some("1234567890.123456"));
    }

    // ── Debug format test ──────────────────────────────────────────

    #[test]
    fn slack_channel_debug_does_not_leak_token() {
        let config = test_config(Some("xoxb-super-secret-token"), Vec::new());
        let channel = SlackChannel::new(&config).unwrap();
        let debug = format!("{:?}", channel);
        assert!(!debug.contains("xoxb-super-secret-token"));
        assert!(debug.contains("SlackChannel"));
    }

    // ── delete_message trait method existence ─────────────────────────
    // We verify the method exists by confirming SlackChannel implements
    // the Channel trait (which now includes delete_message).

    #[test]
    fn slack_channel_implements_channel_trait() {
        let config = test_config(Some("xoxb-test"), Vec::new());
        let channel = SlackChannel::new(&config).unwrap();
        // If this compiles, SlackChannel implements Channel (including delete_message)
        let _: &dyn Channel = &channel;
    }
}
