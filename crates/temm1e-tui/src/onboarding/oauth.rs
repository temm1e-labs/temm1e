//! OAuth device flow integration for the TUI onboarding.

/// Placeholder for OAuth device flow display state.
/// Full implementation requires temm1e-codex-oauth device_flow module.
#[derive(Debug, Clone, Default)]
pub struct OAuthDeviceState {
    pub user_code: Option<String>,
    pub verification_uri: Option<String>,
    pub waiting: bool,
    pub error: Option<String>,
}
