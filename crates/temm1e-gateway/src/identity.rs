//! OAuth Identity Manager — implements the `Identity` trait with OAuth 2.0
//! provider support, PKCE flow, and in-memory user/token management.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use rand::Rng;
use serde::{Deserialize, Serialize};
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

use temm1e_core::types::error::Temm1eError;
use temm1e_core::{AuthResult, Identity};

/// Default permissions assigned to newly registered users.
const DEFAULT_PERMISSIONS: &[&str] = &["read", "execute"];

/// Maximum age of a pending OAuth flow before it is considered stale (seconds).
const FLOW_EXPIRY_SECONDS: i64 = 600;

// ── Data types ───────────────────────────────────────────────────────────────

/// A registered user record with channel binding, permissions, and OAuth tokens.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserRecord {
    pub user_id: String,
    pub channel: String,
    pub permissions: Vec<String>,
    pub oauth_tokens: HashMap<String, OAuthToken>,
    pub created_at: DateTime<Utc>,
}

/// Configuration for a supported OAuth 2.0 provider.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthProviderConfig {
    pub name: String,
    pub client_id: String,
    pub client_secret: String,
    pub auth_url: String,
    pub token_url: String,
    pub scopes: Vec<String>,
    pub redirect_uri: String,
}

/// An OAuth token (access + optional refresh).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthToken {
    pub access_token: String,
    pub refresh_token: Option<String>,
    pub expires_at: Option<DateTime<Utc>>,
    pub provider: String,
}

/// State for an in-progress OAuth flow.
#[derive(Debug, Clone)]
pub struct OAuthFlowState {
    pub state_param: String,
    pub user_id: String,
    pub provider: String,
    pub created_at: DateTime<Utc>,
    pub pkce_verifier: Option<String>,
}

// ── Token exchange response (from provider) ──────────────────────────────────

#[derive(Debug, Deserialize)]
struct TokenResponse {
    access_token: String,
    refresh_token: Option<String>,
    expires_in: Option<i64>,
    #[serde(default)]
    token_type: String,
}

// ── OAuthIdentityManager ─────────────────────────────────────────────────────

/// In-memory identity manager that implements OAuth 2.0 flows.
pub struct OAuthIdentityManager {
    users: RwLock<HashMap<String, UserRecord>>,
    oauth_configs: Vec<OAuthProviderConfig>,
    pending_flows: RwLock<HashMap<String, OAuthFlowState>>,
    http_client: reqwest::Client,
}

impl OAuthIdentityManager {
    /// Create a new identity manager with the given OAuth provider configurations.
    pub fn new(oauth_configs: Vec<OAuthProviderConfig>) -> Self {
        Self {
            users: RwLock::new(HashMap::new()),
            oauth_configs,
            pending_flows: RwLock::new(HashMap::new()),
            http_client: reqwest::Client::new(),
        }
    }

    /// Create an identity manager with a custom HTTP client (useful for testing).
    #[cfg(test)]
    pub fn with_client(
        oauth_configs: Vec<OAuthProviderConfig>,
        http_client: reqwest::Client,
    ) -> Self {
        Self {
            users: RwLock::new(HashMap::new()),
            oauth_configs,
            pending_flows: RwLock::new(HashMap::new()),
            http_client,
        }
    }

    // ── OAuth flow methods ───────────────────────────────────────────────

    /// Start an OAuth flow for a user.
    ///
    /// Generates a CSRF state parameter, an optional PKCE code verifier/challenge,
    /// stores the flow state, and returns the authorization URL the user should
    /// visit.
    pub async fn start_oauth_flow(
        &self,
        user_id: &str,
        provider: &str,
    ) -> Result<String, Temm1eError> {
        let config = self
            .find_provider_config(provider)
            .ok_or_else(|| Temm1eError::Auth(format!("Unknown OAuth provider: {}", provider)))?
            .clone();

        let state_param = uuid::Uuid::new_v4().to_string();
        let pkce_verifier = generate_pkce_verifier();
        let pkce_challenge = generate_pkce_challenge(&pkce_verifier);

        let flow_state = OAuthFlowState {
            state_param: state_param.clone(),
            user_id: user_id.to_string(),
            provider: provider.to_string(),
            created_at: Utc::now(),
            pkce_verifier: Some(pkce_verifier),
        };

        {
            let mut flows = self.pending_flows.write().await;
            flows.insert(state_param.clone(), flow_state);
        }

        let scopes = config.scopes.join(" ");
        let auth_url = format!(
            "{}?client_id={}&redirect_uri={}&response_type=code&state={}&scope={}&code_challenge={}&code_challenge_method=S256",
            config.auth_url,
            urlencoding(&config.client_id),
            urlencoding(&config.redirect_uri),
            urlencoding(&state_param),
            urlencoding(&scopes),
            urlencoding(&pkce_challenge),
        );

        info!(
            user_id = %user_id,
            provider = %provider,
            "Started OAuth flow"
        );

        Ok(auth_url)
    }

    /// Complete an OAuth flow by exchanging the authorization code for tokens.
    ///
    /// Validates the `state` parameter, exchanges `code` at the provider's
    /// token endpoint, stores the resulting token on the user record, and
    /// returns the token.
    pub async fn complete_oauth_flow(
        &self,
        state: &str,
        code: &str,
    ) -> Result<OAuthToken, Temm1eError> {
        // Look up and remove the pending flow
        let flow = {
            let mut flows = self.pending_flows.write().await;
            flows.remove(state).ok_or_else(|| {
                Temm1eError::Auth("Invalid or expired OAuth state parameter".to_string())
            })?
        };

        // Check flow expiry
        let elapsed = Utc::now()
            .signed_duration_since(flow.created_at)
            .num_seconds();
        if elapsed > FLOW_EXPIRY_SECONDS {
            return Err(Temm1eError::Auth(
                "OAuth flow has expired — please restart the authorization".to_string(),
            ));
        }

        let config = self
            .find_provider_config(&flow.provider)
            .ok_or_else(|| {
                Temm1eError::Auth(format!("Provider config not found: {}", flow.provider))
            })?
            .clone();

        // Build token exchange request
        let mut params = vec![
            ("grant_type", "authorization_code".to_string()),
            ("code", code.to_string()),
            ("redirect_uri", config.redirect_uri.clone()),
            ("client_id", config.client_id.clone()),
            ("client_secret", config.client_secret.clone()),
        ];
        if let Some(ref verifier) = flow.pkce_verifier {
            params.push(("code_verifier", verifier.clone()));
        }

        let resp = self
            .http_client
            .post(&config.token_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| Temm1eError::Auth(format!("Token exchange HTTP error: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            return Err(Temm1eError::Auth(format!(
                "Token endpoint returned {}: {}",
                status, body
            )));
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .map_err(|e| Temm1eError::Auth(format!("Failed to parse token response: {}", e)))?;

        let expires_at = token_resp
            .expires_in
            .map(|secs| Utc::now() + chrono::Duration::seconds(secs));

        let token = OAuthToken {
            access_token: token_resp.access_token,
            refresh_token: token_resp.refresh_token,
            expires_at,
            provider: flow.provider.clone(),
        };

        // Store the token on the user
        {
            let mut users = self.users.write().await;
            if let Some(user) = users.get_mut(&flow.user_id) {
                user.oauth_tokens
                    .insert(flow.provider.clone(), token.clone());
            }
        }

        debug!(
            user_id = %flow.user_id,
            provider = %flow.provider,
            token_type = %token_resp.token_type,
            "OAuth flow completed"
        );

        Ok(token)
    }

    /// Refresh an expired OAuth token for a user/provider pair.
    pub async fn refresh_token(
        &self,
        user_id: &str,
        provider: &str,
    ) -> Result<OAuthToken, Temm1eError> {
        let (refresh_tok, config) = {
            let users = self.users.read().await;
            let user = users
                .get(user_id)
                .ok_or_else(|| Temm1eError::Auth(format!("User not found: {}", user_id)))?;
            let current = user.oauth_tokens.get(provider).ok_or_else(|| {
                Temm1eError::Auth(format!(
                    "No token for provider '{}' on user '{}'",
                    provider, user_id
                ))
            })?;
            let refresh = current
                .refresh_token
                .clone()
                .ok_or_else(|| Temm1eError::Auth("No refresh token available".to_string()))?;
            let cfg = self
                .find_provider_config(provider)
                .ok_or_else(|| Temm1eError::Auth(format!("Unknown OAuth provider: {}", provider)))?
                .clone();
            (refresh, cfg)
        };

        let params = [
            ("grant_type", "refresh_token"),
            ("refresh_token", &refresh_tok),
            ("client_id", &config.client_id),
            ("client_secret", &config.client_secret),
        ];

        let resp = self
            .http_client
            .post(&config.token_url)
            .form(&params)
            .send()
            .await
            .map_err(|e| Temm1eError::Auth(format!("Token refresh HTTP error: {}", e)))?;

        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp
                .text()
                .await
                .unwrap_or_else(|_| "<unreadable>".to_string());
            return Err(Temm1eError::Auth(format!(
                "Token refresh endpoint returned {}: {}",
                status, body
            )));
        }

        let token_resp: TokenResponse = resp
            .json()
            .await
            .map_err(|e| Temm1eError::Auth(format!("Failed to parse refresh response: {}", e)))?;

        let expires_at = token_resp
            .expires_in
            .map(|secs| Utc::now() + chrono::Duration::seconds(secs));

        let token = OAuthToken {
            access_token: token_resp.access_token,
            refresh_token: token_resp.refresh_token.or(Some(refresh_tok)),
            expires_at,
            provider: provider.to_string(),
        };

        // Update stored token
        {
            let mut users = self.users.write().await;
            if let Some(user) = users.get_mut(user_id) {
                user.oauth_tokens
                    .insert(provider.to_string(), token.clone());
            }
        }

        info!(user_id = %user_id, provider = %provider, "Refreshed OAuth token");

        Ok(token)
    }

    /// Check whether a token is expired (or has no expiry information).
    pub fn is_token_expired(token: &OAuthToken) -> bool {
        match token.expires_at {
            Some(exp) => Utc::now() >= exp,
            None => false, // Tokens without expiry are treated as non-expiring
        }
    }

    // ── Query helpers ────────────────────────────────────────────────────

    /// Get a user record by ID.
    pub async fn get_user(&self, user_id: &str) -> Option<UserRecord> {
        let users = self.users.read().await;
        users.get(user_id).cloned()
    }

    /// Get the number of pending OAuth flows.
    pub async fn pending_flow_count(&self) -> usize {
        let flows = self.pending_flows.read().await;
        flows.len()
    }

    /// Get a pending flow by state parameter (for inspection/testing).
    pub async fn get_pending_flow(&self, state: &str) -> Option<OAuthFlowState> {
        let flows = self.pending_flows.read().await;
        flows.get(state).cloned()
    }

    // ── Internal helpers ─────────────────────────────────────────────────

    /// Look up an OAuth provider config by name.
    fn find_provider_config(&self, name: &str) -> Option<&OAuthProviderConfig> {
        self.oauth_configs.iter().find(|c| c.name == name)
    }
}

// ── Identity trait implementation ────────────────────────────────────────────

#[async_trait]
impl Identity for OAuthIdentityManager {
    /// Authenticate a user: Allowed if registered, NeedsSetup otherwise.
    async fn authenticate(&self, channel: &str, user_id: &str) -> Result<AuthResult, Temm1eError> {
        let users = self.users.read().await;
        match users.get(user_id) {
            Some(user) => {
                if user.channel != channel {
                    warn!(
                        user_id = %user_id,
                        expected_channel = %user.channel,
                        actual_channel = %channel,
                        "Channel mismatch during authentication"
                    );
                    Ok(AuthResult::Denied {
                        reason: format!("User registered on '{}', not '{}'", user.channel, channel),
                    })
                } else {
                    Ok(AuthResult::Allowed)
                }
            }
            None => Ok(AuthResult::NeedsSetup),
        }
    }

    /// Check if a user has a specific permission.
    async fn has_permission(&self, user_id: &str, permission: &str) -> Result<bool, Temm1eError> {
        let users = self.users.read().await;
        match users.get(user_id) {
            Some(user) => Ok(user.permissions.iter().any(|p| p == permission)),
            None => Err(Temm1eError::Auth(format!("User not found: {}", user_id))),
        }
    }

    /// Register a new user with default permissions.
    async fn register_user(&self, user_id: &str, channel: &str) -> Result<(), Temm1eError> {
        let mut users = self.users.write().await;
        if users.contains_key(user_id) {
            return Err(Temm1eError::Auth(format!(
                "User already registered: {}",
                user_id
            )));
        }

        let record = UserRecord {
            user_id: user_id.to_string(),
            channel: channel.to_string(),
            permissions: DEFAULT_PERMISSIONS.iter().map(|p| p.to_string()).collect(),
            oauth_tokens: HashMap::new(),
            created_at: Utc::now(),
        };

        users.insert(user_id.to_string(), record);
        info!(user_id = %user_id, channel = %channel, "Registered new user");

        Ok(())
    }
}

// ── PKCE helpers ─────────────────────────────────────────────────────────────

/// Generate a cryptographically random PKCE code verifier (43-128 chars, RFC 7636).
fn generate_pkce_verifier() -> String {
    let mut rng = rand::thread_rng();
    let bytes: Vec<u8> = (0..32).map(|_| rng.gen::<u8>()).collect();
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, &bytes)
}

/// Derive the PKCE code challenge from a verifier (S256 method).
fn generate_pkce_challenge(verifier: &str) -> String {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(verifier.as_bytes());
    base64::Engine::encode(&base64::engine::general_purpose::URL_SAFE_NO_PAD, digest)
}

/// Percent-encode a string for use in a URL query parameter.
fn urlencoding(s: &str) -> String {
    url::form_urlencoded::byte_serialize(s.as_bytes()).collect()
}

// ── Axum handler for GET /auth/callback ──────────────────────────────────────

/// Query parameters received on the OAuth callback endpoint.
#[derive(Debug, Deserialize)]
pub struct OAuthCallbackParams {
    pub state: String,
    pub code: String,
}

/// OAuth callback response body.
#[derive(Debug, Serialize)]
pub struct OAuthCallbackResponse {
    pub status: String,
    pub provider: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// Axum handler for `GET /auth/callback?state=...&code=...`.
///
/// Completes the OAuth flow, exchanging the authorization code for tokens.
pub async fn oauth_callback_handler(
    axum::extract::State(identity): axum::extract::State<Arc<OAuthIdentityManager>>,
    axum::extract::Query(params): axum::extract::Query<OAuthCallbackParams>,
) -> axum::response::Response {
    use axum::http::StatusCode;
    use axum::response::IntoResponse;
    use axum::Json;

    match identity
        .complete_oauth_flow(&params.state, &params.code)
        .await
    {
        Ok(token) => {
            let resp = OAuthCallbackResponse {
                status: "ok".to_string(),
                provider: token.provider,
                error: None,
            };
            (StatusCode::OK, Json(resp)).into_response()
        }
        Err(e) => {
            warn!(error = %e, "OAuth callback failed");
            let resp = OAuthCallbackResponse {
                status: "error".to_string(),
                provider: String::new(),
                error: Some(e.to_string()),
            };
            (StatusCode::BAD_REQUEST, Json(resp)).into_response()
        }
    }
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    /// Helper: build a minimal GitHub-like OAuth provider config.
    fn github_config() -> OAuthProviderConfig {
        OAuthProviderConfig {
            name: "github".to_string(),
            client_id: "test_client_id".to_string(),
            client_secret: "test_client_secret".to_string(),
            auth_url: "https://github.com/login/oauth/authorize".to_string(),
            token_url: "https://github.com/login/oauth/access_token".to_string(),
            scopes: vec!["repo".to_string(), "user".to_string()],
            redirect_uri: "http://localhost:8080/auth/callback".to_string(),
        }
    }

    /// Helper: build a Google-like OAuth provider config.
    fn google_config() -> OAuthProviderConfig {
        OAuthProviderConfig {
            name: "google".to_string(),
            client_id: "google_client_id".to_string(),
            client_secret: "google_client_secret".to_string(),
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth".to_string(),
            token_url: "https://oauth2.googleapis.com/token".to_string(),
            scopes: vec!["openid".to_string(), "profile".to_string()],
            redirect_uri: "http://localhost:8080/auth/callback".to_string(),
        }
    }

    /// Helper: build a manager with the GitHub config.
    fn make_manager() -> OAuthIdentityManager {
        OAuthIdentityManager::new(vec![github_config()])
    }

    /// Helper: build a manager with both GitHub and Google configs.
    fn make_multi_provider_manager() -> OAuthIdentityManager {
        OAuthIdentityManager::new(vec![github_config(), google_config()])
    }

    // ── Registration tests ───────────────────────────────────────────────

    #[tokio::test]
    async fn register_user_creates_record_with_defaults() {
        let mgr = make_manager();
        mgr.register_user("user1", "telegram").await.unwrap();

        let user = mgr.get_user("user1").await.unwrap();
        assert_eq!(user.user_id, "user1");
        assert_eq!(user.channel, "telegram");
        assert!(user.permissions.contains(&"read".to_string()));
        assert!(user.permissions.contains(&"execute".to_string()));
        assert!(user.oauth_tokens.is_empty());
    }

    #[tokio::test]
    async fn register_duplicate_user_fails() {
        let mgr = make_manager();
        mgr.register_user("user1", "telegram").await.unwrap();
        let result = mgr.register_user("user1", "discord").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("already registered"));
    }

    // ── Authentication tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn authenticate_unregistered_user_returns_needs_setup() {
        let mgr = make_manager();
        let result = mgr.authenticate("telegram", "unknown").await.unwrap();
        assert!(matches!(result, AuthResult::NeedsSetup));
    }

    #[tokio::test]
    async fn authenticate_registered_user_returns_allowed() {
        let mgr = make_manager();
        mgr.register_user("user1", "telegram").await.unwrap();
        let result = mgr.authenticate("telegram", "user1").await.unwrap();
        assert!(matches!(result, AuthResult::Allowed));
    }

    #[tokio::test]
    async fn authenticate_wrong_channel_returns_denied() {
        let mgr = make_manager();
        mgr.register_user("user1", "telegram").await.unwrap();
        let result = mgr.authenticate("discord", "user1").await.unwrap();
        assert!(matches!(result, AuthResult::Denied { .. }));
        if let AuthResult::Denied { reason } = result {
            assert!(reason.contains("telegram"));
            assert!(reason.contains("discord"));
        }
    }

    // ── Permission tests ─────────────────────────────────────────────────

    #[tokio::test]
    async fn has_permission_returns_true_for_default_perms() {
        let mgr = make_manager();
        mgr.register_user("user1", "cli").await.unwrap();
        assert!(mgr.has_permission("user1", "read").await.unwrap());
        assert!(mgr.has_permission("user1", "execute").await.unwrap());
    }

    #[tokio::test]
    async fn has_permission_returns_false_for_missing_perm() {
        let mgr = make_manager();
        mgr.register_user("user1", "cli").await.unwrap();
        assert!(!mgr.has_permission("user1", "admin").await.unwrap());
    }

    #[tokio::test]
    async fn has_permission_errors_for_unknown_user() {
        let mgr = make_manager();
        let result = mgr.has_permission("ghost", "read").await;
        assert!(result.is_err());
    }

    // ── OAuth URL generation tests ───────────────────────────────────────

    #[tokio::test]
    async fn start_oauth_flow_returns_valid_url() {
        let mgr = make_manager();
        mgr.register_user("user1", "telegram").await.unwrap();

        let url = mgr.start_oauth_flow("user1", "github").await.unwrap();

        assert!(url.starts_with("https://github.com/login/oauth/authorize?"));
        assert!(url.contains("client_id=test_client_id"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("state="));
        assert!(url.contains("scope=repo+user"));
        assert!(url.contains("code_challenge="));
        assert!(url.contains("code_challenge_method=S256"));
    }

    #[tokio::test]
    async fn start_oauth_flow_unknown_provider_fails() {
        let mgr = make_manager();
        let result = mgr.start_oauth_flow("user1", "gitlab").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Unknown OAuth provider"));
    }

    // ── Flow state management tests ──────────────────────────────────────

    #[tokio::test]
    async fn start_flow_creates_pending_state() {
        let mgr = make_manager();
        mgr.register_user("user1", "telegram").await.unwrap();

        assert_eq!(mgr.pending_flow_count().await, 0);
        let url = mgr.start_oauth_flow("user1", "github").await.unwrap();
        assert_eq!(mgr.pending_flow_count().await, 1);

        // Extract state param from the URL
        let parsed = url::Url::parse(&url).unwrap();
        let state = parsed
            .query_pairs()
            .find(|(k, _)| k == "state")
            .unwrap()
            .1
            .to_string();

        let flow = mgr.get_pending_flow(&state).await.unwrap();
        assert_eq!(flow.user_id, "user1");
        assert_eq!(flow.provider, "github");
        assert!(flow.pkce_verifier.is_some());
    }

    #[tokio::test]
    async fn complete_flow_with_invalid_state_fails() {
        let mgr = make_manager();
        let result = mgr.complete_oauth_flow("bogus_state", "code123").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("Invalid or expired"));
    }

    #[tokio::test]
    async fn complete_flow_with_expired_state_fails() {
        let mgr = make_manager();

        // Manually insert an expired flow
        let state = "expired_state".to_string();
        let flow = OAuthFlowState {
            state_param: state.clone(),
            user_id: "user1".to_string(),
            provider: "github".to_string(),
            created_at: Utc::now() - Duration::seconds(FLOW_EXPIRY_SECONDS + 60),
            pkce_verifier: None,
        };
        {
            let mut flows = mgr.pending_flows.write().await;
            flows.insert(state.clone(), flow);
        }

        let result = mgr.complete_oauth_flow(&state, "code123").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("expired"));
    }

    // ── Token expiry tests ───────────────────────────────────────────────

    #[test]
    fn is_token_expired_true_for_past_expiry() {
        let token = OAuthToken {
            access_token: "tok".to_string(),
            refresh_token: None,
            expires_at: Some(Utc::now() - Duration::seconds(60)),
            provider: "github".to_string(),
        };
        assert!(OAuthIdentityManager::is_token_expired(&token));
    }

    #[test]
    fn is_token_expired_false_for_future_expiry() {
        let token = OAuthToken {
            access_token: "tok".to_string(),
            refresh_token: None,
            expires_at: Some(Utc::now() + Duration::hours(1)),
            provider: "github".to_string(),
        };
        assert!(!OAuthIdentityManager::is_token_expired(&token));
    }

    #[test]
    fn is_token_expired_false_when_no_expiry() {
        let token = OAuthToken {
            access_token: "tok".to_string(),
            refresh_token: None,
            expires_at: None,
            provider: "github".to_string(),
        };
        assert!(!OAuthIdentityManager::is_token_expired(&token));
    }

    // ── Multi-provider tests ─────────────────────────────────────────────

    #[tokio::test]
    async fn multiple_providers_generate_distinct_urls() {
        let mgr = make_multi_provider_manager();
        mgr.register_user("user1", "telegram").await.unwrap();

        let github_url = mgr.start_oauth_flow("user1", "github").await.unwrap();
        let google_url = mgr.start_oauth_flow("user1", "google").await.unwrap();

        assert!(github_url.contains("github.com"));
        assert!(google_url.contains("accounts.google.com"));
        assert_eq!(mgr.pending_flow_count().await, 2);
    }

    // ── PKCE tests ───────────────────────────────────────────────────────

    #[test]
    fn pkce_verifier_has_correct_length() {
        let verifier = generate_pkce_verifier();
        // Base64url of 32 bytes → 43 chars
        assert_eq!(verifier.len(), 43);
    }

    #[test]
    fn pkce_challenge_is_deterministic_for_same_verifier() {
        let verifier = "test_verifier_string_for_determinism";
        let c1 = generate_pkce_challenge(verifier);
        let c2 = generate_pkce_challenge(verifier);
        assert_eq!(c1, c2);
    }

    #[test]
    fn pkce_challenge_differs_from_verifier() {
        let verifier = generate_pkce_verifier();
        let challenge = generate_pkce_challenge(&verifier);
        assert_ne!(verifier, challenge);
    }

    // ── UserRecord serialization test ────────────────────────────────────

    #[test]
    fn user_record_serializes_to_json() {
        let record = UserRecord {
            user_id: "u1".to_string(),
            channel: "telegram".to_string(),
            permissions: vec!["read".to_string()],
            oauth_tokens: HashMap::new(),
            created_at: Utc::now(),
        };
        let json = serde_json::to_value(&record).unwrap();
        assert_eq!(json["user_id"], "u1");
        assert_eq!(json["channel"], "telegram");
    }

    // ── Refresh token error path tests ───────────────────────────────────

    #[tokio::test]
    async fn refresh_token_fails_for_unknown_user() {
        let mgr = make_manager();
        let result = mgr.refresh_token("ghost", "github").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("User not found"));
    }

    #[tokio::test]
    async fn refresh_token_fails_when_no_token_stored() {
        let mgr = make_manager();
        mgr.register_user("user1", "telegram").await.unwrap();
        let result = mgr.refresh_token("user1", "github").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No token"));
    }

    #[tokio::test]
    async fn refresh_token_fails_when_no_refresh_token() {
        let mgr = make_manager();
        mgr.register_user("user1", "telegram").await.unwrap();

        // Manually store a token without a refresh_token
        {
            let mut users = mgr.users.write().await;
            let user = users.get_mut("user1").unwrap();
            user.oauth_tokens.insert(
                "github".to_string(),
                OAuthToken {
                    access_token: "access_only".to_string(),
                    refresh_token: None,
                    expires_at: None,
                    provider: "github".to_string(),
                },
            );
        }

        let result = mgr.refresh_token("user1", "github").await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No refresh token"));
    }
}
