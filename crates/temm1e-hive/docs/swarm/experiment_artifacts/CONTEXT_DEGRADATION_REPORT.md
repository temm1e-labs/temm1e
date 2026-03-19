# Context Degradation Benchmark

Date: 2026-03-18 03:21:22 UTC
Model: gemini-3.1-pro-preview

## Hypothesis

Single agent accumulating conversation history degrades on later functions.
Swarm with fresh context maintains consistent quality.

## Results

| Metric | Single Agent | Swarm |
|--------|-------------|-------|
| **Functions passing** | **12/12** | **12/12** |
| Wall clock | 102849ms | 17551ms |
| Speedup | — | 5.86x |
| Tokens | 7379 | 2149 |
| Cost | $0.002435 | $0.000709 |
