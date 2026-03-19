# λ-Memory — Implementation Guide

> The bone. Every file, every function, every SQL statement. Reference this before writing code.

**Status:** Implemented
**Branch:** `gradient_memory`
**Author:** TEMM1E's Lab
**Date:** 2026-03-15
**Prerequisites:** [Design Doc](LAMBDA_MEMORY.md) | [Research](LAMBDA_MEMORY_RESEARCH.md)

---

## Table of Contents

1. [Scope of Changes](#1-scope-of-changes)
2. [Phase 1: Data Layer (temm1e-memory)](#2-phase-1-data-layer)
3. [Phase 2: Core Types (temm1e-core)](#3-phase-2-core-types)
4. [Phase 3: Decay Engine (temm1e-agent)](#4-phase-3-decay-engine)
5. [Phase 4: Context Integration (temm1e-agent/context.rs)](#5-phase-4-context-integration)
6. [Phase 5: Memory Extraction (temm1e-agent/runtime.rs)](#6-phase-5-memory-extraction)
7. [Phase 6: Recall Tool (temm1e-tools)](#7-phase-6-recall-tool)
8. [Phase 7: Configuration](#8-phase-7-configuration)
9. [Migration Strategy](#9-migration-strategy)
10. [Test Plan](#10-test-plan)

---

## 1. Scope of Changes

### Files to Create
| File | Crate | Purpose |
|------|-------|---------|
| `crates/temm1e-agent/src/lambda_memory.rs` | temm1e-agent | Decay engine, scoring, context assembly, packing |
| `crates/temm1e-tools/src/lambda_recall.rs` | temm1e-tools | Hash-based recall tool |

### Files to Modify
| File | Crate | Change |
|------|-------|--------|
| `crates/temm1e-memory/src/sqlite.rs` | temm1e-memory | Add `lambda_memories` table + FTS5, new methods |
| `crates/temm1e-memory/src/lib.rs` | temm1e-memory | Re-export new types |
| `crates/temm1e-core/src/traits/memory.rs` | temm1e-core | Add λ-Memory methods to Memory trait (with default impls) |
| `crates/temm1e-core/src/types/config.rs` | temm1e-core | Add `LambdaMemoryConfig` |
| `crates/temm1e-agent/src/context.rs` | temm1e-agent | Replace Categories 5/5b/6 with λ-Memory assembly |
| `crates/temm1e-agent/src/runtime.rs` | temm1e-agent | Parse `<memory>` blocks from LLM responses |
| `crates/temm1e-agent/src/mod.rs` or `lib.rs` | temm1e-agent | Add `pub mod lambda_memory;` |
| `crates/temm1e-tools/src/lib.rs` | temm1e-tools | Register `LambdaRecallTool` in `create_tools()` |

### Files NOT Modified
- `runtime.rs` main loop structure — untouched, we only add parsing after line ~941
- `learning.rs` — unchanged, learnings still extracted the same way but stored as λ-memories
- `history_pruning.rs` — unchanged
- `prompt_optimizer.rs` — unchanged
- Markdown backend — λ-Memory is SQLite-only (markdown can be added later)

---

## 2. Phase 1: Data Layer (temm1e-memory)

### 2.1 New Table: `lambda_memories`

Add to `SqliteMemory::init_tables()` in `crates/temm1e-memory/src/sqlite.rs`:

```sql
CREATE TABLE IF NOT EXISTS lambda_memories (
    hash            TEXT PRIMARY KEY,
    created_at      INTEGER NOT NULL,
    last_accessed   INTEGER NOT NULL,
    access_count    INTEGER NOT NULL DEFAULT 0,
    importance      REAL NOT NULL DEFAULT 1.0,
    explicit_save   INTEGER NOT NULL DEFAULT 0,
    full_text       TEXT NOT NULL,
    summary_text    TEXT NOT NULL,
    essence_text    TEXT NOT NULL,
    tags            TEXT NOT NULL DEFAULT '[]',
    memory_type     TEXT NOT NULL DEFAULT 'conversation',
    session_id      TEXT NOT NULL
);

CREATE INDEX IF NOT EXISTS idx_lm_importance ON lambda_memories(importance);
CREATE INDEX IF NOT EXISTS idx_lm_last_accessed ON lambda_memories(last_accessed);
CREATE INDEX IF NOT EXISTS idx_lm_session ON lambda_memories(session_id);
CREATE INDEX IF NOT EXISTS idx_lm_type ON lambda_memories(memory_type);
CREATE INDEX IF NOT EXISTS idx_lm_explicit ON lambda_memories(explicit_save);
```

### 2.2 FTS5 Virtual Table

```sql
CREATE VIRTUAL TABLE IF NOT EXISTS lambda_memories_fts USING fts5(
    summary_text,
    essence_text,
    tags,
    content='lambda_memories',
    content_rowid='rowid'
);
```

FTS5 triggers to keep in sync:

```sql
CREATE TRIGGER IF NOT EXISTS lm_fts_insert AFTER INSERT ON lambda_memories BEGIN
    INSERT INTO lambda_memories_fts(rowid, summary_text, essence_text, tags)
    VALUES (new.rowid, new.summary_text, new.essence_text, new.tags);
END;

CREATE TRIGGER IF NOT EXISTS lm_fts_delete BEFORE DELETE ON lambda_memories BEGIN
    INSERT INTO lambda_memories_fts(lambda_memories_fts, rowid, summary_text, essence_text, tags)
    VALUES ('delete', old.rowid, old.summary_text, old.essence_text, old.tags);
END;

CREATE TRIGGER IF NOT EXISTS lm_fts_update AFTER UPDATE ON lambda_memories BEGIN
    INSERT INTO lambda_memories_fts(lambda_memories_fts, rowid, summary_text, essence_text, tags)
    VALUES ('delete', old.rowid, old.summary_text, old.essence_text, old.tags);
    INSERT INTO lambda_memories_fts(rowid, summary_text, essence_text, tags)
    VALUES (new.rowid, new.summary_text, new.essence_text, new.tags);
END;
```

### 2.3 New Methods on Memory Trait

Add to `crates/temm1e-core/src/traits/memory.rs` with **default implementations** so existing backends don't break:

```rust
/// Store a λ-memory entry.
async fn lambda_store(&self, entry: LambdaMemoryEntry) -> Result<(), Temm1eError> {
    let _ = entry;
    Ok(()) // No-op default
}

/// Query λ-memories ordered by importance DESC, limited to `limit`.
async fn lambda_query_candidates(&self, limit: usize) -> Result<Vec<LambdaMemoryEntry>, Temm1eError> {
    let _ = limit;
    Ok(Vec::new()) // Empty default
}

/// Look up a λ-memory by hash prefix.
async fn lambda_recall(&self, hash_prefix: &str) -> Result<Option<LambdaMemoryEntry>, Temm1eError> {
    let _ = hash_prefix;
    Ok(None) // Not found default
}

/// Update last_accessed and access_count for a recalled memory.
async fn lambda_touch(&self, hash: &str) -> Result<(), Temm1eError> {
    let _ = hash;
    Ok(()) // No-op default
}

/// FTS5 search returning (hash, bm25_rank) pairs.
async fn lambda_fts_search(&self, query: &str, limit: usize) -> Result<Vec<(String, f64)>, Temm1eError> {
    let _ = (query, limit);
    Ok(Vec::new()) // Empty default
}

/// Garbage collect expired λ-memories.
async fn lambda_gc(&self, now_epoch: u64, max_age_secs: u64) -> Result<usize, Temm1eError> {
    let _ = (now_epoch, max_age_secs);
    Ok(0) // No-op default
}
```

### 2.4 LambdaMemoryEntry Struct

Add to `crates/temm1e-core/src/traits/memory.rs` (or a new file `lambda.rs` in types):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LambdaMemoryEntry {
    pub hash: String,
    pub created_at: u64,        // unix epoch seconds
    pub last_accessed: u64,     // unix epoch seconds
    pub access_count: u32,
    pub importance: f32,        // 1.0 - 5.0
    pub explicit_save: bool,
    pub full_text: String,
    pub summary_text: String,
    pub essence_text: String,
    pub tags: Vec<String>,
    pub memory_type: LambdaMemoryType,
    pub session_id: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub enum LambdaMemoryType {
    Conversation,
    Knowledge,
    Learning,
}
```

### 2.5 SQLite Implementation

In `crates/temm1e-memory/src/sqlite.rs`, implement the 6 new trait methods:

**lambda_store():**
```rust
async fn lambda_store(&self, entry: LambdaMemoryEntry) -> Result<(), Temm1eError> {
    let tags_json = serde_json::to_string(&entry.tags).unwrap_or_default();
    let memory_type = match entry.memory_type {
        LambdaMemoryType::Conversation => "conversation",
        LambdaMemoryType::Knowledge => "knowledge",
        LambdaMemoryType::Learning => "learning",
    };
    sqlx::query(
        "INSERT OR REPLACE INTO lambda_memories
         (hash, created_at, last_accessed, access_count, importance, explicit_save,
          full_text, summary_text, essence_text, tags, memory_type, session_id)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)"
    )
    .bind(&entry.hash)
    .bind(entry.created_at as i64)
    .bind(entry.last_accessed as i64)
    .bind(entry.access_count as i32)
    .bind(entry.importance)
    .bind(entry.explicit_save as i32)
    .bind(&entry.full_text)
    .bind(&entry.summary_text)
    .bind(&entry.essence_text)
    .bind(&tags_json)
    .bind(memory_type)
    .bind(&entry.session_id)
    .execute(&self.pool)
    .await
    .map_err(|e| Temm1eError::Memory(format!("lambda_store failed: {e}")))?;
    Ok(())
}
```

**lambda_query_candidates():**
```rust
async fn lambda_query_candidates(&self, limit: usize) -> Result<Vec<LambdaMemoryEntry>, Temm1eError> {
    let rows = sqlx::query_as::<_, LambdaMemoryRow>(
        "SELECT * FROM lambda_memories ORDER BY importance DESC LIMIT ?"
    )
    .bind(limit as i64)
    .fetch_all(&self.pool)
    .await
    .map_err(|e| Temm1eError::Memory(format!("lambda_query_candidates failed: {e}")))?;
    Ok(rows.into_iter().map(|r| r.into()).collect())
}
```

**lambda_recall():**
```rust
async fn lambda_recall(&self, hash_prefix: &str) -> Result<Option<LambdaMemoryEntry>, Temm1eError> {
    let pattern = format!("{}%", hash_prefix);
    let row = sqlx::query_as::<_, LambdaMemoryRow>(
        "SELECT * FROM lambda_memories WHERE hash LIKE ? LIMIT 1"
    )
    .bind(&pattern)
    .fetch_optional(&self.pool)
    .await
    .map_err(|e| Temm1eError::Memory(format!("lambda_recall failed: {e}")))?;
    Ok(row.map(|r| r.into()))
}
```

**lambda_touch():**
```rust
async fn lambda_touch(&self, hash: &str) -> Result<(), Temm1eError> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64;
    sqlx::query(
        "UPDATE lambda_memories SET last_accessed = ?, access_count = access_count + 1 WHERE hash = ?"
    )
    .bind(now)
    .bind(hash)
    .execute(&self.pool)
    .await
    .map_err(|e| Temm1eError::Memory(format!("lambda_touch failed: {e}")))?;
    Ok(())
}
```

**lambda_fts_search():**
```rust
async fn lambda_fts_search(&self, query: &str, limit: usize) -> Result<Vec<(String, f64)>, Temm1eError> {
    if query.trim().is_empty() {
        return Ok(Vec::new());
    }
    // Sanitize query for FTS5 — escape special characters
    let sanitized = query.replace('"', "\"\"");
    let rows = sqlx::query_as::<_, (String, f64)>(
        "SELECT lm.hash, rank
         FROM lambda_memories_fts fts
         JOIN lambda_memories lm ON lm.rowid = fts.rowid
         WHERE lambda_memories_fts MATCH ?
         ORDER BY rank
         LIMIT ?"
    )
    .bind(&format!("\"{}\"", sanitized))
    .bind(limit as i64)
    .fetch_all(&self.pool)
    .await
    .map_err(|e| Temm1eError::Memory(format!("lambda_fts_search failed: {e}")))?;
    Ok(rows)
}
```

**lambda_gc():**
```rust
async fn lambda_gc(&self, now_epoch: u64, max_age_secs: u64) -> Result<usize, Temm1eError> {
    let cutoff = (now_epoch - max_age_secs) as i64;
    let result = sqlx::query(
        "DELETE FROM lambda_memories
         WHERE explicit_save = 0
         AND last_accessed < ?
         AND importance < 3.0"
    )
    .bind(cutoff)
    .execute(&self.pool)
    .await
    .map_err(|e| Temm1eError::Memory(format!("lambda_gc failed: {e}")))?;
    Ok(result.rows_affected() as usize)
}
```

---

## 3. Phase 2: Core Types (temm1e-core)

### 3.1 Config: `LambdaMemoryConfig`

Add to `crates/temm1e-core/src/types/config.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LambdaMemoryConfig {
    /// Whether λ-Memory is enabled.
    #[serde(default = "default_true")]
    pub enabled: bool,
    /// Decay rate constant (λ). Higher = faster decay.
    #[serde(default = "default_decay_lambda")]
    pub decay_lambda: f32,
    /// Threshold for full text display.
    #[serde(default = "default_hot")]
    pub hot_threshold: f32,
    /// Threshold for summary display.
    #[serde(default = "default_warm")]
    pub warm_threshold: f32,
    /// Threshold for essence display.
    #[serde(default = "default_cool")]
    pub cool_threshold: f32,
    /// Max memories to score per turn.
    #[serde(default = "default_candidate_limit")]
    pub candidate_limit: usize,
}

fn default_true() -> bool { true }
fn default_decay_lambda() -> f32 { 0.01 }
fn default_hot() -> f32 { 2.0 }
fn default_warm() -> f32 { 1.0 }
fn default_cool() -> f32 { 0.3 }
fn default_candidate_limit() -> usize { 500 }

impl Default for LambdaMemoryConfig {
    fn default() -> Self {
        Self {
            enabled: true,
            decay_lambda: 0.01,
            hot_threshold: 2.0,
            warm_threshold: 1.0,
            cool_threshold: 0.3,
            candidate_limit: 500,
        }
    }
}
```

Add to the main config struct (likely `Temm1eConfig` or `AgentConfig`):
```rust
#[serde(default)]
pub lambda_memory: LambdaMemoryConfig,
```

### 3.2 Re-exports

Ensure `LambdaMemoryEntry`, `LambdaMemoryType`, and `LambdaMemoryConfig` are re-exported from `temm1e-core` so other crates can use them.

---

## 4. Phase 3: Decay Engine (temm1e-agent)

### 4.1 New file: `crates/temm1e-agent/src/lambda_memory.rs`

This is the core module. ~300 lines.

```rust
//! λ-Memory — continuous decay with hash-based recall.
//!
//! Memories fade over time through exponential decay but never disappear.
//! Tem sees faded memories as hashes and can recall them on demand.

use temm1e_core::types::config::LambdaMemoryConfig;
use temm1e_core::{LambdaMemoryEntry, LambdaMemoryType, Memory};
use crate::context::estimate_tokens;

/// Minimum tokens to fit a faded entry (hash + timestamp + essence).
const MIN_ENTRY_TOKENS: usize = 15;

/// Score below which a memory is invisible.
const GONE_THRESHOLD: f32 = 0.01;

// ── Decay Function ─────────────────────────────────────────────

/// Compute the decay score for a memory at time `now`.
///
/// score = importance × exp(−λ × hours_since_last_access)
///
/// This is NEVER stored. It's computed at read time from immutable fields.
pub fn decay_score(entry: &LambdaMemoryEntry, now: u64, lambda: f32) -> f32 {
    let age_hours = (now.saturating_sub(entry.last_accessed)) as f32 / 3600.0;
    entry.importance * (-age_hours * lambda).exp()
}

// ── Adaptive Thresholds ────────────────────────────────────────

pub struct Thresholds {
    pub hot: f32,
    pub warm: f32,
    pub cool: f32,
}

/// Compute effective thresholds based on memory pressure.
///
/// As `budget` shrinks relative to `max_budget`, thresholds rise,
/// causing more memories to display at lower fidelity tiers.
pub fn effective_thresholds(
    budget: usize,
    max_budget: usize,
    config: &LambdaMemoryConfig,
) -> Thresholds {
    let pressure = 1.0 - (budget as f32 / max_budget.max(1) as f32).min(1.0);
    Thresholds {
        hot: config.hot_threshold + (pressure * 2.0),
        warm: config.warm_threshold + (pressure * 1.0),
        cool: config.cool_threshold + (pressure * 0.5),
    }
}

// ── Skull Budget ───────────────────────────────────────────────

/// Calculate the token budget available for λ-Memory.
///
/// Memory is elastic — it gets what's left after everything with higher
/// priority (bone, active conversation, output reserve, guard, older history).
pub fn lambda_budget(
    skull: usize,           // model.context_window
    max_output: usize,      // model.max_output_tokens
    bone_tokens: usize,     // system prompt + tools + DONE + blueprints
    active_tokens: usize,   // recent messages (category 4)
) -> usize {
    let output_reserve = max_output.min(skull / 10);
    let guard = skull / 50; // 2% safety margin
    let occupied = bone_tokens + active_tokens + output_reserve + guard;
    skull.saturating_sub(occupied)
}

// ── Formatting ─────────────────────────────────────────────────

/// Format a memory as HOT (full text + metadata).
fn format_hot(entry: &LambdaMemoryEntry) -> String {
    let accessed = if entry.access_count > 0 {
        format!(" | accessed: {}x", entry.access_count)
    } else {
        String::new()
    };
    let explicit = if entry.explicit_save { " | explicit save" } else { "" };
    format!(
        "[hot] {}\n      (#{} | {} | importance: {:.1}{}{})\n\n",
        entry.full_text,
        &entry.hash[..7.min(entry.hash.len())],
        format_timestamp(entry.created_at),
        entry.importance,
        accessed,
        explicit,
    )
}

/// Format a memory as WARM (summary + hash).
fn format_warm(entry: &LambdaMemoryEntry) -> String {
    format!(
        "[warm] {}\n       (#{} | {})\n\n",
        entry.summary_text,
        &entry.hash[..7.min(entry.hash.len())],
        format_timestamp(entry.created_at),
    )
}

/// Format a memory as COOL (essence + hash).
fn format_cool(entry: &LambdaMemoryEntry) -> String {
    format!(
        "[cool] {} (#{} | {})\n",
        entry.essence_text,
        &entry.hash[..7.min(entry.hash.len())],
        format_timestamp(entry.created_at),
    )
}

/// Format a memory as FADED (hash + timestamp + essence only).
fn format_faded(entry: &LambdaMemoryEntry) -> String {
    format!(
        "[faded] #{} | {} | {}\n",
        &entry.hash[..7.min(entry.hash.len())],
        format_timestamp(entry.created_at),
        entry.essence_text,
    )
}

fn format_timestamp(epoch: u64) -> String {
    chrono::DateTime::from_timestamp(epoch as i64, 0)
        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "unknown".to_string())
}

// ── Best Representation ────────────────────────────────────────

/// Choose the best representation that fits within `remaining` tokens.
/// Falls through from highest fidelity to lowest.
fn best_representation(
    entry: &LambdaMemoryEntry,
    remaining: usize,
    score: f32,
    thresholds: &Thresholds,
) -> (String, usize) {
    if score > thresholds.hot {
        let text = format_hot(entry);
        let cost = estimate_tokens(&text);
        if cost <= remaining { return (text, cost); }
    }
    if score > thresholds.warm {
        let text = format_warm(entry);
        let cost = estimate_tokens(&text);
        if cost <= remaining { return (text, cost); }
    }
    if score > thresholds.cool {
        let text = format_cool(entry);
        let cost = estimate_tokens(&text);
        if cost <= remaining { return (text, cost); }
    }
    if score > GONE_THRESHOLD {
        let text = format_faded(entry);
        let cost = estimate_tokens(&text);
        if cost <= remaining { return (text, cost); }
    }
    (String::new(), 0)
}

// ── Context Assembly ───────────────────────────────────────────

/// Assemble the λ-Memory section for injection into the context window.
///
/// Returns the formatted string and its estimated token count.
pub async fn assemble_lambda_context(
    memory: &dyn Memory,
    budget: usize,
    max_budget: usize,
    config: &LambdaMemoryConfig,
    current_query: &str,
) -> (String, usize) {
    if budget < MIN_ENTRY_TOKENS || !config.enabled {
        return (String::new(), 0);
    }

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    let thresholds = effective_thresholds(budget, max_budget, config);

    // Step 1: Query candidates
    let candidates = match memory.lambda_query_candidates(config.candidate_limit).await {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(error = %e, "λ-Memory candidate query failed");
            return (String::new(), 0);
        }
    };

    if candidates.is_empty() {
        return (String::new(), 0);
    }

    // Step 2: Compute decay scores
    let mut scored: Vec<(f32, &LambdaMemoryEntry)> = candidates
        .iter()
        .map(|m| (decay_score(m, now, config.decay_lambda), m))
        .collect();

    // Step 3: Boost scores for FTS-relevant memories
    if !current_query.is_empty() {
        if let Ok(fts_results) = memory.lambda_fts_search(current_query, 20).await {
            let fts_map: std::collections::HashMap<&str, f64> = fts_results
                .iter()
                .map(|(hash, rank)| (hash.as_str(), *rank))
                .collect();

            for (score, entry) in &mut scored {
                if let Some(&rank) = fts_map.get(entry.hash.as_str()) {
                    // BM25 rank is negative (lower = better match).
                    // Convert to a positive boost: max 2.0 for best matches.
                    let relevance_boost = (1.0 + (-rank).ln().max(0.0) as f32).min(2.0);
                    *score += relevance_boost * 0.4; // 40% weight on relevance
                }
            }
        }
    }

    // Step 4: Sort by final score descending
    scored.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));

    // Step 5: Pack into budget
    let header = "═══ λ-Memory ═══\n\n";
    let footer = "\n═══════════════\n";
    let header_cost = estimate_tokens(header);
    let footer_cost = estimate_tokens(footer);
    let mut remaining = budget.saturating_sub(header_cost + footer_cost);

    let mut output = String::from(header);

    // 5a: Explicit saves first (always included at minimum fidelity)
    for (score, entry) in scored.iter().filter(|(_, e)| e.explicit_save) {
        if remaining < MIN_ENTRY_TOKENS { break; }
        let (text, cost) = best_representation(entry, remaining, *score, &thresholds);
        if cost == 0 { continue; }
        output.push_str(&text);
        remaining -= cost;
    }

    // 5b: Remaining by score
    for (score, entry) in &scored {
        if entry.explicit_save { continue; }
        if remaining < MIN_ENTRY_TOKENS { break; }
        let (text, cost) = best_representation(entry, remaining, *score, &thresholds);
        if cost == 0 { continue; }
        output.push_str(&text);
        remaining -= cost;
    }

    output.push_str(footer);
    let total_cost = estimate_tokens(&output);
    (output, total_cost)
}

// ── Memory Creation ────────────────────────────────────────────

/// Gate: is this turn worth remembering?
pub fn worth_remembering(user_text: &str, has_tool_calls: bool) -> bool {
    let text_lower = user_text.to_lowercase();

    // Explicit request
    if text_lower.contains("remember") && text_lower.contains("this")
        || text_lower.contains("remember:")
        || text_lower.contains("don't forget")
    {
        return true;
    }

    // Decision language
    let decision_words = ["decide", "chose", "choose", "switch", "change", "use",
                          "prefer", "always", "never", "refactor", "rewrite",
                          "deploy", "ship", "merge", "approve", "reject"];
    if decision_words.iter().any(|w| text_lower.contains(w)) {
        return true;
    }

    // Has tool calls and substantive text
    if has_tool_calls && user_text.len() > 80 {
        return true;
    }

    // Emotional markers
    let emotional = ["frustrated", "love", "hate", "amazing", "terrible",
                     "important", "critical", "urgent", "excited", "worried"];
    if emotional.iter().any(|w| text_lower.contains(w)) {
        return true;
    }

    false
}

/// Parse a <memory> block from the LLM response text.
///
/// Expected format:
/// ```text
/// <memory>
/// summary: one sentence summary
/// essence: five words max
/// importance: 3
/// tags: auth, refactor, axum
/// </memory>
/// ```
///
/// Returns None if no block found or parsing fails.
pub fn parse_memory_block(response_text: &str) -> Option<ParsedMemoryBlock> {
    let start = response_text.find("<memory>")?;
    let end = response_text.find("</memory>")?;
    if end <= start { return None; }

    let block = &response_text[start + 8..end].trim();
    let mut summary = String::new();
    let mut essence = String::new();
    let mut importance: f32 = 2.0;
    let mut tags: Vec<String> = Vec::new();

    for line in block.lines() {
        let line = line.trim();
        if let Some(val) = line.strip_prefix("summary:") {
            summary = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("essence:") {
            essence = val.trim().to_string();
        } else if let Some(val) = line.strip_prefix("importance:") {
            importance = val.trim().parse().unwrap_or(2.0).clamp(1.0, 5.0);
        } else if let Some(val) = line.strip_prefix("tags:") {
            tags = val.split(',').map(|t| t.trim().to_string()).filter(|t| !t.is_empty()).collect();
        }
    }

    if summary.is_empty() && essence.is_empty() {
        return None;
    }

    Some(ParsedMemoryBlock {
        summary,
        essence,
        importance,
        tags,
    })
}

pub struct ParsedMemoryBlock {
    pub summary: String,
    pub essence: String,
    pub importance: f32,
    pub tags: Vec<String>,
}

/// Strip <memory>...</memory> blocks from response text before sending to user.
pub fn strip_memory_blocks(text: &str) -> String {
    let mut result = text.to_string();
    while let Some(start) = result.find("<memory>") {
        if let Some(end) = result[start..].find("</memory>") {
            result.replace_range(start..start + end + 9, "");
        } else {
            break;
        }
    }
    result.trim().to_string()
}
```

---

## 5. Phase 4: Context Integration (temm1e-agent/context.rs)

### 5.1 What Changes

Replace the three separate sections (memory search Cat 5, knowledge Cat 5b, learnings Cat 6) with a single call to `assemble_lambda_context()`.

### 5.2 Modification Points

**Before** (lines ~219-355 in current context.rs):
```rust
// ── Category 5: Memory search results (up to 15% of budget)
// ── Category 5b: Knowledge entries
// ── Category 6: Cross-task learnings
```

**After:**
```rust
// ── Category 5: λ-Memory (dynamic budget) ──────────────────────
let lambda_config = LambdaMemoryConfig::default(); // TODO: load from config
let lambda_max_budget = lambda_budget(
    skull_size,          // from model_registry
    model_max_output,    // from model_registry
    fixed_tokens + blueprint_tokens_used,
    recent_tokens,
);
let lambda_current_budget = lambda_max_budget.min(available_after_fixed_and_recent);

let query = extract_latest_query(history);
let (lambda_text, lambda_tokens_used) = lambda_memory::assemble_lambda_context(
    memory,
    lambda_current_budget,
    lambda_max_budget,
    &lambda_config,
    &query,
).await;

let mut lambda_messages: Vec<ChatMessage> = Vec::new();
if !lambda_text.is_empty() {
    lambda_messages.push(ChatMessage {
        role: Role::System,
        content: MessageContent::Text(lambda_text),
    });
}
```

**Also update:**
- The `used_tokens` calculation to include `lambda_tokens_used` instead of separate memory + knowledge + learning tokens
- The budget dashboard to show λ-Memory budget instead of separate memory/learning lines
- The assembly order to use `lambda_messages` instead of `memory_messages + knowledge_messages + learning_messages`
- Import `model_registry::model_limits` to get skull size and max output

### 5.3 Fallback

If λ-Memory is disabled via config, fall back to the existing Category 5/5b/6 logic. Keep the old code behind a `if !lambda_config.enabled { ... }` branch.

---

## 6. Phase 5: Memory Extraction (temm1e-agent/runtime.rs)

### 6.1 Where to Parse

After provider response parsing (current line ~941), before the "if no tool calls → finish" check (current line ~943).

### 6.2 Integration Code

```rust
// ── λ-Memory: Parse <memory> blocks from response ──────────
if !text_parts.is_empty() {
    let combined = text_parts.join("\n");
    if let Some(parsed) = lambda_memory::parse_memory_block(&combined) {
        // Build full_text from the user's message + core of assistant response
        let user_text = lambda_memory::extract_user_text(&session.history);
        let assistant_text = lambda_memory::strip_memory_blocks(&combined);
        let full_text = format!(
            "User: {}\nAssistant: {}",
            truncate(&user_text, 500),
            truncate(&assistant_text, 500),
        );

        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let hash = blake3::hash(
            format!("{}:{}:{}", &session.session_id, round, now).as_bytes()
        ).to_hex()[..12].to_string();

        let entry = LambdaMemoryEntry {
            hash,
            created_at: now,
            last_accessed: now,
            access_count: 0,
            importance: parsed.importance,
            explicit_save: user_text.to_lowercase().contains("remember"),
            full_text,
            summary_text: parsed.summary,
            essence_text: parsed.essence,
            tags: parsed.tags,
            memory_type: LambdaMemoryType::Conversation,
            session_id: session.session_id.clone(),
        };

        if let Err(e) = self.memory.lambda_store(entry).await {
            tracing::warn!(error = %e, "Failed to store λ-memory");
        }
    }

    // Strip <memory> blocks from text_parts before user sees them
    for part in &mut text_parts {
        *part = lambda_memory::strip_memory_blocks(part);
    }
}
```

### 6.3 System Prompt Injection

Add a λ-Memory instruction to the system prompt (in `build_system_prompt()` or via `prompt_optimizer`):

```
For memorable turns (decisions, preferences, important actions), include a <memory> block at the end of your response:
<memory>
summary: (one sentence)
essence: (5 words max)
importance: (1-5: 1=casual, 2=routine, 3=decision, 4=preference, 5=critical)
tags: (up to 5, comma-separated)
</memory>
Do not include this block for trivial turns (greetings, acknowledgments, simple Q&A).
```

### 6.4 Learning Storage Migration

In the learning extraction section (line ~990), store learnings as λ-memories instead of regular MemoryEntries:

```rust
for l in &learnings {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    let hash = blake3::hash(
        format!("learning:{}:{}", &l.task_type, now).as_bytes()
    ).to_hex()[..12].to_string();

    let entry = LambdaMemoryEntry {
        hash,
        created_at: now,
        last_accessed: now,
        access_count: 0,
        importance: match l.outcome {
            TaskOutcome::Success => 2.5,
            TaskOutcome::Partial => 3.0,
            TaskOutcome::Failure => 3.5,
        },
        explicit_save: false,
        full_text: serde_json::to_string(l).unwrap_or_default(),
        summary_text: l.lesson.clone(),
        essence_text: format!("{}: {}", l.task_type, format!("{:?}", l.outcome).to_lowercase()),
        tags: l.approach.clone(),
        memory_type: LambdaMemoryType::Learning,
        session_id: session.session_id.clone(),
    };

    if let Err(e) = self.memory.lambda_store(entry).await {
        tracing::warn!(error = %e, "Failed to store λ-learning");
    }
}
```

---

## 7. Phase 6: Recall Tool (temm1e-tools)

### 7.1 New file: `crates/temm1e-tools/src/lambda_recall.rs`

```rust
//! λ-Memory recall tool — lets Tem retrieve faded memories by hash.

use async_trait::async_trait;
use std::sync::Arc;
use temm1e_core::error::Temm1eError;
use temm1e_core::traits::tool::{Tool, ToolContext, ToolDeclarations, ToolInput, ToolOutput};
use temm1e_core::Memory;

pub struct LambdaRecallTool {
    memory: Arc<dyn Memory>,
}

impl LambdaRecallTool {
    pub fn new(memory: Arc<dyn Memory>) -> Self {
        Self { memory }
    }
}

#[async_trait]
impl Tool for LambdaRecallTool {
    fn name(&self) -> &str {
        "lambda_recall"
    }

    fn description(&self) -> &str {
        "Recall a faded λ-memory by its hash prefix. Use this when you see a \
         [faded] or [cool] memory in your context that you need full details for. \
         Provide the hash shown (e.g., #a7f3b2c) to retrieve the complete memory."
    }

    fn parameters_schema(&self) -> serde_json::Value {
        serde_json::json!({
            "type": "object",
            "properties": {
                "hash": {
                    "type": "string",
                    "description": "The memory hash prefix (e.g., 'a7f3b2c')"
                }
            },
            "required": ["hash"]
        })
    }

    fn declarations(&self) -> ToolDeclarations {
        ToolDeclarations {
            file_access: Vec::new(),
            network_access: Vec::new(),
            shell_access: false,
        }
    }

    async fn execute(
        &self,
        input: ToolInput,
        _ctx: &ToolContext,
    ) -> Result<ToolOutput, Temm1eError> {
        let hash = input
            .arguments
            .get("hash")
            .and_then(|v| v.as_str())
            .ok_or_else(|| Temm1eError::Tool("Missing required parameter: hash".into()))?;

        // Strip leading # if present
        let hash_clean = hash.trim_start_matches('#');

        match self.memory.lambda_recall(hash_clean).await? {
            Some(entry) => {
                // Touch the memory — reheat it
                if let Err(e) = self.memory.lambda_touch(&entry.hash).await {
                    tracing::warn!(error = %e, "Failed to touch recalled λ-memory");
                }

                let tags_str = entry.tags.join(", ");
                let content = format!(
                    "[RECALLED] Full memory content:\n\n\
                     {}\n\n\
                     ---\n\
                     Hash: #{}\n\
                     Created: {}\n\
                     Importance: {:.1}\n\
                     Accessed: {} times\n\
                     Tags: {}\n\
                     Type: {:?}",
                    entry.full_text,
                    &entry.hash[..7.min(entry.hash.len())],
                    chrono::DateTime::from_timestamp(entry.created_at as i64, 0)
                        .map(|dt| dt.format("%Y-%m-%d %H:%M").to_string())
                        .unwrap_or_else(|| "unknown".to_string()),
                    entry.importance,
                    entry.access_count + 1, // +1 because we just touched it
                    tags_str,
                    entry.memory_type,
                );

                Ok(ToolOutput {
                    content,
                    is_error: false,
                })
            }
            None => Ok(ToolOutput {
                content: format!(
                    "No λ-memory found with hash prefix '{}'. \
                     It may have been garbage collected or the hash is incorrect.",
                    hash_clean
                ),
                is_error: false,
            }),
        }
    }
}
```

### 7.2 Registration

In `crates/temm1e-tools/src/lib.rs`, add to `create_tools()`:

```rust
// After the existing memory_manage tool registration
if let Some(ref mem) = memory {
    tools.push(Arc::new(lambda_recall::LambdaRecallTool::new(Arc::clone(mem))));
}
```

Add `pub mod lambda_recall;` to the tools crate's module declarations.

---

## 8. Phase 7: Configuration

### 8.1 temm1e.toml

```toml
[memory.lambda]
enabled = true
decay_lambda = 0.01
hot_threshold = 2.0
warm_threshold = 1.0
cool_threshold = 0.3
candidate_limit = 500
```

### 8.2 Loading

The `LambdaMemoryConfig` should be loaded from config and passed through to `build_context()`. Add it as a parameter or access it from a shared config Arc.

---

## 9. Migration Strategy

### 9.1 Existing Knowledge Entries → λ-Memories

On first startup with λ-Memory enabled, migrate existing `MemoryEntryType::Knowledge` entries:

```rust
async fn migrate_knowledge_to_lambda(memory: &dyn Memory) -> Result<(), Temm1eError> {
    let opts = SearchOpts {
        limit: 1000,
        entry_type_filter: Some(MemoryEntryType::Knowledge),
        ..Default::default()
    };
    let entries = memory.search("", opts).await?;

    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();

    for entry in entries {
        let key = entry.metadata.get("user_key")
            .and_then(|v| v.as_str())
            .unwrap_or("migrated");
        let hash = blake3::hash(
            format!("migrate:{}:{}", &entry.id, now).as_bytes()
        ).to_hex()[..12].to_string();

        let lambda_entry = LambdaMemoryEntry {
            hash,
            created_at: entry.timestamp.timestamp() as u64,
            last_accessed: now,
            access_count: 0,
            importance: 4.0,  // Knowledge entries are high-importance
            explicit_save: true, // Treat as explicit saves
            full_text: entry.content.clone(),
            summary_text: entry.content.clone(), // Same for knowledge
            essence_text: key.to_string(),
            tags: vec!["knowledge".to_string(), "migrated".to_string()],
            memory_type: LambdaMemoryType::Knowledge,
            session_id: entry.session_id.unwrap_or_default(),
        };

        memory.lambda_store(lambda_entry).await?;
    }
    Ok(())
}
```

### 9.2 Backward Compatibility

- Old `memory_entries` table is NOT deleted
- `memory_manage` tool continues to work (stores to old table)
- λ-Memory reads from `lambda_memories` table only
- Config flag `enabled = false` disables λ-Memory and falls back to old Categories 5/5b/6

---

## 10. Test Plan

### 10.1 Unit Tests (in lambda_memory.rs)

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decay_score_at_creation() {
        let entry = test_entry(3.0, 1000, 1000); // importance=3, created=accessed=1000
        assert!((decay_score(&entry, 1000, 0.01) - 3.0).abs() < 0.001);
    }

    #[test]
    fn decay_score_after_24h() {
        let entry = test_entry(3.0, 0, 0);
        let now = 86400; // 24 hours in seconds
        let score = decay_score(&entry, now, 0.01);
        // 3.0 * exp(-24 * 0.01) = 3.0 * 0.7866 = 2.36
        assert!((score - 2.36).abs() < 0.01);
    }

    #[test]
    fn decay_score_after_7_days() {
        let entry = test_entry(3.0, 0, 0);
        let now = 7 * 86400;
        let score = decay_score(&entry, now, 0.01);
        // 3.0 * exp(-168 * 0.01) = 3.0 * 0.186 = 0.559
        assert!((score - 0.559).abs() < 0.01);
    }

    #[test]
    fn high_importance_decays_slower() {
        let low = test_entry(1.0, 0, 0);
        let high = test_entry(5.0, 0, 0);
        let now = 3 * 86400; // 3 days
        assert!(decay_score(&high, now, 0.01) > decay_score(&low, now, 0.01));
    }

    #[test]
    fn parse_memory_block_valid() {
        let text = "Some response\n<memory>\nsummary: did a thing\nessence: thing done\nimportance: 3\ntags: foo, bar\n</memory>";
        let parsed = parse_memory_block(text).unwrap();
        assert_eq!(parsed.summary, "did a thing");
        assert_eq!(parsed.essence, "thing done");
        assert!((parsed.importance - 3.0).abs() < 0.01);
        assert_eq!(parsed.tags, vec!["foo", "bar"]);
    }

    #[test]
    fn parse_memory_block_missing() {
        assert!(parse_memory_block("no block here").is_none());
    }

    #[test]
    fn strip_memory_blocks_clean() {
        let text = "Hello world\n<memory>\nsummary: test\n</memory>\nGoodbye";
        assert_eq!(strip_memory_blocks(text), "Hello world\nGoodbye");
    }

    #[test]
    fn worth_remembering_explicit() {
        assert!(worth_remembering("remember this: use tabs not spaces", false));
    }

    #[test]
    fn worth_remembering_trivial() {
        assert!(!worth_remembering("thanks", false));
        assert!(!worth_remembering("ok", false));
    }

    #[test]
    fn effective_thresholds_no_pressure() {
        let config = LambdaMemoryConfig::default();
        let t = effective_thresholds(10000, 10000, &config);
        assert!((t.hot - 2.0).abs() < 0.01);
    }

    #[test]
    fn effective_thresholds_full_pressure() {
        let config = LambdaMemoryConfig::default();
        let t = effective_thresholds(0, 10000, &config);
        assert!((t.hot - 4.0).abs() < 0.01);
    }

    fn test_entry(importance: f32, created_at: u64, last_accessed: u64) -> LambdaMemoryEntry {
        LambdaMemoryEntry {
            hash: "test1234abcd".to_string(),
            created_at,
            last_accessed,
            access_count: 0,
            importance,
            explicit_save: false,
            full_text: "test full".to_string(),
            summary_text: "test summary".to_string(),
            essence_text: "test".to_string(),
            tags: vec!["test".to_string()],
            memory_type: LambdaMemoryType::Conversation,
            session_id: "test-session".to_string(),
        }
    }
}
```

### 10.2 Integration Tests

- **SQLite round-trip**: Store → query → recall → touch → verify access_count incremented
- **FTS5 search**: Store 10 memories with different tags → search → verify relevance ranking
- **GC**: Store old memories → run gc → verify only non-explicit old ones deleted
- **Context assembly**: Mock memory with 20 entries → assemble with various budgets → verify output format and token count

### 10.3 Live Test (30-Turn GPT-5.2 Conversation)

Run via TEMM1E CLI:
1. 10 turns of varied tasks (file ops, shell, questions, decisions)
2. Verify `<memory>` blocks are parsed and stored
3. Wait simulated time or adjust λ for faster decay
4. 10 more turns — verify memories appear at correct tiers
5. Trigger recall — verify full content retrieved and memory reheats
6. 10 final turns — verify budget adapts, old memories fade
7. Check `lambda_memories` table for correct entries

---

## Dependency Additions

### Cargo.toml changes

**temm1e-agent:**
```toml
blake3 = "1"       # For hashing memory entries
```

**temm1e-tools:**
```toml
# No new dependencies — uses existing temm1e-core traits
```

**temm1e-core:**
```toml
# No new dependencies — just new types
```

**temm1e-memory:**
```toml
# No new dependencies — SQLite FTS5 is built into sqlx/sqlite
```

---

## Implementation Order

1. **Core types** (LambdaMemoryEntry, LambdaMemoryType, LambdaMemoryConfig) — no dependencies
2. **Memory trait** additions — default impls, no breakage
3. **SQLite implementation** — new table + FTS5 + 6 methods
4. **lambda_memory.rs** — decay engine, parsing, formatting
5. **lambda_recall tool** — standalone, depends on Memory trait
6. **context.rs integration** — replace Cat 5/5b/6
7. **runtime.rs integration** — parse `<memory>` blocks
8. **Config loading** — wire up temm1e.toml
9. **Migration** — knowledge entries → lambda_memories
10. **Tests** — unit + integration + live
