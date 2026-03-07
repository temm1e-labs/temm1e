//! SkyClaw Providers crate
//!
//! LLM provider integrations. Currently supports:
//! - **Anthropic** (Claude models via the Messages API)
//! - **OpenAI-compatible** (OpenAI, Ollama, vLLM, LM Studio, Groq, Mistral, etc.)

#![allow(dead_code)]

pub mod anthropic;
pub mod openai_compat;

pub use anthropic::AnthropicProvider;
pub use openai_compat::OpenAICompatProvider;

use skyclaw_core::types::config::ProviderConfig;
use skyclaw_core::types::error::SkyclawError;
use skyclaw_core::Provider;

/// Create a provider from configuration.
///
/// The `name` field in `ProviderConfig` determines which backend to use:
/// - `"anthropic"` -> `AnthropicProvider`
/// - `"openai"` | `"openai-compatible"` | anything else -> `OpenAICompatProvider`
///
/// `api_key` must be set. `base_url` is optional (defaults depend on provider).
pub fn create_provider(
    config: &ProviderConfig,
) -> Result<Box<dyn Provider>, SkyclawError> {
    let name = config
        .name
        .as_deref()
        .unwrap_or("openai-compatible");

    let api_key = config
        .api_key
        .clone()
        .ok_or_else(|| SkyclawError::Config("Provider api_key is required".into()))?;

    match name {
        "anthropic" => {
            let mut provider = AnthropicProvider::new(api_key);
            if let Some(ref base_url) = config.base_url {
                provider = provider.with_base_url(base_url.clone());
            }
            Ok(Box::new(provider))
        }
        _ => {
            // Treat everything else as OpenAI-compatible
            let mut provider = OpenAICompatProvider::new(api_key);
            if let Some(ref base_url) = config.base_url {
                provider = provider.with_base_url(base_url.clone());
            }
            Ok(Box::new(provider))
        }
    }
}
