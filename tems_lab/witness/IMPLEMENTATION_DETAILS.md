# Witness: Implementation Details

> Every struct, every function, every SQL table, every integration point, every property test.
> Written at 100% confidence. Codebase integration surface verified against current `main` at 2026-04-13.
> Companion to `RESEARCH_PAPER.md`. Ready to implement.

---

## 0. Pointer to Research Paper

See [`RESEARCH_PAPER.md`](RESEARCH_PAPER.md) for the theoretical framework, literature anchors, problem definition, and zero-downside guarantee. This document translates the design into code-level specification: exact types, schemas, prompts, integration hooks, and a phased bring-up plan.

**Non-goal:** this document does not contain implementation code. It specifies what will be implemented so that review can happen before any code is written, per TEMM1E's zero-risk protocol.

---

## 1. New Dependencies

### Workspace additions (root `Cargo.toml` `[workspace.dependencies]`)

```toml
regex = "1.11"                          # Tier 0 predicate regex matching
sha2 = "0.10"                           # Hash-chain SHA256
hex = "0.4"                             # Hash display / storage
which = "7.0"                           # Command-path resolution for CommandExits
```

**Already in workspace (no changes needed):** tokio, tokio-util, chrono, sqlx, serde, serde_json, async-trait, tracing, uuid, thiserror.

### Crate-specific gotchas discovered during research

| Crate | Gotcha | Mitigation |
|---|---|---|
| `regex` | Panics on catastrophic backtracking if user predicate is malformed | Wrap compilation in `Regex::new` fallible path; reject Oath if predicate regex won't compile |
| `sha2` | Hasher state must be fresh per entry | Instantiate `Sha256::new()` inside every `hash_entry()` call, never share |
| `which` | Unix/Windows pathing differs for `which` | Use `which::which()` cross-platform; fall back to `None` on Windows if needed |
| sqlx SQLite | `ALTER TABLE ADD COLUMN` on existing table is idempotent; `BEFORE UPDATE/DELETE` triggers must be added in a separate `CREATE TRIGGER` call after `CREATE TABLE` | Two-step schema init: create table, then create triggers |
| `tokio::process::Command` | `Command::output()` does not apply timeout by default | Wrap in `tokio::time::timeout(duration, cmd.output())` for every Tier 0 command predicate |
| Hash chain determinism | JSON serialization ordering matters for hash stability | Use `serde_json::to_vec` with BTreeMap-based struct field ordering; freeze schema version in hash |

---

## 2. Crate Structure

```
crates/temm1e-witness/
├── Cargo.toml
└── src/
    ├── lib.rs                 — Public API: Witness, Ledger, Oath, Verdict, entry points
    ├── types.rs               — Core types: Oath, Predicate, Evidence, Verdict, LedgerEntry, SubTaskStatus
    ├── predicates/
    │   ├── mod.rs             — Predicate enum + dispatch
    │   ├── filesystem.rs      — FileExists, FileAbsent, DirectoryExists, FileContains, etc.
    │   ├── command.rs         — CommandExits, CommandOutputContains, CommandDurationUnder
    │   ├── process.rs         — ProcessAlive, PortListening
    │   ├── network.rs         — HttpStatus, HttpBodyContains
    │   ├── vcs.rs             — GitFileInDiff, GitDiffLineCountAtMost, etc.
    │   ├── text.rs            — GrepPresent, GrepAbsent, GrepCountAtLeast
    │   ├── time.rs            — ElapsedUnder
    │   └── composite.rs       — AllOf, AnyOf, NotOf
    ├── predicate_sets.rs      — TOML predicate set loader + template interpolation
    ├── auto_detect.rs         — Project type detection from file markers
    ├── oath.rs                — Oath sealing, Spec Reviewer (schema check + optional LLM)
    ├── witness.rs             — Tiered verifier: Tier 0 dispatch, Tier 1/2 LLM orchestration
    ├── ledger.rs              — Append-only ledger with hash chain, SQLite-backed
    ├── anchor.rs              — Watchdog Root Anchor client (IPC to temm1e-watchdog)
    ├── config.rs              — Witness config struct, TOML deserialization, defaults
    ├── metrics.rs             — Cost/latency tracking, Ledger rollup queries
    ├── status.rs              — Witness-specific phase extensions for AgentTaskStatus
    ├── error.rs               — WitnessError enum (mirrors Temm1eError::Witness variant)
    └── tests/
        ├── laws.rs            — Property tests for the Four (Five) Laws
        ├── redteam.rs         — Red-team Oaths (deliberately broken tasks)
        ├── predicates.rs      — Unit tests for every Tier 0 primitive
        ├── predicate_sets.rs  — TOML parse and interpolation tests
        ├── ledger.rs          — Hash-chain integrity tests
        └── end_to_end.rs      — Full Oath → Verify → Ledger flow
```

---

## 3. Cargo.toml

```toml
[package]
name = "temm1e-witness"
version.workspace = true
edition.workspace = true
license.workspace = true
rust-version.workspace = true
description = "Witness: pre-committed verification ledger for TEMM1E agents"

[dependencies]
temm1e-core = { path = "../temm1e-core" }
temm1e-memory = { path = "../temm1e-memory" }
tokio = { workspace = true, features = ["rt", "time", "sync", "process", "macros"] }
tokio-util = { workspace = true }
chrono = { workspace = true, features = ["serde"] }
sqlx = { workspace = true, features = ["runtime-tokio", "sqlite"] }
serde = { workspace = true, features = ["derive"] }
serde_json = { workspace = true }
async-trait = { workspace = true }
tracing = { workspace = true }
uuid = { workspace = true, features = ["v4"] }
thiserror = { workspace = true }
regex = { workspace = true }
sha2 = { workspace = true }
hex = { workspace = true }
which = { workspace = true }
toml = { workspace = true }
reqwest = { workspace = true, features = ["json"] }

[dev-dependencies]
temm1e-test-utils = { path = "../temm1e-test-utils" }
tempfile = { workspace = true }
proptest = { workspace = true }
```

Add `temm1e-witness` to the root `Cargo.toml` `[workspace] members` list.

---

## 4. SQLite Schema

The Ledger table is added to `temm1e-memory/src/sqlite.rs` in the existing `init_schema()` method. It coexists with `memory_entries`, `lambda_memories`, `tool_reliability`, `classification_outcomes`, and `skill_usage`.

### 4.1 Primary Ledger Table

```sql
CREATE TABLE IF NOT EXISTS witness_ledger (
    entry_id         INTEGER PRIMARY KEY AUTOINCREMENT,
    session_id       TEXT NOT NULL,
    subtask_id       TEXT,
    root_goal_id     TEXT,
    entry_type       TEXT NOT NULL,        -- OathSealed | ClaimSubmitted | EvidenceProduced
                                            -- | VerdictRendered | SkipRequested | SkipApproved
                                            -- | SkipDenied | TaskCompleted | TaskFailed
                                            -- | TamperAlarm | CostSkipped
    payload_json     TEXT NOT NULL,        -- Serialized entry payload
    payload_hash     BLOB NOT NULL,        -- SHA256(payload_json bytes)
    prev_entry_hash  BLOB,                 -- Previous entry's entry_hash (NULL for first entry)
    entry_hash       BLOB NOT NULL UNIQUE, -- SHA256(prev_entry_hash || payload_hash || created_at_ms)
    schema_version   INTEGER NOT NULL DEFAULT 1,
    witness_cost_usd REAL NOT NULL DEFAULT 0.0,
    witness_latency_ms INTEGER NOT NULL DEFAULT 0,
    created_at_ms    INTEGER NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_witness_ledger_session ON witness_ledger(session_id);
CREATE INDEX IF NOT EXISTS idx_witness_ledger_subtask ON witness_ledger(subtask_id);
CREATE INDEX IF NOT EXISTS idx_witness_ledger_entry_type ON witness_ledger(entry_type);
CREATE INDEX IF NOT EXISTS idx_witness_ledger_created_at ON witness_ledger(created_at_ms);
```

### 4.2 Append-Only Enforcement (SQL triggers)

```sql
CREATE TRIGGER IF NOT EXISTS witness_ledger_no_update
BEFORE UPDATE ON witness_ledger
BEGIN
    SELECT RAISE(ABORT, 'witness_ledger is append-only: UPDATE is forbidden');
END;

CREATE TRIGGER IF NOT EXISTS witness_ledger_no_delete
BEFORE DELETE ON witness_ledger
BEGIN
    SELECT RAISE(ABORT, 'witness_ledger is append-only: DELETE is forbidden');
END;
```

These triggers prevent row mutation at the database level. Any attempt to `UPDATE` or `DELETE` will raise a SQL error. Tampering detection happens at two additional layers:

1. **Hash chain verification.** Re-chaining from entry 1 must produce the stored `entry_hash` for every row.
2. **Watchdog anchor comparison.** The live root hash must equal the watchdog's sealed copy.

### 4.3 Root Anchor State Table (optional, local audit only)

```sql
CREATE TABLE IF NOT EXISTS witness_root_anchors (
    session_id       TEXT NOT NULL,
    sealed_at_ms     INTEGER NOT NULL,
    sealed_entry_id  INTEGER NOT NULL,
    sealed_root_hash BLOB NOT NULL,
    PRIMARY KEY (session_id, sealed_at_ms)
);
```

This is a mirror of the watchdog's authoritative sealed roots, for observability queries within the main process. The authoritative copy remains in the watchdog.

### 4.4 Schema Migration Strategy

The Ledger is added via idempotent `CREATE TABLE IF NOT EXISTS` + `CREATE TRIGGER IF NOT EXISTS`. No migration of existing data is required (no `memory_entries` rows are affected). Existing installations upgrade transparently on first run.

---

## 5. Core Types

### 5.1 Oath and Predicate Types (`types.rs`)

```rust
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;
use std::time::Duration;

pub type SubtaskId = String;
pub type SessionId = String;
pub type RootGoalId = String;
pub type EvidenceId = String;

/// A sealed pre-commitment of what "done" means for a subtask.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Oath {
    pub subtask_id: SubtaskId,
    pub root_goal_id: RootGoalId,
    pub goal: String,                           // natural language intent
    pub preconditions: Vec<Predicate>,          // optional, checked before execution
    pub postconditions: Vec<Predicate>,         // REQUIRED, at least one Tier 0
    pub evidence_required: Vec<EvidenceSpec>,
    pub rollback: Option<String>,               // free text advice, not executed
    pub active_predicate_sets: Vec<String>,     // e.g. ["rust", "docs"]
    pub template_vars: BTreeMap<String, String>,// interpolation values
    pub sealed_hash: String,                    // hex SHA256 of above
    pub sealed_at: DateTime<Utc>,
}

/// Machine-checkable postcondition.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Predicate {
    // File system (Tier 0)
    FileExists { path: PathBuf },
    FileAbsent { path: PathBuf },
    DirectoryExists { path: PathBuf },
    FileContains { path: PathBuf, regex: String },
    FileDoesNotContain { path: PathBuf, regex: String },
    FileHashEquals { path: PathBuf, sha256: String },
    FileSizeInRange { path: PathBuf, min_bytes: u64, max_bytes: u64 },
    FileModifiedWithin { path: PathBuf, duration_secs: u64 },

    // Command execution (Tier 0)
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

    // Process (Tier 0)
    ProcessAlive { name_or_pid: String },
    PortListening { port: u16, interface: Option<String> },

    // Network (Tier 0)
    HttpStatus { url: String, method: String, expected_status: u16 },
    HttpBodyContains { url: String, method: String, regex: String },

    // Version control (Tier 0)
    GitFileInDiff { path: PathBuf, include_staged: bool, include_unstaged: bool },
    GitDiffLineCountAtMost { max: u64, scope: GitScope },
    GitNewFilesMatch { glob: String },
    GitCommitMessageMatches { regex: String, commits_back: u32 },

    // Text search (Tier 0)
    GrepPresent { pattern: String, path_glob: String },
    GrepAbsent { pattern: String, path_glob: String },
    GrepCountAtLeast { pattern: String, path_glob: String, n: u32 },

    // Time (Tier 0)
    ElapsedUnder { start_marker: String, max_secs: u64 },

    // Composites (Tier 0)
    AllOf { predicates: Vec<Predicate> },
    AnyOf { predicates: Vec<Predicate> },
    NotOf { predicate: Box<Predicate> },

    // Tier 1 (cheap aspect verifier — clean-slate LLM)
    AspectVerifier {
        rubric: String,
        evidence_refs: Vec<EvidenceId>,
        advisory: bool,
    },

    // Tier 2 (adversarial auditor — rare, last resort)
    AdversarialJudge {
        rubric: String,
        evidence_refs: Vec<EvidenceId>,
        advisory: bool,                         // TRUE by default
    },
}

impl Predicate {
    /// True if this predicate is checkable without any LLM call.
    pub fn is_tier0(&self) -> bool {
        !matches!(self, Predicate::AspectVerifier { .. } | Predicate::AdversarialJudge { .. })
    }
}

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OutputStream { Stdout, Stderr, Either }

#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitScope { Staged, Unstaged, Both, LastCommit }

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EvidenceSpec {
    pub id: EvidenceId,
    pub kind: EvidenceKind,
    pub description: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceKind {
    File { path: PathBuf },
    CommandOutput { cmd: String, args: Vec<String> },
    TestResult { test_name: String },
    HttpResponse { url: String },
    Free,
}
```

### 5.2 Evidence, Claim, Verdict (`types.rs` continued)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Evidence {
    pub id: EvidenceId,
    pub subtask_id: SubtaskId,
    pub produced_at: DateTime<Utc>,
    pub produced_by_tool: Option<String>,
    pub kind: EvidenceKind,
    pub blob_hash: String,                  // hex SHA256 of contents
    pub blob_size: u64,
    pub preview: String,                    // first 200 chars for audit visibility
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
    pub reason: String,                     // human-readable summary
    pub cost_usd: f64,
    pub latency_ms: u64,
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
    pub tier: u8,                           // 0, 1, or 2
    pub outcome: VerdictOutcome,
    pub detail: String,                     // what was checked, what was found
    pub advisory: bool,                     // if true, FAIL here does not fail the verdict
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
```

### 5.3 LedgerEntry (`types.rs` continued)

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LedgerEntry {
    pub entry_id: i64,
    pub session_id: SessionId,
    pub subtask_id: Option<SubtaskId>,
    pub root_goal_id: Option<RootGoalId>,
    pub entry_type: LedgerEntryType,
    pub payload: LedgerPayload,
    pub payload_hash: String,               // hex SHA256
    pub prev_entry_hash: Option<String>,    // hex SHA256 of previous entry
    pub entry_hash: String,                 // hex SHA256 of this entry
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
    SkipRequested { subtask_id: SubtaskId, reason: String, requested_at: DateTime<Utc> },
    SkipApproved { subtask_id: SubtaskId, reason: String, approved_at: DateTime<Utc> },
    SkipDenied { subtask_id: SubtaskId, reason: String, denied_at: DateTime<Utc> },
    TaskCompleted { root_goal_id: RootGoalId, completed_at: DateTime<Utc> },
    TaskFailed { root_goal_id: RootGoalId, failure_summary: String, failed_at: DateTime<Utc> },
    TamperAlarm { detected_at: DateTime<Utc>, expected_root: String, actual_root: String },
    CostSkipped { subtask_id: SubtaskId, predicate_index: usize, reason: String },
}
```

### 5.4 SubTaskStatus Extension (`types.rs` continued)

```rust
/// Witness-aware SubTaskStatus. Replaces the existing
/// `temm1e-agent/src/task_decomposition.rs::SubTaskStatus`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum SubTaskStatus {
    NotStarted,
    InProgress,
    Claimed,
    Verified,
    Failed { reason: String },
    SkipRequested { reason: String },
    SkipApproved { reason: String },
}
```

The existing enum has four variants (`Pending, Running, Completed, Failed`). Migration is additive: add the three new variants, update all `match` arms. Existing callers that produce `Completed` transition to `Claimed` (which then becomes `Verified` after Witness approval) — this is the breaking change that enforces Law 2. A feature flag `[witness] enabled = false` at startup short-circuits this and falls back to the legacy behavior for backward compatibility during P1.

---

## 6. Tier 0 Predicate Catalog (Implementation Summary)

Each primitive is a pure function with this signature:

```rust
pub trait PredicateChecker: Send + Sync {
    async fn check(
        &self,
        predicate: &Predicate,
        context: &CheckContext,
    ) -> Result<PredicateCheckResult, WitnessError>;
}

pub struct CheckContext {
    pub workspace_root: PathBuf,
    pub env: BTreeMap<String, String>,
    pub started_at: DateTime<Utc>,
}

pub struct PredicateCheckResult {
    pub outcome: VerdictOutcome,
    pub detail: String,
    pub latency_ms: u64,
}
```

### 6.1 File System Checkers (`predicates/filesystem.rs`)

- `check_file_exists(path)` → `tokio::fs::metadata(path).await.is_ok()`.
- `check_file_absent(path)` → inverse.
- `check_directory_exists(path)` → `metadata.is_dir()`.
- `check_file_contains(path, regex)` → read file, compile regex once, `regex.is_match(contents)`. Reject if file >10 MB (sanity).
- `check_file_does_not_contain(path, regex)` → inverse.
- `check_file_hash_equals(path, sha256)` → stream read, update hasher, compare hex.
- `check_file_size_in_range(path, min, max)` → `metadata.len()` in range.
- `check_file_modified_within(path, duration)` → `metadata.modified()` within now - duration.

### 6.2 Command Checkers (`predicates/command.rs`)

- `check_command_exits(cmd, args, expected, cwd, timeout)`:
  - Resolve cmd via `which::which(cmd)`. Fail Inconclusive if not found.
  - Spawn via `tokio::process::Command`, set cwd if provided.
  - Wrap in `tokio::time::timeout(Duration::from_millis(timeout), cmd.output())`.
  - On timeout → Inconclusive, detail "timed out after Nms".
  - On success → compare `status.code() == Some(expected)`.
- `check_command_output_contains(cmd, args, regex, stream, ...)`:
  - Same spawn logic.
  - Apply regex to stdout / stderr / either per `stream`.
- `check_command_output_absent(...)`: inverse.
- `check_command_duration_under(cmd, max_ms, ...)`:
  - Run, measure wall-clock, compare.

### 6.3 Process Checkers (`predicates/process.rs`)

- `check_process_alive(name_or_pid)`:
  - If numeric → check `/proc/{pid}` on Linux, `kill(pid, 0)` on Unix, `OpenProcess` on Windows via `winapi` (optional).
  - If name → run `pgrep -f name` on Unix (Inconclusive on Windows if no sysinfo crate).
- `check_port_listening(port, interface)`:
  - Attempt `TcpListener::bind` — if it fails with `AddrInUse`, port is already bound (Pass).
  - Alternatively use `/proc/net/tcp` parse on Linux, `netstat` fallback.

### 6.4 Network Checkers (`predicates/network.rs`)

- `check_http_status(url, method, expected_status)`:
  - Use `reqwest::Client`, apply default timeout 10s.
  - Compare `response.status().as_u16() == expected_status`.
- `check_http_body_contains(url, method, regex)`:
  - Fetch, apply regex to body (cap body read at 1 MB).

### 6.5 VCS Checkers (`predicates/vcs.rs`)

- `check_git_file_in_diff(path, staged, unstaged)`:
  - `git diff --name-only` (unstaged) and `git diff --cached --name-only` (staged).
  - Check path appears in the requested set.
- `check_git_diff_line_count_at_most(max, scope)`:
  - `git diff --numstat` for relevant scope, sum added + deleted.
- `check_git_new_files_match(glob)`:
  - `git status --porcelain`, filter `??` lines, match glob.
- `check_git_commit_message_matches(regex, commits_back)`:
  - `git log -n N --format=%B`, run regex.

### 6.6 Text Search Checkers (`predicates/text.rs`)

- `check_grep_present(pattern, path_glob)`:
  - Expand glob via `glob` crate, read each file, run compiled regex.
  - Cap per-file read at 10 MB, cap total files at 5000.
- `check_grep_absent(pattern, path_glob)`: inverse.
- `check_grep_count_at_least(pattern, path_glob, n)`:
  - Count total matches across all files, compare to `n`.
  - This is the **wiring check**: ensures a symbol is referenced from ≥n distinct locations.

### 6.7 Time Checker (`predicates/time.rs`)

- `check_elapsed_under(start_marker, max_secs)`:
  - Look up `start_marker` in Witness's marker store (set at subtask start).
  - Compute elapsed, compare.

### 6.8 Composite Checkers (`predicates/composite.rs`)

- `check_all_of(predicates)`: all must pass.
- `check_any_of(predicates)`: at least one must pass.
- `check_not_of(predicate)`: inverse of inner.

Composites are evaluated via the same `check()` recursive dispatch.

---

## 7. Predicate Set Configuration

### 7.1 Default `witness.toml` (ships with TEMM1E)

```toml
# Default Witness predicate sets. Users can override by creating
# .witness.toml in their repo root, or adding per-workspace sets.

[witness]
enabled = true
activate_on = "standard_and_complex"       # "simple" | "standard_and_complex" | "complex" | "all"
override_strictness = "auto"               # "auto" | "observe" | "warn" | "block"
max_overhead_pct = 15                      # Hard cap on Witness spend per task
degrade_to_tier0_on_cap = true
tier1_enabled = true
tier1_calls_per_subtask = 2
tier2_enabled = true
tier2_advisory_only = true                 # Tier 2 never overrides Tier 0
show_per_task_readout = true               # Per-task Witness line in final reply

[witness.anchor]
watchdog_socket = "/tmp/temm1e_watchdog.sock"
anchor_interval_secs = 5
anchor_timeout_ms = 2000

[witness.verifier]
# Under single-model policy, verifier uses the agent's model.
# Users who want multi-family can override here.
use_agent_model = true
model_override = ""                        # Empty means use agent's model
max_input_tokens = 2000                    # Clean-slate context cap
max_output_tokens = 200                    # Structured verdict is small
cache_system_prompt = true

[witness.set.rust]
test_passes = "CommandExits(cmd='cargo', args=['test', '${test_name}'], exit=0, timeout_ms=300000)"
lint_clean = "CommandExits(cmd='cargo', args=['clippy', '--', '-D', 'warnings'], exit=0, timeout_ms=300000)"
fmt_clean = "CommandExits(cmd='cargo', args=['fmt', '--check'], exit=0, timeout_ms=30000)"
no_stubs = "GrepAbsent(pattern='todo!\\(|unimplemented!\\(|panic!\\(\"(stub|unimplemented)', path_glob='${target_files}')"
symbol_wired = "GrepCountAtLeast(pattern='${symbol}', path_glob='${crate_dir}', n=2)"

[witness.set.python]
test_passes = "CommandExits(cmd='pytest', args=['${test_name}'], exit=0, timeout_ms=300000)"
lint_clean = "CommandExits(cmd='ruff', args=['check', '.'], exit=0, timeout_ms=60000)"
type_check = "CommandExits(cmd='mypy', args=['.'], exit=0, timeout_ms=180000)"
no_stubs = "GrepAbsent(pattern='pass\\s*#.*TODO|raise NotImplementedError|\\.\\.\\.\\s*$', path_glob='${target_files}')"
symbol_wired = "GrepCountAtLeast(pattern='${symbol}', path_glob='**/*.py', n=2)"

[witness.set.javascript]
test_passes = "CommandExits(cmd='npm', args=['test', '--', '${test_name}'], exit=0, timeout_ms=300000)"
lint_clean = "CommandExits(cmd='npm', args=['run', 'lint'], exit=0, timeout_ms=120000)"
build_clean = "CommandExits(cmd='npm', args=['run', 'build'], exit=0, timeout_ms=300000)"
no_stubs = "GrepAbsent(pattern='throw new Error\\([\"\\']unimplemented|// TODO:|// FIXME:', path_glob='${target_files}')"
symbol_wired = "GrepCountAtLeast(pattern='${symbol}', path_glob='src/**/*.{ts,tsx,js,jsx}', n=2)"

[witness.set.go]
test_passes = "CommandExits(cmd='go', args=['test', './...'], exit=0, timeout_ms=300000)"
vet_clean = "CommandExits(cmd='go', args=['vet', './...'], exit=0, timeout_ms=60000)"
build_clean = "CommandExits(cmd='go', args=['build', './...'], exit=0, timeout_ms=300000)"
no_stubs = "GrepAbsent(pattern='panic\\([\"\\'](not implemented|todo|stub)|// TODO:', path_glob='${target_files}')"
symbol_wired = "GrepCountAtLeast(pattern='${symbol}', path_glob='**/*.go', n=2)"

[witness.set.shell]
syntax_ok = "CommandExits(cmd='bash', args=['-n', '${script}'], exit=0, timeout_ms=10000)"
runs_clean = "CommandExits(cmd='${script}', args=[], exit=0, timeout_ms=60000)"
no_user_paths = "GrepAbsent(pattern='/home/[a-z]+|/Users/[a-z]+', path_glob='${script}')"

[witness.set.docs]
file_exists = "FileExists(path='${doc_path}')"
mentions_feature = "FileContains(path='${doc_path}', regex='${feature_name}')"
no_todo = "GrepAbsent(pattern='TODO|FIXME|XXX', path_glob='${doc_path}')"
links_valid = "CommandExits(cmd='markdown-link-check', args=['${doc_path}'], exit=0, timeout_ms=60000)"

[witness.set.config]
syntax_valid = "CommandExits(cmd='${validator_cmd}', args=[], exit=0, timeout_ms=30000)"
service_responds = "HttpStatus(url='${service_url}', method='GET', expected_status=200)"
has_entry = "FileContains(path='${config_path}', regex='${expected_entry}')"

[witness.set.data]
script_runs = "CommandExits(cmd='python', args=['${script}'], exit=0, timeout_ms=600000)"
output_exists = "FileExists(path='${output_path}')"
output_reasonable = "FileSizeInRange(path='${output_path}', min_bytes=1024, max_bytes=10737418240)"
report_has_metric = "FileContains(path='${report_path}', regex='${metric_name}:\\s*[0-9.]+')"
```

### 7.2 Auto-Detection Markers (`auto_detect.rs`)

```rust
pub fn detect_active_sets(workspace_root: &Path) -> Vec<String> {
    let mut sets = Vec::new();
    let check = |file: &str| workspace_root.join(file).exists();

    if check("Cargo.toml") { sets.push("rust".into()); }
    if check("package.json") {
        sets.push("javascript".into());
        if check("tsconfig.json") { sets.push("typescript".into()); }
    }
    if check("pyproject.toml") || check("setup.py") || check("requirements.txt") {
        sets.push("python".into());
    }
    if check("go.mod") { sets.push("go".into()); }
    if check("composer.json") { sets.push("php".into()); }
    if check("Gemfile") { sets.push("ruby".into()); }
    if check("pom.xml") || check("build.gradle") || check("build.gradle.kts") {
        sets.push("java".into());
    }
    // ... see full list in RESEARCH_PAPER.md §6.4

    sets.push("docs".into());       // Always active (README, etc.)
    sets.push("shell".into());      // Always active as fallback
    sets
}
```

### 7.3 Template Interpolation (`predicate_sets.rs`)

Templates use `${var}` syntax. Interpolation happens at Oath creation time:

```rust
pub fn interpolate(template: &str, vars: &BTreeMap<String, String>) -> Result<String, WitnessError> {
    let re = Regex::new(r"\$\{([a-zA-Z_][a-zA-Z0-9_]*)\}").unwrap();
    let mut result = template.to_string();
    for caps in re.captures_iter(template) {
        let var_name = &caps[1];
        let value = vars.get(var_name)
            .ok_or_else(|| WitnessError::MissingTemplateVar(var_name.to_string()))?;
        result = result.replace(&caps[0], value);
    }
    Ok(result)
}
```

A template with an unresolved variable fails Oath sealing loudly. No silent defaults.

---

## 8. Integration Hooks (Verified Line Numbers)

All line numbers verified against `main` at 2026-04-13.

### 8.1 Runtime Exit Gate — `crates/temm1e-agent/src/runtime.rs`

**Current state (lines 1804–1808):**
```rust
// ── Status: Finishing ────────────────────────────────
if let Some(ref tx) = status_tx {
    tx.send_modify(|s| {
        s.phase = AgentTaskPhase::Finishing;
    });
}
```

**Current state (lines 2159–2173):**
```rust
// ── Status: Done ─────────────────────────────────
if let Some(ref tx) = status_tx {
    tx.send_modify(|s| {
        s.phase = AgentTaskPhase::Done;
        s.tools_executed = turn_tools_used;
    });
}

return Ok((
    OutboundMessage {
        chat_id: msg.chat_id.clone(),
        text: reply_text,
        reply_to: Some(msg.id.clone()),
        parse_mode: None,
    },
```

**Witness hook — inserted between line 1808 and line 2159:**

```rust
// ── Witness verification gate ─────────────────────────
if let Some(witness) = witness_ref.as_ref() {
    match witness.verify_all(&session_id, &task_graph).await {
        Ok(final_verdict) => {
            // Rewrite reply_text to reflect verdict (Law 4 + Law 5).
            reply_text = witness.compose_final_reply(&reply_text, &final_verdict);
            // Emit VerdictRendered + TaskCompleted/TaskFailed to Ledger.
            witness.finalize_session(&session_id, &final_verdict).await?;

            // Status: VerdictRendered
            if let Some(ref tx) = status_tx {
                tx.send_modify(|s| {
                    s.phase = AgentTaskPhase::VerdictRendered {
                        outcome: final_verdict.outcome,
                        pass_count: final_verdict.pass_count,
                        fail_count: final_verdict.fail_count,
                    };
                });
            }
        }
        Err(e) => {
            tracing::warn!(error = %e, "Witness verification error; falling back to legacy path");
            // Law 5: do not block delivery on Witness error.
        }
    }
}
```

**Fallback exit gate (line 2556):** same hook, same logic.

### 8.2 AgentTaskPhase Extension — `crates/temm1e-agent/src/agent_task_status.rs`

**Current enum (lines 17–56):** Preparing, Classifying, CallingProvider, ExecutingTool, ToolCompleted, Finishing, Done, Interrupted.

**Add after line 51 (Finishing) and before line 52 (Done):**

```rust
/// An Oath has been sealed in the Ledger for this subtask.
OathSealed {
    subtask_id: String,
    postcondition_count: u32,
},
/// Witness is actively verifying a subtask's claim.
WitnessVerifying {
    subtask_id: String,
    tier: u8,
    predicate_index: u32,
    predicate_total: u32,
},
/// A verdict has been rendered for a subtask or the root goal.
VerdictRendered {
    outcome: VerdictOutcome,
    pass_count: u32,
    fail_count: u32,
},
/// A tamper alarm was raised — further verdicts halted.
TamperAlarm {
    expected_root: String,
    actual_root: String,
},
```

All existing `match` arms across the codebase must add these new variants. Use `_ => {}` as the default arm in any non-exhaustive match during migration.

### 8.3 SubTask Extension — `crates/temm1e-agent/src/task_decomposition.rs`

**Phase 1 scoping note.** The existing `SubTaskStatus` enum (`Pending/Running/Completed/Failed`) must NOT be replaced in Phase 1. Replacing it would be a compile-time breaking change that cannot be feature-flagged — every match arm across the codebase would need simultaneous updating. Phase 1 Witness is therefore **additive-only** with respect to `SubTaskStatus`.

**Phase 1 scope:** Witness in Phase 1 operates on the **Root Oath only** — no subtask-graph decomposition is integrated. This captures the most important pathologies (Fiction, Handwave, Stub-Wire Lie, Premature Closure) because the Root Oath's postconditions describe the full task. Subtask-level verification (Forgetting, Retroactive Rationalization at the subtask level) is deferred to Phase 3+ when we can consolidate status tracking.

**Phase 1 additive changes to `task_decomposition.rs`:** none. The file is unchanged.

**Phase 1 new types (in `temm1e-witness` crate, NOT in `temm1e-agent`):**

```rust
// crates/temm1e-witness/src/types.rs

/// Witness-owned subtask status, independent of temm1e-agent's SubTaskStatus.
/// Phase 1 uses this for Root Oath tracking only (root goal as a single "subtask").
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

/// A Witness-tracked work item. For Phase 1, there is one per session: the root goal.
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
```

**Phase 3+ (deferred):** the existing `SubTaskStatus` is retired and replaced with `WitnessSubTaskStatus` via a coordinated migration. Until then, the two types coexist; Witness does not touch the legacy one.

**`mark_completed()` at lines 177–185:** unchanged in Phase 1. Witness does not attempt to intercept legacy subtask completion in v1.

### 8.4 ExecutionProfile Extension — `crates/temm1e-core/src/types/optimization.rs`

**Current `VerifyMode` (lines 22–29):**
```rust
pub enum VerifyMode {
    Skip,
    RuleBased,
    LlmVerify,
}
```

**Replace with:**
```rust
pub enum VerifyMode {
    Skip,
    RuleBased,
    LlmVerify,
    Witness(WitnessStrictness),           // NEW
}

pub enum WitnessStrictness {
    Observe,                              // L1: record, do not block
    Warn,                                 // L2: warn in final reply
    Block,                                // L3: block completion on FAIL
    BlockWithRetry,                       // L5: block + retry loop (opt-in)
}
```

**Complexity → Profile mapping (lines 51–97):**

```rust
pub fn complex() -> Self {
    Self {
        prompt_tier: PromptTier::Full,
        verify_mode: VerifyMode::Witness(WitnessStrictness::Block), // P3 default
        use_learn: true,
        max_iterations: 10,
        max_tool_output_chars: 30_000,
        skip_tool_loop: false,
    }
}

pub fn standard() -> Self {
    Self {
        prompt_tier: PromptTier::Standard,
        verify_mode: VerifyMode::Witness(WitnessStrictness::Warn),  // P3 default
        // ...
    }
}

pub fn simple() -> Self {
    Self {
        prompt_tier: PromptTier::Basic,
        verify_mode: VerifyMode::Witness(WitnessStrictness::Observe), // P3 default
        // ...
    }
}
```

### 8.5 DoneCriteria Replacement — `crates/temm1e-agent/src/done_criteria.rs`

**Current state (`runtime.rs:1030`):**
```rust
let mut _done_criteria = DoneCriteria::new();
```

This is instantiated but never populated — it's a stub. Witness replaces it:

```rust
// Witness integration: seal Root Oath at task start.
let root_oath = if let Some(witness) = witness_ref.as_ref() {
    witness.seal_root_oath(&session_id, &msg.text, &classification).await.ok()
} else {
    None
};
```

The old `done_criteria` module can be deprecated or rewritten as a thin shim over Witness's Root Oath.

### 8.6 Watchdog Root Anchor — `crates/temm1e-watchdog/src/main.rs`

**Current state (lines 83–146):** single-threaded PID monitoring loop. Dependencies: only `clap`. The crate doc-comment explicitly says *"No AI, no network, no fancy features. The smaller the surface, the smaller the bug surface."* We must honor this.

**Design: file-based anchor, no new dependencies.** The watchdog does NOT read SQLite. Instead, the main process writes the latest Ledger root hash to a plain file after every append, and the watchdog periodically reads that file and seals a read-only copy.

**Anchor protocol:**

1. **Main process side** (in `temm1e-witness::ledger::append`): after every successful Ledger write, write the new `entry_hash` (hex, 64 bytes + newline) to `${TEMM1E_HOME}/witness_latest_root.hex`. This is a "live" file the main process controls.
2. **Watchdog side** (new background thread): every N seconds (default 5), read `witness_latest_root.hex`, copy the bytes to `${TEMM1E_HOME}/witness_sealed_root.hex`, and `chmod 0400` so only the watchdog's UID can rewrite it. On Windows, use `SetFileAttributes FILE_ATTRIBUTE_READONLY` via `std::fs::set_permissions` readonly flag.
3. **Verification** (in `temm1e-witness::verify_all`): before trusting any verdict, read the sealed copy. Compare to the current live latest hash. Mismatch → append `TamperAlarm` entry, halt further verdicts until manual clear.

**Watchdog extension (std-only, no new deps):**

```rust
fn main() -> std::process::ExitCode {
    let args = Args::parse();
    // ... existing setup ...

    // Spawn the Witness Root Anchor thread if --witness-root-path is provided.
    let anchor_stop = Arc::new(AtomicBool::new(false));
    if let Some(live_path) = args.witness_root_path.clone() {
        let stop_clone = anchor_stop.clone();
        let sealed_path = args.witness_sealed_path.clone()
            .unwrap_or_else(|| default_sealed_path(&live_path));
        let interval_secs = args.witness_anchor_interval_secs.unwrap_or(5);
        thread::spawn(move || {
            root_anchor_loop(live_path, sealed_path, interval_secs, stop_clone);
        });
    }

    // Existing PID monitoring loop continues unchanged.
}

/// Watch the witness_latest_root.hex file; periodically copy to sealed location.
fn root_anchor_loop(
    live_path: PathBuf,
    sealed_path: PathBuf,
    interval_secs: u64,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::SeqCst) {
        match std::fs::read(&live_path) {
            Ok(bytes) => {
                if let Err(e) = write_sealed(&sealed_path, &bytes) {
                    eprintln!("temm1e-watchdog: seal write failed: {e}");
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                // Live file not yet created by main process; wait.
            }
            Err(e) => {
                eprintln!("temm1e-watchdog: live read failed: {e}");
            }
        }
        thread::sleep(Duration::from_secs(interval_secs));
    }
}

fn write_sealed(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    // Write atomically: write to .tmp, chmod, rename.
    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, bytes)?;
    set_readonly(&tmp)?;
    std::fs::rename(&tmp, path)?;
    Ok(())
}

#[cfg(unix)]
fn set_readonly(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_mode(0o400);
    std::fs::set_permissions(path, perms)
}

#[cfg(windows)]
fn set_readonly(path: &Path) -> std::io::Result<()> {
    let mut perms = std::fs::metadata(path)?.permissions();
    perms.set_readonly(true);
    std::fs::set_permissions(path, perms)
}
```

**New CLI args (additive, all optional):**

```rust
/// Path to the Witness live root hash file (written by the main process).
#[arg(long)]
witness_root_path: Option<PathBuf>,

/// Path where the watchdog writes the sealed (read-only) copy.
#[arg(long)]
witness_sealed_path: Option<PathBuf>,

/// How often to re-seal (seconds).
#[arg(long)]
witness_anchor_interval_secs: Option<u64>,
```

**Trust model:** the main process has write access to `witness_latest_root.hex` (it must, to append to the Ledger). But it does NOT have write access to `witness_sealed_root.hex` — the watchdog chmod-400s this immediately after writing. If the main agent (or anything running in its UID) tries to modify `witness_sealed_root.hex`, it gets EACCES. On the next anchor-loop tick, the watchdog re-seals from its own view of the live file.

**Attack path consideration:** if an attacker has the main process UID, they can rewrite `witness_latest_root.hex` to match a forged Ledger state. However, the *sealed* file still reflects a prior honest hash. Mismatch is detected on the next verdict. An attacker would also need to write to `witness_sealed_root.hex` to fully cover their tracks — which requires root or the watchdog's UID. Running the watchdog as a different user (recommended) closes this. Running as the same user is still better than nothing: there is a window where mismatch is detected.

**Zero new dependencies in temm1e-watchdog Cargo.toml.** Stays `[clap]`-only. The surface stays ≤300 lines.

### 8.7 Trust Layer Wiring — `crates/temm1e-cambium/src/trust.rs`

**Current `record_success()` (lines 35–60)** and **`record_failure()` (lines 66–79)** are called from the Cambium pipeline. Add a new entry point:

```rust
impl TrustEngine {
    /// Called by Witness when a verdict is rendered.
    pub fn record_verdict(&mut self, verdict: &temm1e_witness::Verdict) {
        match verdict.outcome {
            VerdictOutcome::Pass => self.record_success(TrustLevel::AutonomousBasic),
            VerdictOutcome::Fail => self.record_failure(),
            VerdictOutcome::Inconclusive => {
                // Do not move trust either direction.
                tracing::debug!(subtask = %verdict.subtask_id, "inconclusive verdict, trust unchanged");
            }
        }
    }
}
```

Witness calls `trust_engine.record_verdict(&verdict)` on every verdict. Autonomy levels become evidence-bound.

### 8.8 Executor Output Interception — `crates/temm1e-agent/src/executor.rs`

**Current tool result flow (lines 55–171).** Witness needs to observe tool outputs as Evidence without the agent LLM seeing Witness's own calls.

**Hook at line 127** (after `execute_tool()` returns):

```rust
let output = execute_tool(&call.name, call.arguments.clone(), &tools, &session).await;

// Witness: capture tool output as Evidence if an Oath is active.
if let Some(witness) = session.witness.as_ref() {
    witness.record_tool_evidence(
        &session.session_id,
        &call.id,
        &call.name,
        &output,
    ).await.ok();
}
```

Witness's `record_tool_evidence` writes to the Ledger as `EvidenceProduced` without modifying the agent's `ToolCallResult`. The agent LLM sees the tool output as normal. Witness has its own view.

### 8.9 Classifier Output Extension — `crates/temm1e-agent/src/llm_classifier.rs`

**Current `MessageClassification` (lines 23–32):** has `category`, `chat_text`, `difficulty`, `blueprint_hint`.

**No changes needed.** Witness is driven by `difficulty` (Simple/Standard/Complex), which already exists.

Oath creation is triggered in the runtime loop immediately after classification completes (runtime.rs:~1030 area, where the unused `DoneCriteria` currently lives).

---

## 9. Oath Creation Flow

### 9.1 Planner Prompt Extension

The existing Planner prompt (used in `runtime.rs` Planning phase) is extended with an **Oath Generation Block**:

```
[Oath Generation — Required]

Before executing, produce a frozen commitment of what "done" means.

Available predicate sets for this project: ${active_sets}

Output format (JSON):
{
  "root_oath": {
    "goal": "One-paragraph description of what the user asked for.",
    "postconditions": [
      { "kind": "FileExists", "path": "..." },
      { "kind": "GrepPresent", "pattern": "...", "path_glob": "..." },
      ...
    ],
    "evidence_required": [ ... ]
  }
}

Rules:
1. At least ONE postcondition must be a deterministic Tier 0 predicate (not AspectVerifier or AdversarialJudge).
2. If the user's request involves writing code, include at least one wiring check (GrepCountAtLeast) and one stub check (GrepAbsent).
3. Do NOT use the "AspectVerifier" or "AdversarialJudge" predicate unless no Tier 0 check could answer the question.
4. Use template placeholders from the active predicate sets where applicable.
5. Keep the postcondition count between 2 and 8 for the Root Oath.
```

The JSON output is parsed by Witness. Invalid JSON → rejection, Planner retries once. Still invalid → fall back to auto-generated minimal Oath (FileExists for declared files).

### 9.2 Spec Reviewer

A deterministic schema check runs first:

```rust
pub fn review_oath_schema(oath: &Oath) -> Result<(), WitnessError> {
    if oath.postconditions.is_empty() {
        return Err(WitnessError::LenientOath("no postconditions".into()));
    }
    if !oath.postconditions.iter().any(|p| p.is_tier0()) {
        return Err(WitnessError::LenientOath("no Tier 0 predicate".into()));
    }
    // If the goal mentions code, require a wiring check + stub check.
    if mentions_code(&oath.goal) {
        let has_wiring = oath.postconditions.iter()
            .any(|p| matches!(p, Predicate::GrepCountAtLeast { n, .. } if *n >= 2));
        let has_stub_check = oath.postconditions.iter()
            .any(|p| matches!(p, Predicate::GrepAbsent { pattern, .. } if pattern.contains("todo") || pattern.contains("NotImplemented")));
        if !has_wiring { return Err(WitnessError::LenientOath("code task missing wiring check".into())); }
        if !has_stub_check { return Err(WitnessError::LenientOath("code task missing stub check".into())); }
    }
    Ok(())
}
```

If schema check fails, Planner receives the failure reason and must retry. If the optional LLM rigor check is enabled, it runs only after schema check passes and produces a "the Oath is too lenient" verdict if a stub could satisfy all listed predicates.

### 9.3 Oath Sealing

```rust
pub async fn seal_oath(
    &self,
    oath: Oath,
    session_id: &SessionId,
) -> Result<Oath, WitnessError> {
    review_oath_schema(&oath)?;
    let sealed_hash = hash_oath(&oath);
    let mut sealed = oath;
    sealed.sealed_hash = sealed_hash.clone();
    sealed.sealed_at = Utc::now();

    // Write to Ledger immediately.
    self.ledger.append(LedgerEntry {
        session_id: session_id.clone(),
        subtask_id: Some(sealed.subtask_id.clone()),
        root_goal_id: Some(sealed.root_goal_id.clone()),
        entry_type: LedgerEntryType::OathSealed,
        payload: LedgerPayload::OathSealed(sealed.clone()),
        // ... hash fields populated by ledger.append()
    }).await?;

    Ok(sealed)
}

fn hash_oath(oath: &Oath) -> String {
    let mut h = Sha256::new();
    let bytes = serde_json::to_vec(&oath).unwrap();
    h.update(&bytes);
    hex::encode(h.finalize())
}
```

---

## 10. Tier 1/2 Verifier API

### 10.1 Clean-Slate Context Rule

Tier 1/2 calls **must not** see the agent's conversation history. Enforced by building the prompt from scratch:

```rust
pub async fn call_tier1_verifier(
    &self,
    oath: &Oath,
    predicate: &Predicate,
    evidence: &[Evidence],
) -> Result<VerdictOutcome, WitnessError> {
    let system_prompt = STATIC_TIER1_SYSTEM_PROMPT; // Cached by provider
    let user_prompt = format!(
        "Oath subtask: {}\n\n\
         Predicate to verify: {}\n\n\
         Evidence:\n{}\n\n\
         Does the evidence satisfy the predicate? Reply ONLY as JSON: \
         {{\"verdict\": \"PASS\" | \"FAIL\", \"reason\": \"...\"}}",
        oath.goal,
        serde_json::to_string_pretty(predicate).unwrap(),
        format_evidence(evidence),
    );

    let response = self.provider.complete(CompletionRequest {
        system: Some(system_prompt.into()),
        messages: vec![Message::user(user_prompt)],
        max_tokens: Some(self.config.max_output_tokens),
        temperature: Some(0.0),
        // Clean-slate: NO history, NO tools, NO prior context.
        ..Default::default()
    }).await?;

    parse_structured_verdict(&response.text)
}
```

### 10.2 Static System Prompts (Cached)

**Tier 1 system prompt (static, prompt-cached):**
```
You are a predicate verifier. Your job: given a single machine-checkable predicate, a piece of evidence, and a subtask goal, decide if the evidence satisfies the predicate.

RULES:
1. Reply ONLY with JSON: {"verdict": "PASS" | "FAIL", "reason": "brief explanation"}
2. Base your verdict ONLY on the evidence shown. Do not speculate about unseen state.
3. If the evidence is insufficient to decide, reply {"verdict": "FAIL", "reason": "insufficient evidence: ..."}.
4. Do not rewrite the predicate. Do not suggest improvements. Do not argue.
5. Your verdict is binary. No "partially" or "mostly".
```

**Tier 2 adversarial auditor system prompt (static, prompt-cached):**
```
You are a skeptical auditor. Your job: find the cheapest way the claim could be false.

Given a subtask goal, a predicate, and evidence, your task is to:
1. Assume the claim is false until the evidence forces you otherwise.
2. Identify any way the evidence could have been produced without the predicate holding.
3. If you can construct any plausible scenario where the evidence exists but the predicate fails, reply FAIL.

RULES:
1. Reply ONLY with JSON: {"verdict": "PASS" | "FAIL", "reason": "the cheapest falsification scenario"}
2. Your verdict is advisory. A stronger check will override you.
3. Do not be generous. Your bias is toward finding failure.
```

Both prompts are static and are cached via provider-native prompt caching (90% discount on cached reads for Anthropic; similar for other providers).

### 10.3 Verifier Budget

```rust
pub struct TierBudget {
    pub max_calls_per_subtask: u32,
    pub max_tokens_per_call: u32,
    pub max_cost_per_task_usd: f64,
}
```

Witness tracks cumulative cost per task. If `max_cost_per_task_usd` is exceeded, remaining LLM predicates become `Inconclusive` with reason `CostSkipped`, and a `CostSkipped` Ledger entry is appended for audit. Per P5 / Law 5, this does not block delivery — the final reply reports "N predicates skipped due to budget cap."

---

## 11. Configuration Schema

### 11.1 Precedence

1. Built-in defaults (compiled into `temm1e-witness`).
2. System config: `~/.temm1e/witness.toml`.
3. Repo config: `./.witness.toml` (repo root).
4. Environment overrides: `TEMM1E_WITNESS_*` env vars.

Later sources override earlier. Repo-level `.witness.toml` is the most common user touch point.

### 11.2 Full Schema

See §7.1 for the default TOML. Additional fields for advanced users:

```toml
[witness.limits]
max_predicates_per_oath = 12
max_subtasks_per_root = 50
max_ledger_entries_per_session = 10000

[witness.ledger]
database_path = "~/.temm1e/witness.db"
retention_days = 365               # Entries older than this are archived, not deleted
archive_path = "~/.temm1e/witness_archive/"

[witness.rollout]
current_phase = "P1"               # "P1" | "P2" | "P3" | "P4" | "P5"
disagreement_threshold_p2 = 0.05
disagreement_threshold_p3 = 0.02

[witness.observability]
emit_otel_spans = true
log_level = "info"                 # "trace" | "debug" | "info" | "warn" | "error"
```

---

## 12. Property Tests for the Five Laws

Located in `crates/temm1e-witness/tests/laws.rs`.

### 12.1 Law 1 — Pre-Commitment

```rust
#[tokio::test]
async fn law1_no_subtask_executes_without_sealed_oath() {
    let witness = setup_test_witness().await;
    let subtask_id = "st-001".to_string();

    // Attempt to submit a claim without sealing an Oath first.
    let result = witness.submit_claim(Claim {
        subtask_id: subtask_id.clone(),
        claim_text: "I did the thing".into(),
        evidence_refs: vec![],
        claimed_at: Utc::now(),
        agent_step_id: 0,
    }).await;

    assert!(matches!(result, Err(WitnessError::NoSealedOath(_))));
}

proptest! {
    #[test]
    fn law1_every_claim_has_prior_oath_sealed_entry(claim in arb_claim()) {
        // For any generated Claim, the Ledger must contain an OathSealed
        // entry for that subtask_id BEFORE the ClaimSubmitted entry.
        // ...
    }
}
```

### 12.2 Law 2 — Independent Verdict

```rust
#[test]
fn law2_mark_completed_is_private_to_witness_compile_check() {
    // This test exists as a compile-time assertion: attempting to call
    // `SubTaskStatus::transition_to(Verified)` from outside `temm1e-witness`
    // must fail to compile. Enforced via `pub(crate)` visibility on the
    // transition function. Verified manually by the crate structure.
}

#[tokio::test]
async fn law2_only_witness_produces_verified_status() {
    let witness = setup_test_witness().await;
    let oath = create_oath_with_passing_predicates();
    witness.seal_oath(oath.clone(), &"sess".into()).await.unwrap();

    // Witness renders a PASS verdict.
    let verdict = witness.verify_subtask(&oath.subtask_id).await.unwrap();
    assert_eq!(verdict.outcome, VerdictOutcome::Pass);

    // Status should now be Verified.
    let status = witness.get_subtask_status(&oath.subtask_id).await.unwrap();
    assert!(matches!(status, SubTaskStatus::Verified));
}
```

### 12.3 Law 3 — Immutable History

```rust
#[tokio::test]
async fn law3_hash_chain_detects_tampering() {
    let witness = setup_test_witness().await;
    for i in 0..100 {
        witness.ledger.append(test_entry(i)).await.unwrap();
    }

    // Recompute chain from scratch.
    let all_entries = witness.ledger.read_all().await.unwrap();
    let recomputed = recompute_chain(&all_entries);
    let stored_root = all_entries.last().unwrap().entry_hash.clone();
    assert_eq!(recomputed, stored_root);

    // Tamper with row 50 via direct SQL (bypassing append-only trigger for test).
    tamper_row_directly(50, b"evil payload").await;

    // Recompute should no longer match.
    let all_entries = witness.ledger.read_all().await.unwrap();
    let recomputed = recompute_chain(&all_entries);
    assert_ne!(recomputed, stored_root);
}

#[tokio::test]
async fn law3_update_and_delete_raise_sql_errors() {
    let witness = setup_test_witness().await;
    witness.ledger.append(test_entry(0)).await.unwrap();

    let update_result = raw_sql(&witness, "UPDATE witness_ledger SET payload_json = 'x' WHERE entry_id = 1").await;
    assert!(update_result.is_err());

    let delete_result = raw_sql(&witness, "DELETE FROM witness_ledger WHERE entry_id = 1").await;
    assert!(delete_result.is_err());
}
```

### 12.4 Law 4 — Loud Failure

```rust
#[tokio::test]
async fn law4_failed_verdict_produces_honest_final_reply() {
    let witness = setup_test_witness().await;
    let oath = create_oath_with_one_failing_predicate();
    witness.seal_oath(oath, &"sess".into()).await.unwrap();

    let agent_proposed_reply = "Done!";
    let verdict = witness.verify_all(&"sess".into(), &test_task_graph()).await.unwrap();
    let final_reply = witness.compose_final_reply(agent_proposed_reply, &verdict);

    assert!(final_reply.contains("Partial completion"));
    assert!(final_reply.contains("Could not verify"));
    assert!(!final_reply.eq("Done!"));
}
```

### 12.5 Law 5 — Narrative-Only FAIL

```rust
#[test]
fn law5_witness_crate_has_no_destructive_apis() {
    // Static assertion: grep the temm1e-witness crate source for forbidden APIs.
    // This test uses a build script or manual check; we verify at CI time.
    let source = read_all_source_files("crates/temm1e-witness/src");
    assert!(!source.contains("std::fs::remove_file"));
    assert!(!source.contains("std::fs::remove_dir"));
    assert!(!source.contains("Command::new(\"git\").arg(\"reset\")"));
    assert!(!source.contains("Command::new(\"kill\")"));
}

#[tokio::test]
async fn law5_failed_verdict_preserves_files() {
    let tmp = tempfile::tempdir().unwrap();
    let test_file = tmp.path().join("work.txt");
    std::fs::write(&test_file, "important work").unwrap();

    let witness = setup_test_witness_in(tmp.path()).await;
    let oath = create_oath_with_failing_predicate_about(&test_file);
    witness.seal_oath(oath, &"sess".into()).await.unwrap();

    let _verdict = witness.verify_all(&"sess".into(), &test_task_graph()).await.unwrap();

    // File is still there.
    assert!(test_file.exists());
    assert_eq!(std::fs::read_to_string(&test_file).unwrap(), "important work");
}

#[tokio::test]
async fn law5_failed_verdict_preserves_git_state() {
    let tmp = setup_git_repo_with_dirty_working_tree().await;
    let witness = setup_test_witness_in(tmp.path()).await;
    let oath = create_oath_with_failing_predicate();
    let _ = witness.verify_all(&"sess".into(), &test_task_graph()).await;

    let post_status = run_git_status(tmp.path());
    assert_eq!(post_status, pre_status_captured_before_witness_run);
}
```

---

## 13. Red-Team Oaths

Located in `crates/temm1e-witness/tests/redteam.rs`.

```rust
#[tokio::test]
async fn redteam_fake_completion_is_caught() {
    // Agent writes nothing; Oath requires FileExists.
    let tmp = tempfile::tempdir().unwrap();
    let witness = setup_test_witness_in(tmp.path()).await;
    let oath = oath_requiring_file(tmp.path().join("nonexistent.rs"));
    witness.seal_oath(oath.clone(), &"sess".into()).await.unwrap();

    let verdict = witness.verify_subtask(&oath.subtask_id).await.unwrap();
    assert_eq!(verdict.outcome, VerdictOutcome::Fail);
    assert!(verdict.reason.contains("FileExists"));
}

#[tokio::test]
async fn redteam_stub_wire_is_caught() {
    // Agent writes a module with only `todo!()` body.
    let tmp = write_test_rust_file_with_todo(&tmp);
    let oath = oath_for_rust_module_with_stub_check();
    // ...
    let verdict = witness.verify_subtask(&oath.subtask_id).await.unwrap();
    assert_eq!(verdict.outcome, VerdictOutcome::Fail);
}

#[tokio::test]
async fn redteam_unwired_symbol_is_caught() {
    // Agent writes function but never calls it.
    let tmp = write_test_rust_file_with_unwired_fn(&tmp);
    let oath = oath_with_wiring_check("fn my_new_function", tmp.path(), 2);
    // ...
    let verdict = witness.verify_subtask(&oath.subtask_id).await.unwrap();
    assert_eq!(verdict.outcome, VerdictOutcome::Fail);
    assert!(verdict.reason.contains("GrepCountAtLeast"));
}

#[tokio::test]
async fn redteam_trivial_test_is_caught_when_specific_assertion_required() {
    // Agent writes `fn test_foo() { assert!(true) }`.
    // If the Oath requires FileContains(test_file, regex = "assert.*expected"),
    // the trivial test should fail the more specific assertion predicate.
    // ...
}

#[tokio::test]
async fn redteam_retroactive_oath_weakening_is_rejected() {
    let witness = setup_test_witness().await;
    let oath = create_oath();
    let sealed = witness.seal_oath(oath.clone(), &"sess".into()).await.unwrap();

    // Attempt to amend the sealed Oath.
    let mut weakened = sealed.clone();
    weakened.postconditions.pop();
    let result = witness.seal_oath(weakened, &"sess".into()).await;
    // Should either reject as already sealed, or seal as a separate Oath
    // (but Witness verifies against ALL sealed Oaths for the subtask, so the
    // original still blocks).
    assert!(result.is_err() || witness_still_checks_original_oath());
}
```

---

## 14. Metrics and Observability

### 14.1 New OTEL Metrics

Added to `crates/temm1e-observable/src/lib.rs`:

```rust
pub const METRIC_OATHS_SEALED: &str = "temm1e.witness.oaths_sealed";
pub const METRIC_VERDICTS_PASS: &str = "temm1e.witness.verdicts.pass";
pub const METRIC_VERDICTS_FAIL: &str = "temm1e.witness.verdicts.fail";
pub const METRIC_VERDICTS_INCONCLUSIVE: &str = "temm1e.witness.verdicts.inconclusive";
pub const METRIC_LEDGER_TAMPER_ALARMS: &str = "temm1e.witness.tamper_alarms";
pub const METRIC_WITNESS_COST_USD: &str = "temm1e.witness.cost_usd";
pub const METRIC_WITNESS_LATENCY_MS: &str = "temm1e.witness.latency_ms";
pub const METRIC_TIER0_PREDICATES_CHECKED: &str = "temm1e.witness.tier0.predicates_checked";
pub const METRIC_TIER1_CALLS: &str = "temm1e.witness.tier1.calls";
pub const METRIC_TIER2_CALLS: &str = "temm1e.witness.tier2.calls";
pub const METRIC_COST_SKIPPED_PREDICATES: &str = "temm1e.witness.cost_skipped_predicates";
```

### 14.2 Per-Task Readout Format

Appended to the final reply for Complex tasks when `[witness] show_per_task_readout = true`:

```
─────────────────────────────────────────
Witness: 4/5 PASS, 1 FAIL
Cost: $0.18 (7.2% of task)
Latency: +2.3s
Tier usage: 4 × Tier 0, 1 × Tier 1, 0 × Tier 2

Failed predicates:
  • GrepCountAtLeast("fts5_search", "src/", n=2): found only 1 match
    (in src/fts5.rs:12). Likely missing call site in factory.rs.

Run /witness last for full details.
─────────────────────────────────────────
```

### 14.3 Session Rollup Command

`/witness report` produces:

```
Session: sess-2026-04-13-001
Tasks: 12 (8 Simple, 3 Standard, 1 Complex)
Predicates checked: 47 (44 PASS, 3 FAIL, 0 Inconclusive)
Cost: $1.87 total, 6.1% of session LLM spend
Latency: +18.4s total
Cost-skipped: 0 predicates

FAIL breakdown:
  • GrepCountAtLeast: 2 (wiring checks)
  • CommandExits: 1 (cargo clippy warning)
```

### 14.4 Ledger Query Tool

`/witness history [--session X] [--since Y]` produces a paginated view of Ledger entries. Useful for debugging disagreements.

---

## 15. Rollout Checklist

### Phase 1 Scope Note (important)

Phase 1 intentionally narrows the design in two ways to preserve 100/0 and zero-risk:

1. **Root Oath only.** Witness in Phase 1 verifies the top-level Root Oath for a session. No subtask-graph decomposition is integrated yet. The Root Oath is sufficient to capture Fiction, Handwave, Stub-Wire Lie, and Premature Closure because its postconditions describe the full task. Forgetting and Retroactive Rationalization at the subtask level are deferred to Phase 3+.
2. **SubTaskStatus left alone.** The existing `task_decomposition.rs::SubTaskStatus` enum is NOT touched in Phase 1 — a breaking change there cannot be feature-flagged. Witness owns its own `WitnessSubTaskStatus` in the `temm1e-witness` crate. Consolidation is a Phase 3+ task.
3. **File-based watchdog anchor.** The watchdog keeps its "only clap as dependency" discipline. Root anchoring is a plain-file protocol (see §8.6), not SQLite access from the watchdog.

### Phase 0 — Design Review (current)

- [x] Research paper written and reviewed
- [x] Implementation details document written and reviewed
- [ ] User approval on both documents
- [ ] Five Laws translated into property test outlines
- [ ] No code written

### Phase 1 — Infrastructure Only (feature flag off)

- [ ] Create `crates/temm1e-witness` with crate skeleton
- [ ] Add to workspace `Cargo.toml` members
- [ ] Implement `types.rs` (all core types)
- [ ] Implement `ledger.rs` (SQLite append-only with hash chain)
- [ ] Implement `anchor.rs` (Unix domain socket client — stub if watchdog not extended yet)
- [ ] Implement all Tier 0 predicates in `predicates/*.rs`
- [ ] Implement `predicate_sets.rs` (TOML loader, template interpolation)
- [ ] Implement `auto_detect.rs`
- [ ] Implement `oath.rs` (sealing + Spec Reviewer schema check)
- [ ] Implement `witness.rs` (Tier 0 dispatch only, Tier 1/2 stubbed)
- [ ] Add `witness_ledger` table to `temm1e-memory/src/sqlite.rs` schema init
- [ ] Extend `temm1e-watchdog` with Root Anchor thread
- [ ] Extend `AgentTaskPhase` with new variants
- [ ] Extend `VerifyMode` with `Witness(strictness)` variant
- [ ] Extend `SubTask` with `oath`, `evidence`, `verdicts` fields
- [ ] Extend `SubTaskStatus` with new variants
- [ ] Write all property tests for Five Laws (§12)
- [ ] Write all red-team Oath tests (§13)
- [ ] Write all Tier 0 predicate unit tests
- [ ] Add new OTEL metrics
- [ ] **Feature flag:** `[witness] enabled = false` by default
- [ ] Run full TEMM1E test suite; confirm no regressions
- [ ] CI gate: `cargo check`, `cargo clippy -- -D warnings`, `cargo fmt --check`, `cargo test --workspace`

**Exit criterion:** all tests green, no user-visible behavior change, Witness is dormant.

### Phase 2 — PassiveLedger Activation (L1 on all tasks)

- [ ] Flip `[witness] enabled = true`, `override_strictness = "observe"`
- [ ] Hook Root Oath sealing at `runtime.rs:~1030` (replaces unused `DoneCriteria`)
- [ ] Hook tool evidence recording in `executor.rs:127`
- [ ] Hook verify_all at `runtime.rs:1808` → do not modify `reply_text`, only record verdicts
- [ ] Hook `TrustEngine::record_verdict` after each verdict
- [ ] Deploy to self-testing environment (the user's local TEMM1E instance)
- [ ] Run the 10-turn CLI self-test from MEMORY.md workflow preferences
- [ ] Measure: ledger write success rate, Oath sealing success rate, verdict outcomes, cost/task, latency/task
- [ ] Measure: verdict-disagreement rate on real tasks
- [ ] Run 1–2 weeks of real use

**Exit criterion:** verdict-disagreement rate <5% on Complex tasks; no regressions in task completion.

### Phase 3 — Complex-Block (L3 on Complex, L2 on Standard)

- [ ] Flip default strictness for Complex from `observe` to `block`, for Standard to `warn`
- [ ] Activate `compose_final_reply()` to rewrite replies on FAIL
- [ ] Run CLI self-test
- [ ] Deploy
- [ ] Measure: user override rate (`/force-complete` usage), false-positive rate, user-visible rejection rate

**Exit criterion:** verdict-disagreement rate <2% on Complex; `/force-complete` usage <10% of failed tasks.

### Phase 4 — Standard-Block (opt-in)

- [ ] Document config for users: `[witness] override_strictness = "block"` to get L3 on Standard

### Phase 5 — Retry Loop (opt-in)

- [ ] Document L5 configuration: `[witness.rollout] current_phase = "P5"` enables auto-retry on FAIL

---

## 16. Known Limitations and Trade-offs

1. **Single-model bias in Tier 1/2.** By design per the user's policy. Mitigated by clean-slate context, structured output, and Tier 2 advisory-only. Documented per predicate in the Oath.
2. **Subjective predicate handling.** Witness cannot check "is this prose good?" beyond basic containment. Future work: aspect-verifier fine-tuning.
3. **Predicate set quality depends on maintainers.** If the default `witness.set.python` is lenient, Python tasks will pass weakly. Users can override per-repo; defaults are public and reviewable.
4. **Oath generation is an LLM call, so Oath quality depends on model.** A model that writes lenient Oaths defeats Witness. Mitigations: Spec Reviewer enforces minimums, user sees Oath before execution on Complex.
5. **Tier 0 predicates can be slow.** `cargo test` takes minutes. Witness does not make this faster — it parallelizes where possible and enforces per-Oath wall-clock budgets.
6. **Witness cannot prove absence of bugs.** "Test passes" does not mean "code is correct" — this is documented in every relevant Evaluation section. Witness proves *the claim matches the pre-registered contract*, not correctness.
7. **Cross-platform gotchas.** Some predicates (`ProcessAlive`, `PortListening`) have platform-specific implementations. Windows support may be degraded to `Inconclusive` for a subset of predicates in v1.
8. **Ledger size growth.** At ~10 entries per Complex task, heavy users accumulate ~10k entries/month. Retention policy (`retention_days = 365`) archives but does not delete. Requires monitoring.
9. **Watchdog extension touches a second crate.** The watchdog must be updated in lock-step with the main binary for new Ledger formats. Schema versioning is our mitigation.

---

## 17. Bring-Up Plan Summary

1. **Review this document and the research paper.** User approval on both.
2. **Phase 1:** implement `temm1e-witness` crate with all primitives, tests, and dormant integration. No user impact. Zero-risk.
3. **Phase 2:** flip `enabled = true` in observe mode. Measure real-world data. Still zero user impact.
4. **Phase 3:** advance to Complex-Block based on data. User-visible change: final-reply narrative becomes honest on Complex failures.
5. **Phase 4–5:** opt-in expansion.

Every phase is reversible via one config flip. Every phase has a measured advancement criterion. Nothing irreversible happens at any point.

---

*End of implementation details. Awaiting review.*
