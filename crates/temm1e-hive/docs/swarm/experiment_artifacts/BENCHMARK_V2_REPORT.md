# Project Benchmark v2 Results (Difficulty 8/10)

Date: 2026-03-17 17:37:39 UTC
Model: gemini-3.1-pro-preview

## What makes this 8/10

- High-level spec only (no prescribed function signatures)
- Workers read ACTUAL generated dependency files
- Compile-fix loop: generate → cargo check → feed errors → retry (max 3)
- Final verification: cargo check + cargo test on REAL output

## Results

| Metric | Single Agent | Swarm |
|--------|-------------|-------|
| Wall clock | 195664ms | 593730ms |
| Speedup | — | 0.33x |
| Tokens | 8987 | 19631 |
| API calls (inc retries) | 10 | 19 |
| Total attempts | 10 | 19 |
| Files clean (1st try) | 2/8 | 0/8 |
| Cost | $0.001483 | $0.003239 |
| cargo check | PASS | FAIL |
| cargo test | PASS | FAIL |
| Tests passed | 5 | 0 |
| Lines | 387 | 217 |
