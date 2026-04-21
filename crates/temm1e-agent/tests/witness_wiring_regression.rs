//! Regression tests for the Witness wiring path (`witness_init` + the
//! `with_witness_attachments` builder method). These guard the v5.5.0
//! shipping invariant: when `[witness] enabled = false`, the runtime
//! must behave identically to a runtime built without ANY Witness
//! wiring at all — same shape, same builder chain, same observable
//! behavior. Existing users who opt out cannot regress.
//!
//! Two property tests cover the symmetry directly. Together they prove
//! the wiring is purely additive and reversible at config time.

use std::sync::Arc;

use temm1e_agent::witness_init::{build_witness_attachments, WitnessAttachments};
use temm1e_agent::AgentRuntime;
use temm1e_core::traits::{Memory, Provider, Tool};
use temm1e_core::types::config::WitnessConfig;
use temm1e_test_utils::{MockMemory, MockProvider};

fn baseline_runtime() -> AgentRuntime {
    let provider: Arc<dyn Provider> = Arc::new(MockProvider::with_text("ok"));
    let memory: Arc<dyn Memory> = Arc::new(MockMemory::new());
    let tools: Vec<Arc<dyn Tool>> = vec![];
    AgentRuntime::new(provider, memory, tools, "test-model".to_string(), None)
}

#[tokio::test]
async fn disabled_config_returns_no_attachments() {
    // T1.a: Master switch off → factory returns None → no wiring possible.
    let cfg = WitnessConfig {
        enabled: false,
        ..WitnessConfig::default()
    };
    let attached = build_witness_attachments(&cfg).await.unwrap();
    assert!(
        attached.is_none(),
        "WitnessConfig.enabled=false must produce zero attachments"
    );
}

#[tokio::test]
async fn applying_none_attachments_is_identity() {
    // T1.b: with_witness_attachments(None) on a runtime is a no-op.
    // The returned runtime has no witness, no trust engine, no auto-planner.
    // We can't introspect AgentRuntime fields directly (private), so we
    // assert via behavior: the runtime must drive a process_message
    // without any Witness-induced reply mutation, identical to baseline.
    let baseline = baseline_runtime();
    let wired_with_none = baseline_runtime().with_witness_attachments(None);

    use temm1e_core::types::message::InboundMessage;
    use temm1e_core::types::rbac::Role;
    use temm1e_core::types::session::SessionContext;

    let make_msg = || InboundMessage {
        id: "test-1".into(),
        channel: "test".into(),
        chat_id: "test".into(),
        user_id: "test".into(),
        username: None,
        text: Some("hello".into()),
        attachments: vec![],
        reply_to: None,
        timestamp: chrono::Utc::now(),
    };
    let make_session = || SessionContext {
        session_id: "test".into(),
        channel: "test".into(),
        chat_id: "test".into(),
        user_id: "test".into(),
        role: Role::Admin,
        history: vec![],
        workspace_path: std::path::PathBuf::from("."),
        read_tracker: Arc::new(tokio::sync::RwLock::new(std::collections::HashSet::new())),
    };

    let mut s_baseline = make_session();
    let mut s_wired = make_session();

    let (reply_baseline, _) = baseline
        .process_message(&make_msg(), &mut s_baseline, None, None, None, None, None)
        .await
        .expect("baseline runtime processes message");
    let (reply_wired, _) = wired_with_none
        .process_message(&make_msg(), &mut s_wired, None, None, None, None, None)
        .await
        .expect("wired-with-None runtime processes message");

    assert_eq!(
        reply_baseline.text, reply_wired.text,
        "with_witness_attachments(None) must preserve byte-identical reply"
    );
}

#[tokio::test]
async fn enabled_config_attachments_carry_correct_strictness() {
    // T1.c: When enabled with non-default strictness, the attachment
    // surfaces the right WitnessStrictness value so the runtime gate
    // applies the requested behavior.
    let tmp = tempfile::tempdir().unwrap();

    for (input, expected) in [
        (
            "observe",
            temm1e_witness::config::WitnessStrictness::Observe,
        ),
        ("warn", temm1e_witness::config::WitnessStrictness::Warn),
        ("block", temm1e_witness::config::WitnessStrictness::Block),
        (
            "block_with_retry",
            temm1e_witness::config::WitnessStrictness::BlockWithRetry,
        ),
    ] {
        let cfg = WitnessConfig {
            strictness: input.to_string(),
            ledger_path: Some(
                tmp.path()
                    .join(format!("witness-{}.db", input))
                    .to_string_lossy()
                    .into(),
            ),
            ..WitnessConfig::default()
        };

        let attached: WitnessAttachments = build_witness_attachments(&cfg)
            .await
            .unwrap()
            .unwrap_or_else(|| panic!("strictness={} should yield attachments", input));

        assert_eq!(
            attached.strictness, expected,
            "strictness '{}' did not parse to expected variant",
            input
        );
    }
}

#[tokio::test]
async fn auto_planner_oath_flag_threads_through() {
    // T1.d: The auto_planner_oath bool must round-trip from config to attachment
    // so the runtime sees the user's choice, not a hardcoded default.
    let tmp = tempfile::tempdir().unwrap();

    let cfg_off = WitnessConfig {
        ledger_path: Some(
            tmp.path()
                .join("witness-planner.db")
                .to_string_lossy()
                .into(),
        ),
        auto_planner_oath: false,
        ..WitnessConfig::default()
    };
    let off = build_witness_attachments(&cfg_off).await.unwrap().unwrap();
    assert!(
        !off.auto_planner_oath,
        "auto_planner_oath=false must persist"
    );

    let cfg_on = WitnessConfig {
        ledger_path: Some(
            tmp.path()
                .join("witness-planner-on.db")
                .to_string_lossy()
                .into(),
        ),
        auto_planner_oath: true,
        ..WitnessConfig::default()
    };
    let on = build_witness_attachments(&cfg_on).await.unwrap().unwrap();
    assert!(on.auto_planner_oath, "auto_planner_oath=true must persist");
}
