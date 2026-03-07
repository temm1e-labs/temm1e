//! SkyClaw Channels crate
//!
//! Provides messaging channel implementations (CLI, Telegram, etc.) that
//! conform to the `Channel` and `FileTransfer` traits defined in `skyclaw-core`.

pub mod cli;
pub mod file_transfer;

#[cfg(feature = "telegram")]
pub mod telegram;

// Re-exports for convenience
pub use cli::CliChannel;
pub use file_transfer::{save_received_file, read_file_for_sending};

#[cfg(feature = "telegram")]
pub use telegram::TelegramChannel;

use skyclaw_core::types::config::ChannelConfig;
use skyclaw_core::types::error::SkyclawError;
use skyclaw_core::Channel;
use std::path::PathBuf;

/// Factory function to create a channel by name.
///
/// Supported channel names:
/// - `"cli"` — always available
/// - `"telegram"` — requires the `telegram` feature
///
/// Returns an error if the channel name is unknown or the required feature is
/// not enabled.
#[allow(unused_variables)]
pub fn create_channel(
    name: &str,
    config: &ChannelConfig,
    workspace: PathBuf,
) -> Result<Box<dyn Channel>, SkyclawError> {
    match name {
        "cli" => Ok(Box::new(CliChannel::new(workspace))),

        #[cfg(feature = "telegram")]
        "telegram" => Ok(Box::new(TelegramChannel::new(config)?)),

        #[cfg(not(feature = "telegram"))]
        "telegram" => Err(SkyclawError::Config(
            "Telegram support is not enabled. Compile with --features telegram".into(),
        )),

        other => Err(SkyclawError::Config(format!("Unknown channel: {other}"))),
    }
}
