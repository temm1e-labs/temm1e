//! System lifecycle events emitted to owner-facing channels.
//!
//! These are out-of-band notifications about the gateway itself (startup,
//! shutdown, etc.) — distinct from in-band agent messages. They take the
//! direct `Channel.send_message` path; no LLM round-trip.
//!
//! The enum is `#[non_exhaustive]` so future variants can be added without
//! a breaking change. v5.5.5 fires `Startup` and `Shutdown` only;
//! `WatchdogRestart`, `UpdateApplied`, and `FatalError` are defined for
//! follow-up wiring (each needs separate plumbing — marker files for the
//! first two, a sync-channel relay from the panic hook for the third).
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub enum SystemEvent {
    /// Gateway has come online and at least one channel is ready.
    Startup {
        version: String,
        channels: Vec<String>,
    },
    /// Gateway is shutting down (graceful drain in progress).
    Shutdown { reason: ShutdownReason },
    /// Process was restarted by the watchdog after a crash. Not fired in
    /// v5.5.5 — needs a watchdog→main marker file.
    WatchdogRestart { previous_version: Option<String> },
    /// In-place binary update completed. Not fired in v5.5.5 — needs a
    /// post-update marker file written by `temm1e update apply`.
    UpdateApplied {
        from_version: String,
        to_version: String,
    },
    /// Unrecoverable error caught by the global panic hook. Not fired in
    /// v5.5.5 — panic hooks cannot `.await` and need a sync-channel relay.
    FatalError {
        summary: String,
        location: Option<String>,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum ShutdownReason {
    /// User pressed Ctrl+C or sent SIGINT/SIGTERM.
    CtrlC,
    /// Shutdown initiated by an in-place update.
    Update,
    /// Shutdown due to an unrecoverable error.
    FatalError,
}

impl SystemEvent {
    /// Stable identifier for filtering/metrics. Lowercase snake_case.
    pub fn kind(&self) -> &'static str {
        match self {
            SystemEvent::Startup { .. } => "startup",
            SystemEvent::Shutdown { .. } => "shutdown",
            SystemEvent::WatchdogRestart { .. } => "watchdog_restart",
            SystemEvent::UpdateApplied { .. } => "update_applied",
            SystemEvent::FatalError { .. } => "fatal_error",
        }
    }

    /// Owner-facing message text. Plain (no Markdown) so version strings
    /// like `5.5.5-rc.1` cannot trip Telegram's parser.
    pub fn format_message(&self) -> String {
        match self {
            SystemEvent::Startup { version, channels } => {
                if channels.is_empty() {
                    format!("TEMM1E v{version} is online.")
                } else {
                    format!(
                        "TEMM1E v{version} is online. Channels: {}.",
                        channels.join(", ")
                    )
                }
            }
            SystemEvent::Shutdown { reason } => match reason {
                ShutdownReason::CtrlC => "TEMM1E is shutting down (Ctrl+C).".to_string(),
                ShutdownReason::Update => "TEMM1E is shutting down for update.".to_string(),
                ShutdownReason::FatalError => {
                    "TEMM1E is shutting down after a fatal error.".to_string()
                }
            },
            SystemEvent::WatchdogRestart { previous_version } => match previous_version {
                Some(prev) => format!("TEMM1E recovered from a crash (was v{prev})."),
                None => "TEMM1E recovered from a crash.".to_string(),
            },
            SystemEvent::UpdateApplied {
                from_version,
                to_version,
            } => {
                format!("TEMM1E updated from v{from_version} to v{to_version}.")
            }
            SystemEvent::FatalError { summary, location } => match location {
                Some(loc) => format!("TEMM1E hit a fatal error at {loc}: {summary}"),
                None => format!("TEMM1E hit a fatal error: {summary}"),
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn startup_format_includes_version_and_channels() {
        let ev = SystemEvent::Startup {
            version: "5.5.5".into(),
            channels: vec!["telegram".into(), "discord".into()],
        };
        let s = ev.format_message();
        assert!(s.contains("5.5.5"), "msg: {s}");
        assert!(s.contains("online"), "msg: {s}");
        assert!(s.contains("telegram"), "msg: {s}");
        assert!(s.contains("discord"), "msg: {s}");
        // Plain text — no leftover format placeholders.
        assert!(!s.contains('{'), "msg leaked a brace: {s}");
    }

    #[test]
    fn startup_format_handles_empty_channels() {
        let ev = SystemEvent::Startup {
            version: "5.5.5".into(),
            channels: vec![],
        };
        let s = ev.format_message();
        assert!(s.contains("5.5.5"));
        assert!(s.contains("online"));
        assert!(!s.contains("Channels:"));
    }

    #[test]
    fn shutdown_format_reflects_reason() {
        let ctrl_c = SystemEvent::Shutdown {
            reason: ShutdownReason::CtrlC,
        };
        assert!(ctrl_c.format_message().contains("Ctrl+C"));

        let upd = SystemEvent::Shutdown {
            reason: ShutdownReason::Update,
        };
        assert!(upd.format_message().contains("update"));

        let fe = SystemEvent::Shutdown {
            reason: ShutdownReason::FatalError,
        };
        assert!(fe.format_message().contains("fatal"));
    }

    #[test]
    fn kind_strings_are_stable() {
        assert_eq!(
            SystemEvent::Startup {
                version: "x".into(),
                channels: vec![]
            }
            .kind(),
            "startup"
        );
        assert_eq!(
            SystemEvent::Shutdown {
                reason: ShutdownReason::CtrlC
            }
            .kind(),
            "shutdown"
        );
        assert_eq!(
            SystemEvent::WatchdogRestart {
                previous_version: None
            }
            .kind(),
            "watchdog_restart"
        );
        assert_eq!(
            SystemEvent::UpdateApplied {
                from_version: "1".into(),
                to_version: "2".into()
            }
            .kind(),
            "update_applied"
        );
        assert_eq!(
            SystemEvent::FatalError {
                summary: "x".into(),
                location: None
            }
            .kind(),
            "fatal_error"
        );
    }

    #[test]
    fn deferred_variants_format_cleanly() {
        let wr = SystemEvent::WatchdogRestart {
            previous_version: Some("5.5.4".into()),
        };
        assert!(wr.format_message().contains("5.5.4"));

        let upd = SystemEvent::UpdateApplied {
            from_version: "5.5.4".into(),
            to_version: "5.5.5".into(),
        };
        let s = upd.format_message();
        assert!(s.contains("5.5.4"));
        assert!(s.contains("5.5.5"));

        let fe = SystemEvent::FatalError {
            summary: "boom".into(),
            location: Some("main.rs:42".into()),
        };
        let s = fe.format_message();
        assert!(s.contains("boom"));
        assert!(s.contains("main.rs:42"));
    }
}
