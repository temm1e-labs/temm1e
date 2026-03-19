//! Credential management — loading, saving, and detecting API keys.
//!
//! Extracted from `main.rs` so that both the CLI binary and `temm1e-tui`
//! can share credential logic without duplication.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use tracing;

use crate::types::error::Temm1eError;

// ── Data Types ──────────────────────────────────────────────────────

/// Credentials file layout (multi-provider, multi-key).
///
/// ```toml
/// active = "anthropic"
///
/// [[providers]]
/// name = "anthropic"
/// keys = ["sk-ant-key1", "sk-ant-key2"]
/// model = "claude-sonnet-4-6"
/// ```
#[derive(Serialize, Deserialize, Default, Clone, Debug)]
pub struct CredentialsFile {
    /// Name of the currently active provider.
    #[serde(default)]
    pub active: String,
    /// All configured providers.
    #[serde(default)]
    pub providers: Vec<CredentialsProvider>,
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct CredentialsProvider {
    pub name: String,
    #[serde(default)]
    pub keys: Vec<String>,
    pub model: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
}

/// Result of credential detection from user input.
#[derive(Debug, Clone)]
pub struct DetectedCredential {
    pub provider: &'static str,
    pub api_key: String,
    pub base_url: Option<String>,
}

// ── Path Helpers ────────────────────────────────────────────────────

/// Returns `~/.temm1e/credentials.toml`.
pub fn credentials_path() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".temm1e")
        .join("credentials.toml")
}

// ── Placeholder Detection ───────────────────────────────────────────

/// Reject obviously fake / placeholder API keys before they reach any provider.
pub fn is_placeholder_key(key: &str) -> bool {
    let k = key.trim().to_lowercase();
    if k.len() < 10 {
        return true;
    }
    let placeholders = [
        "paste_your",
        "your_key",
        "your_api",
        "your-key",
        "your-api",
        "insert_your",
        "insert-your",
        "put_your",
        "put-your",
        "replace_with",
        "replace-with",
        "enter_your",
        "enter-your",
        "placeholder",
        "xxxxxxxx",
        "your_token",
        "your-token",
        "_here",
    ];
    for p in &placeholders {
        if k.contains(p) {
            return true;
        }
    }
    // All same character (e.g. "aaaaaaaaaa")
    if k.len() >= 10 && k.chars().all(|c| c == k.chars().next().unwrap_or('a')) {
        return true;
    }
    false
}

// ── Provider Name Normalization ─────────────────────────────────────

/// Normalize provider name string to a static str.
pub fn normalize_provider_name(name: &str) -> Option<&'static str> {
    match name.to_lowercase().as_str() {
        "anthropic" | "claude" => Some("anthropic"),
        "openai" | "gpt" => Some("openai"),
        "gemini" | "google" => Some("gemini"),
        "grok" | "xai" => Some("grok"),
        "openrouter" => Some("openrouter"),
        "minimax" => Some("minimax"),
        "zai" | "zhipu" | "glm" => Some("zai"),
        "ollama" => Some("ollama"),
        _ => None,
    }
}

// ── API Key Detection ───────────────────────────────────────────────

/// Detect API provider from user input. Supports multiple formats:
///
/// 1. Raw key (auto-detect): `sk-ant-xxx`
/// 2. Explicit provider:key: `minimax:eyJhbG...`
/// 3. Proxy config: `proxy provider:openai base_url:https://my-proxy/v1 key:sk-xxx`
pub fn detect_api_key(text: &str) -> Option<DetectedCredential> {
    let trimmed = text.trim();

    // Format 3: Proxy config
    let lower = trimmed.to_lowercase();
    if lower.starts_with("proxy") {
        let result = parse_proxy_config(trimmed);
        if let Some(ref cred) = result {
            if is_placeholder_key(&cred.api_key) {
                return None;
            }
        }
        return result;
    }

    // Format 2: Explicit provider:key
    if let Some((provider, key)) = trimmed.split_once(':') {
        let p = provider.to_lowercase();
        if p != "http" && p != "https" {
            match p.as_str() {
                "anthropic" | "openai" | "gemini" | "grok" | "xai" | "openrouter" | "minimax"
                | "zai" | "zhipu" | "ollama" => {
                    if key.len() >= 8 && !is_placeholder_key(key) {
                        return Some(DetectedCredential {
                            provider: match p.as_str() {
                                "anthropic" => "anthropic",
                                "openai" => "openai",
                                "gemini" => "gemini",
                                "grok" | "xai" => "grok",
                                "openrouter" => "openrouter",
                                "minimax" => "minimax",
                                "zai" | "zhipu" => "zai",
                                "ollama" => "ollama",
                                _ => unreachable!(),
                            },
                            api_key: key.to_string(),
                            base_url: None,
                        });
                    }
                }
                _ => {}
            }
        }
    }

    // Format 1: Auto-detect from key prefix
    if is_placeholder_key(trimmed) {
        return None;
    }
    if trimmed.starts_with("sk-ant-") {
        Some(DetectedCredential {
            provider: "anthropic",
            api_key: trimmed.to_string(),
            base_url: None,
        })
    } else if trimmed.starts_with("sk-or-") {
        Some(DetectedCredential {
            provider: "openrouter",
            api_key: trimmed.to_string(),
            base_url: None,
        })
    } else if trimmed.starts_with("xai-") {
        Some(DetectedCredential {
            provider: "grok",
            api_key: trimmed.to_string(),
            base_url: None,
        })
    } else if trimmed.starts_with("sk-") {
        Some(DetectedCredential {
            provider: "openai",
            api_key: trimmed.to_string(),
            base_url: None,
        })
    } else if trimmed.starts_with("AIzaSy") {
        Some(DetectedCredential {
            provider: "gemini",
            api_key: trimmed.to_string(),
            base_url: None,
        })
    } else {
        None
    }
}

/// Parse proxy configuration from user input.
fn parse_proxy_config(text: &str) -> Option<DetectedCredential> {
    let tokens: Vec<&str> = text.split_whitespace().collect();
    if tokens.len() < 3 {
        return None;
    }

    let mut provider: Option<&'static str> = None;
    let mut base_url: Option<String> = None;
    let mut api_key: Option<String> = None;

    let mut i = 1;
    while i < tokens.len() {
        let token = tokens[i];
        let lower = token.to_lowercase();

        if let Some((k, v)) = token.split_once(':') {
            let k_lower = k.to_lowercase();
            match k_lower.as_str() {
                "provider" | "type" => {
                    provider = normalize_provider_name(v);
                }
                "base_url" | "url" | "endpoint" | "host" => {
                    base_url = Some(v.to_string());
                }
                "key" | "api_key" | "apikey" | "token" => {
                    api_key = Some(v.to_string());
                }
                _ => {
                    if v.starts_with("//") || v.starts_with("http") {
                        base_url = Some(token.to_string());
                    } else if normalize_provider_name(&lower).is_some() {
                        provider = normalize_provider_name(k);
                        api_key = Some(v.to_string());
                    }
                }
            }
        } else if token.starts_with("http://") || token.starts_with("https://") {
            base_url = Some(token.to_string());
        } else if normalize_provider_name(&lower).is_some() && provider.is_none() {
            provider = normalize_provider_name(&lower);
        } else if token.len() >= 8 && api_key.is_none() {
            api_key = Some(token.to_string());
        }

        i += 1;
    }

    let provider = provider.unwrap_or("openai");
    let api_key = api_key?;

    Some(DetectedCredential {
        provider,
        api_key,
        base_url,
    })
}

// ── Credential File Operations ──────────────────────────────────────

/// Load the full credentials file. Falls back to legacy single-provider format.
pub fn load_credentials_file() -> Option<CredentialsFile> {
    let path = credentials_path();
    let content = std::fs::read_to_string(&path).ok()?;

    // Try new format first
    if let Ok(creds) = toml::from_str::<CredentialsFile>(&content) {
        if !creds.providers.is_empty() {
            return Some(creds);
        }
    }

    // Fallback: legacy single-provider format
    let table: toml::Table = content.parse().ok()?;
    let provider = table.get("provider")?.as_table()?;
    let name = provider.get("name")?.as_str()?.to_string();
    let key = provider.get("api_key")?.as_str()?.to_string();
    let model = provider.get("model")?.as_str()?.to_string();
    if name.is_empty() || key.is_empty() {
        return None;
    }
    Some(CredentialsFile {
        active: name.clone(),
        providers: vec![CredentialsProvider {
            name,
            keys: vec![key],
            model,
            base_url: None,
        }],
    })
}

/// Save credentials — appends key to existing provider or creates new entry.
pub async fn save_credentials(
    provider_name: &str,
    api_key: &str,
    model: &str,
    custom_base_url: Option<&str>,
) -> Result<(), Temm1eError> {
    let dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join(".temm1e");
    tokio::fs::create_dir_all(&dir).await?;
    let path = dir.join("credentials.toml");

    let mut creds = load_credentials_file().unwrap_or_default();

    let match_fn = |p: &CredentialsProvider| -> bool {
        p.name == provider_name && p.base_url == custom_base_url.map(|s| s.to_string())
    };

    if let Some(existing) = creds.providers.iter_mut().find(|p| match_fn(p)) {
        if !existing.keys.contains(&api_key.to_string()) {
            existing.keys.push(api_key.to_string());
            tracing::info!(
                provider = %provider_name,
                total_keys = existing.keys.len(),
                "Added new key to existing provider"
            );
        }
        existing.model = model.to_string();
    } else {
        creds.providers.push(CredentialsProvider {
            name: provider_name.to_string(),
            keys: vec![api_key.to_string()],
            model: model.to_string(),
            base_url: custom_base_url.map(|s| s.to_string()),
        });
    }

    creds.active = provider_name.to_string();

    let content = toml::to_string_pretty(&creds)
        .map_err(|e| Temm1eError::Config(format!("Failed to serialize credentials: {e}")))?;
    tokio::fs::write(&path, content).await?;
    tracing::info!(path = %path.display(), provider = %provider_name, "Credentials saved");
    Ok(())
}

/// Load the active provider's credentials.
/// Returns `(provider_name, api_key, model)`.
/// Filters out placeholder/dummy keys.
pub fn load_saved_credentials() -> Option<(String, String, String)> {
    let creds = load_credentials_file()?;
    let provider = creds
        .providers
        .iter()
        .find(|p| p.name == creds.active)
        .or_else(|| creds.providers.first())?;
    let first_valid_key = provider
        .keys
        .iter()
        .find(|k| !is_placeholder_key(k))?
        .clone();
    if provider.name.is_empty() || first_valid_key.is_empty() {
        return None;
    }
    Some((
        provider.name.clone(),
        first_valid_key,
        provider.model.clone(),
    ))
}

/// Load all keys for the active provider.
/// Returns `(name, keys, model, base_url)`.
/// Filters out placeholder/dummy keys.
pub fn load_active_provider_keys() -> Option<(String, Vec<String>, String, Option<String>)> {
    let creds = load_credentials_file()?;
    let provider = creds
        .providers
        .iter()
        .find(|p| p.name == creds.active)
        .or_else(|| creds.providers.first())?;
    let valid_keys: Vec<String> = provider
        .keys
        .iter()
        .filter(|k| !is_placeholder_key(k))
        .cloned()
        .collect();
    if provider.name.is_empty() || valid_keys.is_empty() {
        return None;
    }
    Some((
        provider.name.clone(),
        valid_keys,
        provider.model.clone(),
        provider.base_url.clone(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholder_keys_rejected() {
        assert!(is_placeholder_key("short"));
        assert!(is_placeholder_key("paste_your_key_here"));
        assert!(is_placeholder_key("aaaaaaaaaa"));
        assert!(is_placeholder_key("YOUR_API_KEY"));
        assert!(is_placeholder_key("placeholder_key"));
    }

    #[test]
    fn real_keys_accepted() {
        assert!(!is_placeholder_key("sk-ant-api03-realkey1234567890"));
        assert!(!is_placeholder_key("sk-proj-realkey1234567890abcdef"));
        assert!(!is_placeholder_key("xai-realkey1234567890"));
    }

    #[test]
    fn detect_anthropic_key() {
        let cred = detect_api_key("sk-ant-api03-abc123").unwrap();
        assert_eq!(cred.provider, "anthropic");
        assert_eq!(cred.api_key, "sk-ant-api03-abc123");
        assert!(cred.base_url.is_none());
    }

    #[test]
    fn detect_openai_key() {
        let cred = detect_api_key("sk-proj-abc123def456").unwrap();
        assert_eq!(cred.provider, "openai");
    }

    #[test]
    fn detect_openrouter_key() {
        let cred = detect_api_key("sk-or-v1-abc123def456").unwrap();
        assert_eq!(cred.provider, "openrouter");
    }

    #[test]
    fn detect_grok_key() {
        let cred = detect_api_key("xai-abc123def456").unwrap();
        assert_eq!(cred.provider, "grok");
    }

    #[test]
    fn detect_gemini_key() {
        let cred = detect_api_key("AIzaSyABCDEF1234567890").unwrap();
        assert_eq!(cred.provider, "gemini");
    }

    #[test]
    fn detect_explicit_provider_key() {
        let cred = detect_api_key("minimax:eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9").unwrap();
        assert_eq!(cred.provider, "minimax");
        assert_eq!(cred.api_key, "eyJhbGciOiJSUzI1NiIsInR5cCI6IkpXVCJ9");
    }

    #[test]
    fn detect_proxy_config() {
        let cred =
            detect_api_key("proxy openai https://my-proxy.com/v1 sk-real-key-12345678").unwrap();
        assert_eq!(cred.provider, "openai");
        assert_eq!(cred.api_key, "sk-real-key-12345678");
        assert_eq!(cred.base_url.as_deref(), Some("https://my-proxy.com/v1"));
    }

    #[test]
    fn detect_proxy_kv_format() {
        let cred = detect_api_key(
            "proxy provider:anthropic base_url:https://gateway.ai/v1 key:sk-ant-api03-real12345678",
        )
        .unwrap();
        assert_eq!(cred.provider, "anthropic");
        assert_eq!(cred.api_key, "sk-ant-api03-real12345678");
        assert_eq!(cred.base_url.as_deref(), Some("https://gateway.ai/v1"));
    }

    #[test]
    fn reject_placeholder_in_detect() {
        assert!(detect_api_key("paste_your_key_here").is_none());
        assert!(detect_api_key("short").is_none());
    }

    #[test]
    fn normalize_known_providers() {
        assert_eq!(normalize_provider_name("anthropic"), Some("anthropic"));
        assert_eq!(normalize_provider_name("claude"), Some("anthropic"));
        assert_eq!(normalize_provider_name("openai"), Some("openai"));
        assert_eq!(normalize_provider_name("gpt"), Some("openai"));
        assert_eq!(normalize_provider_name("gemini"), Some("gemini"));
        assert_eq!(normalize_provider_name("google"), Some("gemini"));
        assert_eq!(normalize_provider_name("grok"), Some("grok"));
        assert_eq!(normalize_provider_name("xai"), Some("grok"));
        assert_eq!(normalize_provider_name("openrouter"), Some("openrouter"));
        assert_eq!(normalize_provider_name("minimax"), Some("minimax"));
        assert_eq!(normalize_provider_name("zai"), Some("zai"));
        assert_eq!(normalize_provider_name("zhipu"), Some("zai"));
        assert_eq!(normalize_provider_name("glm"), Some("zai"));
        assert_eq!(normalize_provider_name("ollama"), Some("ollama"));
        assert_eq!(normalize_provider_name("unknown"), None);
    }
}
