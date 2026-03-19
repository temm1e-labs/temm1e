//! TEMM1E Providers crate
//!
//! LLM provider integrations. Currently supports:
//! - **Anthropic** (Claude models via the Messages API)
//! - **OpenAI-compatible** (OpenAI, Ollama, vLLM, LM Studio, Groq, Mistral, etc.)
//! - **xAI Grok** (via OpenAI-compatible endpoint)
//! - **OpenRouter** (290+ models via OpenAI-compatible endpoint)
//! - **MiniMax** (via OpenAI-compatible endpoint)
//! - **Google Gemini** (via OpenAI-compatible endpoint)
//! - **Z.ai / Zhipu AI** (GLM models via OpenAI-compatible endpoint)

#![allow(dead_code)]

pub mod anthropic;
pub mod gemini;
pub mod openai_compat;

pub use anthropic::AnthropicProvider;
pub use gemini::GeminiProvider;
pub use openai_compat::OpenAICompatProvider;

use temm1e_core::types::config::ProviderConfig;
use temm1e_core::types::error::Temm1eError;
use temm1e_core::Provider;

/// Create a provider from configuration.
///
/// The `name` field in `ProviderConfig` determines which backend to use:
/// - `"anthropic"` -> `AnthropicProvider` (native Messages API)
/// - `"gemini"` -> `GeminiProvider` (native generateContent API)
/// - `"grok"` | `"xai"` -> `OpenAICompatProvider` with `https://api.x.ai/v1`
/// - `"openrouter"` -> `OpenAICompatProvider` with `https://openrouter.ai/api/v1`
/// - `"minimax"` -> `OpenAICompatProvider` with `https://api.minimax.io/v1`
/// - anything else -> `OpenAICompatProvider` (defaults to OpenAI)
///
/// `api_key` must be set. `base_url` is optional (overrides the preset default).
pub fn create_provider(config: &ProviderConfig) -> Result<Box<dyn Provider>, Temm1eError> {
    let name = config.name.as_deref().unwrap_or("openai-compatible");

    let all_keys = config.all_keys();
    let api_key = all_keys
        .first()
        .cloned()
        .or_else(|| config.api_key.clone())
        .ok_or_else(|| Temm1eError::Config("Provider api_key is required".into()))?;

    match name {
        "anthropic" => {
            let mut provider = AnthropicProvider::new(api_key).with_keys(all_keys);
            if let Some(ref base_url) = config.base_url {
                provider = provider.with_base_url(base_url.clone());
            }
            Ok(Box::new(provider))
        }
        "gemini" => {
            // Native Gemini API — properly handles systemInstruction.
            let provider = GeminiProvider::new(api_key);
            Ok(Box::new(provider))
        }
        "grok" | "xai" => {
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.x.ai/v1".to_string());
            let provider = OpenAICompatProvider::new(api_key)
                .with_keys(all_keys)
                .with_base_url(base_url)
                .with_extra_headers(config.extra_headers.clone());
            Ok(Box::new(provider))
        }
        "openrouter" => {
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "https://openrouter.ai/api/v1".to_string());
            let provider = OpenAICompatProvider::new(api_key)
                .with_keys(all_keys)
                .with_base_url(base_url)
                .with_extra_headers(config.extra_headers.clone());
            Ok(Box::new(provider))
        }
        "minimax" => {
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.minimax.io/v1".to_string());
            let provider = OpenAICompatProvider::new(api_key)
                .with_keys(all_keys)
                .with_base_url(base_url)
                .with_extra_headers(config.extra_headers.clone());
            Ok(Box::new(provider))
        }
        "zai" | "zhipu" => {
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "https://api.z.ai/api/paas/v4".to_string());
            let provider = OpenAICompatProvider::new(api_key)
                .with_keys(all_keys)
                .with_base_url(base_url)
                .with_extra_headers(config.extra_headers.clone());
            Ok(Box::new(provider))
        }
        "ollama" => {
            let base_url = config
                .base_url
                .clone()
                .unwrap_or_else(|| "https://ollama.com/v1".to_string());
            let provider = OpenAICompatProvider::new(api_key)
                .with_keys(all_keys)
                .with_base_url(base_url)
                .with_extra_headers(config.extra_headers.clone());
            Ok(Box::new(provider))
        }
        _ => {
            let mut provider = OpenAICompatProvider::new(api_key)
                .with_keys(all_keys)
                .with_extra_headers(config.extra_headers.clone());
            if let Some(ref base_url) = config.base_url {
                provider = provider.with_base_url(base_url.clone());
            }
            Ok(Box::new(provider))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn config_with_name(name: &str) -> ProviderConfig {
        ProviderConfig {
            name: Some(name.to_string()),
            api_key: Some("test-key".to_string()),
            keys: vec![],
            model: None,
            base_url: None,
            extra_headers: HashMap::new(),
        }
    }

    #[test]
    fn create_anthropic_provider() {
        let provider = create_provider(&config_with_name("anthropic")).unwrap();
        assert_eq!(provider.name(), "anthropic");
    }

    #[test]
    fn create_openai_provider() {
        let provider = create_provider(&config_with_name("openai")).unwrap();
        assert_eq!(provider.name(), "openai-compatible");
    }

    #[test]
    fn create_grok_provider() {
        let provider = create_provider(&config_with_name("grok")).unwrap();
        assert_eq!(provider.name(), "openai-compatible");
    }

    #[test]
    fn create_xai_provider() {
        let provider = create_provider(&config_with_name("xai")).unwrap();
        assert_eq!(provider.name(), "openai-compatible");
    }

    #[test]
    fn create_openrouter_provider() {
        let provider = create_provider(&config_with_name("openrouter")).unwrap();
        assert_eq!(provider.name(), "openai-compatible");
    }

    #[test]
    fn create_minimax_provider() {
        let provider = create_provider(&config_with_name("minimax")).unwrap();
        assert_eq!(provider.name(), "openai-compatible");
    }

    #[test]
    fn create_zai_provider() {
        let provider = create_provider(&config_with_name("zai")).unwrap();
        assert_eq!(provider.name(), "openai-compatible");
    }

    #[test]
    fn create_zhipu_provider() {
        let provider = create_provider(&config_with_name("zhipu")).unwrap();
        assert_eq!(provider.name(), "openai-compatible");
    }

    #[test]
    fn create_ollama_provider() {
        let provider = create_provider(&config_with_name("ollama")).unwrap();
        assert_eq!(provider.name(), "openai-compatible");
    }

    #[test]
    fn create_default_provider_without_name() {
        let config = ProviderConfig {
            name: None,
            api_key: Some("test-key".to_string()),
            keys: vec![],
            model: None,
            base_url: None,
            extra_headers: HashMap::new(),
        };
        let provider = create_provider(&config).unwrap();
        assert_eq!(provider.name(), "openai-compatible");
    }

    #[test]
    fn create_provider_without_api_key_fails() {
        let config = ProviderConfig {
            name: Some("anthropic".to_string()),
            api_key: None,
            keys: vec![],
            model: None,
            base_url: None,
            extra_headers: HashMap::new(),
        };
        assert!(create_provider(&config).is_err());
    }
}
