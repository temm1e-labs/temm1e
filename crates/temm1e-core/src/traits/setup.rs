//! Setup link generation trait for secure API key onboarding.
//!
//! Allows tools to generate OTK-based setup links without directly
//! depending on the gateway crate's `SetupTokenStore`.

use async_trait::async_trait;

/// Generates secure setup links for API key onboarding.
#[async_trait]
pub trait SetupLinkGenerator: Send + Sync {
    /// Generate a setup link for the given chat. Returns a full URL
    /// with the OTK embedded in the fragment (e.g. `https://...#<hex>`).
    async fn generate_link(&self, chat_id: &str) -> String;
}
