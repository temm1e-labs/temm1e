//! Witness integration smoke test (Phase 1, standalone).
//!
//! This test proves that `temm1e-witness` can be driven end-to-end against
//! a simulated agent trajectory from inside the `temm1e-agent` crate, without
//! touching the live runtime hot path. It exercises:
//!
//! 1. Oath creation + Spec Reviewer rejection of lenient oaths.
//! 2. Oath sealing into a real SQLite ledger.
//! 3. Witness verification against simulated "agent output" (real files).
//! 4. Final-reply composition on both PASS and FAIL outcomes.
//! 5. Full Law 1–5 invariants.
//!
//! Phase 2 will wire Witness into `runtime.rs` as a post-Finishing gate.
//! Phase 1 demonstrates the integration is feasible and zero-risk.

use std::path::PathBuf;
use tempfile::tempdir;

use temm1e_witness::{
    config::WitnessStrictness,
    ledger::Ledger,
    oath::seal_oath,
    types::{Oath, Predicate, VerdictOutcome},
    witness::Witness,
    WitnessError,
};

/// Helper: build a Ledger, a Witness, and a scratch workspace.
async fn bootstrap() -> (std::sync::Arc<Witness>, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let ledger = Ledger::open("sqlite::memory:").await.unwrap();
    let witness = std::sync::Arc::new(Witness::new(ledger, dir.path().to_path_buf()));
    (witness, dir)
}

/// Simulate an agent that actually produces working files.
/// Returns the list of files it created.
async fn simulate_honest_agent(dir: &std::path::Path) -> Vec<PathBuf> {
    let module = dir.join("my_module.py");
    tokio::fs::write(
        &module,
        "def my_function():\n    return 42\n\nif __name__ == '__main__':\n    print(my_function())\n",
    )
    .await
    .unwrap();

    let test_file = dir.join("test_my_module.py");
    tokio::fs::write(
        &test_file,
        "from my_module import my_function\n\ndef test_my_function():\n    assert my_function() == 42\n",
    )
    .await
    .unwrap();

    vec![module, test_file]
}

/// Simulate a lying agent that claims to have implemented a function
/// but leaves only a stub.
async fn simulate_lying_agent(dir: &std::path::Path) -> Vec<PathBuf> {
    let module = dir.join("my_module.py");
    tokio::fs::write(
        &module,
        "def my_function():\n    raise NotImplementedError  # TODO: implement\n",
    )
    .await
    .unwrap();
    vec![module]
}

/// Simulate an agent that forgets to write the file at all.
async fn simulate_fiction_agent(_dir: &std::path::Path) -> Vec<PathBuf> {
    // Writes nothing.
    vec![]
}

/// Build the Root Oath for "implement my_function" with the standard
/// code-task rigor: FileExists + wiring check + anti-stub check.
fn code_oath(session_id: &str, dir: &std::path::Path) -> Oath {
    Oath::draft(
        "root",
        "root-goal-1",
        session_id,
        "implement my_function in a new module",
    )
    .with_postcondition(Predicate::FileExists {
        path: dir.join("my_module.py"),
    })
    .with_postcondition(Predicate::GrepCountAtLeast {
        pattern: "my_function".into(),
        path_glob: "*.py".into(),
        n: 2,
    })
    .with_postcondition(Predicate::GrepAbsent {
        pattern: "raise NotImplementedError|# TODO".into(),
        path_glob: "*.py".into(),
    })
}

#[tokio::test]
async fn witness_passes_honest_agent() {
    let (witness, dir) = bootstrap().await;
    simulate_honest_agent(dir.path()).await;

    let oath = code_oath("sess-honest", dir.path());
    let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
    let verdict = witness.verify_oath(&sealed).await.unwrap();

    assert_eq!(verdict.outcome, VerdictOutcome::Pass);
    assert_eq!(verdict.fail_count(), 0);
    assert_eq!(verdict.pass_count(), 3);
}

#[tokio::test]
async fn witness_catches_lying_agent() {
    let (witness, dir) = bootstrap().await;
    simulate_lying_agent(dir.path()).await;

    let oath = code_oath("sess-lying", dir.path());
    let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
    let verdict = witness.verify_oath(&sealed).await.unwrap();

    // File exists → pass. Wiring check fails (only 1 match). Anti-stub fails
    // (NotImplementedError present).
    assert_eq!(verdict.outcome, VerdictOutcome::Fail);
    assert!(verdict.fail_count() >= 2);
    assert!(verdict.reason.contains("fail"));
}

#[tokio::test]
async fn witness_catches_fiction_agent() {
    let (witness, dir) = bootstrap().await;
    simulate_fiction_agent(dir.path()).await;

    let oath = code_oath("sess-fiction", dir.path());
    let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
    let verdict = witness.verify_oath(&sealed).await.unwrap();

    // File doesn't exist → fail. Wiring check fails (no files match glob).
    // Anti-stub is vacuously true (no files to scan).
    assert_eq!(verdict.outcome, VerdictOutcome::Fail);
    assert!(verdict.fail_count() >= 2);
}

#[tokio::test]
async fn lenient_oath_rejected_by_spec_reviewer() {
    let (witness, dir) = bootstrap().await;
    simulate_honest_agent(dir.path()).await;

    // No wiring check and no anti-stub check → should be rejected.
    let oath = Oath::draft("root", "root-1", "sess-1", "implement my_function").with_postcondition(
        Predicate::FileExists {
            path: dir.path().join("my_module.py"),
        },
    );
    let result = seal_oath(witness.ledger(), oath).await;
    assert!(matches!(result, Err(WitnessError::LenientOath(_))));
}

#[tokio::test]
async fn final_reply_rewritten_honestly_on_fail() {
    let (witness, dir) = bootstrap().await;
    simulate_lying_agent(dir.path()).await;

    let oath = code_oath("sess-lying", dir.path());
    let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
    let verdict = witness.verify_oath(&sealed).await.unwrap();

    // Agent claims "Done!", Witness rewrites to honest partial completion.
    let agent_reply = "Done! I've implemented my_function.";
    let final_reply = witness.compose_final_reply(agent_reply, &verdict, WitnessStrictness::Block);

    assert!(final_reply.contains("Partial completion"));
    assert!(final_reply.contains("Could not verify"));
    assert!(!final_reply.starts_with("Done!"));
}

#[tokio::test]
async fn law5_lying_agent_files_are_not_deleted() {
    let (witness, dir) = bootstrap().await;
    let created = simulate_lying_agent(dir.path()).await;

    // Snapshot file state before Witness.
    let before: Vec<(PathBuf, Vec<u8>)> = created
        .iter()
        .map(|p| (p.clone(), std::fs::read(p).unwrap()))
        .collect();

    let oath = code_oath("sess-lying", dir.path());
    let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
    let _verdict = witness.verify_oath(&sealed).await.unwrap();

    // Law 5: Witness must not have touched any files, even after FAIL.
    for (path, expected_bytes) in &before {
        assert!(
            path.exists(),
            "Law 5 violation: file {} was deleted",
            path.display()
        );
        let actual = std::fs::read(path).unwrap();
        assert_eq!(
            &actual,
            expected_bytes,
            "Law 5 violation: file {} was modified",
            path.display()
        );
    }
}

#[tokio::test]
async fn law3_ledger_hash_chain_intact_after_verification() {
    let (witness, dir) = bootstrap().await;
    simulate_honest_agent(dir.path()).await;

    let oath = code_oath("sess-a", dir.path());
    let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
    witness.verify_oath(&sealed).await.unwrap();

    // Chain must verify cleanly.
    witness.ledger().verify_integrity().await.unwrap();
    // At minimum: OathSealed + VerdictRendered.
    assert!(witness.ledger().count_total().await.unwrap() >= 2);
}

#[tokio::test]
async fn runtime_with_cambium_trust_compiles_and_attaches() {
    // Smoke test that the with_cambium_trust builder works end-to-end and
    // the TrustEngine handle is attached. The actual record_verdict call
    // happens inside the runtime gate, which requires a full process_message
    // cycle — covered separately by run-time integration when needed.
    use std::sync::Arc;
    use temm1e_agent::runtime::AgentRuntime;
    use temm1e_cambium::trust::TrustEngine;
    use temm1e_core::types::cambium::TrustState;
    use temm1e_test_utils::{MockMemory, MockProvider};
    use temm1e_witness::config::WitnessStrictness;
    use tokio::sync::Mutex;

    let (witness, _dir) = bootstrap().await;
    let trust = Arc::new(Mutex::new(TrustEngine::new(TrustState::default(), None)));

    let provider: Arc<dyn temm1e_core::traits::Provider> = Arc::new(MockProvider::with_text("ok"));
    let memory: Arc<dyn temm1e_core::traits::Memory> = Arc::new(MockMemory::new());
    let _runtime = AgentRuntime::new(provider, memory, vec![], "test-model".into(), None)
        .with_witness(witness.clone(), WitnessStrictness::Block, true)
        .with_cambium_trust(trust.clone());

    // The runtime is constructed and the trust handle is reachable. Initial
    // state: no verdicts recorded, no streak.
    let t = trust.lock().await;
    assert_eq!(t.state().level3_streak, 0);
    assert_eq!(t.state().level2_streak, 0);
    assert_eq!(t.state().recent_rollbacks, 0);
}

#[tokio::test]
async fn runtime_with_witness_builder_attaches_witness() {
    // Construct a minimal AgentRuntime with a Witness attached via the
    // new with_witness() builder. This proves the plumbing compiles and
    // the witness is reachable through the runtime.
    use std::sync::Arc;
    use temm1e_agent::runtime::AgentRuntime;
    use temm1e_test_utils::{MockMemory, MockProvider};
    use temm1e_witness::config::WitnessStrictness;

    let (witness, dir) = bootstrap().await;

    // Seal an Oath for a session and simulate an honest agent.
    simulate_honest_agent(dir.path()).await;
    let oath = code_oath("sess-runtime", dir.path());
    let (_, _) = seal_oath(witness.ledger(), oath).await.unwrap();

    // Build a runtime with witness attached (Observe strictness so we
    // can validate that the verdict flows through without mutating work).
    let provider: Arc<dyn temm1e_core::traits::Provider> = Arc::new(MockProvider::with_text("ok"));
    let memory: Arc<dyn temm1e_core::traits::Memory> = Arc::new(MockMemory::new());
    let _runtime = AgentRuntime::new(provider, memory, vec![], "test-model".to_string(), None)
        .with_witness(witness.clone(), WitnessStrictness::Observe, true);

    // Confirm the Witness can still see the sealed oath via its own API.
    // (The full runtime hook path is exercised when process_message runs;
    // this smoke test validates the builder + crate graph.)
    let active = witness.active_oath("sess-runtime").await.unwrap();
    assert!(active.is_some(), "sealed oath should be visible");
    assert_eq!(active.unwrap().subtask_id, "root");
}

#[tokio::test]
async fn cross_session_ledger_isolation() {
    let (witness, dir) = bootstrap().await;
    simulate_honest_agent(dir.path()).await;

    let (sealed_a, _) = seal_oath(witness.ledger(), code_oath("sess-a", dir.path()))
        .await
        .unwrap();
    witness.verify_oath(&sealed_a).await.unwrap();

    let (sealed_b, _) = seal_oath(witness.ledger(), code_oath("sess-b", dir.path()))
        .await
        .unwrap();
    witness.verify_oath(&sealed_b).await.unwrap();

    let a = witness.ledger().read_session("sess-a").await.unwrap();
    let b = witness.ledger().read_session("sess-b").await.unwrap();

    assert!(a.iter().all(|e| e.session_id == "sess-a"));
    assert!(b.iter().all(|e| e.session_id == "sess-b"));
    // Each session has at least an OathSealed and VerdictRendered entry.
    assert!(a.len() >= 2);
    assert!(b.len() >= 2);
}
