# TEMM1E Model Registry

**Last verified: 2026-03-16** — All values sourced from official provider documentation and OpenRouter.

This registry defines context window and max output token limits for every known model. TEMM1E uses these values to set `max_context_tokens` and `max_output_tokens` automatically per model, so users get the full capability of their chosen model without manual tuning.

## How It Works

1. When a model is selected, TEMM1E looks up its limits in `model_limits()`
2. If found, `max_context_tokens` and `max_output_tokens` are set to the model's actual values
3. If unknown, conservative defaults apply (128K context / 16K output)
4. Users can still override via `[agent] max_context_tokens` in config

## Anthropic

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `claude-sonnet-4-6` | 200,000 | 64,000 | $3.00 / $15.00 |
| `claude-opus-4-6` | 200,000 | 128,000 | $15.00 / $75.00 |
| `claude-haiku-4-5` | 200,000 | 64,000 | $0.80 / $4.00 |

Notes:
- 1M context available with `context-1m-2025-08-07` beta header (not used by default)
- Opus 128K output may require `output-128k-2025-02-19` beta header

## OpenAI

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `gpt-5.4` | 1,050,000 | 128,000 | $2.50 / $15.00 |
| `gpt-5.2` | 400,000 | 128,000 | $2.00 / $8.00 |
| `gpt-4.1` | 1,047,576 | 32,768 | $2.00 / $8.00 |
| `gpt-4.1-mini` | 1,047,576 | 32,768 | $0.40 / $1.60 |
| `o4-mini` | 200,000 | 100,000 | $1.10 / $4.40 |

## OpenAI Codex (OAuth)

| Model ID | Context Window | Max Output | Notes |
|----------|---------------|------------|-------|
| `gpt-5.4` | 1,050,000 | 128,000 | Recommended for general agent use; $2.50/$15.00 |
| `gpt-5.3-codex` | 400,000 | 128,000 | Coding-specialized |
| `gpt-5.3-codex-spark` | 128,000 | 128,000 | Research preview (Cerebras) |
| `gpt-5.2-codex` | 400,000 | 128,000 | Coding-specialized |
| `gpt-5.1-codex` | 400,000 | 128,000 | Coding-specialized |
| `gpt-5` | 400,000 | 128,000 | |
| `gpt-5-codex-mini` | 400,000 | 128,000 | |
| `gpt-5-mini` | 400,000 | 128,000 | |

## Google Gemini

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `gemini-3-flash-preview` | 1,048,576 | 65,536 | $0.15 / $0.60 |
| `gemini-3.1-pro-preview` | 1,048,576 | 65,536 | $1.25 / $10.00 |
| `gemini-3.1-flash-lite-preview` | 1,048,576 | 65,536 | $0.25 / $1.50 |
| `gemini-2.5-flash` | 1,048,576 | 65,536 | $0.15 / $0.60 |
| `gemini-2.5-pro` | 1,048,576 | 65,536 | $1.25 / $10.00 |

## xAI Grok

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `grok-4-1-fast-non-reasoning` | 2,000,000 | 30,000 | $2.00 / $10.00 |
| `grok-3` | 131,072 | 131,072 | $3.00 / $15.00 |

## Z.ai (Zhipu AI)

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `glm-4.7-flash` | 200,000 | 131,072 | Free / Free |
| `glm-4.7` | 200,000 | 131,072 | $0.28 / $0.28 |
| `glm-5` | 200,000 | 131,072 | $0.70 / $0.70 |
| `glm-4.6v` | 131,072 | 32,768 | $0.28 / $0.28 |

Note: `glm-5-code` does not exist as a separate API model. GLM-5 handles code tasks natively.

## MiniMax

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `MiniMax-M2.5` | 204,800 | 196,608 | $0.50 / $1.10 |

## Popular Proxy Models (OpenRouter / Third-Party)

These models are commonly used through OpenRouter and other OpenAI-compatible proxies. TEMM1E recognizes them by name regardless of which proxy routes them.

### Meta Llama

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `meta-llama/llama-4-maverick` | 1,048,576 | 16,384 | $0.15 / $0.60 |
| `meta-llama/llama-4-scout` | 10,000,000 | 16,384 | $0.08 / $0.30 |
| `llama-4-maverick` | 1,048,576 | 16,384 | Alias |
| `llama-4-scout` | 10,000,000 | 16,384 | Alias |

### DeepSeek

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `deepseek/deepseek-v3.2` | 163,840 | 65,536 | $0.25 / $0.40 |
| `deepseek/deepseek-r1-0528` | 163,840 | 65,536 | $0.45 / $2.15 |
| `deepseek/deepseek-r1` | 64,000 | 16,000 | $0.70 / $2.50 |
| `deepseek-v3.2` | 163,840 | 65,536 | Alias |
| `deepseek-r1` | 64,000 | 16,000 | Alias |
| `deepseek-r1-0528` | 163,840 | 65,536 | Alias |

### Qwen (Alibaba)

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `qwen/qwen3-coder` | 262,144 | 262,000 | $0.22 / $1.00 |
| `qwen/qwen3-235b-a22b` | 131,072 | 32,768 | $0.455 / $1.82 |
| `qwen/qwen3-max` | 262,144 | 32,768 | $1.20 / $6.00 |
| `qwen/qwen3.5-plus-02-15` | 1,000,000 | 32,768 | $0.26 / $1.56 |
| `qwen/qwen-2.5-7b-instruct` | 32,768 | 8,192 | Free |
| `qwen3-coder` | 262,144 | 262,000 | Alias |
| `qwen3-235b-a22b` | 131,072 | 32,768 | Alias |

### Mistral

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `mistralai/mistral-large-2512` | 262,144 | 32,768 | $0.50 / $1.50 |
| `mistralai/mistral-medium-3` | 131,072 | 32,768 | $0.40 / $2.00 |
| `mistralai/mistral-medium-3.1` | 32,000 | 32,768 | $0.10 / $0.30 |
| `mistral-large-2512` | 262,144 | 32,768 | Alias |
| `mistral-medium-3` | 131,072 | 32,768 | Alias |

### Cohere

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `cohere/command-a` | 256,000 | 4,096 | $2.50 / $10.00 |
| `cohere/command-r-plus-08-2024` | 128,000 | 4,096 | $2.50 / $10.00 |
| `command-a` | 256,000 | 4,096 | Alias |
| `command-r-plus` | 128,000 | 4,096 | Alias |

### OpenRouter Stealth

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `openrouter/hunter-alpha` | 1,048,576 | 32,000 | Free / Free |
| `hunter-alpha` | 1,048,576 | 32,000 | Alias |

Note: Hunter Alpha is a 1T-parameter stealth model optimized for agentic use. All prompts and completions are logged by the provider.

### Microsoft

| Model ID | Context Window | Max Output | Pricing (In/Out per 1M) |
|----------|---------------|------------|--------------------------|
| `microsoft/phi-4` | 16,384 | 16,384 | $0.06 / $0.14 |
| `phi-4` | 16,384 | 16,384 | Alias |

## Default Fallback

For any model NOT in this registry:
- **Context window**: 128,000 tokens
- **Max output**: 16,384 tokens

This is conservative enough to work with most modern models while not wasting small-context models' budgets.

## Updating This Registry

When adding a new model:
1. WebSearch for official docs with exact `context_window` and `max_output_tokens`
2. Add to this document AND to `model_limits()` in `src/main.rs`
3. Include both the full provider-prefixed name (`provider/model`) and the short alias (`model`)
4. Run tests: `cargo test -- model_limits`
