use crate::types::error::Temm1eError;
use async_trait::async_trait;

/// Tunnel trait — secure external access (Cloudflare, Tailscale, ngrok, etc.)
#[async_trait]
pub trait Tunnel: Send + Sync {
    /// Start the tunnel and return the public URL
    async fn start(&mut self, local_port: u16) -> Result<String, Temm1eError>;

    /// Stop the tunnel
    async fn stop(&mut self) -> Result<(), Temm1eError>;

    /// Get the current public URL (None if not running)
    fn public_url(&self) -> Option<&str>;

    /// Tunnel provider name (e.g., "cloudflare", "ngrok")
    fn provider_name(&self) -> &str;
}
