//! TEMM1E Channels crate
//!
//! Provides messaging channel implementations (CLI, Telegram, etc.) that
//! conform to the `Channel` and `FileTransfer` traits defined in `temm1e-core`.

pub mod cli;
pub mod file_transfer;

#[cfg(feature = "telegram")]
pub mod telegram;

#[cfg(feature = "discord")]
pub mod discord;

#[cfg(feature = "slack")]
pub mod slack;

// Re-exports for convenience
pub use cli::CliChannel;
pub use file_transfer::{read_file_for_sending, save_received_file};

#[cfg(feature = "telegram")]
pub use telegram::TelegramChannel;

#[cfg(feature = "discord")]
pub use discord::DiscordChannel;

#[cfg(feature = "slack")]
pub use slack::SlackChannel;

use std::path::PathBuf;
use temm1e_core::types::config::ChannelConfig;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::Channel;

/// Factory function to create a channel by name.
///
/// Supported channel names:
/// - `"cli"` — always available
/// - `"telegram"` — requires the `telegram` feature
/// - `"discord"` — requires the `discord` feature
/// - `"slack"` — requires the `slack` feature
///
/// Returns an error if the channel name is unknown or the required feature is
/// not enabled.
#[allow(unused_variables)]
pub fn create_channel(
    name: &str,
    config: &ChannelConfig,
    workspace: PathBuf,
) -> Result<Box<dyn Channel>, Temm1eError> {
    match name {
        "cli" => Ok(Box::new(CliChannel::new(workspace))),

        #[cfg(feature = "telegram")]
        "telegram" => Ok(Box::new(TelegramChannel::new(config)?)),

        #[cfg(not(feature = "telegram"))]
        "telegram" => Err(Temm1eError::Config(
            "Telegram support is not enabled. Compile with --features telegram".into(),
        )),

        #[cfg(feature = "discord")]
        "discord" => Ok(Box::new(DiscordChannel::new(config)?)),

        #[cfg(not(feature = "discord"))]
        "discord" => Err(Temm1eError::Config(
            "Discord support is not enabled. Compile with --features discord".into(),
        )),

        #[cfg(feature = "slack")]
        "slack" => Ok(Box::new(SlackChannel::new(config)?)),

        #[cfg(not(feature = "slack"))]
        "slack" => Err(Temm1eError::Config(
            "Slack support is not enabled. Compile with --features slack".into(),
        )),

        other => Err(Temm1eError::Config(format!("Unknown channel: {other}"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_cli_channel() {
        let config = ChannelConfig {
            enabled: true,
            token: None,
            allowlist: Vec::new(),
            file_transfer: true,
            max_file_size: None,
        };
        let channel = create_channel("cli", &config, "/tmp".into()).unwrap();
        assert_eq!(channel.name(), "cli");
        assert!(channel.is_allowed("anyone"));
    }

    #[test]
    fn create_unknown_channel_fails() {
        let config = ChannelConfig {
            enabled: true,
            token: None,
            allowlist: Vec::new(),
            file_transfer: false,
            max_file_size: None,
        };
        let result = create_channel("smoke_signal", &config, "/tmp".into());
        assert!(result.is_err());
    }

    // ── CLI channel delete_message default no-op ─────────────────────
    // The CLI channel does not override delete_message, so it inherits
    // the Channel trait's default no-op implementation.

    #[tokio::test]
    async fn cli_delete_message_is_noop() {
        let channel = CliChannel::new("/tmp".into());
        // delete_message should succeed silently (no-op)
        let result = channel.delete_message("cli", "123").await;
        assert!(result.is_ok(), "CLI delete_message should be a no-op");
    }

    #[test]
    fn cli_channel_implements_channel_trait() {
        let channel = CliChannel::new("/tmp".into());
        // If this compiles, CliChannel implements Channel (including delete_message)
        let _: &dyn Channel = &channel;
    }
}
