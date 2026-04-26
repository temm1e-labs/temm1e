//! SystemNotifier — generalized owner-event delivery.
//!
//! Sends [`SystemEvent`]s (startup, shutdown, etc.) to a configured set of
//! recipients via direct `Channel.send_message`. Distinct from the heartbeat
//! runner, which injects synthetic `InboundMessage`s into the agent loop —
//! system events are pre-formatted facts, no LLM round-trip needed.
//!
//! Wired in `src/main.rs` at three sites:
//!   1. After `channel_map` is built — construct if `notifications.enabled`.
//!   2. After gateway task spawn, before `ctrl_c` block — fire `Startup`.
//!   3. After `ctrl_c` wakes, before `drop(msg_tx)` — fire `Shutdown`.
//!
//! Daemon re-exec note: parent exits before reaching the gateway-spawn site,
//! so only the child fires `Startup`. No double-firing.
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use temm1e_core::types::config::{HeartbeatConfig, NotificationsConfig};
use temm1e_core::types::message::{OutboundMessage, ParseMode};
use temm1e_core::types::system_event::SystemEvent;
use temm1e_core::Channel;

/// Resolved recipient: a channel name + chat ID pair.
#[derive(Debug, Clone)]
pub struct NotifierRecipient {
    pub channel: String,
    pub chat_id: String,
}

/// Resolve the effective recipient list from config + legacy fallback.
///
/// Resolution order:
///   1. `notifications.recipients` non-empty → use it as-is.
///   2. Empty list AND `heartbeat.report_to = Some(non-empty)` AND
///      `primary_channel_name = Some(name)` → derive a single recipient
///      `{channel: name, chat_id: report_to}` (kimptoc-compat for GH-41).
///   3. Otherwise → empty Vec.
///
/// The `notifications.enabled` flag is checked *outside* this function — it
/// short-circuits construction in `main.rs` so this remains a pure mapping.
pub fn resolve_recipients(
    notif: &NotificationsConfig,
    heartbeat: &HeartbeatConfig,
    primary_channel_name: Option<&str>,
) -> Vec<NotifierRecipient> {
    if !notif.recipients.is_empty() {
        return notif
            .recipients
            .iter()
            .map(|r| NotifierRecipient {
                channel: r.channel.clone(),
                chat_id: r.chat_id.clone(),
            })
            .collect();
    }
    match (heartbeat.report_to.as_deref(), primary_channel_name) {
        (Some(chat_id), Some(channel)) if !chat_id.is_empty() => {
            tracing::info!(
                channel = %channel,
                chat_id = %chat_id,
                "SystemNotifier: deriving single recipient from heartbeat.report_to (GH-41 fallback)"
            );
            vec![NotifierRecipient {
                channel: channel.to_string(),
                chat_id: chat_id.to_string(),
            }]
        }
        _ => Vec::new(),
    }
}

/// Sends [`SystemEvent`]s to all configured recipients. Non-fatal — failures
/// are logged and dropped, never propagated.
#[derive(Clone)]
pub struct SystemNotifier {
    recipients: Vec<NotifierRecipient>,
    channels: Arc<HashMap<String, Arc<dyn Channel>>>,
}

impl SystemNotifier {
    pub fn new(
        recipients: Vec<NotifierRecipient>,
        channels: Arc<HashMap<String, Arc<dyn Channel>>>,
    ) -> Self {
        Self {
            recipients,
            channels,
        }
    }

    /// Number of resolved recipients. Useful for the startup log line.
    pub fn recipient_count(&self) -> usize {
        self.recipients.len()
    }

    /// Send the event to every recipient. Logs warnings for unknown channels
    /// or send failures; never returns Err. Caller may wrap in
    /// `tokio::time::timeout` for shutdown bounds.
    pub async fn notify(&self, event: SystemEvent) {
        if self.recipients.is_empty() {
            tracing::debug!(
                kind = event.kind(),
                "SystemNotifier: no recipients — skipping"
            );
            return;
        }
        let text = event.format_message();
        tracing::info!(
            kind = event.kind(),
            recipients = self.recipients.len(),
            "SystemNotifier: dispatching system event"
        );
        for r in &self.recipients {
            let Some(channel) = self.channels.get(&r.channel) else {
                tracing::warn!(
                    channel = %r.channel,
                    "SystemNotifier: recipient channel not registered — skipping"
                );
                continue;
            };
            let msg = OutboundMessage {
                chat_id: r.chat_id.clone(),
                text: text.clone(),
                reply_to: None,
                // Plain — version strings like "5.5.5-rc.1" can trip Markdown.
                parse_mode: Some(ParseMode::Plain),
            };
            send_with_retry(channel.as_ref(), msg).await;
        }
    }
}

/// Send a message with up to 3 attempts and exponential backoff. Skips retries
/// for permanent failures (chat-not-found, bot-blocked, parse errors). Mirrors
/// the helper in `src/main.rs` — duplicated here for v5.5.5 zero-risk;
/// dedup planned for v5.6+.
async fn send_with_retry(sender: &dyn Channel, msg: OutboundMessage) {
    let mut attempt = 0u32;
    loop {
        attempt += 1;
        match sender.send_message(msg.clone()).await {
            Ok(_) => return,
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("message is too long")
                    || err_str.contains("can't parse")
                    || err_str.contains("chat not found")
                    || err_str.contains("bot was blocked")
                    || err_str.contains("CHAT_WRITE_FORBIDDEN")
                {
                    tracing::error!(
                        error = %e,
                        "SystemNotifier: non-retryable send failure — message lost"
                    );
                    return;
                }
                if attempt >= 3 {
                    tracing::error!(
                        error = %e,
                        attempt,
                        "SystemNotifier: failed to send after 3 attempts — message lost"
                    );
                    return;
                }
                tracing::warn!(
                    error = %e,
                    attempt,
                    "SystemNotifier: send failed, retrying"
                );
                tokio::time::sleep(Duration::from_millis(500 * (1 << attempt))).await;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use temm1e_core::types::config::NotifierRecipientConfig;

    fn cfg(recipients: Vec<NotifierRecipientConfig>) -> NotificationsConfig {
        NotificationsConfig {
            enabled: true,
            recipients,
        }
    }

    fn hb(report_to: Option<&str>) -> HeartbeatConfig {
        HeartbeatConfig {
            enabled: false,
            interval: "30m".into(),
            checklist: "HEARTBEAT.md".into(),
            report_to: report_to.map(str::to_string),
            active_hours: None,
        }
    }

    #[test]
    fn explicit_list_wins_over_fallback() {
        let resolved = resolve_recipients(
            &cfg(vec![
                NotifierRecipientConfig {
                    channel: "telegram".into(),
                    chat_id: "111".into(),
                },
                NotifierRecipientConfig {
                    channel: "discord".into(),
                    chat_id: "222".into(),
                },
            ]),
            &hb(Some("999")),
            Some("telegram"),
        );
        assert_eq!(resolved.len(), 2);
        assert_eq!(resolved[0].channel, "telegram");
        assert_eq!(resolved[0].chat_id, "111");
        assert_eq!(resolved[1].channel, "discord");
        assert_eq!(resolved[1].chat_id, "222");
    }

    #[test]
    fn fallback_derives_single_recipient_from_heartbeat() {
        let resolved = resolve_recipients(&cfg(vec![]), &hb(Some("123")), Some("telegram"));
        assert_eq!(resolved.len(), 1);
        assert_eq!(resolved[0].channel, "telegram");
        assert_eq!(resolved[0].chat_id, "123");
    }

    #[test]
    fn fallback_empty_when_no_report_to() {
        let resolved = resolve_recipients(&cfg(vec![]), &hb(None), Some("telegram"));
        assert!(resolved.is_empty());
    }

    #[test]
    fn fallback_empty_when_no_primary_channel() {
        let resolved = resolve_recipients(&cfg(vec![]), &hb(Some("123")), None);
        assert!(resolved.is_empty());
    }

    #[test]
    fn fallback_treats_empty_string_report_to_as_unset() {
        let resolved = resolve_recipients(&cfg(vec![]), &hb(Some("")), Some("telegram"));
        assert!(resolved.is_empty());
    }
}
