//! Token storage and auto-refresh for OpenAI Codex OAuth tokens.
//!
//! Tokens are stored in `~/.temm1e/oauth.json`. Access tokens expire in ~1 hour
//! and are auto-refreshed using the refresh token. A Mutex ensures only one
//! refresh happens at a time (prevents `refresh_token_reused` errors).

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use temm1e_core::types::error::Temm1eError;
use tokio::sync::Mutex;

/// OAuth token set — stored in ~/.temm1e/oauth.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexOAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64, // Unix timestamp
    pub email: String,
    pub account_id: String,
}

/// Thread-safe token store with auto-refresh.
pub struct TokenStore {
    tokens: Mutex<CodexOAuthTokens>,
    path: PathBuf,
    client: reqwest::Client,
}

/// The OpenAI auth token endpoint.
const TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
/// The public Codex CLI client ID (used by OpenClaw, Roo Code, OpenCode, etc.)
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Refresh buffer — refresh if within this many seconds of expiry.
const REFRESH_BUFFER_SECS: u64 = 300; // 5 minutes

impl TokenStore {
    /// Create a new token store from saved tokens.
    pub fn new(tokens: CodexOAuthTokens) -> Self {
        Self {
            path: Self::default_path(),
            tokens: Mutex::new(tokens),
            client: reqwest::Client::new(),
        }
    }

    /// Load tokens from ~/.temm1e/oauth.json
    pub fn load() -> Result<Self, Temm1eError> {
        let path = Self::default_path();
        let content = std::fs::read_to_string(&path).map_err(|e| {
            Temm1eError::Auth(format!(
                "No OAuth tokens found at {}. Run `temm1e auth login` first. ({})",
                path.display(),
                e
            ))
        })?;
        let tokens: CodexOAuthTokens = serde_json::from_str(&content)
            .map_err(|e| Temm1eError::Auth(format!("Failed to parse OAuth tokens: {}", e)))?;
        Ok(Self {
            path,
            tokens: Mutex::new(tokens),
            client: reqwest::Client::new(),
        })
    }

    /// Get a fresh access token, auto-refreshing if near expiry.
    ///
    /// The Mutex ensures only one refresh happens at a time — concurrent callers
    /// will wait for the refresh to complete and then get the fresh token.
    pub async fn get_access_token(&self) -> Result<String, Temm1eError> {
        let mut tokens = self.tokens.lock().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        if tokens.expires_at > now + REFRESH_BUFFER_SECS {
            return Ok(tokens.access_token.clone());
        }

        tracing::info!(email = %tokens.email, "Refreshing Codex OAuth token");
        let new_tokens = Self::refresh_token(&self.client, &tokens.refresh_token).await?;

        // Preserve email and account_id from the original tokens
        let updated = CodexOAuthTokens {
            access_token: new_tokens.access_token,
            refresh_token: new_tokens.refresh_token,
            expires_at: new_tokens.expires_at,
            email: tokens.email.clone(),
            account_id: tokens.account_id.clone(),
        };

        self.save_to_disk(&updated)?;
        *tokens = updated.clone();
        tracing::info!("Codex OAuth token refreshed successfully");

        Ok(updated.access_token)
    }

    /// Get a clone of the current tokens (for export).
    pub async fn get_tokens(&self) -> CodexOAuthTokens {
        self.tokens.lock().await.clone()
    }

    /// Get the current email (for display purposes).
    pub async fn email(&self) -> String {
        self.tokens.lock().await.email.clone()
    }

    /// Get the current account ID.
    pub async fn account_id(&self) -> String {
        self.tokens.lock().await.account_id.clone()
    }

    /// Check if the token is expired or will expire within the buffer period.
    pub async fn is_expired(&self) -> bool {
        let tokens = self.tokens.lock().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        tokens.expires_at <= now + REFRESH_BUFFER_SECS
    }

    /// Get the expiry time as a human-readable string.
    pub async fn expires_in(&self) -> String {
        let tokens = self.tokens.lock().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        if tokens.expires_at <= now {
            "expired".to_string()
        } else {
            let remaining = tokens.expires_at - now;
            if remaining > 3600 {
                format!("{}h {}m", remaining / 3600, (remaining % 3600) / 60)
            } else {
                format!("{}m", remaining / 60)
            }
        }
    }

    /// Save tokens to disk.
    pub fn save_to_disk(&self, tokens: &CodexOAuthTokens) -> Result<(), Temm1eError> {
        let dir = self.path.parent().unwrap_or(std::path::Path::new("."));
        std::fs::create_dir_all(dir)
            .map_err(|e| Temm1eError::Auth(format!("Failed to create dir: {}", e)))?;
        let content = serde_json::to_string_pretty(tokens)
            .map_err(|e| Temm1eError::Auth(format!("Failed to serialize tokens: {}", e)))?;
        std::fs::write(&self.path, content)
            .map_err(|e| Temm1eError::Auth(format!("Failed to write tokens: {}", e)))?;
        tracing::debug!(path = %self.path.display(), "OAuth tokens saved");
        Ok(())
    }

    /// Refresh the access token using the refresh token.
    async fn refresh_token(
        client: &reqwest::Client,
        refresh_token: &str,
    ) -> Result<RefreshResponse, Temm1eError> {
        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", refresh_token),
            ("client_id", CLIENT_ID),
        ];

        let resp = client
            .post(TOKEN_ENDPOINT)
            .form(&params)
            .send()
            .await
            .map_err(|e| Temm1eError::Auth(format!("Token refresh request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(Temm1eError::Auth(format!(
                "Token refresh failed ({}): {}",
                status, body
            )));
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .map_err(|e| Temm1eError::Auth(format!("Failed to parse refresh response: {}", e)))?;

        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        Ok(RefreshResponse {
            access_token: token_resp.access_token,
            refresh_token: token_resp
                .refresh_token
                .unwrap_or_else(|| refresh_token.to_string()),
            expires_at: now + token_resp.expires_in.unwrap_or(3600),
        })
    }

    /// Default path: ~/.temm1e/oauth.json
    fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".temm1e")
            .join("oauth.json")
    }

    /// Delete the token file (for logout).
    pub fn delete() -> Result<(), Temm1eError> {
        let path = Self::default_path();
        if path.exists() {
            std::fs::remove_file(&path)
                .map_err(|e| Temm1eError::Auth(format!("Failed to delete tokens: {}", e)))?;
        }
        Ok(())
    }

    /// Check if tokens exist on disk.
    pub fn exists() -> bool {
        Self::default_path().exists()
    }
}

/// Raw token response from OpenAI auth endpoint.
#[derive(Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    #[allow(dead_code)]
    id_token: Option<String>,
}

/// Internal struct for refresh results.
struct RefreshResponse {
    access_token: String,
    refresh_token: String,
    expires_at: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_path_ends_with_oauth_json() {
        let path = TokenStore::default_path();
        assert!(path.ends_with("oauth.json"));
        assert!(path.to_string_lossy().contains(".temm1e"));
    }

    #[test]
    fn token_serialization_roundtrip() {
        let tokens = CodexOAuthTokens {
            access_token: "eyJhb-test".to_string(),
            refresh_token: "ort_test_refresh".to_string(),
            expires_at: 1710180000,
            email: "test@example.com".to_string(),
            account_id: "org-test123".to_string(),
        };
        let json = serde_json::to_string(&tokens).unwrap();
        let parsed: CodexOAuthTokens = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.email, "test@example.com");
        assert_eq!(parsed.expires_at, 1710180000);
    }

    #[tokio::test]
    async fn token_store_expiry_check() {
        let tokens = CodexOAuthTokens {
            access_token: "test".to_string(),
            refresh_token: "test".to_string(),
            expires_at: 0, // Already expired
            email: "test@example.com".to_string(),
            account_id: "org-test".to_string(),
        };
        let store = TokenStore::new(tokens);
        assert!(store.is_expired().await);
    }

    #[tokio::test]
    async fn token_store_not_expired() {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let tokens = CodexOAuthTokens {
            access_token: "test".to_string(),
            refresh_token: "test".to_string(),
            expires_at: now + 7200, // 2 hours from now
            email: "test@example.com".to_string(),
            account_id: "org-test".to_string(),
        };
        let store = TokenStore::new(tokens);
        assert!(!store.is_expired().await);
    }
}
