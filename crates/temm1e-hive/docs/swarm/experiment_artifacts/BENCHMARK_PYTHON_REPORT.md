# Python Project Benchmark (Difficulty 8/10)

Date: 2026-03-17 18:03:14 UTC
Model: gemini-3.1-pro-preview

## Why Python (fair test)

- Per-file syntax check: `python -m py_compile` — each worker verifies independently
- No all-or-nothing compilation (unlike Rust's cargo check)
- Final test: `pytest` on complete project

## Results

| Metric | Single | Swarm |
|--------|--------|-------|
| Wall clock | 566351ms | 391185ms |
| Speedup | — | 1.45x |
| Tokens | 31642 | 32400 |
| API calls | 17 | 17 |
| Total attempts | 17 | 17 |
| Files clean 1st try | 5/7 | 4/7 |
| Cost | $0.010442 | $0.010692 |
| pytest | FAIL | FAIL |
| Tests passed | 0 | 0 |
| Lines | 405 | 419 |
