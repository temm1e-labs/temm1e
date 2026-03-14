//! Model registry — known context window and output token limits for LLM models.
//!
//! Every model entry is sourced from official provider documentation.
//! See `docs/MODEL_REGISTRY.md` for the full reference with sources and pricing.
//!
//! # Usage
//!
//! ```
//! use temm1e_core::types::model_registry::model_limits;
//!
//! let (context_window, max_output) = model_limits("claude-sonnet-4-6");
//! assert_eq!(context_window, 200_000);
//! assert_eq!(max_output, 64_000);
//! ```

/// Model capability limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ModelLimits {
    /// Maximum input context window in tokens.
    pub context_window: usize,
    /// Maximum output tokens the model can generate.
    pub max_output_tokens: usize,
}

/// Default limits for unknown models — conservative enough for most modern
/// models while not wasting small-context budgets.
pub const DEFAULT_LIMITS: ModelLimits = ModelLimits {
    context_window: 128_000,
    max_output_tokens: 16_384,
};

/// Look up the context window and max output tokens for a model.
///
/// Returns `(context_window, max_output_tokens)`.
///
/// Models are matched by exact name first, then by suffix (stripping provider
/// prefixes like `openai/`, `anthropic/`, etc.) to handle OpenRouter-style
/// namespaced model IDs.
pub fn model_limits(model: &str) -> (usize, usize) {
    let limits = lookup(model)
        .or_else(|| {
            // Try stripping provider prefix: "provider/model" → "model"
            model.split('/').next_back().and_then(lookup)
        })
        .unwrap_or(DEFAULT_LIMITS);
    (limits.context_window, limits.max_output_tokens)
}

/// Return just the ModelLimits struct for a model.
pub fn get_model_limits(model: &str) -> ModelLimits {
    lookup(model)
        .or_else(|| model.split('/').next_back().and_then(lookup))
        .unwrap_or(DEFAULT_LIMITS)
}

fn lookup(model: &str) -> Option<ModelLimits> {
    Some(match model {
        // ── Anthropic ─────────────────────────────────────────────────
        "claude-sonnet-4-6" | "claude-sonnet-4-20250514" | "claude-sonnet-4-0" => ModelLimits {
            context_window: 200_000,
            max_output_tokens: 64_000,
        },
        "claude-opus-4-6" | "claude-opus-4-20250514" | "claude-opus-4-0" => ModelLimits {
            context_window: 200_000,
            max_output_tokens: 128_000,
        },
        "claude-haiku-4-5" | "claude-haiku-4-5-20251001" => ModelLimits {
            context_window: 200_000,
            max_output_tokens: 64_000,
        },

        // ── OpenAI ────────────────────────────────────────────────────
        "gpt-5.4" => ModelLimits {
            context_window: 1_050_000,
            max_output_tokens: 128_000,
        },
        "gpt-5.3-codex" | "gpt-5.3-codex-spark" => ModelLimits {
            context_window: 400_000,
            max_output_tokens: 128_000,
        },
        "gpt-5.2" | "gpt-5.2-codex" => ModelLimits {
            context_window: 400_000,
            max_output_tokens: 128_000,
        },
        "gpt-5.1-codex" | "gpt-5.1-codex-mini" => ModelLimits {
            context_window: 400_000,
            max_output_tokens: 128_000,
        },
        "gpt-5" | "gpt-5-codex" | "gpt-5-codex-mini" | "gpt-5-mini" => ModelLimits {
            context_window: 400_000,
            max_output_tokens: 128_000,
        },
        "gpt-4.1" => ModelLimits {
            context_window: 1_047_576,
            max_output_tokens: 32_768,
        },
        "gpt-4.1-mini" | "gpt-4.1-nano" => ModelLimits {
            context_window: 1_047_576,
            max_output_tokens: 32_768,
        },
        "o4-mini" => ModelLimits {
            context_window: 200_000,
            max_output_tokens: 100_000,
        },
        "gpt-4o" | "gpt-4o-2024-08-06" => ModelLimits {
            context_window: 128_000,
            max_output_tokens: 16_384,
        },
        "gpt-4o-mini" => ModelLimits {
            context_window: 128_000,
            max_output_tokens: 16_384,
        },
        "gpt-3.5-turbo" => ModelLimits {
            context_window: 16_385,
            max_output_tokens: 4_096,
        },

        // ── Google Gemini ─────────────────────────────────────────────
        "gemini-3-flash-preview" | "gemini-3-flash" => ModelLimits {
            context_window: 1_048_576,
            max_output_tokens: 65_536,
        },
        "gemini-3.1-pro-preview" | "gemini-3.1-pro" | "gemini-3-pro" => ModelLimits {
            context_window: 1_048_576,
            max_output_tokens: 65_536,
        },
        "gemini-2.5-flash" | "gemini-2.5-flash-preview-05-20" => ModelLimits {
            context_window: 1_048_576,
            max_output_tokens: 65_536,
        },
        "gemini-2.5-pro" | "gemini-2.5-pro-preview-06-05" => ModelLimits {
            context_window: 1_048_576,
            max_output_tokens: 65_536,
        },

        // ── xAI Grok ──────────────────────────────────────────────────
        "grok-4-1-fast-non-reasoning" | "grok-4-1-fast" | "grok-4.1-fast" => ModelLimits {
            context_window: 2_000_000,
            max_output_tokens: 30_000,
        },
        "grok-3" | "grok-3-latest" => ModelLimits {
            context_window: 131_072,
            max_output_tokens: 131_072,
        },

        // ── Z.ai (Zhipu AI) ──────────────────────────────────────────
        "glm-4.7-flash" => ModelLimits {
            context_window: 200_000,
            max_output_tokens: 131_072,
        },
        "glm-4.7" => ModelLimits {
            context_window: 200_000,
            max_output_tokens: 131_072,
        },
        "glm-5" => ModelLimits {
            context_window: 200_000,
            max_output_tokens: 131_072,
        },
        "glm-4.6v" => ModelLimits {
            context_window: 131_072,
            max_output_tokens: 32_768,
        },

        // ── MiniMax ───────────────────────────────────────────────────
        "MiniMax-M2.5" | "minimax-m2.5" => ModelLimits {
            context_window: 204_800,
            max_output_tokens: 196_608,
        },

        // ── Meta Llama ────────────────────────────────────────────────
        "llama-4-maverick" | "meta-llama/llama-4-maverick" => ModelLimits {
            context_window: 1_048_576,
            max_output_tokens: 16_384,
        },
        "llama-4-scout" | "meta-llama/llama-4-scout" => ModelLimits {
            context_window: 10_000_000,
            max_output_tokens: 16_384,
        },

        // ── DeepSeek ──────────────────────────────────────────────────
        "deepseek-v3.2" | "deepseek/deepseek-v3.2" => ModelLimits {
            context_window: 163_840,
            max_output_tokens: 65_536,
        },
        "deepseek-r1-0528" | "deepseek/deepseek-r1-0528" => ModelLimits {
            context_window: 163_840,
            max_output_tokens: 65_536,
        },
        "deepseek-r1" | "deepseek/deepseek-r1" => ModelLimits {
            context_window: 64_000,
            max_output_tokens: 16_000,
        },
        "deepseek-v3" | "deepseek/deepseek-v3-0324" => ModelLimits {
            context_window: 163_840,
            max_output_tokens: 65_536,
        },

        // ── Qwen (Alibaba) ───────────────────────────────────────────
        "qwen3-coder" | "qwen/qwen3-coder" => ModelLimits {
            context_window: 262_144,
            max_output_tokens: 262_000,
        },
        "qwen3-235b-a22b" | "qwen/qwen3-235b-a22b" => ModelLimits {
            context_window: 131_072,
            max_output_tokens: 32_768,
        },
        "qwen3-max" | "qwen/qwen3-max" => ModelLimits {
            context_window: 262_144,
            max_output_tokens: 32_768,
        },
        "qwen3.5-plus-02-15" | "qwen/qwen3.5-plus-02-15" | "qwen3.5-plus" => ModelLimits {
            context_window: 1_000_000,
            max_output_tokens: 32_768,
        },
        "qwen-2.5-7b-instruct" | "qwen/qwen-2.5-7b-instruct" => ModelLimits {
            context_window: 32_768,
            max_output_tokens: 8_192,
        },
        "qwen-2.5-72b-instruct" | "qwen/qwen-2.5-72b-instruct" => ModelLimits {
            context_window: 131_072,
            max_output_tokens: 8_192,
        },

        // ── Mistral ──────────────────────────────────────────────────
        "mistral-large-2512" | "mistralai/mistral-large-2512" => ModelLimits {
            context_window: 262_144,
            max_output_tokens: 32_768,
        },
        "mistral-medium-3" | "mistralai/mistral-medium-3" => ModelLimits {
            context_window: 131_072,
            max_output_tokens: 32_768,
        },
        "mistral-medium-3.1" | "mistralai/mistral-medium-3.1" => ModelLimits {
            context_window: 32_000,
            max_output_tokens: 32_768,
        },

        // ── Cohere ───────────────────────────────────────────────────
        "command-a" | "cohere/command-a" => ModelLimits {
            context_window: 256_000,
            max_output_tokens: 4_096,
        },
        "command-r-plus" | "cohere/command-r-plus-08-2024" | "command-r-plus-08-2024" => {
            ModelLimits {
                context_window: 128_000,
                max_output_tokens: 4_096,
            }
        }

        // ── Microsoft ────────────────────────────────────────────────
        "phi-4" | "microsoft/phi-4" => ModelLimits {
            context_window: 16_384,
            max_output_tokens: 16_384,
        },

        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn known_anthropic_models() {
        let (ctx, out) = model_limits("claude-sonnet-4-6");
        assert_eq!(ctx, 200_000);
        assert_eq!(out, 64_000);

        let (ctx, out) = model_limits("claude-opus-4-6");
        assert_eq!(ctx, 200_000);
        assert_eq!(out, 128_000);
    }

    #[test]
    fn known_openai_models() {
        let (ctx, out) = model_limits("gpt-5.4");
        assert_eq!(ctx, 1_050_000);
        assert_eq!(out, 128_000);

        let (ctx, out) = model_limits("gpt-4.1");
        assert_eq!(ctx, 1_047_576);
        assert_eq!(out, 32_768);

        let (ctx, out) = model_limits("o4-mini");
        assert_eq!(ctx, 200_000);
        assert_eq!(out, 100_000);
    }

    #[test]
    fn known_gemini_models() {
        let (ctx, out) = model_limits("gemini-3-flash-preview");
        assert_eq!(ctx, 1_048_576);
        assert_eq!(out, 65_536);
    }

    #[test]
    fn known_grok_models() {
        let (ctx, out) = model_limits("grok-4-1-fast-non-reasoning");
        assert_eq!(ctx, 2_000_000);
        assert_eq!(out, 30_000);
    }

    #[test]
    fn known_proxy_models() {
        // DeepSeek
        let (ctx, out) = model_limits("deepseek-v3.2");
        assert_eq!(ctx, 163_840);
        assert_eq!(out, 65_536);

        // Qwen via OpenRouter prefix
        let (ctx, out) = model_limits("qwen/qwen3-coder");
        assert_eq!(ctx, 262_144);
        assert_eq!(out, 262_000);

        // Qwen small model (issue #6)
        let (ctx, out) = model_limits("qwen/qwen-2.5-7b-instruct");
        assert_eq!(ctx, 32_768);
        assert_eq!(out, 8_192);

        // Llama
        let (ctx, out) = model_limits("meta-llama/llama-4-maverick");
        assert_eq!(ctx, 1_048_576);
        assert_eq!(out, 16_384);

        // Mistral
        let (ctx, out) = model_limits("mistral-large-2512");
        assert_eq!(ctx, 262_144);
        assert_eq!(out, 32_768);
    }

    #[test]
    fn provider_prefix_stripping() {
        // "anthropic/claude-sonnet-4-6" → strips to "claude-sonnet-4-6"
        let (ctx, out) = model_limits("anthropic/claude-sonnet-4-6");
        assert_eq!(ctx, 200_000);
        assert_eq!(out, 64_000);

        // "openai/gpt-5.2" → strips to "gpt-5.2"
        let (ctx, out) = model_limits("openai/gpt-5.2");
        assert_eq!(ctx, 400_000);
        assert_eq!(out, 128_000);
    }

    #[test]
    fn unknown_model_gets_defaults() {
        let (ctx, out) = model_limits("some-unknown-model-v99");
        assert_eq!(ctx, DEFAULT_LIMITS.context_window);
        assert_eq!(out, DEFAULT_LIMITS.max_output_tokens);
    }

    #[test]
    fn zai_models() {
        let (ctx, out) = model_limits("glm-4.7-flash");
        assert_eq!(ctx, 200_000);
        assert_eq!(out, 131_072);

        let (ctx, out) = model_limits("glm-4.6v");
        assert_eq!(ctx, 131_072);
        assert_eq!(out, 32_768);
    }

    #[test]
    fn minimax_model() {
        let (ctx, out) = model_limits("MiniMax-M2.5");
        assert_eq!(ctx, 204_800);
        assert_eq!(out, 196_608);
    }
}
