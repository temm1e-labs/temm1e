//! Property tests for the Five Laws of Witness.
//!
//! These tests are the canonical invariants Witness must uphold.
//! See `tems_lab/witness/RESEARCH_PAPER.md` §5.5 for the full statement.
//!
//! Each test is named `law<N>_<description>` for easy grep-ability:
//!
//! - Law 1 — Pre-Commitment: no subtask executes without a sealed Oath
//!   containing at least one machine-checkable postcondition.
//! - Law 2 — Independent Verdict: only Witness produces Verified outcomes.
//! - Law 3 — Immutable History: ledger is append-only and hash-chained;
//!   tampering is detectable.
//! - Law 4 — Loud Failure: unverifiable claims surface as explicit failure.
//! - Law 5 — Narrative-Only FAIL: FAIL never deletes/rolls back work.

use std::path::PathBuf;
use std::sync::Arc;
use tempfile::tempdir;

use temm1e_witness::{
    config::WitnessStrictness,
    ledger::Ledger,
    oath::{review_oath_schema, seal_oath},
    types::{LedgerPayload, Oath, Predicate, VerdictOutcome, WitnessSubTaskStatus},
    witness::Witness,
    WitnessError,
};

async fn bootstrap() -> (Arc<Witness>, tempfile::TempDir) {
    let dir = tempdir().unwrap();
    let ledger = Ledger::open("sqlite::memory:").await.unwrap();
    let witness = Arc::new(Witness::new(ledger, dir.path().to_path_buf()));
    (witness, dir)
}

// ===========================================================================
// Law 1 — Pre-Commitment
// ===========================================================================

#[tokio::test]
async fn law1_oath_with_no_postconditions_rejected() {
    let oath = Oath::draft("st-1", "root-1", "sess-1", "just do something");
    let result = review_oath_schema(&oath);
    assert!(matches!(result, Err(WitnessError::LenientOath(_))));
}

#[tokio::test]
async fn law1_oath_with_only_tier1_rejected() {
    let oath = Oath::draft("st-1", "root-1", "sess-1", "something subjective").with_postcondition(
        Predicate::AspectVerifier {
            rubric: "is it good?".into(),
            evidence_refs: vec![],
            advisory: false,
        },
    );
    let result = review_oath_schema(&oath);
    assert!(matches!(result, Err(WitnessError::LenientOath(_))));
}

#[tokio::test]
async fn law1_code_task_without_wiring_check_rejected() {
    let oath = Oath::draft("st-1", "root-1", "sess-1", "implement a new function")
        .with_postcondition(Predicate::FileExists {
            path: PathBuf::from("src/new.py"),
        })
        .with_postcondition(Predicate::GrepAbsent {
            pattern: "TODO".into(),
            path_glob: "*.py".into(),
        });
    let result = review_oath_schema(&oath);
    assert!(matches!(result, Err(WitnessError::LenientOath(msg)) if msg.contains("wiring")));
}

#[tokio::test]
async fn law1_code_task_without_stub_check_rejected() {
    let oath = Oath::draft("st-1", "root-1", "sess-1", "implement function foo")
        .with_postcondition(Predicate::FileExists {
            path: PathBuf::from("src/foo.py"),
        })
        .with_postcondition(Predicate::GrepCountAtLeast {
            pattern: "foo".into(),
            path_glob: "**/*.py".into(),
            n: 2,
        });
    let result = review_oath_schema(&oath);
    assert!(matches!(result, Err(WitnessError::LenientOath(msg)) if msg.contains("stub")));
}

#[tokio::test]
async fn law1_sealing_writes_oath_to_ledger() {
    let (witness, _dir) = bootstrap().await;
    let oath = Oath::draft("st-1", "root-1", "sess-1", "reply with a file").with_postcondition(
        Predicate::FileExists {
            path: PathBuf::from("/tmp/x"),
        },
    );
    let (sealed, entry_id) = seal_oath(witness.ledger(), oath).await.unwrap();

    assert!(sealed.is_sealed());
    assert_eq!(entry_id, 1);
    let entries = witness.ledger().read_session("sess-1").await.unwrap();
    assert!(entries
        .iter()
        .any(|e| matches!(e.payload, LedgerPayload::OathSealed(_))));
}

// ===========================================================================
// Law 2 — Independent Verdict
// ===========================================================================

#[tokio::test]
async fn law2_only_witness_produces_verified_terminal_state() {
    // Default construction is NotStarted — not terminal, not Verified.
    assert!(!WitnessSubTaskStatus::NotStarted.is_terminal());
    assert!(!WitnessSubTaskStatus::InProgress.is_terminal());
    assert!(!WitnessSubTaskStatus::Claimed.is_terminal());
    assert!(WitnessSubTaskStatus::Verified.is_terminal());
}

#[tokio::test]
async fn law2_verify_is_the_only_verified_producer() {
    let (witness, dir) = bootstrap().await;
    let f = dir.path().join("a.txt");
    tokio::fs::write(&f, "x").await.unwrap();

    let oath = Oath::draft("st-1", "root-1", "sess-1", "touch a file")
        .with_postcondition(Predicate::FileExists { path: f });
    let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();

    let verdict = witness.verify_oath(&sealed).await.unwrap();
    assert_eq!(verdict.outcome, VerdictOutcome::Pass);

    // The Verdict entry appears in the Ledger, signed by Witness.
    let entries = witness.ledger().read_session("sess-1").await.unwrap();
    assert!(entries
        .iter()
        .any(|e| matches!(e.payload, LedgerPayload::VerdictRendered(_))));
}

#[tokio::test]
async fn law2_unsealed_oath_cannot_be_verified() {
    let (witness, _dir) = bootstrap().await;
    let oath = Oath::draft("st-1", "root-1", "sess-1", "x");
    // Never sealed → verify_oath must refuse.
    let r = witness.verify_oath(&oath).await;
    assert!(matches!(r, Err(WitnessError::NoSealedOath(_))));
}

// ===========================================================================
// Law 3 — Immutable History
// ===========================================================================

#[tokio::test]
async fn law3_append_only_preserves_hash_chain() {
    let (witness, dir) = bootstrap().await;
    let f = dir.path().join("a.txt");
    tokio::fs::write(&f, "x").await.unwrap();

    for i in 0..5 {
        let oath = Oath::draft(
            format!("st-{}", i),
            "root-1",
            format!("sess-{}", i),
            "touch a file",
        )
        .with_postcondition(Predicate::FileExists { path: f.clone() });
        let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
        witness.verify_oath(&sealed).await.unwrap();
        tokio::time::sleep(std::time::Duration::from_millis(2)).await;
    }
    witness.ledger().verify_integrity().await.unwrap();
}

#[tokio::test]
async fn law3_duplicate_append_produces_distinct_hashes() {
    let (witness, dir) = bootstrap().await;
    let f = dir.path().join("a.txt");
    tokio::fs::write(&f, "x").await.unwrap();

    let oath1 = Oath::draft("st-1", "root-1", "sess-1", "touch a file")
        .with_postcondition(Predicate::FileExists { path: f.clone() });
    let (s1, _) = seal_oath(witness.ledger(), oath1).await.unwrap();

    tokio::time::sleep(std::time::Duration::from_millis(2)).await;

    let oath2 = Oath::draft("st-2", "root-1", "sess-1", "touch a file")
        .with_postcondition(Predicate::FileExists { path: f.clone() });
    let (s2, _) = seal_oath(witness.ledger(), oath2).await.unwrap();

    // Distinct subtask IDs yield distinct Oath hashes.
    assert_ne!(s1.sealed_hash, s2.sealed_hash);

    // And the ledger chain has two distinct entry_hashes.
    let entries = witness.ledger().read_session("sess-1").await.unwrap();
    let oath_entries: Vec<_> = entries
        .iter()
        .filter(|e| matches!(e.payload, LedgerPayload::OathSealed(_)))
        .collect();
    assert_eq!(oath_entries.len(), 2);
    assert_ne!(oath_entries[0].entry_hash, oath_entries[1].entry_hash);
}

// ===========================================================================
// Law 4 — Loud Failure
// ===========================================================================

#[tokio::test]
async fn law4_fail_verdict_surfaces_in_final_reply_block() {
    let (witness, dir) = bootstrap().await;
    // Do not create the file — the predicate will fail.
    let oath = Oath::draft("st-1", "root-1", "sess-1", "make the file").with_postcondition(
        Predicate::FileExists {
            path: dir.path().join("missing.txt"),
        },
    );
    let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
    let verdict = witness.verify_oath(&sealed).await.unwrap();

    assert_eq!(verdict.outcome, VerdictOutcome::Fail);

    let reply = witness.compose_final_reply("Done!", &verdict, WitnessStrictness::Block);
    assert!(reply.contains("Partial completion"));
    assert!(reply.contains("Could not verify"));
    assert!(!reply.starts_with("Done!"));
}

#[tokio::test]
async fn law4_fail_verdict_warns_in_final_reply_warn() {
    let (witness, dir) = bootstrap().await;
    let oath = Oath::draft("st-1", "root-1", "sess-1", "make the file").with_postcondition(
        Predicate::FileExists {
            path: dir.path().join("missing.txt"),
        },
    );
    let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
    let verdict = witness.verify_oath(&sealed).await.unwrap();

    let reply = witness.compose_final_reply("Done!", &verdict, WitnessStrictness::Warn);
    assert!(reply.starts_with("Done!"));
    assert!(reply.contains("Witness:"));
}

#[tokio::test]
async fn law4_observe_mode_does_not_rewrite_reply() {
    let (witness, dir) = bootstrap().await;
    let oath = Oath::draft("st-1", "root-1", "sess-1", "make the file").with_postcondition(
        Predicate::FileExists {
            path: dir.path().join("missing.txt"),
        },
    );
    let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
    let verdict = witness.verify_oath(&sealed).await.unwrap();

    let reply = witness.compose_final_reply("Done!", &verdict, WitnessStrictness::Observe);
    assert_eq!(reply, "Done!");
}

// ===========================================================================
// Law 5 — Narrative-Only FAIL
// ===========================================================================

#[tokio::test]
async fn law5_fail_verdict_does_not_delete_files() {
    let (witness, dir) = bootstrap().await;
    let sentinel = dir.path().join("sentinel.txt");
    tokio::fs::write(&sentinel, "original contents")
        .await
        .unwrap();

    // Oath references a MISSING file, guaranteeing FAIL.
    let oath =
        Oath::draft("st-1", "root-1", "sess-1", "x").with_postcondition(Predicate::FileExists {
            path: dir.path().join("missing.txt"),
        });
    let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
    let verdict = witness.verify_oath(&sealed).await.unwrap();
    assert_eq!(verdict.outcome, VerdictOutcome::Fail);

    // Sentinel file must survive Witness's FAIL verdict.
    assert!(sentinel.exists(), "Law 5 violation: sentinel file deleted");
    let contents = std::fs::read_to_string(&sentinel).unwrap();
    assert_eq!(contents, "original contents");
}

#[tokio::test]
async fn law5_fail_verdict_does_not_truncate_files() {
    let (witness, dir) = bootstrap().await;
    let sentinel = dir.path().join("big.bin");
    let blob: Vec<u8> = (0..1024).map(|i| (i % 256) as u8).collect();
    tokio::fs::write(&sentinel, &blob).await.unwrap();

    let oath =
        Oath::draft("st-1", "root-1", "sess-1", "x").with_postcondition(Predicate::FileExists {
            path: dir.path().join("never.txt"),
        });
    let (sealed, _) = seal_oath(witness.ledger(), oath).await.unwrap();
    let _ = witness.verify_oath(&sealed).await.unwrap();

    // Blob is untouched.
    let actual = std::fs::read(&sentinel).unwrap();
    assert_eq!(actual, blob);
}

#[tokio::test]
async fn law5_witness_crate_source_has_no_destructive_apis() {
    // Scan the entire temm1e-witness src directory for destructive API
    // patterns. Sentinels are constructed via concat!() so the literal
    // strings do not appear in any source file.
    use std::path::Path;

    let crate_src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let forbidden: &[&str] = &[
        concat!("remove", "_file"),
        concat!("remove", "_dir_all"),
        concat!("fs::remove", "_dir"),
        concat!("git re", "set --hard"),
        concat!("rm ", "-rf"),
    ];

    for entry in walk(&crate_src) {
        if entry.extension().and_then(|e| e.to_str()) != Some("rs") {
            continue;
        }
        let src = std::fs::read_to_string(&entry).unwrap();
        for pat in forbidden {
            assert!(
                !src.contains(pat),
                "Law 5 violation: {} contains destructive API `{}`",
                entry.display(),
                pat
            );
        }
    }
}

fn walk(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut out = Vec::new();
    if !root.exists() {
        return out;
    }
    for entry in std::fs::read_dir(root).unwrap() {
        let e = entry.unwrap();
        let p = e.path();
        if p.is_dir() {
            out.extend(walk(&p));
        } else {
            out.push(p);
        }
    }
    out
}
