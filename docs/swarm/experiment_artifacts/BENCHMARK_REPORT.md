# TEMM1E Hive — Project Build Benchmark (Verified)

**Date:** 2026-03-18
**Model:** gemini-3.1-flash-lite-preview
**Task:** Build "taskforge" — a Rust library with SQLite CRUD, search, and 5 integration tests
**Verification:** `cargo check` PASS + `cargo test` PASS (5/5 tests green) — BOTH modes

---

## Final Results

| Metric | Single Agent | Swarm |
|--------|-------------|-------|
| **Wall clock** | **19,184ms** | **14,203ms** |
| **Speedup** | — | **1.35x** |
| Total tokens | 6,142 | 7,513 |
| Token ratio | — | 1.22x |
| API calls | 8 (serial) | 9 (4 tiers, parallel within each) |
| Cost (USD) | $0.001013 | $0.001240 |
| **cargo check** | **PASS** | **PASS** |
| **cargo test** | **PASS (5/5)** | **PASS (5/5)** |
| Lines generated | 322 | 321 |

## Verification Proof

### Single Agent Output
```
running 5 tests
test tests::test_create_and_get ... ok
test tests::test_delete_task ... ok
test tests::test_update_status ... ok
test tests::test_search_by_status ... ok
test tests::test_list_tasks ... ok
test result: ok. 5 passed; 0 failed
```

### Swarm Output
```
running 5 tests
test tests::test_delete_task ... ok
test tests::test_update_status ... ok
test tests::test_create_and_get ... ok
test tests::test_list_tasks ... ok
test tests::test_search_by_status ... ok
test result: ok. 5 passed; 0 failed
```

## Execution Timeline

### Single Agent (serial — 19.2s)
```
t=0.0s   Cargo.toml        2.1s
t=2.4s   src/error.rs      1.9s
t=4.6s   src/models.rs     1.7s
t=6.6s   src/db.rs         1.6s
t=8.5s   src/crud.rs       3.1s
t=11.9s  src/search.rs     1.7s
t=13.9s  src/lib.rs        1.4s
t=15.6s  tests/            3.2s
t=19.2s  DONE (8 serial API calls)
```

### Swarm (4 dependency tiers — 14.2s)
```
t=0.0s   Tier 0: [Cargo.toml + error.rs + models.rs + lib.rs]  4 parallel  3.3s
t=3.5s   Tier 1: [db.rs + integration.rs]                      2 parallel  3.9s
t=7.6s   Tier 2: [crud.rs + search.rs]                         2 parallel  3.4s
t=11.2s  Tier 3: [integration.rs final]                        1 serial    2.9s
t=14.2s  DONE (9 API calls, 4 tiers)
```

## What This Proves

1. **Both modes produce compilable, tested code.** 5/5 integration tests pass on both artifacts. The quality is equal.

2. **Swarm is 1.35x faster** for this DAG structure (4 dependency tiers). The parallelism within each tier is real — Tier 0 runs 4 API calls simultaneously instead of serially.

3. **Token overhead is 22%.** The swarm uses 1,371 more tokens (queen decomposition + duplicate integration test generation). This is the coordination cost.

4. **Cost overhead is 22%.** $0.001240 vs $0.001013. At Gemini Flash Lite prices, this is negligible ($0.000227 difference).

5. **Equal quality.** Both produce ~320 lines of working Rust with identical test results. Neither is better or worse — the prompts are identical, only the execution order differs.

## All Benchmark Results Combined

| Benchmark | Single | Swarm | Speedup | Token Ratio |
|-----------|--------|-------|---------|-------------|
| 5 independent subtasks | 7,989ms | 1,844ms | **4.33x** | 1.01x |
| 8-file project (4 tiers) | 19,184ms | 14,203ms | **1.35x** | 1.22x |
| Simple chat | 1,261ms | 1,338ms | 1.0x | 1.00x |

Speedup = `parallel_width / dependency_depth`. More independent work = more speedup.

## Budget

| Item | Cost |
|------|------|
| All benchmarks to date | ~$0.010 |
| **Budget remaining** | **$29.99** |

## Artifacts

- `single_agent/taskforge/` — cargo check PASS, cargo test 5/5 PASS
- `swarm_agent/taskforge/` — cargo check PASS, cargo test 5/5 PASS
- Both contain: Cargo.toml, src/{lib,error,models,db,crud,search}.rs, tests/integration.rs
