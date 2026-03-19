# Project Benchmark v2 Results (Difficulty 8/10)

Date: 2026-03-18
Model: gemini-3.1-flash-lite-preview

## What makes this 8/10

- High-level spec only (no prescribed function signatures)
- Workers read ACTUAL generated dependency files as context
- Compile-fix loop: generate → cargo check → feed errors back → retry (max 3)
- Final verification: cargo check + cargo test on REAL output
- Measures API calls including retries

## Results

| Metric | Single Agent | Swarm |
|--------|-------------|-------|
| Wall clock | 50,720ms | 43,248ms |
| Speedup | — | 1.17x |
| Tokens | 11,609 | 13,708 |
| API calls (inc retries) | 14 | 16 |
| Total attempts | 14 | 16 |
| Files clean (1st try) | **0/8** | **3/8** |
| Cost | $0.001915 | $0.002262 |
| **cargo check** | **FAIL** | **PASS** |
| cargo test | FAIL | FAIL |
| Tests passed | 0 | 0 |
| Lines | 278 | 302 |

## Key Findings

### 1. Swarm produces compilable code, single agent doesn't

The swarm's `cargo check` PASSED. The single agent's FAILED.

**Why:** The single agent's lib.rs accumulated bad context across retries — it started using `sqlx::query_as!` compile-time macros (which need DATABASE_URL env) and invented wrong module paths (`crate::models` when modules weren't declared). After 3 attempts it was still broken because each retry carried the full error history.

The swarm's workers operated with clean, scoped context. The crud.rs and search.rs workers in Tier 2 read the ACTUAL generated error.rs, models.rs, and db.rs files — so they imported the real types. This is the dependency-reading advantage.

### 2. Swarm gets 3 files clean on first try vs 0 for single agent

Single agent: every file had compilation errors because the project was incomplete (modules declared but not yet written). It burned all 3 retry attempts on Cargo.toml and lib.rs trying to fix cascading issues.

Swarm: independent files (error.rs, models.rs, lib.rs) compiled on first attempt because they have no dependencies. Tier 2 files (crud.rs, search.rs) also compiled clean because they read the actual generated dependency files.

### 3. Both test suites failed

Single agent: `cargo check` itself failed, so tests never ran.

Swarm: `cargo check` passed but the integration test file has issues:
- Tier 1 generated a preliminary integration test, then Tier 3 overwrote it with a different version
- The Tier 3 version used wrong crate name and sqlx migration macros
- This is a real swarm coordination bug: the same file shouldn't be in multiple tiers

### 4. The compile-fix loop is genuinely harder

With the v1 benchmark (prescribed signatures), both modes passed. With v2 (high-level specs + real dependency reading + compile-fix), only the swarm's library code compiled. This proves the difficulty increase is real.

## What the swarm did better

1. **Scoped context** — each worker saw only its dependencies, not accumulated error history
2. **Real dependency reading** — Tier 2 workers read ACTUAL generated files, caught real type names
3. **Parallel compile-fix** — independent files fixed themselves simultaneously
4. **Higher first-try success** — 3/8 vs 0/8

## What needs fixing

1. **File assignment conflict** — integration.rs was in both Tier 1 and Tier 3, causing overwrites
2. **Test prompt coherence** — the test file needs to see ALL generated source to write correct tests
3. **Crate name drift** — LLM sometimes changes crate name in test files

## Honest Assessment

The swarm's architectural advantage is real: scoped context + real dependency reading produces better code than serial context accumulation. But the test generation problem shows that some tasks (integration tests that need to understand the WHOLE project) don't parallelize well — they need to be the final serial step with full project visibility.

This is a genuine 8/10 difficulty finding. The v1 benchmark (3/10) wouldn't have caught this.
