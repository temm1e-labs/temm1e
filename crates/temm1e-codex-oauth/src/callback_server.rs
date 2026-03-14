//! Temporary HTTP server for the OAuth callback.
//!
//! Binds to `127.0.0.1:{port}` and waits for the OAuth redirect with the
//! authorization code. Shuts down immediately after receiving the code.

use axum::{extract::Query, response::Html, routing::get, Router};
use serde::Deserialize;
use std::net::TcpListener;
use temm1e_core::types::error::Temm1eError;
use tokio::sync::oneshot;

/// Query parameters from the OAuth callback redirect.
#[derive(Deserialize)]
pub struct CallbackParams {
    pub code: Option<String>,
    pub state: Option<String>,
    pub error: Option<String>,
    pub error_description: Option<String>,
}

/// Result from the callback server — the authorization code.
pub struct CallbackResult {
    pub code: String,
    pub state: String,
}

/// Start a temporary callback server and wait for the OAuth redirect.
///
/// Returns the authorization code from the callback, or an error if the
/// callback contains an error or times out.
pub async fn wait_for_callback(
    expected_state: &str,
    timeout_secs: u64,
    port: Option<u16>,
) -> Result<(CallbackResult, u16), Temm1eError> {
    let (tx, rx) = oneshot::channel::<Result<CallbackResult, Temm1eError>>();
    let tx = std::sync::Arc::new(std::sync::Mutex::new(Some(tx)));

    let expected = expected_state.to_string();
    let tx_clone = tx.clone();

    let app = Router::new().route(
        "/auth/callback",
        get(move |Query(params): Query<CallbackParams>| {
            let tx = tx_clone.clone();
            let expected = expected.clone();
            async move {
                let mut guard = tx.lock().unwrap();
                if let Some(sender) = guard.take() {
                    if let Some(error) = params.error {
                        let desc = params.error_description.unwrap_or_default();
                        let _ = sender.send(Err(Temm1eError::Auth(format!(
                            "OAuth error: {} — {}",
                            error, desc
                        ))));
                        return Html(error_page(&error, &desc));
                    }

                    match (params.code, params.state) {
                        (Some(code), Some(state)) => {
                            if state != expected {
                                let _ = sender.send(Err(Temm1eError::Auth(
                                    "OAuth state mismatch — possible CSRF attack".to_string(),
                                )));
                                return Html(error_page("State mismatch", "Security check failed"));
                            }
                            let _ = sender.send(Ok(CallbackResult { code, state }));
                            Html(success_page())
                        }
                        _ => {
                            let _ = sender.send(Err(Temm1eError::Auth(
                                "Missing code or state in OAuth callback".to_string(),
                            )));
                            Html(error_page(
                                "Missing parameters",
                                "The callback URL is missing required parameters",
                            ))
                        }
                    }
                } else {
                    Html(error_page(
                        "Already processed",
                        "This callback has already been handled",
                    ))
                }
            }
        }),
    );

    // Use the provided port (from login_browser) or find one
    let port = match port {
        Some(p) => p,
        None => find_available_port()?,
    };
    let addr = format!("127.0.0.1:{}", port);
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .map_err(|e| Temm1eError::Auth(format!("Failed to bind callback server: {}", e)))?;

    tracing::debug!(port = port, "OAuth callback server listening");

    // Spawn the server in the background
    let server = tokio::spawn(async move {
        axum::serve(listener, app).await.ok();
    });

    // Wait for the callback or timeout
    let result = tokio::time::timeout(std::time::Duration::from_secs(timeout_secs), rx)
        .await
        .map_err(|_| {
            Temm1eError::Auth(format!(
                "OAuth login timed out after {}s — did you complete the browser login?",
                timeout_secs
            ))
        })?
        .map_err(|_| Temm1eError::Auth("OAuth callback channel closed".to_string()))?;

    server.abort();
    result.map(|r| (r, port))
}

/// Find an available TCP port starting from 1455.
fn find_available_port() -> Result<u16, Temm1eError> {
    for port in 1455..1555 {
        if TcpListener::bind(format!("127.0.0.1:{}", port)).is_ok() {
            return Ok(port);
        }
    }
    Err(Temm1eError::Auth(
        "Could not find an available port for OAuth callback (tried 1455-1554)".to_string(),
    ))
}

fn success_page() -> String {
    r#"<!DOCTYPE html>
<html><head><title>TEMM1E — Authenticated</title></head>
<body style="font-family:system-ui;display:flex;justify-content:center;align-items:center;height:100vh;margin:0;background:#0a0a0a;color:#e0e0e0;">
<div style="text-align:center;max-width:400px;">
<h1 style="color:#4ade80;">Authentication Successful</h1>
<p>You can close this tab and return to TEMM1E.</p>
</div></body></html>"#
        .to_string()
}

fn error_page(error: &str, description: &str) -> String {
    format!(
        r#"<!DOCTYPE html>
<html><head><title>TEMM1E — Auth Error</title></head>
<body style="font-family:system-ui;display:flex;justify-content:center;align-items:center;height:100vh;margin:0;background:#0a0a0a;color:#e0e0e0;">
<div style="text-align:center;max-width:400px;">
<h1 style="color:#f87171;">Authentication Failed</h1>
<p><strong>{}</strong></p>
<p>{}</p>
<p>Please try again.</p>
</div></body></html>"#,
        error, description
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_port_succeeds() {
        let port = find_available_port().unwrap();
        assert!(port >= 1455);
        assert!(port < 1555);
    }
    
    #[test]
    fn find_port_cross_platform() {
        // Should work on all OS since it's just TCP binding
        let port1 = find_available_port().unwrap();
        let port2 = find_available_port().unwrap();
        
        // Should find different ports
        assert_ne!(port1, port2);
        
        // Both should be in valid range
        assert!(port1 >= 1455 && port1 < 1555);
        assert!(port2 >= 1455 && port2 < 1555);
        
        // Should actually be bindable on all OS
        let listener1 = std::net::TcpListener::bind(format!("127.0.0.1:{}", port1));
        let listener2 = std::net::TcpListener::bind(format!("127.0.0.1:{}", port2));
        
        assert!(listener1.is_ok());
        assert!(listener2.is_ok());
    }

    #[test]
    fn success_page_contains_message() {
        let html = success_page();
        assert!(html.contains("Authentication Successful"));
    }

    #[test]
    fn error_page_contains_error() {
        let html = error_page("test_error", "test_desc");
        assert!(html.contains("test_error"));
        assert!(html.contains("test_desc"));
    }
}
