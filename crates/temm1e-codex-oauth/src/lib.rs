//! TEMM1E Codex OAuth — OpenAI Codex subscription via OAuth PKCE
//!
//! Enables TEMM1E users to authenticate with their ChatGPT Plus/Pro subscription
//! instead of an API key. Uses the OpenAI Responses API (not Chat Completions).
//!
//! # Architecture
//!
//! This crate is intentionally isolated behind a feature flag (`codex-oauth`).
//! If OpenAI blocks third-party OAuth usage, this entire crate compiles away
//! to nothing — zero impact on the rest of TEMM1E.
//!
//! # Modules
//!
//! - `pkce` — PKCE verifier/challenge generation (S256)
//! - `token_store` — Token persistence, auto-refresh with Mutex
//! - `callback_server` — Temporary HTTP server for OAuth redirect
//! - `responses_provider` — `CodexResponsesProvider` implementing `Provider` trait

pub mod callback_server;
pub mod pkce;
pub mod responses_provider;
pub mod token_store;

pub use responses_provider::CodexResponsesProvider;
pub use token_store::{CodexOAuthTokens, TokenStore};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use temm1e_core::types::error::Temm1eError;

/// The public Codex CLI client ID.
const CLIENT_ID: &str = "app_EMoamEEZ73f0CkXaXp7hrann";
/// Authorization endpoint.
const AUTH_ENDPOINT: &str = "https://auth.openai.com/oauth/authorize";
/// Token exchange endpoint.
const TOKEN_ENDPOINT: &str = "https://auth.openai.com/oauth/token";
/// OAuth scopes.
const SCOPES: &str = "openid profile email offline_access";
/// Timeout for the OAuth callback (5 minutes).
const LOGIN_TIMEOUT_SECS: u64 = 300;

/// Run the full OAuth PKCE login flow.
///
/// If `headless` is true, prints the URL and waits for the user to paste the
/// callback URL. Otherwise, opens the browser and starts a local callback server.
///
/// Returns the token store on success.
pub async fn login(headless: bool) -> Result<TokenStore, Temm1eError> {
    let pkce_pair = pkce::PkceChallenge::generate();
    let state = pkce::generate_state();

    if headless {
        login_headless(&pkce_pair, &state).await
    } else {
        login_browser(&pkce_pair, &state).await
    }
}

/// Browser-based login: opens browser + local callback server.
async fn login_browser(pkce: &pkce::PkceChallenge, state: &str) -> Result<TokenStore, Temm1eError> {
    // Find port and build redirect_uri before starting server
    let port = find_available_port()?;
    let redirect_uri = format!("http://localhost:{}/auth/callback", port);

    let auth_url = build_auth_url(&pkce.challenge, state, &redirect_uri);

    // Open browser
    tracing::info!("Opening browser for OpenAI authentication...");
    if let Err(e) = open_browser(&auth_url) {
        tracing::warn!(error = %e, "Failed to open browser — use headless mode");
        println!("\nCould not open browser automatically.");
        println!("Open this URL manually:\n\n{}\n", auth_url);
    }

    // Wait for callback on the same port we told OpenAI about
    let (result, _port) =
        callback_server::wait_for_callback(state, LOGIN_TIMEOUT_SECS, Some(port)).await?;

    // Exchange code for tokens
    exchange_code(&result.code, &pkce.verifier, &redirect_uri).await
}

/// Headless login: prints URL, user pastes callback URL.
async fn login_headless(
    pkce: &pkce::PkceChallenge,
    state: &str,
) -> Result<TokenStore, Temm1eError> {
    let redirect_uri = "http://localhost:1455/auth/callback".to_string();
    let auth_url = build_auth_url(&pkce.challenge, state, &redirect_uri);

    println!("\n  Open this URL in your browser to authenticate:\n");
    println!("  {}\n", auth_url);
    println!("  After signing in, paste the URL you were redirected to:");

    // Read the redirect URL from stdin
    let mut input = String::new();
    std::io::stdin()
        .read_line(&mut input)
        .map_err(|e| Temm1eError::Auth(format!("Failed to read input: {}", e)))?;

    let input = input.trim();

    // Extract the code and state from the pasted URL
    let url =
        url::Url::parse(input).map_err(|e| Temm1eError::Auth(format!("Invalid URL: {}", e)))?;

    let code = url
        .query_pairs()
        .find(|(k, _)| k == "code")
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| Temm1eError::Auth("No 'code' parameter in URL".to_string()))?;

    let received_state = url
        .query_pairs()
        .find(|(k, _)| k == "state")
        .map(|(_, v)| v.to_string())
        .ok_or_else(|| Temm1eError::Auth("No 'state' parameter in URL".to_string()))?;

    if received_state != state {
        return Err(Temm1eError::Auth(
            "State mismatch — possible CSRF. Try again.".to_string(),
        ));
    }

    exchange_code(&code, &pkce.verifier, &redirect_uri).await
}

/// Build the full authorization URL.
fn build_auth_url(challenge: &str, state: &str, redirect_uri: &str) -> String {
    format!(
        "{}?client_id={}&redirect_uri={}&response_type=code&scope={}&state={}&code_challenge={}&code_challenge_method=S256&id_token_add_organizations=true&codex_cli_simplified_flow=true",
        AUTH_ENDPOINT,
        CLIENT_ID,
        urlencoding(redirect_uri),
        urlencoding(SCOPES),
        state,
        challenge,
    )
}

/// Simple URL encoding for query parameters.
fn urlencoding(s: &str) -> String {
    s.replace(' ', "+").replace(':', "%3A").replace('/', "%2F")
}

/// Exchange the authorization code for tokens.
async fn exchange_code(
    code: &str,
    verifier: &str,
    redirect_uri: &str,
) -> Result<TokenStore, Temm1eError> {
    let client = reqwest::Client::new();

    let params = [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("redirect_uri", redirect_uri),
        ("client_id", CLIENT_ID),
        ("code_verifier", verifier),
    ];

    let resp = client
        .post(TOKEN_ENDPOINT)
        .form(&params)
        .send()
        .await
        .map_err(|e| Temm1eError::Auth(format!("Token exchange request failed: {}", e)))?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        return Err(Temm1eError::Auth(format!(
            "Token exchange failed ({}): {}",
            status, body
        )));
    }

    let token_resp: TokenExchangeResponse = resp
        .json()
        .await
        .map_err(|e| Temm1eError::Auth(format!("Failed to parse token response: {}", e)))?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    // Decode email and account_id from the id_token JWT (base64 decode payload, no signature check)
    let (email, account_id) = decode_id_token(&token_resp.id_token.unwrap_or_default());

    let tokens = CodexOAuthTokens {
        access_token: token_resp.access_token,
        refresh_token: token_resp.refresh_token.unwrap_or_default(),
        expires_at: now + token_resp.expires_in.unwrap_or(3600),
        email,
        account_id,
    };

    let store = TokenStore::new(tokens.clone());
    store.save_to_disk(&tokens)?;

    // Scope probe: make a minimal test call to verify the token works
    tracing::info!("Verifying OAuth token with a test API call...");
    let probe_client = reqwest::Client::new();
    let probe_resp = probe_client
        .post("https://chatgpt.com/backend-api/codex/responses")
        .header("Authorization", format!("Bearer {}", tokens.access_token))
        .json(&serde_json::json!({
            "model": "gpt-5.4",
            "instructions": "Reply with OK",
            "input": [{"role": "user", "content": "Say OK"}],
            "store": false,
            "stream": true,
        }))
        .send()
        .await;

    match probe_resp {
        Ok(r) if r.status().is_success() => {
            tracing::info!("OAuth token verified — API access confirmed");
        }
        Ok(r) if r.status().as_u16() == 403 => {
            let body = r.text().await.unwrap_or_default();
            tracing::warn!(
                "Token lacks API access (403): {}. OAuth succeeded but your subscription may not include API access.",
                body
            );
        }
        Ok(r) => {
            let status = r.status();
            let body = r.text().await.unwrap_or_default();
            tracing::warn!(status = %status, "Probe returned unexpected status: {}", body);
        }
        Err(e) => {
            tracing::warn!(error = %e, "Probe request failed (will try anyway)");
        }
    }

    Ok(store)
}

/// Decode email and account_id from a JWT id_token (base64 payload, no signature verification).
fn decode_id_token(id_token: &str) -> (String, String) {
    let parts: Vec<&str> = id_token.split('.').collect();
    if parts.len() < 2 {
        return ("unknown".to_string(), "unknown".to_string());
    }

    let payload = match URL_SAFE_NO_PAD.decode(parts[1]) {
        Ok(bytes) => bytes,
        Err(_) => {
            // Try with padding
            let padded = format!("{}{}", parts[1], "=".repeat((4 - parts[1].len() % 4) % 4));
            match base64::engine::general_purpose::URL_SAFE.decode(&padded) {
                Ok(bytes) => bytes,
                Err(_) => return ("unknown".to_string(), "unknown".to_string()),
            }
        }
    };

    let claims: serde_json::Value =
        serde_json::from_slice(&payload).unwrap_or(serde_json::Value::Null);

    let email = claims
        .get("email")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    // Try multiple fields for account/org ID
    // Priority: account_id > org_id > organization_id > https://api.openai.com/account_id > sub
    let account_id = claims
        .get("account_id")
        .or_else(|| claims.get("org_id"))
        .or_else(|| claims.get("organization_id"))
        .or_else(|| claims.get("https://api.openai.com/account_id"))
        .or_else(|| claims.get("sub"))
        .and_then(|v| v.as_str())
        .unwrap_or("unknown")
        .to_string();

    (email, account_id)
}

/// Open a URL in the default browser.
fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to open browser on macOS: {}", e))?;
    }
    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(url)
            .spawn()
            .map_err(|e| format!("Failed to open browser on Linux: {}", e))?;
    }
    #[cfg(target_os = "windows")]
    {
        // Try PowerShell first (more reliable for complex URLs)
        let powershell_result = std::process::Command::new("powershell")
            .args(["-Command", &format!("Start-Process '{}'", url)])
            .spawn();
            
        match powershell_result {
            Ok(_) => {
                // PowerShell succeeded
            }
            Err(_) => {
                // Fallback to CMD with proper quoting
                // Use: cmd /c start "" "URL"
                std::process::Command::new("cmd")
                    .args(["/c", "start", "", url])
                    .spawn()
                    .map_err(|e| format!("Failed to open browser on Windows (both PowerShell and CMD failed): {}", e))?;
            }
        }
    }
    #[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
    {
        return Err("Unsupported operating system for browser opening".to_string());
    }
    Ok(())
}

/// Find an available port (same as callback_server but accessible from lib).
fn find_available_port() -> Result<u16, Temm1eError> {
    for port in 1455..1555 {
        if std::net::TcpListener::bind(format!("127.0.0.1:{}", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(Temm1eError::Auth(
        "Could not find an available port for OAuth callback".to_string(),
    ))
}

/// Token exchange response from OpenAI.
#[derive(serde::Deserialize)]
struct TokenExchangeResponse {
    access_token: String,
    refresh_token: Option<String>,
    #[serde(default)]
    expires_in: Option<u64>,
    id_token: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_url_contains_required_params() {
        let url = build_auth_url(
            "test_challenge",
            "test_state",
            "http://localhost:1455/auth/callback",
        );
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("code_challenge=test_challenge"));
        assert!(url.contains("state=test_state"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("openid"));
    }

    #[test]
    fn decode_id_token_extracts_email_and_account() {
        // Build a fake JWT with email and account_id in payload
        let payload = serde_json::json!({
            "email": "test@example.com",
            "sub": "user-123",
            "account_id": "org-abc"
        });
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let fake_jwt = format!("header.{}.signature", payload_b64);

        let (email, account_id) = decode_id_token(&fake_jwt);
        assert_eq!(email, "test@example.com");
        assert_eq!(account_id, "org-abc");
    }
    
    #[test]
    fn decode_id_token_prioritizes_account_id_field() {
        // Test priority: account_id > org_id > sub
        let payload = serde_json::json!({
            "email": "test@example.com",
            "sub": "user-123",
            "org_id": "org-456",
            "account_id": "org-789"
        });
        let payload_b64 = URL_SAFE_NO_PAD.encode(serde_json::to_vec(&payload).unwrap());
        let fake_jwt = format!("header.{}.signature", payload_b64);

        let (_, account_id) = decode_id_token(&fake_jwt);
        assert_eq!(account_id, "org-789"); // account_id has highest priority
    }

    #[test]
    fn decode_invalid_jwt_returns_unknown() {
        let (email, account_id) = decode_id_token("not-a-jwt");
        assert_eq!(email, "unknown");
        assert_eq!(account_id, "unknown");
    }

    #[test]
    fn urlencoding_encodes_spaces_and_colons() {
        assert_eq!(urlencoding("hello world"), "hello+world");
        assert_eq!(
            urlencoding("http://example.com"),
            "http%3A%2F%2Fexample.com"
        );
    }
    
    #[test]
    fn open_browser_handles_complex_urls() {
        let complex_url = "https://auth.openai.com/oauth/authorize?client_id=test&redirect_uri=http://localhost:1455/callback&response_type=code&scope=openid+profile+email&state=abc123&code_challenge=xyz&code_challenge_method=S256";
        
        // This should not panic on any OS
        // We can't easily test actual browser opening in unit tests,
        // but we can ensure the function doesn't crash with complex URLs
        let result = open_browser(complex_url);
        
        // On supported OS, it should either succeed or fail gracefully
        // On unsupported OS, it should return an error message
        match result {
            Ok(_) => {
                // Success - browser command was spawned
            }
            Err(msg) => {
                // Should be a descriptive error message
                assert!(!msg.is_empty());
                // On unsupported OS, should mention that
                if cfg!(not(any(target_os = "windows", target_os = "macos", target_os = "linux"))) {
                    assert!(msg.contains("Unsupported operating system"));
                }
            }
        }
    }
    
    #[test]
    fn pkce_and_state_generation_works() {
        let pkce = pkce::PkceChallenge::generate();
        let state = pkce::generate_state();
        
        // PKCE verifier should be 43 characters (32 bytes base64url)
        assert_eq!(pkce.verifier.len(), 43);
        
        // Challenge should be different from verifier (SHA256 hash)
        assert_ne!(pkce.verifier, pkce.challenge);
        
        // State should be non-empty and unique
        assert!(!state.is_empty());
        let state2 = pkce::generate_state();
        assert_ne!(state, state2);
    }
    
    #[test]
    fn auth_url_building_cross_platform() {
        let challenge = "test_challenge_123";
        let state = "test_state_456";
        let redirect_uri = "http://localhost:1455/auth/callback";
        
        let url = build_auth_url(challenge, state, redirect_uri);
        
        // Should contain all required OAuth parameters
        assert!(url.contains("client_id=app_EMoamEEZ73f0CkXaXp7hrann"));
        assert!(url.contains("code_challenge=test_challenge_123"));
        assert!(url.contains("state=test_state_456"));
        assert!(url.contains("response_type=code"));
        assert!(url.contains("code_challenge_method=S256"));
        assert!(url.contains("scope="));
        assert!(url.contains("openid"));
        assert!(url.contains("redirect_uri="));
        
        // URL should be properly encoded
        assert!(url.contains("http%3A%2F%2Flocalhost%3A1455"));
    }
    
    #[test]
    fn find_available_port_works() {
        // This should work on all OS since it's just TCP binding
        let port = find_available_port();
        assert!(port.is_ok());
        
        let port_num = port.unwrap();
        assert!(port_num >= 1455);
        assert!(port_num < 1555);
        
        // Port should actually be available
        let listener = std::net::TcpListener::bind(format!("127.0.0.1:{}", port_num));
        assert!(listener.is_ok());
    }
}
