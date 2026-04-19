//! WhatsApp Web channel — connects to WhatsApp via the multi-device Web
//! protocol using `wa-rs`. The bot operates as a linked device on
//! the user's personal WhatsApp account.
//!
//! **Default behavior**: AllowAll — anyone who messages your number can
//! chat with the bot. This matches how WhatsApp Web works: your number
//! is a public endpoint, not a dedicated bot account.
//!
//! **WARNING**: This channel uses an unofficial protocol and may violate
//! WhatsApp's Terms of Service. Account bans are possible. Use at your
//! own risk with an expendable phone number.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, RwLock};

use async_trait::async_trait;
use bytes::Bytes;
use futures::stream::BoxStream;
use tokio::sync::mpsc;

use temm1e_core::types::config::ChannelConfig;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::file::{FileMetadata, OutboundFile, ReceivedFile};
use temm1e_core::types::message::{InboundMessage, OutboundMessage};
use temm1e_core::{Channel, FileTransfer};

use crate::whatsapp_common::{normalize_phone, safe_truncate};

// wa-rs imports (stable Rust fork of whatsapp-rust)
use wa_rs::bot::Bot;
use wa_rs::Client;
use wa_rs_core::types::events::Event;
use wa_rs_sqlite_storage::SqliteStore;
use wa_rs_tokio_transport::TokioWebSocketTransportFactory;
use wa_rs_ureq_http::UreqHttpClient;

// ── Constants ────────────────────────────────────────────────────────

/// Default DB path for WhatsApp Web session.
const DEFAULT_DB_PATH: &str = ".temm1e/whatsapp_web.db";

// ── Policy enums ─────────────────────────────────────────────────────

/// DM (direct message) policy for the WhatsApp Web channel.
///
/// Default is `AllowAll` — WhatsApp Web runs as your personal number,
/// so anyone who messages you should be able to chat with the bot.
/// Use `Allowlist` or `DenyAll` to restrict access.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DmPolicy {
    /// Only respond to allowlisted phone numbers.
    Allowlist,
    /// Respond to all DMs (default for WhatsApp Web).
    AllowAll,
    /// Do not respond to any DMs.
    DenyAll,
}

impl DmPolicy {
    fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "allowlist" => DmPolicy::Allowlist,
            "deny_all" | "denyall" => DmPolicy::DenyAll,
            _ => DmPolicy::AllowAll,
        }
    }
}

/// Group chat policy for the WhatsApp Web channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GroupPolicy {
    /// Respond to all group messages.
    Respond,
    /// Ignore all group messages (default).
    Ignore,
}

impl GroupPolicy {
    fn from_str(s: &str) -> Self {
        match s.to_lowercase().as_str() {
            "respond" => GroupPolicy::Respond,
            _ => GroupPolicy::Ignore,
        }
    }
}

// ── Channel struct ───────────────────────────────────────────────────

/// WhatsApp Web channel using the unofficial multi-device protocol.
pub struct WhatsAppWebChannel {
    dm_policy: DmPolicy,
    group_policy: GroupPolicy,
    db_path: String,
    allowlist: Arc<RwLock<Vec<String>>>,
    tx: mpsc::Sender<InboundMessage>,
    rx: Option<mpsc::Receiver<InboundMessage>>,
    shutdown: Arc<AtomicBool>,
    bot_handle: Option<tokio::task::JoinHandle<()>>,
    client: Arc<std::sync::RwLock<Option<Arc<Client>>>>,
    file_transfer_enabled: bool,
    connected: Arc<AtomicBool>,
}

impl WhatsAppWebChannel {
    /// Create a new WhatsApp Web channel.
    ///
    /// The `token` field is repurposed as a config string:
    /// `"dm_policy:group_policy:db_path"` (all optional, colon-separated).
    ///
    /// Defaults: `allow_all:ignore` — anyone can DM, groups are ignored.
    pub fn new(config: &ChannelConfig) -> Result<Self, Temm1eError> {
        let (dm_policy, group_policy, db_path) = if let Some(ref token) = config.token {
            let parts: Vec<&str> = token.splitn(3, ':').collect();
            let dm = parts
                .first()
                .map(|s| DmPolicy::from_str(s))
                .unwrap_or(DmPolicy::AllowAll);
            let group = parts
                .get(1)
                .map(|s| GroupPolicy::from_str(s))
                .unwrap_or(GroupPolicy::Ignore);
            let path = parts.get(2).map(|s| s.to_string());
            (dm, group, path)
        } else {
            (DmPolicy::AllowAll, GroupPolicy::Ignore, None)
        };

        let db_path = if let Some(ref p) = db_path {
            p.clone()
        } else {
            let home = dirs::home_dir()
                .ok_or_else(|| Temm1eError::Config("Cannot determine home directory".into()))?;
            let path = home.join(DEFAULT_DB_PATH);
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| Temm1eError::Config(format!("Failed to create dir: {e}")))?;
            }
            path.to_string_lossy().to_string()
        };

        let (tx, rx) = mpsc::channel(256);

        // For Allowlist mode, load the allowlist from config
        let allowlist = if dm_policy == DmPolicy::Allowlist && !config.allowlist.is_empty() {
            config
                .allowlist
                .iter()
                .map(|p| normalize_phone(p))
                .collect()
        } else {
            Vec::new()
        };

        Ok(Self {
            dm_policy,
            group_policy,
            db_path,
            allowlist: Arc::new(RwLock::new(allowlist)),
            tx,
            rx: Some(rx),
            shutdown: Arc::new(AtomicBool::new(false)),
            bot_handle: None,
            client: Arc::new(std::sync::RwLock::new(None)),
            file_transfer_enabled: config.file_transfer,
            connected: Arc::new(AtomicBool::new(false)),
        })
    }

    /// Take the inbound message receiver. The gateway should call this once.
    pub fn take_receiver(&mut self) -> Option<mpsc::Receiver<InboundMessage>> {
        self.rx.take()
    }

    fn check_allowed_dm(&self, user_id: &str) -> bool {
        match self.dm_policy {
            DmPolicy::DenyAll => false,
            DmPolicy::AllowAll => true,
            DmPolicy::Allowlist => {
                let list = self.allowlist.read().unwrap_or_else(|p| p.into_inner());
                if list.is_empty() {
                    return false; // Empty allowlist denies all (DF-16)
                }
                if list.iter().any(|a| a == "*") {
                    return true;
                }
                let normalized = normalize_phone(user_id);
                list.iter().any(|a| normalize_phone(a) == normalized)
            }
        }
    }
}

// ── Channel trait impl ───────────────────────────────────────────────

#[async_trait]
impl Channel for WhatsAppWebChannel {
    fn name(&self) -> &str {
        "whatsapp-web"
    }

    async fn start(&mut self) -> Result<(), Temm1eError> {
        tracing::info!(
            dm_policy = ?self.dm_policy,
            group_policy = ?self.group_policy,
            db_path = %self.db_path,
            "WhatsApp Web channel starting"
        );

        let backend = Arc::new(
            SqliteStore::new(&self.db_path)
                .await
                .map_err(|e| Temm1eError::Channel(format!("Failed to open WhatsApp DB: {e}")))?,
        );

        let tx = self.tx.clone();
        let allowlist = self.allowlist.clone();
        let connected = self.connected.clone();
        let client_slot = self.client.clone();
        let dm_policy = self.dm_policy.clone();
        let group_policy = self.group_policy.clone();

        let mut bot = Bot::builder()
            .with_backend(backend)
            .with_transport_factory(TokioWebSocketTransportFactory::new())
            .with_http_client(UreqHttpClient::new())
            .on_event(move |event, client| {
                let tx = tx.clone();
                let allowlist = allowlist.clone();
                let connected = connected.clone();
                let client_slot = client_slot.clone();
                let dm_policy = dm_policy.clone();
                let group_policy = group_policy.clone();
                async move {
                    match event {
                        Event::PairingQrCode { ref code, .. } => {
                            if let Ok(qr) = qrcode::QrCode::new(code.as_bytes()) {
                                // Save as SVG for easy scanning. Fallback to the OS temp
                                // dir (cross-platform) if the user's home dir can't be
                                // resolved — `/tmp` doesn't exist on Windows.
                                let svg_path = dirs::home_dir()
                                    .unwrap_or_else(std::env::temp_dir)
                                    .join(".temm1e")
                                    .join("whatsapp_qr.svg");
                                if let Some(parent) = svg_path.parent() {
                                    let _ = std::fs::create_dir_all(parent);
                                }
                                let image = qr
                                    .render::<qrcode::render::svg::Color>()
                                    .quiet_zone(true)
                                    .min_dimensions(400, 400)
                                    .build();
                                if let Err(e) = std::fs::write(&svg_path, &image) {
                                    tracing::warn!(error = %e, "Failed to save QR SVG");
                                }

                                // Compact terminal QR
                                let rendered = qr
                                    .render::<char>()
                                    .quiet_zone(false)
                                    .module_dimensions(1, 1)
                                    .build();
                                println!("\n{rendered}");
                                println!();
                                println!("  QR saved: {}", svg_path.display());

                                // Platform-aware open command
                                #[cfg(target_os = "macos")]
                                {
                                    println!("  Run: open {}", svg_path.display());
                                    let _ = std::process::Command::new("open")
                                        .arg(&svg_path)
                                        .spawn();
                                }
                                #[cfg(target_os = "windows")]
                                {
                                    println!("  Run: start {}", svg_path.display());
                                    let _ = std::process::Command::new("cmd")
                                        .args(["/C", "start", &svg_path.to_string_lossy()])
                                        .spawn();
                                }
                                #[cfg(target_os = "linux")]
                                {
                                    println!("  Run: xdg-open {}", svg_path.display());
                                    let _ = std::process::Command::new("xdg-open")
                                        .arg(&svg_path)
                                        .spawn();
                                }

                                println!();
                                println!(
                                    "  Scan with WhatsApp > Settings > Linked Devices > Link a Device"
                                );
                                println!();
                            }
                        }
                        Event::PairSuccess(_) => {
                            tracing::info!("WhatsApp Web paired successfully");
                        }
                        Event::Connected(_) => {
                            tracing::info!("WhatsApp Web connected");
                            connected.store(true, Ordering::Relaxed);
                            if let Ok(mut slot) = client_slot.write() {
                                *slot = Some(client.clone());
                            }
                        }
                        Event::Disconnected(ref reason) => {
                            tracing::warn!(reason = ?reason, "WhatsApp Web disconnected");
                            connected.store(false, Ordering::Relaxed);
                        }
                        Event::LoggedOut(_) => {
                            tracing::warn!("WhatsApp Web logged out — re-scan QR");
                            connected.store(false, Ordering::Relaxed);
                        }
                        Event::Message(ref msg, ref info) => {
                            // Skip own messages — prevents self-reply loops
                            if info.source.is_from_me {
                                return;
                            }

                            let is_group = info.source.is_group;

                            // Apply group policy
                            if is_group {
                                match group_policy {
                                    GroupPolicy::Ignore => return,
                                    GroupPolicy::Respond => {}
                                }
                            }

                            let sender_jid = &info.source.sender;
                            let user_id = sender_jid.user.to_string();

                            // Apply DM policy
                            if !is_group {
                                match dm_policy {
                                    DmPolicy::DenyAll => return,
                                    DmPolicy::AllowAll => {}
                                    DmPolicy::Allowlist => {
                                        let list =
                                            allowlist.read().unwrap_or_else(|p| p.into_inner());
                                        // Empty allowlist in Allowlist mode = allow all
                                        // (user explicitly chose allowlist but didn't add anyone)
                                        if !list.is_empty() {
                                            let normalized = normalize_phone(&user_id);
                                            if !list.iter().any(|a| {
                                                normalize_phone(a) == normalized || a == "*"
                                            }) {
                                                return;
                                            }
                                        }
                                    }
                                }
                            }

                            // Extract text content
                            use wa_rs::proto_helpers::MessageExt;
                            let base = msg.get_base_message();
                            let text = base
                                .text_content()
                                .map(|s| s.to_string())
                                .or_else(|| base.get_caption().map(|s| s.to_string()));

                            if text.is_none() {
                                return;
                            }

                            let chat_jid = info.source.chat.to_string();
                            let inbound = InboundMessage {
                                id: info.id.to_string(),
                                channel: "whatsapp-web".to_string(),
                                chat_id: chat_jid,
                                user_id: user_id.clone(),
                                username: Some(info.push_name.clone()),
                                text,
                                attachments: Vec::new(),
                                reply_to: None,
                                timestamp: info.timestamp,
                            };

                            if let Err(e) = tx.send(inbound).await {
                                tracing::error!(error = %e, "Failed to forward WhatsApp message");
                            }
                        }
                        _ => {}
                    }
                }
            })
            .build()
            .await
            .map_err(|e| Temm1eError::Channel(format!("Failed to build WhatsApp bot: {e}")))?;

        let handle = tokio::spawn(async move {
            match bot.run().await {
                Ok(bot_handle) => {
                    tracing::info!("WhatsApp Web bot running");
                    let _ = bot_handle.await;
                }
                Err(e) => {
                    tracing::error!(error = %e, "WhatsApp Web bot failed to start");
                }
            }
        });

        self.bot_handle = Some(handle);
        tracing::info!("WhatsApp Web channel started — scan QR code to connect");
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Temm1eError> {
        self.shutdown.store(true, Ordering::Relaxed);
        if let Ok(slot) = self.client.read() {
            if let Some(ref client) = *slot {
                drop(client.disconnect());
            }
        }
        if let Some(handle) = self.bot_handle.take() {
            handle.abort();
            let _ = tokio::time::timeout(std::time::Duration::from_secs(5), handle).await;
        }
        self.connected.store(false, Ordering::Relaxed);
        tracing::info!("WhatsApp Web channel stopped");
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
        if !self.connected.load(Ordering::Relaxed) {
            return Err(Temm1eError::Channel(
                "WhatsApp Web not connected — scan QR code first".into(),
            ));
        }

        let client = {
            let slot = self
                .client
                .read()
                .map_err(|_| Temm1eError::Channel("Client lock poisoned".into()))?;
            slot.clone()
                .ok_or_else(|| Temm1eError::Channel("WhatsApp Web client not available".into()))?
        };

        let text = safe_truncate(&msg.text, 65536);

        // Parse JID from chat_id — safely handle malformed input
        let to_jid = if let Some((user, domain)) = msg.chat_id.split_once('@') {
            wa_rs_binary::jid::Jid::new(user, domain)
        } else {
            // Plain phone number → personal chat
            wa_rs_binary::jid::Jid::new(&msg.chat_id, "s.whatsapp.net")
        };

        let wa_msg = wa_rs::wa_rs_proto::whatsapp::Message {
            conversation: Some(text.to_string()),
            ..Default::default()
        };

        client
            .send_message(to_jid, wa_msg)
            .await
            .map_err(|e| Temm1eError::Channel(format!("WhatsApp Web send failed: {e}")))?;

        Ok(())
    }

    fn is_allowed(&self, user_id: &str) -> bool {
        self.check_allowed_dm(user_id)
    }

    fn file_transfer(&self) -> Option<&dyn FileTransfer> {
        if self.file_transfer_enabled {
            Some(self)
        } else {
            None
        }
    }

    async fn delete_message(&self, _chat_id: &str, _message_id: &str) -> Result<(), Temm1eError> {
        Ok(())
    }
}

// ── FileTransfer ─────────────────────────────────────────────────────

#[async_trait]
impl FileTransfer for WhatsAppWebChannel {
    async fn receive_file(&self, _msg: &InboundMessage) -> Result<Vec<ReceivedFile>, Temm1eError> {
        // TODO: media download via client.download()
        Err(Temm1eError::FileTransfer(
            "WhatsApp Web file receive not yet implemented".into(),
        ))
    }

    async fn send_file(&self, _chat_id: &str, _file: OutboundFile) -> Result<(), Temm1eError> {
        // TODO: media upload via client.upload() + send
        Err(Temm1eError::FileTransfer(
            "WhatsApp Web file send not yet implemented".into(),
        ))
    }

    async fn send_file_stream(
        &self,
        _chat_id: &str,
        _stream: BoxStream<'_, Bytes>,
        _metadata: FileMetadata,
    ) -> Result<(), Temm1eError> {
        Err(Temm1eError::FileTransfer(
            "WhatsApp Web streaming upload not supported".into(),
        ))
    }

    fn max_file_size(&self) -> usize {
        100 * 1024 * 1024
    }
}

// ── Tests ────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    fn test_config(token: Option<&str>) -> temm1e_core::types::config::ChannelConfig {
        temm1e_core::types::config::ChannelConfig {
            enabled: true,
            token: token.map(|t| t.to_string()),
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        }
    }

    #[test]
    fn channel_name() {
        let config = test_config(None);
        let channel = WhatsAppWebChannel::new(&config).unwrap();
        assert_eq!(channel.name(), "whatsapp-web");
    }

    #[test]
    fn default_policy_is_allow_all() {
        let config = test_config(None);
        let channel = WhatsAppWebChannel::new(&config).unwrap();
        assert_eq!(channel.dm_policy, DmPolicy::AllowAll);
        assert_eq!(channel.group_policy, GroupPolicy::Ignore);
    }

    #[test]
    fn allow_all_allows_everyone() {
        let config = test_config(None);
        let channel = WhatsAppWebChannel::new(&config).unwrap();
        assert!(channel.is_allowed("15551234567"));
        assert!(channel.is_allowed("anyone"));
        assert!(channel.is_allowed("84987654321"));
    }

    #[test]
    fn parse_policies_from_token() {
        let config = test_config(Some("allowlist:respond"));
        let channel = WhatsAppWebChannel::new(&config).unwrap();
        assert_eq!(channel.dm_policy, DmPolicy::Allowlist);
        assert_eq!(channel.group_policy, GroupPolicy::Respond);
    }

    #[test]
    fn deny_all_policy() {
        let config = test_config(Some("deny_all:ignore"));
        let channel = WhatsAppWebChannel::new(&config).unwrap();
        assert!(!channel.is_allowed("15551234567"));
    }

    #[test]
    fn allowlist_with_entries() {
        let mut config = test_config(Some("allowlist:ignore"));
        config.allowlist = vec!["+15551234567".to_string()];
        let channel = WhatsAppWebChannel::new(&config).unwrap();
        assert!(channel.is_allowed("15551234567"));
        assert!(!channel.is_allowed("9999999999"));
    }

    #[test]
    fn allowlist_empty_allows_all() {
        // Allowlist mode but no entries = allow everyone
        // (user chose allowlist but didn't configure it yet)
        let config = test_config(Some("allowlist:ignore"));
        let channel = WhatsAppWebChannel::new(&config).unwrap();
        assert!(channel.is_allowed("15551234567"));
    }

    #[test]
    fn take_receiver_once() {
        let config = test_config(None);
        let mut channel = WhatsAppWebChannel::new(&config).unwrap();
        assert!(channel.take_receiver().is_some());
        assert!(channel.take_receiver().is_none());
    }

    #[test]
    fn file_transfer_available() {
        let config = test_config(None);
        let channel = WhatsAppWebChannel::new(&config).unwrap();
        assert!(channel.file_transfer().is_some());
    }

    #[test]
    fn file_transfer_disabled() {
        let mut config = test_config(None);
        config.file_transfer = false;
        let channel = WhatsAppWebChannel::new(&config).unwrap();
        assert!(channel.file_transfer().is_none());
    }

    #[test]
    fn wildcard_allowlist() {
        let mut config = test_config(Some("allowlist:ignore"));
        config.allowlist = vec!["*".to_string()];
        let channel = WhatsAppWebChannel::new(&config).unwrap();
        assert!(channel.is_allowed("anyone"));
    }
}
