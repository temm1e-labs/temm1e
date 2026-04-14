//! # temm1e-witness
//!
//! Witness is TEMM1E's verification system for preventing hallucinated
//! completion in agentic AI. It implements the **Oath / Witness / Ledger**
//! trinity: the agent pre-commits a machine-checkable contract (the Oath)
//! before executing a task; an independent Witness verifies the contract
//! against real world state; and a tamper-evident Ledger records every
//! claim, verdict, and evidence hash.
//!
//! See `tems_lab/witness/RESEARCH_PAPER.md` for the theoretical framework
//! and `tems_lab/witness/IMPLEMENTATION_DETAILS.md` for the implementation
//! specification.
//!
//! ## The Five Laws
//!
//! Witness's behavior is governed by five invariants, enforced as property
//! tests in `tests/laws.rs`:
//!
//! 1. **Pre-Commitment.** No subtask executes without a sealed Oath
//!    containing at least one machine-checkable postcondition.
//! 2. **Independent Verdict.** A subtask's status transitions to `Verified`
//!    only through a Witness verdict — the agent has no self-marking API.
//! 3. **Immutable History.** The Ledger is append-only, hash-chained, and
//!    the root is anchored in `temm1e-watchdog`.
//! 4. **Loud Failure.** Any unverifiable claim surfaces as an explicit
//!    failure in the final reply, never as silent success.
//! 5. **Narrative-Only FAIL.** A FAIL verdict controls the agent's
//!    final-reply narrative — it never deletes files, rolls back diffs,
//!    or blocks delivery.
//!
//! ## Scope
//!
//! Phase 1 implements the Root Oath verification path. Subtask-graph
//! integration is deferred to Phase 3+. See `IMPLEMENTATION_DETAILS.md`
//! §15 "Phase 1 Scope Note" for details.

pub mod auto_detect;
pub mod config;
pub mod error;
pub mod ledger;
pub mod oath;
pub mod planner;
pub mod predicate_sets;
pub mod predicates;
pub mod types;
pub mod witness;

pub use error::WitnessError;
pub use ledger::Ledger;
pub use oath::seal_oath;
pub use types::{
    Claim, Evidence, EvidenceId, EvidenceKind, EvidenceSpec, LedgerEntry, LedgerEntryType,
    LedgerPayload, Oath, OutputStream, Predicate, PredicateResult, RootGoalId, SessionId,
    SubtaskId, TierUsage, Verdict, VerdictOutcome, WitnessSubTaskStatus, WitnessSubtask,
};
pub use witness::Witness;
