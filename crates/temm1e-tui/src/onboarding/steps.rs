//! Onboarding step definitions and state machine.

use crate::widgets::select_list::{SelectItem, SelectState};

/// Onboarding wizard states.
#[derive(Debug, Clone)]
pub enum OnboardingStep {
    Welcome,
    SelectMode(SelectState<String>),
    SelectProvider(SelectState<String>),
    EnterApiKey {
        provider: String,
        input: String,
        error: Option<String>,
    },
    ValidatingKey {
        provider: String,
    },
    SelectModel(SelectState<String>),
    Confirm {
        provider: String,
        model: String,
    },
    Saving,
    Done,
}

/// Create the mode selection list.
pub fn mode_select_items() -> Vec<SelectItem<String>> {
    vec![
        SelectItem {
            value: "auto".to_string(),
            label: "Auto".to_string(),
            description: "Adapts personality to each task (recommended)".to_string(),
        },
        SelectItem {
            value: "play".to_string(),
            label: "Play  :3".to_string(),
            description: "Warm, energetic, slightly chaotic".to_string(),
        },
        SelectItem {
            value: "work".to_string(),
            label: "Work >:3".to_string(),
            description: "Sharp, precise, structured".to_string(),
        },
        SelectItem {
            value: "pro".to_string(),
            label: "Pro".to_string(),
            description: "Professional, no emoticons, consultant tone".to_string(),
        },
        SelectItem {
            value: "none".to_string(),
            label: "None".to_string(),
            description: "No personality, minimal identity prompt".to_string(),
        },
    ]
}

/// Create the provider selection list.
pub fn provider_select_items() -> Vec<SelectItem<String>> {
    vec![
        SelectItem {
            value: "anthropic".to_string(),
            label: "Anthropic".to_string(),
            description: "Claude models (recommended)".to_string(),
        },
        SelectItem {
            value: "openai".to_string(),
            label: "OpenAI".to_string(),
            description: "GPT models".to_string(),
        },
        SelectItem {
            value: "gemini".to_string(),
            label: "Gemini".to_string(),
            description: "Google Gemini".to_string(),
        },
        SelectItem {
            value: "grok".to_string(),
            label: "Grok".to_string(),
            description: "xAI Grok".to_string(),
        },
        SelectItem {
            value: "openrouter".to_string(),
            label: "OpenRouter".to_string(),
            description: "Multiple providers via proxy".to_string(),
        },
        SelectItem {
            value: "zai".to_string(),
            label: "Z.ai".to_string(),
            description: "Zhipu GLM models".to_string(),
        },
        SelectItem {
            value: "minimax".to_string(),
            label: "MiniMax".to_string(),
            description: "MiniMax models".to_string(),
        },
        SelectItem {
            value: "ollama".to_string(),
            label: "Ollama".to_string(),
            description: "Local models via Ollama".to_string(),
        },
    ]
}

/// Create the model selection list for a provider.
pub fn model_select_items(provider: &str) -> Vec<SelectItem<String>> {
    use temm1e_core::types::model_registry::{
        available_models_for_provider, is_vision_model, model_limits,
    };

    available_models_for_provider(provider)
        .into_iter()
        .map(|model| {
            let (ctx_window, max_output) = model_limits(model);
            let vision = if is_vision_model(model) {
                " | Vision"
            } else {
                ""
            };
            SelectItem {
                value: model.to_string(),
                label: model.to_string(),
                description: format!(
                    "{}K ctx / {}K out{}",
                    ctx_window / 1000,
                    max_output / 1000,
                    vision,
                ),
            }
        })
        .collect()
}
