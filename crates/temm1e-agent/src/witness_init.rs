//! Witness runtime attachment factory.
//!
//! Builds the three runtime hooks (`with_witness`, `with_cambium_trust`,
//! `with_auto_planner_oath`) from the TOML-side `WitnessConfig` once per
//! process. Callers reuse the returned `Arc`s across every `AgentRuntime`
//! construction site (Start, Chat, TUI, plus the ~25 secondary rebuilds
//! when providers/models switch).
//!
//! Returns `None` when `WitnessConfig::enabled = false` so the wiring path
//! is a no-op for users who opt out. All failure modes (Ledger DB open
//! error, malformed strictness string) propagate as `WitnessInitError`
//! and are surfaced at startup, not during the user's first turn.

use std::path::PathBuf;
use std::sync::Arc;

use temm1e_cambium::trust::TrustEngine;
use temm1e_core::types::cambium::TrustState;
use temm1e_core::types::config::WitnessConfig;
use temm1e_witness::config::WitnessStrictness;
use temm1e_witness::ledger::Ledger;
use temm1e_witness::witness::Witness;
use tokio::sync::Mutex;

use crate::runtime::AgentRuntime;

/// Holds the three runtime attachments produced from a single `WitnessConfig`.
/// `Clone` is cheap — all heavy state lives behind `Arc`s so cloning just
/// bumps refcounts. This lets spawned tasks capture their own copy.
#[derive(Clone)]
pub struct WitnessAttachments {
    pub witness: Arc<Witness>,
    pub trust: Arc<Mutex<TrustEngine>>,
    pub strictness: WitnessStrictness,
    pub show_readout: bool,
    pub auto_planner_oath: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum WitnessInitError {
    #[error("ledger open failed at {path}: {source}")]
    LedgerOpen {
        path: String,
        #[source]
        source: temm1e_witness::error::WitnessError,
    },
    #[error("invalid strictness '{0}' (expected: observe|warn|block|block_with_retry)")]
    InvalidStrictness(String),
    #[error("workspace path resolution failed: {0}")]
    Workspace(#[from] std::io::Error),
}

/// Build the witness attachments. Returns `Ok(None)` if disabled.
pub async fn build_witness_attachments(
    cfg: &WitnessConfig,
) -> Result<Option<WitnessAttachments>, WitnessInitError> {
    if !cfg.enabled {
        return Ok(None);
    }

    let strictness = parse_strictness(&cfg.strictness)?;
    let ledger_path = resolve_ledger_path(cfg.ledger_path.as_deref());
    let ledger_url = format!("sqlite:{}?mode=rwc", ledger_path.display());

    let ledger =
        Ledger::open(&ledger_url)
            .await
            .map_err(|source| WitnessInitError::LedgerOpen {
                path: ledger_url.clone(),
                source,
            })?;

    let workspace = std::env::current_dir()?;
    let witness = Arc::new(Witness::new(ledger, workspace));
    let trust = Arc::new(Mutex::new(TrustEngine::new(TrustState::default(), None)));

    Ok(Some(WitnessAttachments {
        witness,
        trust,
        strictness,
        show_readout: cfg.show_readout,
        auto_planner_oath: cfg.auto_planner_oath,
    }))
}

impl AgentRuntime {
    /// Chain Witness wiring onto a runtime if attachments are present.
    /// Used at every user-facing `AgentRuntime` construction site — including
    /// the ~25 rebuilds when providers/models switch at runtime. Pass `None`
    /// to no-op for opt-out users.
    pub fn with_witness_attachments(self, attachments: Option<&WitnessAttachments>) -> Self {
        match attachments {
            Some(att) => self
                .with_witness(att.witness.clone(), att.strictness, att.show_readout)
                .with_cambium_trust(att.trust.clone())
                .with_auto_planner_oath(att.auto_planner_oath),
            None => self,
        }
    }

    /// Same as `with_witness_attachments` but forces `auto_planner_oath = false`
    /// regardless of what the config says. The Witness and TrustEngine stay
    /// attached so the Ledger still records any manually-sealed Oaths and
    /// the TrustEngine reflects worker verdicts when present.
    ///
    /// Currently unused — v5.5.0 ships Hive workers in **active** mode with
    /// parent workspace_path propagation (see `src/main.rs` Hive dispatch
    /// site). Retained as public API for future call sites that want the
    /// observer layer without proactive Planner generation (e.g. low-budget
    /// worker pools, read-only eval agents).
    pub fn with_witness_attachments_passive(
        self,
        attachments: Option<&WitnessAttachments>,
    ) -> Self {
        match attachments {
            Some(att) => self
                .with_witness(att.witness.clone(), att.strictness, att.show_readout)
                .with_cambium_trust(att.trust.clone()),
            None => self,
        }
    }
}

fn parse_strictness(s: &str) -> Result<WitnessStrictness, WitnessInitError> {
    match s.to_ascii_lowercase().as_str() {
        "observe" => Ok(WitnessStrictness::Observe),
        "warn" => Ok(WitnessStrictness::Warn),
        "block" => Ok(WitnessStrictness::Block),
        "block_with_retry" | "blockwithretry" => Ok(WitnessStrictness::BlockWithRetry),
        other => Err(WitnessInitError::InvalidStrictness(other.to_string())),
    }
}

fn resolve_ledger_path(override_path: Option<&str>) -> PathBuf {
    if let Some(p) = override_path {
        return PathBuf::from(p);
    }
    let mut home = dirs::home_dir().unwrap_or_else(|| PathBuf::from("."));
    home.push(".temm1e");
    home.push("witness.db");
    home
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_all_four_strictness_variants() {
        assert_eq!(
            parse_strictness("observe").unwrap(),
            WitnessStrictness::Observe
        );
        assert_eq!(parse_strictness("warn").unwrap(), WitnessStrictness::Warn);
        assert_eq!(parse_strictness("WARN").unwrap(), WitnessStrictness::Warn);
        assert_eq!(parse_strictness("block").unwrap(), WitnessStrictness::Block);
        assert_eq!(
            parse_strictness("block_with_retry").unwrap(),
            WitnessStrictness::BlockWithRetry
        );
        assert_eq!(
            parse_strictness("blockwithretry").unwrap(),
            WitnessStrictness::BlockWithRetry
        );
    }

    #[test]
    fn invalid_strictness_rejected() {
        let err = parse_strictness("yolo").unwrap_err();
        assert!(matches!(err, WitnessInitError::InvalidStrictness(_)));
    }

    #[tokio::test]
    async fn disabled_config_returns_none() {
        let cfg = WitnessConfig {
            enabled: false,
            ..WitnessConfig::default()
        };
        let result = build_witness_attachments(&cfg).await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn enabled_config_builds_attachments() {
        let tmp = tempfile::tempdir().unwrap();
        let cfg = WitnessConfig {
            ledger_path: Some(tmp.path().join("witness.db").to_string_lossy().into()),
            ..WitnessConfig::default()
        };
        let attached = build_witness_attachments(&cfg).await.unwrap().unwrap();
        assert_eq!(attached.strictness, WitnessStrictness::Warn);
        assert!(attached.auto_planner_oath);
    }
}
