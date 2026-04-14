//! Oath sealing and Spec Reviewer.
//!
//! An Oath is the agent's frozen commitment to a set of machine-checkable
//! postconditions that must hold when a subtask is complete. Sealing an Oath
//! means:
//!
//! 1. Running the Spec Reviewer (schema check — deterministic, no LLM).
//! 2. Computing the SHA256 hash over the Oath's contents.
//! 3. Appending an `OathSealed` entry to the Ledger.
//!
//! The Spec Reviewer rejects lenient Oaths. Code-producing tasks require
//! both a wiring check (symbol referenced ≥2 times) and a stub check
//! (anti-pattern like `todo!`, `raise NotImplementedError`, etc.).

use crate::error::WitnessError;
use crate::ledger::Ledger;
use crate::types::{LedgerPayload, Oath};
use chrono::Utc;
use sha2::{Digest, Sha256};
use std::sync::Arc;

/// Seal an Oath: run Spec Reviewer, compute hash, append to Ledger.
///
/// Returns the sealed Oath (with `sealed_hash` and `sealed_at` populated)
/// and the Ledger entry ID of the `OathSealed` record.
pub async fn seal_oath(ledger: &Arc<Ledger>, mut oath: Oath) -> Result<(Oath, i64), WitnessError> {
    if oath.is_sealed() {
        return Err(WitnessError::AlreadySealed(oath.subtask_id.clone()));
    }

    review_oath_schema(&oath)?;

    oath.sealed_at = Utc::now();
    oath.sealed_hash = hash_oath(&oath);

    let entry = ledger
        .append(
            oath.session_id.clone(),
            Some(oath.subtask_id.clone()),
            Some(oath.root_goal_id.clone()),
            LedgerPayload::OathSealed(oath.clone()),
            0.0,
            0,
        )
        .await?;

    Ok((oath, entry.entry_id))
}

/// Review an Oath's schema for minimum rigor.
///
/// Enforced rules:
/// - At least one postcondition.
/// - At least one Tier 0 postcondition (deterministic).
/// - For code-producing tasks (heuristic: goal mentions code/file/symbol
///   terms), require at least one wiring check and one stub/placeholder
///   anti-pattern check.
pub fn review_oath_schema(oath: &Oath) -> Result<(), WitnessError> {
    if oath.postconditions.is_empty() {
        return Err(WitnessError::LenientOath(
            "no postconditions declared".into(),
        ));
    }
    if !oath.postconditions.iter().any(|p| p.is_tier0()) {
        return Err(WitnessError::LenientOath(
            "no Tier 0 (deterministic) postcondition — at least one required".into(),
        ));
    }

    if mentions_code(&oath.goal) {
        let has_wiring = oath.postconditions.iter().any(|p| p.is_wiring_check());
        let has_stub_check = oath.postconditions.iter().any(|p| p.is_stub_check());
        if !has_wiring {
            return Err(WitnessError::LenientOath(
                "code-producing task must include a wiring check (GrepCountAtLeast n>=2)".into(),
            ));
        }
        if !has_stub_check {
            return Err(WitnessError::LenientOath(
                "code-producing task must include an anti-stub check (GrepAbsent on todo/unimplemented/stub patterns)".into(),
            ));
        }
    }

    Ok(())
}

/// Compute a stable SHA256 hash over an Oath. Uses serde_json canonical
/// serialization (field order determined by struct definition order).
pub fn hash_oath(oath: &Oath) -> String {
    // Exclude sealed_hash itself from the hash to avoid circular dependency.
    let mut clone = oath.clone();
    clone.sealed_hash = String::new();
    let bytes = serde_json::to_vec(&clone).expect("Oath serializes");
    let mut h = Sha256::new();
    h.update(&bytes);
    hex::encode(h.finalize())
}

/// Heuristic: does this goal look like it's producing or touching code?
fn mentions_code(goal: &str) -> bool {
    let g = goal.to_lowercase();
    const KEYWORDS: &[&str] = &[
        "add ",
        "implement",
        "write ",
        "create ",
        "build ",
        "modify ",
        "fix ",
        "refactor",
        "wire ",
        "integrate",
        "function",
        "module",
        "class ",
        "method",
        "endpoint",
        "component",
        "test ",
        "struct",
        "enum ",
        "trait",
        "backend",
        "frontend",
        "api ",
        "crate ",
        "library",
        "package",
        ".rs",
        ".py",
        ".js",
        ".ts",
        ".go",
        ".java",
        ".rb",
        ".php",
    ];
    KEYWORDS.iter().any(|kw| g.contains(kw))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Predicate;
    use std::path::PathBuf;

    fn draft_goal(goal: &str) -> Oath {
        Oath::draft("st-1", "root-1", "sess-1", goal)
    }

    #[test]
    fn review_empty_postconditions_fails() {
        let oath = draft_goal("just do it");
        let r = review_oath_schema(&oath);
        assert!(matches!(r, Err(WitnessError::LenientOath(_))));
    }

    #[test]
    fn review_no_tier0_fails() {
        let oath = draft_goal("just do it").with_postcondition(Predicate::AspectVerifier {
            rubric: "looks good?".into(),
            evidence_refs: vec![],
            advisory: true,
        });
        let r = review_oath_schema(&oath);
        assert!(matches!(r, Err(WitnessError::LenientOath(_))));
    }

    #[test]
    fn review_non_code_with_tier0_passes() {
        let oath = draft_goal("reply to the user with a greeting").with_postcondition(
            Predicate::FileExists {
                path: PathBuf::from("/tmp/greeting.txt"),
            },
        );
        assert!(review_oath_schema(&oath).is_ok());
    }

    #[test]
    fn review_code_task_missing_wiring_fails() {
        let oath = draft_goal("add a new module called foo")
            .with_postcondition(Predicate::FileExists {
                path: PathBuf::from("src/foo.rs"),
            })
            .with_postcondition(Predicate::GrepAbsent {
                pattern: "todo!".into(),
                path_glob: "src/foo.rs".into(),
            });
        let r = review_oath_schema(&oath);
        assert!(matches!(r, Err(WitnessError::LenientOath(msg)) if msg.contains("wiring")));
    }

    #[test]
    fn review_code_task_missing_stub_check_fails() {
        let oath = draft_goal("implement function bar")
            .with_postcondition(Predicate::FileExists {
                path: PathBuf::from("src/bar.rs"),
            })
            .with_postcondition(Predicate::GrepCountAtLeast {
                pattern: "bar".into(),
                path_glob: "src/**/*.rs".into(),
                n: 2,
            });
        let r = review_oath_schema(&oath);
        assert!(matches!(r, Err(WitnessError::LenientOath(msg)) if msg.contains("stub")));
    }

    #[test]
    fn review_code_task_with_all_checks_passes() {
        let oath = draft_goal("add new function foo")
            .with_postcondition(Predicate::FileExists {
                path: PathBuf::from("src/foo.rs"),
            })
            .with_postcondition(Predicate::GrepCountAtLeast {
                pattern: "foo".into(),
                path_glob: "src/**/*.rs".into(),
                n: 2,
            })
            .with_postcondition(Predicate::GrepAbsent {
                pattern: "todo!".into(),
                path_glob: "src/foo.rs".into(),
            });
        assert!(review_oath_schema(&oath).is_ok());
    }

    #[test]
    fn hash_is_deterministic() {
        let oath = draft_goal("goal A").with_postcondition(Predicate::FileExists {
            path: PathBuf::from("/tmp/a"),
        });
        let h1 = hash_oath(&oath);
        let h2 = hash_oath(&oath);
        assert_eq!(h1, h2);
        assert_eq!(h1.len(), 64);
    }

    #[test]
    fn hash_changes_when_postcondition_changes() {
        let oath1 = draft_goal("goal").with_postcondition(Predicate::FileExists {
            path: PathBuf::from("/tmp/a"),
        });
        let oath2 = draft_goal("goal").with_postcondition(Predicate::FileExists {
            path: PathBuf::from("/tmp/b"),
        });
        assert_ne!(hash_oath(&oath1), hash_oath(&oath2));
    }

    #[test]
    fn hash_excludes_sealed_hash_itself() {
        // Sealing again should not change the hash.
        let mut oath = draft_goal("goal").with_postcondition(Predicate::FileExists {
            path: PathBuf::from("/tmp/a"),
        });
        let h1 = hash_oath(&oath);
        oath.sealed_hash = "fake".into();
        let h2 = hash_oath(&oath);
        assert_eq!(h1, h2);
    }

    #[tokio::test]
    async fn seal_oath_writes_ledger_entry() {
        let ledger = Ledger::open("sqlite::memory:").await.unwrap();
        let oath = draft_goal("add new function foo")
            .with_postcondition(Predicate::FileExists {
                path: PathBuf::from("src/foo.rs"),
            })
            .with_postcondition(Predicate::GrepCountAtLeast {
                pattern: "foo".into(),
                path_glob: "src/**/*.rs".into(),
                n: 2,
            })
            .with_postcondition(Predicate::GrepAbsent {
                pattern: "todo!".into(),
                path_glob: "src/foo.rs".into(),
            });

        let (sealed, entry_id) = seal_oath(&ledger, oath).await.unwrap();
        assert!(sealed.is_sealed());
        assert_eq!(entry_id, 1);
        assert_eq!(ledger.count_total().await.unwrap(), 1);
    }

    #[tokio::test]
    async fn seal_oath_rejects_lenient() {
        let ledger = Ledger::open("sqlite::memory:").await.unwrap();
        let oath = draft_goal("just say hi");
        let r = seal_oath(&ledger, oath).await;
        assert!(matches!(r, Err(WitnessError::LenientOath(_))));
        assert_eq!(ledger.count_total().await.unwrap(), 0);
    }

    #[tokio::test]
    async fn seal_oath_rejects_double_seal() {
        let ledger = Ledger::open("sqlite::memory:").await.unwrap();
        let mut oath = draft_goal("reply").with_postcondition(Predicate::FileExists {
            path: PathBuf::from("/tmp/x"),
        });
        oath.sealed_hash = "a".repeat(64);
        let r = seal_oath(&ledger, oath).await;
        assert!(matches!(r, Err(WitnessError::AlreadySealed(_))));
    }
}
