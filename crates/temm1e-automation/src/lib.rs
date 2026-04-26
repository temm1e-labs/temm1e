//! TEMM1E Automation — heartbeat runner, system notifier, task scheduling,
//! and autonomous agent execution.

pub mod duration;
pub mod heartbeat;
pub mod system_notifier;

pub use heartbeat::HeartbeatRunner;
pub use system_notifier::{resolve_recipients, NotifierRecipient, SystemNotifier};
