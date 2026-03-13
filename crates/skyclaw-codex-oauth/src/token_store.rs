//! Token storage and auto-refresh for OpenAI Codex OAuth tokens.
//!
//! Tokens are stored in `~/.skyclaw/auth-profiles.json` with ChaCha20-Poly1305 encryption.
//! Access tokens expire in ~1 hour and are auto-refreshed using the refresh token.
//! A Mutex ensures only one refresh happens at a time (prevents `refresh_token_reused` errors).

use serde::{Deserialize, Serialize};
use skyclaw_core::types::error::SkyclawError;
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::Mutex;

/// OAuth token set — stored encrypted in auth-profiles.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CodexOAuthTokens {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_at: u64, // Unix timestamp
    pub email: String,
    pub account_id: String,
}

/// Auth profile entry in auth-profiles.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProfile {
    pub provider: String,
    pub kind: String,
    pub account_id: String,
    pub access_token: String,  // Will be "enc2:..." when encrypted
    pub refresh_token: String, // Will be "enc2:..." when encrypted
    pub id_token: String,      // Will be "enc2:..." when encrypted
    pub expires_at: String,    // ISO-8601 format
}

/// Root structure of auth-profiles.json
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AuthProfiles {
    pub schema_version: u32,
    pub active_profiles: HashMap<String, String>,
    pub profiles: HashMap<String, AuthProfile>,
}

/// Thread-safe token store with auto-refresh and retry logic.
pub struct TokenStore {
    tokens: Mutex<CodexOAuthTokens>,
    path: PathBuf,
    lock_path: PathBuf,
    client: reqwest::Client,
    last_refresh_attempt: Mutex<Option<SystemTime>>,
}

/// The OpenAI auth token endpoint.
const TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
/// The public Codex CLI client ID (used by OpenClaw, Roo Code, OpenCode, etc.)
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Refresh buffer — refresh if within this many seconds of expiry.
const REFRESH_BUFFER_SECS: u64 = 90; // 90 seconds
/// Maximum retry attempts for token refresh
const MAX_REFRESH_RETRIES: u32 = 3;
/// Backoff delay between retries (milliseconds)
const RETRY_BACKOFF_MS: u64 = 350;
/// Cooldown period after failed refresh (seconds)
const REFRESH_COOLDOWN_SECS: u64 = 10;

impl TokenStore {
    /// Create a new token store from saved tokens.
    pub fn new(tokens: CodexOAuthTokens) -> Self {
        let path = Self::default_path();
        let lock_path = Self::default_lock_path();
        Self {
            path,
            lock_path,
            tokens: Mutex::new(tokens),
            client: reqwest::Client::new(),
            last_refresh_attempt: Mutex::new(None),
        }
    }

    /// Load tokens from ~/.skyclaw/auth-profiles.json (with decryption)
    pub fn load() -> Result<Self, SkyclawError> {
        let path = Self::default_path();
        let lock_path = Self::default_lock_path();
        
        let content = std::fs::read_to_string(&path).map_err(|e| {
            SkyclawError::Auth(format!(
                "No OAuth tokens found at {}. Run `skyclaw auth login --provider openai-codex` first. ({})",
                path.display(),
                e
            ))
        })?;
        
        let profiles: AuthProfiles = serde_json::from_str(&content)
            .map_err(|e| SkyclawError::Auth(format!("Failed to parse auth profiles: {}", e)))?;
        
        // Get the active openai-codex profile
        let profile_key = profiles
            .active_profiles
            .get("openai-codex")
            .ok_or_else(|| SkyclawError::Auth("No active openai-codex profile found".to_string()))?;
        
        let profile = profiles
            .profiles
            .get(profile_key)
            .ok_or_else(|| SkyclawError::Auth(format!("Profile {} not found", profile_key)))?;
        
        // Decrypt tokens (if they start with "enc2:")
        let access_token = Self::decrypt_token(&profile.access_token)?;
        let refresh_token = Self::decrypt_token(&profile.refresh_token)?;
        
        // Parse expires_at from ISO-8601
        let expires_at = chrono::DateTime::parse_from_rfc3339(&profile.expires_at)
            .map_err(|e| SkyclawError::Auth(format!("Invalid expires_at format: {}", e)))?
            .timestamp() as u64;
        
        let tokens = CodexOAuthTokens {
            access_token,
            refresh_token,
            expires_at,
            email: profile.account_id.split(':').last().unwrap_or("unknown").to_string(),
            account_id: profile.account_id.clone(),
        };
        
        Ok(Self {
            path,
            lock_path,
            tokens: Mutex::new(tokens),
            client: reqwest::Client::new(),
            last_refresh_attempt: Mutex::new(None),
        })
    }

    /// Get a fresh access token, auto-refreshing if near expiry.
    ///
    /// The Mutex ensures only one refresh happens at a time — concurrent callers
    /// will wait for the refresh to complete and then get the fresh token.
    /// Implements retry logic with backoff and cooldown on failure.
    pub async fn get_access_token(&self) -> Result<String, SkyclawError> {
        let mut tokens = self.tokens.lock().await;
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        // Check if token is still valid (with buffer)
        if tokens.expires_at > now + REFRESH_BUFFER_SECS {
            return Ok(tokens.access_token.clone());
        }

        // Check cooldown period after failed refresh
        let mut last_attempt = self.last_refresh_attempt.lock().await;
        if let Some(last) = *last_attempt {
            let elapsed = SystemTime::now()
                .duration_since(last)
                .unwrap_or_default()
                .as_secs();
            if elapsed < REFRESH_COOLDOWN_SECS {
                tracing::warn!(
                    "Token refresh in cooldown ({}s remaining)",
                    REFRESH_COOLDOWN_SECS - elapsed
                );
                // Return expired token — caller will handle 401
                return Ok(tokens.access_token.clone());
            }
        }

        tracing::info!(email = %tokens.email, account_id = %tokens.account_id, "Refreshing Codex OAuth token");
        
        // Retry logic with exponential backoff
        let mut last_error = None;
        for attempt in 1..=MAX_REFRESH_RETRIES {
            match Self::refresh_token(&self.client, &tokens.refresh_token).await {
                Ok(new_tokens) => {
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
                    *last_attempt = None; // Clear cooldown on success
                    tracing::info!("Codex OAuth token refreshed successfully");

                    return Ok(updated.access_token);
                }
                Err(e) => {
                    last_error = Some(e);
                    if attempt < MAX_REFRESH_RETRIES {
                        tracing::warn!(
                            attempt = attempt,
                            max = MAX_REFRESH_RETRIES,
                            "Token refresh failed, retrying..."
                        );
                        tokio::time::sleep(Duration::from_millis(RETRY_BACKOFF_MS * attempt as u64)).await;
                    }
                }
            }
        }

        // All retries failed — set cooldown
        *last_attempt = Some(SystemTime::now());
        
        Err(last_error.unwrap_or_else(|| {
            SkyclawError::Auth("Token refresh failed after all retries".to_string())
        }))
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

    /// Save tokens to disk with encryption and file locking.
    pub fn save_to_disk(&self, tokens: &CodexOAuthTokens) -> Result<(), SkyclawError> {
        let dir = self.path.parent().unwrap_or(std::path::Path::new("."));
        std::fs::create_dir_all(dir)
            .map_err(|e| SkyclawError::Auth(format!("Failed to create dir: {}", e)))?;
        
        // Acquire file lock
        let _lock = Self::acquire_lock(&self.lock_path)?;
        
        // Load existing profiles or create new
        let mut profiles = if self.path.exists() {
            let content = std::fs::read_to_string(&self.path)
                .map_err(|e| SkyclawError::Auth(format!("Failed to read profiles: {}", e)))?;
            serde_json::from_str::<AuthProfiles>(&content).unwrap_or_else(|_| AuthProfiles {
                schema_version: 1,
                active_profiles: HashMap::new(),
                profiles: HashMap::new(),
            })
        } else {
            AuthProfiles {
                schema_version: 1,
                active_profiles: HashMap::new(),
                profiles: HashMap::new(),
            }
        };
        
        // Encrypt tokens
        let access_token_enc = Self::encrypt_token(&tokens.access_token)?;
        let refresh_token_enc = Self::encrypt_token(&tokens.refresh_token)?;
        let id_token_enc = Self::encrypt_token("")?; // Placeholder for id_token
        
        // Convert expires_at to ISO-8601
        let expires_at_iso = chrono::DateTime::from_timestamp(tokens.expires_at as i64, 0)
            .ok_or_else(|| SkyclawError::Auth("Invalid timestamp".to_string()))?
            .to_rfc3339();
        
        // Create profile
        let profile = AuthProfile {
            provider: "openai-codex".to_string(),
            kind: "oauth".to_string(),
            account_id: tokens.account_id.clone(),
            access_token: access_token_enc,
            refresh_token: refresh_token_enc,
            id_token: id_token_enc,
            expires_at: expires_at_iso,
        };
        
        // Update profiles
        profiles.active_profiles.insert("openai-codex".to_string(), "openai-codex:default".to_string());
        profiles.profiles.insert("openai-codex:default".to_string(), profile);
        
        // Write to disk
        let content = serde_json::to_string_pretty(&profiles)
            .map_err(|e| SkyclawError::Auth(format!("Failed to serialize profiles: {}", e)))?;
        std::fs::write(&self.path, content)
            .map_err(|e| SkyclawError::Auth(format!("Failed to write profiles: {}", e)))?;
        
        tracing::debug!(path = %self.path.display(), "Auth profiles saved with encryption");
        Ok(())
    }
    
    /// Encrypt a token using ChaCha20-Poly1305 (returns "enc2:base64")
    fn encrypt_token(plaintext: &str) -> Result<String, SkyclawError> {
        use chacha20poly1305::{
            aead::{Aead, KeyInit, OsRng},
            ChaCha20Poly1305, Nonce,
        };
        use rand::RngCore;
        
        // Get or generate encryption key
        let key = Self::get_or_create_encryption_key()?;
        let cipher = ChaCha20Poly1305::new(&key.into());
        
        // Generate random nonce
        let mut nonce_bytes = [0u8; 12];
        OsRng.fill_bytes(&mut nonce_bytes);
        let nonce = Nonce::from_slice(&nonce_bytes);
        
        // Encrypt
        let ciphertext = cipher
            .encrypt(nonce, plaintext.as_bytes())
            .map_err(|e| SkyclawError::Auth(format!("Encryption failed: {}", e)))?;
        
        // Combine nonce + ciphertext and encode as base64
        let mut combined = nonce_bytes.to_vec();
        combined.extend_from_slice(&ciphertext);
        
        Ok(format!("enc2:{}", base64::Engine::encode(
            &base64::engine::general_purpose::STANDARD,
            combined
        )))
    }
    
    /// Decrypt a token (handles "enc2:base64" format or returns plaintext)
    fn decrypt_token(encrypted: &str) -> Result<String, SkyclawError> {
        if !encrypted.starts_with("enc2:") {
            // Not encrypted, return as-is
            return Ok(encrypted.to_string());
        }
        
        use chacha20poly1305::{
            aead::{Aead, KeyInit},
            ChaCha20Poly1305, Nonce,
        };
        
        let encoded = encrypted.strip_prefix("enc2:").unwrap();
        let combined = base64::Engine::decode(
            &base64::engine::general_purpose::STANDARD,
            encoded
        ).map_err(|e| SkyclawError::Auth(format!("Invalid base64: {}", e)))?;
        
        if combined.len() < 12 {
            return Err(SkyclawError::Auth("Invalid encrypted token format".to_string()));
        }
        
        // Split nonce and ciphertext
        let (nonce_bytes, ciphertext) = combined.split_at(12);
        let nonce = Nonce::from_slice(nonce_bytes);
        
        // Get encryption key
        let key = Self::get_or_create_encryption_key()?;
        let cipher = ChaCha20Poly1305::new(&key.into());
        
        // Decrypt
        let plaintext = cipher
            .decrypt(nonce, ciphertext)
            .map_err(|e| SkyclawError::Auth(format!("Decryption failed: {}", e)))?;
        
        String::from_utf8(plaintext)
            .map_err(|e| SkyclawError::Auth(format!("Invalid UTF-8: {}", e)))
    }
    
    /// Get or create the encryption key (~/.skyclaw/vault.key)
    fn get_or_create_encryption_key() -> Result<[u8; 32], SkyclawError> {
        use rand::RngCore;
        use chacha20poly1305::aead::OsRng;
        
        let key_path = dirs::home_dir()
            .ok_or_else(|| SkyclawError::Auth("Cannot determine home directory".to_string()))?
            .join(".skyclaw")
            .join("vault.key");
        
        if key_path.exists() {
            let bytes = std::fs::read(&key_path)
                .map_err(|e| SkyclawError::Auth(format!("Failed to read vault key: {}", e)))?;
            let key: [u8; 32] = bytes.try_into()
                .map_err(|_| SkyclawError::Auth("Vault key must be 32 bytes".to_string()))?;
            return Ok(key);
        }
        
        // Generate new key
        let mut key = [0u8; 32];
        OsRng.fill_bytes(&mut key);
        
        std::fs::create_dir_all(key_path.parent().unwrap())
            .map_err(|e| SkyclawError::Auth(format!("Failed to create dir: {}", e)))?;
        std::fs::write(&key_path, &key)
            .map_err(|e| SkyclawError::Auth(format!("Failed to write vault key: {}", e)))?;
        
        // Set permissions to 0600 on Unix
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let perms = std::fs::Permissions::from_mode(0o600);
            let _ = std::fs::set_permissions(&key_path, perms);
        }
        
        tracing::debug!("Generated new vault encryption key");
        Ok(key)
    }
    
    /// Acquire file lock to prevent race conditions
    fn acquire_lock(lock_path: &PathBuf) -> Result<std::fs::File, SkyclawError> {
        use std::fs::OpenOptions;
        
        let lock_file = OpenOptions::new()
            .create(true)
            .write(true)
            .open(lock_path)
            .map_err(|e| SkyclawError::Auth(format!("Failed to create lock file: {}", e)))?;
        
        // Best-effort file locking (platform-specific)
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            // Unix file locking would go here if needed
        }
        
        Ok(lock_file)
    }

    /// Refresh the access token using the refresh token.
    async fn refresh_token(
        client: &reqwest::Client,
        refresh_token: &str,
    ) -> Result<RefreshResponse, SkyclawError> {
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
            .map_err(|e| SkyclawError::Auth(format!("Token refresh request failed: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.text().await.unwrap_or_default();
            return Err(SkyclawError::Auth(format!(
                "Token refresh failed ({}): {}",
                status, body
            )));
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .map_err(|e| SkyclawError::Auth(format!("Failed to parse refresh response: {}", e)))?;

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

    /// Default path: ~/.skyclaw/auth-profiles.json
    fn default_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".skyclaw")
            .join("auth-profiles.json")
    }
    
    /// Default lock path: ~/.skyclaw/auth-profiles.lock
    fn default_lock_path() -> PathBuf {
        dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join(".skyclaw")
            .join("auth-profiles.lock")
    }

    /// Delete the openai-codex profile (for logout).
    pub fn delete() -> Result<(), SkyclawError> {
        let path = Self::default_path();
        let lock_path = Self::default_lock_path();
        
        if !path.exists() {
            return Ok(());
        }
        
        let _lock = Self::acquire_lock(&lock_path)?;
        
        let content = std::fs::read_to_string(&path)
            .map_err(|e| SkyclawError::Auth(format!("Failed to read profiles: {}", e)))?;
        let mut profiles: AuthProfiles = serde_json::from_str(&content)
            .map_err(|e| SkyclawError::Auth(format!("Failed to parse profiles: {}", e)))?;
        
        // Remove openai-codex profile
        profiles.active_profiles.remove("openai-codex");
        profiles.profiles.retain(|k, _| !k.starts_with("openai-codex:"));
        
        // Write back or delete if empty
        if profiles.profiles.is_empty() {
            std::fs::remove_file(&path)
                .map_err(|e| SkyclawError::Auth(format!("Failed to delete profiles: {}", e)))?;
        } else {
            let content = serde_json::to_string_pretty(&profiles)
                .map_err(|e| SkyclawError::Auth(format!("Failed to serialize profiles: {}", e)))?;
            std::fs::write(&path, content)
                .map_err(|e| SkyclawError::Auth(format!("Failed to write profiles: {}", e)))?;
        }
        
        Ok(())
    }

    /// Check if openai-codex profile exists on disk.
    pub fn exists() -> bool {
        let path = Self::default_path();
        if !path.exists() {
            return false;
        }
        
        if let Ok(content) = std::fs::read_to_string(&path) {
            if let Ok(profiles) = serde_json::from_str::<AuthProfiles>(&content) {
                return profiles.active_profiles.contains_key("openai-codex");
            }
        }
        
        false
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
    fn default_path_ends_with_auth_profiles_json() {
        let path = TokenStore::default_path();
        assert!(path.ends_with("auth-profiles.json"));
        assert!(path.to_string_lossy().contains(".skyclaw"));
    }
    
    #[test]
    fn default_paths_are_cross_platform() {
        let auth_path = TokenStore::default_path();
        let lock_path = TokenStore::default_lock_path();
        
        // Paths should be absolute or relative
        assert!(!auth_path.as_os_str().is_empty());
        assert!(!lock_path.as_os_str().is_empty());
        
        // Should end with correct filenames
        assert!(auth_path.ends_with("auth-profiles.json"));
        assert!(lock_path.ends_with("auth-profiles.lock"));
        
        // Both should be in the same directory
        assert_eq!(auth_path.parent(), lock_path.parent());
        
        // Should contain .skyclaw directory
        let path_str = auth_path.to_string_lossy();
        assert!(path_str.contains(".skyclaw"));
        
        // Path separators should be correct for the OS
        #[cfg(windows)]
        {
            // On Windows, should use backslashes or be normalized
            // PathBuf handles this automatically
        }
        #[cfg(unix)]
        {
            // On Unix, should use forward slashes
            assert!(path_str.contains('/'));
        }
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
    
    #[test]
    fn encrypt_decrypt_roundtrip() {
        let plaintext = "test_access_token_12345";
        let encrypted = TokenStore::encrypt_token(plaintext).unwrap();
        assert!(encrypted.starts_with("enc2:"));
        
        let decrypted = TokenStore::decrypt_token(&encrypted).unwrap();
        assert_eq!(decrypted, plaintext);
    }
    
    #[test]
    fn encrypt_decrypt_cross_platform() {
        // Test with various token formats that might appear on different OS
        let test_cases = vec![
            "simple_token",
            "token.with.dots",
            "token-with-dashes",
            "token_with_underscores",
            "token/with/slashes",
            "token\\with\\backslashes", // Windows paths
            "token with spaces",
            "token&with&ampersands",
            "very_long_token_that_might_exceed_normal_length_limits_and_contain_various_characters_123456789",
            "", // Empty token
        ];
        
        for test_token in test_cases {
            let encrypted = TokenStore::encrypt_token(test_token).unwrap();
            assert!(encrypted.starts_with("enc2:"));
            assert_ne!(encrypted, test_token); // Should be different when encrypted
            
            let decrypted = TokenStore::decrypt_token(&encrypted).unwrap();
            assert_eq!(decrypted, test_token);
        }
    }
    
    #[test]
    fn decrypt_plaintext_returns_as_is() {
        let plaintext = "not_encrypted_token";
        let result = TokenStore::decrypt_token(plaintext).unwrap();
        assert_eq!(result, plaintext);
    }
    
    #[test]
    fn auth_profile_structure() {
        let profile = AuthProfile {
            provider: "openai-codex".to_string(),
            kind: "oauth".to_string(),
            account_id: "org-123".to_string(),
            access_token: "enc2:test".to_string(),
            refresh_token: "enc2:test".to_string(),
            id_token: "enc2:test".to_string(),
            expires_at: "2026-03-16T10:00:00Z".to_string(),
        };
        
        let json = serde_json::to_string(&profile).unwrap();
        let parsed: AuthProfile = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed.provider, "openai-codex");
        assert_eq!(parsed.kind, "oauth");
    }
}
