//! Core Witness types: Oath, Predicate, Evidence, Claim, Verdict, LedgerEntry.
//!
//! These types are the public vocabulary of Witness. They are kept deliberately
//! small and closed — new predicate types require adding enum variants here.
//!
//! See `tems_lab/witness/IMPLEMENTATION_DETAILS.md` §5 for the full specification.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

// ---------------------------------------------------------------------------
// Type aliases
// ---------------------------------------------------------------------------

pub type SubtaskId = String;
pub type SessionId = String;
pub type RootGoalId = String;
pub type EvidenceId = String;

// ---------------------------------------------------------------------------
// Predicate
// ---------------------------------------------------------------------------

/// A machine-checkable postcondition.
///
/// Tier 0 predicates (FileExists through NotOf) are deterministic and
/// LLM-free. Tier 1 (`AspectVerifier`) and Tier 2 (`AdversarialJudge`)
/// use LLM calls and are never authoritative alone — a Tier 0 failure
/// always overrides higher-tier passes.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Predicate {
    // ------- File system (Tier 0, 8 variants) -------
    FileExists {
        path: PathBuf,
    },
    FileAbsent {
        path: PathBuf,
    },
    DirectoryExists {
        path: PathBuf,
    },
    FileContains {
        path: PathBuf,
        regex: String,
    },
    FileDoesNotContain {
        path: PathBuf,
        regex: String,
    },
    FileHashEquals {
        path: PathBuf,
        sha256_hex: String,
    },
    FileSizeInRange {
        path: PathBuf,
        min_bytes: u64,
        max_bytes: u64,
    },
    FileModifiedWithin {
        path: PathBuf,
        duration_secs: u64,
    },

    // ------- Command execution (Tier 0, 4 variants) -------
    CommandExits {
        cmd: String,
        args: Vec<String>,
        expected_code: i32,
        cwd: Option<PathBuf>,
        timeout_ms: u64,
    },
    CommandOutputContains {
        cmd: String,
        args: Vec<String>,
        regex: String,
        stream: OutputStream,
        cwd: Option<PathBuf>,
        timeout_ms: u64,
    },
    CommandOutputAbsent {
        cmd: String,
        args: Vec<String>,
        regex: String,
        stream: OutputStream,
        cwd: Option<PathBuf>,
        timeout_ms: u64,
    },
    CommandDurationUnder {
        cmd: String,
        args: Vec<String>,
        max_ms: u64,
        cwd: Option<PathBuf>,
    },

    // ------- Process and system (Tier 0, 2 variants) -------
    ProcessAlive {
        name_or_pid: String,
    },
    PortListening {
        port: u16,
        interface: Option<String>,
    },

    // ------- Network (Tier 0, 2 variants) -------
    HttpStatus {
        url: String,
        method: String,
        expected_status: u16,
    },
    HttpBodyContains {
        url: String,
        method: String,
        regex: String,
    },

    // ------- Version control (Tier 0, 4 variants) -------
    GitFileInDiff {
        path: PathBuf,
        include_staged: bool,
        include_unstaged: bool,
    },
    GitDiffLineCountAtMost {
        max: u64,
        scope: GitScope,
    },
    GitNewFilesMatch {
        glob: String,
    },
    GitCommitMessageMatches {
        regex: String,
        commits_back: u32,
    },

    // ------- Text search (Tier 0, 3 variants) -------
    GrepPresent {
        pattern: String,
        path_glob: String,
    },
    GrepAbsent {
        pattern: String,
        path_glob: String,
    },
    GrepCountAtLeast {
        pattern: String,
        path_glob: String,
        n: u32,
    },

    // ------- Time (Tier 0, 1 variant) -------
    ElapsedUnder {
        start_marker: String,
        max_secs: u64,
    },

    // ------- Composite (Tier 0, 3 variants) -------
    AllOf {
        predicates: Vec<Predicate>,
    },
    AnyOf {
        predicates: Vec<Predicate>,
    },
    NotOf {
        predicate: Box<Predicate>,
    },

    // ------- Tier 1 (cheap aspect verifier — clean-slate LLM) -------
    AspectVerifier {
        rubric: String,
        evidence_refs: Vec<EvidenceId>,
        advisory: bool,
    },

    // ------- Tier 2 (adversarial auditor — last resort, advisory) -------
    AdversarialJudge {
        rubric: String,
        evidence_refs: Vec<EvidenceId>,
        advisory: bool,
    },
}

impl Predicate {
    /// Returns true if this predicate can be checked without any LLM call.
    pub fn is_tier0(&self) -> bool {
        !matches!(
            self,
            Predicate::AspectVerifier { .. } | Predicate::AdversarialJudge { .. }
        )
    }

    /// Returns the tier (0, 1, or 2) this predicate requires.
    pub fn tier(&self) -> u8 {
        match self {
            Predicate::AspectVerifier { .. } => 1,
            Predicate::AdversarialJudge { .. } => 2,
            _ => 0,
        }
    }

    /// Returns true if this is a wiring check (GrepCountAtLeast with n >= 2).
    /// Required by the Spec Reviewer for code-producing tasks.
    pub fn is_wiring_check(&self) -> bool {
        matches!(self, Predicate::GrepCountAtLeast { n, .. } if *n >= 2)
    }

    /// Returns true if this is a stub/placeholder anti-pattern check.
    /// Required by the Spec Reviewer for code-producing tasks.
    pub fn is_stub_check(&self) -> bool {
        match self {
            Predicate::GrepAbsent { pattern, .. }
            | Predicate::FileDoesNotContain { regex: pattern, .. } => {
                let p = pattern.to_lowercase();
                p.contains("todo")
                    || p.contains("unimplemented")
                    || p.contains("notimplementederror")
                    || p.contains("stub")
                    || p.contains("placeholder")
            }
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream {
    Stdout,
    Stderr,
    Either,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum GitScope {
    Staged,
    Unstaged,
    Both,
    LastCommit,
}

// ---------------------------------------------------------------------------
// Oath
// ---------------------------------------------------------------------------

/// A sealed pre-commitment of what "done" means for a subtask (or the root goal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Oath {
    pub subtask_id: SubtaskId,
    pub root_goal_id: RootGoalId,
    pub session_id: SessionId,
    pub goal: String,
    pub preconditions: Vec<Predicate>,
    pub postconditions: Vec<Predicate>,
    pub evidence_required: Vec<EvidenceSpec>,
    pub rollback: Option<String>,
    pub active_predicate_sets: Vec<String>,
    pub template_vars: BTreeMap<String, String>,
    /// Hex-encoded SHA256 over the above fields (zero-hash until sealed).
    pub sealed_hash: String,
    pub sealed_at: DateTime<Utc>,
}

impl Oath {
    /// Create a draft Oath with empty sealed_hash. Must be sealed before use.
    pub fn draft(
        subtask_id: impl Into<SubtaskId>,
        root_goal_id: impl Into<RootGoalId>,
        session_id: impl Into<SessionId>,
        goal: impl Into<String>,
    ) -> Self {
        Self {
            subtask_id: subtask_id.into(),
            root_goal_id: root_goal_id.into(),
            session_id: session_id.into(),
            goal: goal.into(),
            preconditions: Vec::new(),
            postconditions: Vec::new(),
            evidence_required: Vec::new(),
            rollback: None,
            active_predicate_sets: Vec::new(),
            template_vars: BTreeMap::new(),
            sealed_hash: String::new(),
            sealed_at: DateTime::<Utc>::from_timestamp(0, 0).unwrap(),
        }
    }

    /// Returns true if this Oath has been sealed.
    pub fn is_sealed(&self) -> bool {
        !self.sealed_hash.is_empty() && self.sealed_hash.len() == 64
    }

    /// Add a postcondition. Chainable.
    pub fn with_postcondition(mut self, p: Predicate) -> Self {
        self.postconditions.push(p);
        self
    }

    /// Add a precondition. Chainable.
    pub fn with_precondition(mut self, p: Predicate) -> Self {
        self.preconditions.push(p);
        self
    }

    /// Count of Tier 0 postconditions.
    pub fn tier0_count(&self) -> usize {
        self.postconditions.iter().filter(|p| p.is_tier0()).count()
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceSpec {
    pub id: EvidenceId,
    pub kind: EvidenceKind,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum EvidenceKind {
    File { path: PathBuf },
    CommandOutput { cmd: String, args: Vec<String> },
    TestResult { test_name: String },
    HttpResponse { url: String },
    Free,
}

// ---------------------------------------------------------------------------
// Evidence, Claim, Verdict
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub id: EvidenceId,
    pub subtask_id: SubtaskId,
    pub produced_at: DateTime<Utc>,
    pub produced_by_tool: Option<String>,
    pub kind: EvidenceKind,
    /// Hex SHA256 of the evidence contents.
    pub blob_hash: String,
    pub blob_size: u64,
    /// First 200 chars for audit visibility.
    pub preview: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Claim {
    pub subtask_id: SubtaskId,
    pub claimed_at: DateTime<Utc>,
    pub claim_text: String,
    pub evidence_refs: Vec<EvidenceId>,
    pub agent_step_id: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Verdict {
    pub subtask_id: SubtaskId,
    pub rendered_at: DateTime<Utc>,
    pub outcome: VerdictOutcome,
    pub per_predicate: Vec<PredicateResult>,
    pub tier_usage: TierUsage,
    pub reason: String,
    pub cost_usd: f64,
    pub latency_ms: u64,
}

impl Verdict {
    pub fn pass_count(&self) -> u32 {
        self.per_predicate
            .iter()
            .filter(|r| r.outcome == VerdictOutcome::Pass)
            .count() as u32
    }

    pub fn fail_count(&self) -> u32 {
        self.per_predicate
            .iter()
            .filter(|r| r.outcome == VerdictOutcome::Fail)
            .count() as u32
    }

    pub fn inconclusive_count(&self) -> u32 {
        self.per_predicate
            .iter()
            .filter(|r| r.outcome == VerdictOutcome::Inconclusive)
            .count() as u32
    }

    pub fn total_count(&self) -> u32 {
        self.per_predicate.len() as u32
    }

    pub fn is_pass(&self) -> bool {
        self.outcome == VerdictOutcome::Pass
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum VerdictOutcome {
    Pass,
    Fail,
    Inconclusive,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PredicateResult {
    pub predicate: Predicate,
    pub tier: u8,
    pub outcome: VerdictOutcome,
    pub detail: String,
    pub advisory: bool,
    pub latency_ms: u64,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TierUsage {
    pub tier0_calls: u32,
    pub tier1_calls: u32,
    pub tier2_calls: u32,
    pub tier0_latency_ms: u64,
    pub tier1_latency_ms: u64,
    pub tier2_latency_ms: u64,
}

// ---------------------------------------------------------------------------
// Ledger entry
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub entry_id: i64,
    pub session_id: SessionId,
    pub subtask_id: Option<SubtaskId>,
    pub root_goal_id: Option<RootGoalId>,
    pub entry_type: LedgerEntryType,
    pub payload: LedgerPayload,
    pub payload_hash: String,
    pub prev_entry_hash: Option<String>,
    pub entry_hash: String,
    pub schema_version: u32,
    pub cost_usd: f64,
    pub latency_ms: u64,
    pub created_at: DateTime<Utc>,
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LedgerEntryType {
    OathSealed,
    ClaimSubmitted,
    EvidenceProduced,
    VerdictRendered,
    SkipRequested,
    SkipApproved,
    SkipDenied,
    TaskCompleted,
    TaskFailed,
    TamperAlarm,
    CostSkipped,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum LedgerPayload {
    OathSealed(Oath),
    ClaimSubmitted(Claim),
    EvidenceProduced(Evidence),
    VerdictRendered(Verdict),
    SkipRequested {
        subtask_id: SubtaskId,
        reason: String,
        requested_at: DateTime<Utc>,
    },
    SkipApproved {
        subtask_id: SubtaskId,
        reason: String,
        approved_at: DateTime<Utc>,
    },
    SkipDenied {
        subtask_id: SubtaskId,
        reason: String,
        denied_at: DateTime<Utc>,
    },
    TaskCompleted {
        root_goal_id: RootGoalId,
        completed_at: DateTime<Utc>,
    },
    TaskFailed {
        root_goal_id: RootGoalId,
        failure_summary: String,
        failed_at: DateTime<Utc>,
    },
    TamperAlarm {
        detected_at: DateTime<Utc>,
        expected_root: String,
        actual_root: String,
    },
    CostSkipped {
        subtask_id: SubtaskId,
        predicate_index: usize,
        reason: String,
    },
}

impl LedgerPayload {
    pub fn entry_type(&self) -> LedgerEntryType {
        match self {
            LedgerPayload::OathSealed(_) => LedgerEntryType::OathSealed,
            LedgerPayload::ClaimSubmitted(_) => LedgerEntryType::ClaimSubmitted,
            LedgerPayload::EvidenceProduced(_) => LedgerEntryType::EvidenceProduced,
            LedgerPayload::VerdictRendered(_) => LedgerEntryType::VerdictRendered,
            LedgerPayload::SkipRequested { .. } => LedgerEntryType::SkipRequested,
            LedgerPayload::SkipApproved { .. } => LedgerEntryType::SkipApproved,
            LedgerPayload::SkipDenied { .. } => LedgerEntryType::SkipDenied,
            LedgerPayload::TaskCompleted { .. } => LedgerEntryType::TaskCompleted,
            LedgerPayload::TaskFailed { .. } => LedgerEntryType::TaskFailed,
            LedgerPayload::TamperAlarm { .. } => LedgerEntryType::TamperAlarm,
            LedgerPayload::CostSkipped { .. } => LedgerEntryType::CostSkipped,
        }
    }
}

// ---------------------------------------------------------------------------
// Witness-owned subtask status and subtask
// ---------------------------------------------------------------------------

/// Witness's own subtask status, independent of `temm1e_agent::SubTaskStatus`.
///
/// In Phase 1 Witness uses this to track the root goal only (one per session).
/// In Phase 3+, this will replace the legacy `SubTaskStatus`.
///
/// The transitions are enforced internally: once in `Verified`, the only
/// further transitions are informational (no re-entry to `InProgress`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum WitnessSubTaskStatus {
    NotStarted,
    InProgress,
    Claimed,
    Verified,
    Failed { reason: String },
    SkipRequested { reason: String },
    SkipApproved { reason: String },
}

impl WitnessSubTaskStatus {
    pub fn is_terminal(&self) -> bool {
        matches!(
            self,
            WitnessSubTaskStatus::Verified
                | WitnessSubTaskStatus::Failed { .. }
                | WitnessSubTaskStatus::SkipApproved { .. }
        )
    }
}

/// A Witness-tracked work item. Phase 1: one per session (the root goal).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WitnessSubtask {
    pub id: SubtaskId,
    pub root_goal_id: RootGoalId,
    pub session_id: SessionId,
    pub oath: Oath,
    pub status: WitnessSubTaskStatus,
    pub evidence: Vec<Evidence>,
    pub verdicts: Vec<Verdict>,
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn predicate_is_tier0_correctly() {
        assert!(Predicate::FileExists {
            path: PathBuf::from("/tmp/x")
        }
        .is_tier0());
        assert!(Predicate::GrepPresent {
            pattern: "foo".into(),
            path_glob: "*.rs".into()
        }
        .is_tier0());
        assert!(!Predicate::AspectVerifier {
            rubric: "is it clear?".into(),
            evidence_refs: vec![],
            advisory: false,
        }
        .is_tier0());
        assert!(!Predicate::AdversarialJudge {
            rubric: "is it false?".into(),
            evidence_refs: vec![],
            advisory: true,
        }
        .is_tier0());
    }

    #[test]
    fn predicate_tier_numbers() {
        assert_eq!(
            Predicate::FileExists {
                path: PathBuf::from("/tmp/x")
            }
            .tier(),
            0
        );
        assert_eq!(
            Predicate::AspectVerifier {
                rubric: "x".into(),
                evidence_refs: vec![],
                advisory: false,
            }
            .tier(),
            1
        );
        assert_eq!(
            Predicate::AdversarialJudge {
                rubric: "x".into(),
                evidence_refs: vec![],
                advisory: true,
            }
            .tier(),
            2
        );
    }

    #[test]
    fn wiring_check_detection() {
        let wiring = Predicate::GrepCountAtLeast {
            pattern: "my_fn".into(),
            path_glob: "src/**/*.rs".into(),
            n: 2,
        };
        assert!(wiring.is_wiring_check());

        let single = Predicate::GrepCountAtLeast {
            pattern: "my_fn".into(),
            path_glob: "src/**/*.rs".into(),
            n: 1,
        };
        assert!(!single.is_wiring_check());
    }

    #[test]
    fn stub_check_detection() {
        let p = Predicate::GrepAbsent {
            pattern: "todo!\\(|unimplemented!\\(".into(),
            path_glob: "src/**/*.rs".into(),
        };
        assert!(p.is_stub_check());

        let p2 = Predicate::GrepAbsent {
            pattern: "NotImplementedError".into(),
            path_glob: "**/*.py".into(),
        };
        assert!(p2.is_stub_check());

        let not_stub = Predicate::GrepAbsent {
            pattern: "console.log".into(),
            path_glob: "**/*.js".into(),
        };
        assert!(!not_stub.is_stub_check());
    }

    #[test]
    fn oath_draft_is_not_sealed() {
        let oath = Oath::draft("st-1", "root-1", "sess-1", "do a thing");
        assert!(!oath.is_sealed());
        assert_eq!(oath.tier0_count(), 0);
    }

    #[test]
    fn oath_tier0_count() {
        let oath = Oath::draft("st-1", "root-1", "sess-1", "goal")
            .with_postcondition(Predicate::FileExists {
                path: PathBuf::from("/tmp/a"),
            })
            .with_postcondition(Predicate::AspectVerifier {
                rubric: "is it good?".into(),
                evidence_refs: vec![],
                advisory: true,
            });
        assert_eq!(oath.tier0_count(), 1);
    }

    #[test]
    fn witness_subtask_status_terminal() {
        assert!(WitnessSubTaskStatus::Verified.is_terminal());
        assert!(WitnessSubTaskStatus::Failed {
            reason: "nope".into()
        }
        .is_terminal());
        assert!(WitnessSubTaskStatus::SkipApproved {
            reason: "ok".into()
        }
        .is_terminal());
        assert!(!WitnessSubTaskStatus::InProgress.is_terminal());
        assert!(!WitnessSubTaskStatus::Claimed.is_terminal());
    }

    #[test]
    fn ledger_payload_entry_type() {
        let oath = Oath::draft("st-1", "root-1", "sess-1", "goal");
        let payload = LedgerPayload::OathSealed(oath);
        assert_eq!(payload.entry_type(), LedgerEntryType::OathSealed);
    }
}
