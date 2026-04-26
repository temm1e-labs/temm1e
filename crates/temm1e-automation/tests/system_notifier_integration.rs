//! End-to-end SystemNotifier test using a capturing mock Channel.
//!
//! Verifies that `notify` resolves recipients, formats the event text,
//! and reaches `send_message` on the right channel — without depending on
//! a real Telegram/Discord/Slack connection.
use async_trait::async_trait;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use temm1e_automation::{NotifierRecipient, SystemNotifier};
use temm1e_core::types::error::Temm1eError;
use temm1e_core::types::message::OutboundMessage;
use temm1e_core::types::system_event::{ShutdownReason, SystemEvent};
use temm1e_core::{Channel, FileTransfer};

/// Captures every OutboundMessage in an Arc<Mutex<Vec<_>>>.
struct CapturingChannel {
    name: String,
    messages: Arc<Mutex<Vec<OutboundMessage>>>,
}

impl CapturingChannel {
    fn new(name: &str) -> (Self, Arc<Mutex<Vec<OutboundMessage>>>) {
        let messages: Arc<Mutex<Vec<OutboundMessage>>> = Arc::new(Mutex::new(Vec::new()));
        (
            Self {
                name: name.to_string(),
                messages: messages.clone(),
            },
            messages,
        )
    }
}

#[async_trait]
impl Channel for CapturingChannel {
    fn name(&self) -> &str {
        &self.name
    }

    async fn start(&mut self) -> Result<(), Temm1eError> {
        Ok(())
    }

    async fn stop(&mut self) -> Result<(), Temm1eError> {
        Ok(())
    }

    async fn send_message(&self, msg: OutboundMessage) -> Result<(), Temm1eError> {
        self.messages.lock().unwrap().push(msg);
        Ok(())
    }

    fn file_transfer(&self) -> Option<&dyn FileTransfer> {
        None
    }

    fn is_allowed(&self, _user_id: &str) -> bool {
        true
    }
}

#[tokio::test]
async fn startup_event_reaches_recipient() {
    let (chan, captured) = CapturingChannel::new("telegram");
    let mut map: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    map.insert("telegram".into(), Arc::new(chan));
    let channels = Arc::new(map);

    let notifier = SystemNotifier::new(
        vec![NotifierRecipient {
            channel: "telegram".into(),
            chat_id: "owner-123".into(),
        }],
        channels,
    );

    notifier
        .notify(SystemEvent::Startup {
            version: "5.5.5".into(),
            channels: vec!["telegram".into()],
        })
        .await;

    let sent = captured.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].chat_id, "owner-123");
    assert!(sent[0].text.contains("5.5.5"));
    assert!(sent[0].text.contains("online"));
}

#[tokio::test]
async fn shutdown_event_reaches_recipient() {
    let (chan, captured) = CapturingChannel::new("telegram");
    let mut map: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    map.insert("telegram".into(), Arc::new(chan));
    let channels = Arc::new(map);

    let notifier = SystemNotifier::new(
        vec![NotifierRecipient {
            channel: "telegram".into(),
            chat_id: "owner-123".into(),
        }],
        channels,
    );

    notifier
        .notify(SystemEvent::Shutdown {
            reason: ShutdownReason::CtrlC,
        })
        .await;

    let sent = captured.lock().unwrap();
    assert_eq!(sent.len(), 1);
    assert!(sent[0].text.contains("Ctrl+C"));
}

#[tokio::test]
async fn fan_out_to_multiple_recipients() {
    let (tg_chan, tg_captured) = CapturingChannel::new("telegram");
    let (disc_chan, disc_captured) = CapturingChannel::new("discord");
    let mut map: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    map.insert("telegram".into(), Arc::new(tg_chan));
    map.insert("discord".into(), Arc::new(disc_chan));
    let channels = Arc::new(map);

    let notifier = SystemNotifier::new(
        vec![
            NotifierRecipient {
                channel: "telegram".into(),
                chat_id: "tg-owner".into(),
            },
            NotifierRecipient {
                channel: "discord".into(),
                chat_id: "disc-owner".into(),
            },
        ],
        channels,
    );

    notifier
        .notify(SystemEvent::Startup {
            version: "5.5.5".into(),
            channels: vec!["telegram".into(), "discord".into()],
        })
        .await;

    let tg = tg_captured.lock().unwrap();
    let disc = disc_captured.lock().unwrap();
    assert_eq!(tg.len(), 1);
    assert_eq!(disc.len(), 1);
    assert_eq!(tg[0].chat_id, "tg-owner");
    assert_eq!(disc[0].chat_id, "disc-owner");
}

#[tokio::test]
async fn unknown_channel_is_skipped_without_panic() {
    let (chan, captured) = CapturingChannel::new("telegram");
    let mut map: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    map.insert("telegram".into(), Arc::new(chan));
    let channels = Arc::new(map);

    let notifier = SystemNotifier::new(
        vec![
            NotifierRecipient {
                channel: "telegram".into(),
                chat_id: "tg-owner".into(),
            },
            NotifierRecipient {
                channel: "slack".into(), // not registered
                chat_id: "slack-owner".into(),
            },
        ],
        channels,
    );

    notifier
        .notify(SystemEvent::Startup {
            version: "5.5.5".into(),
            channels: vec!["telegram".into()],
        })
        .await;

    let sent = captured.lock().unwrap();
    assert_eq!(sent.len(), 1, "telegram still received its message");
}

#[tokio::test]
async fn empty_recipients_is_noop() {
    let (chan, captured) = CapturingChannel::new("telegram");
    let mut map: HashMap<String, Arc<dyn Channel>> = HashMap::new();
    map.insert("telegram".into(), Arc::new(chan));
    let channels = Arc::new(map);

    let notifier = SystemNotifier::new(vec![], channels);

    notifier
        .notify(SystemEvent::Startup {
            version: "5.5.5".into(),
            channels: vec![],
        })
        .await;

    assert!(captured.lock().unwrap().is_empty());
}
