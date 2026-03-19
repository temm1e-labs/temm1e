# V2 Tem's Mind — Experiment Insights & Future Improvements

**Date:** 2026-03-10
**Related:** [Benchmark Report](TEMS_MIND_V2_BENCHMARK_TOOLS.md) | [v1 log](benchmark_v2_tools_v1_log.txt) | [v2 log](benchmark_v2_tools_v2_log.txt)

---

## Results Summary

| Metric | Value | Context |
|--------|-------|---------|
| Overall cost reduction | 4.8% | Mixed traffic (trivial through compound) |
| Multi-step compound tasks | **12.2%** | V2's actual optimization target |
| Best individual compound turn | **35.9%** (T17) | Write script + run + verify |
| Classification accuracy | 100% (20/20) | Zero LLM overhead |
| Reliability | 100% both versions | 20/20 turns |

---

## Key Insight: Where V2 Actually Saves Money

V2's savings come almost entirely from **compound multi-step tasks** that require multiple tool rounds. The mechanism:

```
fewer API rounds → less cumulative context → fewer input tokens → lower cost
```

Trivial/Simple tiers show ~0% savings because smart LLMs (GPT-5.2) already don't call tools for greetings or factual questions. The `skip_tool_loop` and `max_iterations` caps are safety nets, not active cost reducers with this model.

**The 4.8% overall number undersells V2** — it's diluted by 12 turns costing ~$0.006 each where there's nothing to optimize. The representative number is **12.2% on compound tasks** — the most expensive task category where savings compound.

---

## What Didn't Work As Expected

### 1. Trivial/Simple prompt stratification savings are negligible

- **Theory**: Minimal/Basic prompt tiers save ~90-128 tokens per turn
- **Reality**: At GPT-5.2 pricing, this saves fractions of a cent per turn
- System prompt is already small (~150 tokens for Standard) — reducing to ~22 (Minimal) saves ~$0.00001/turn
- **Next step**: Test with Claude Opus or GPT-4o where input token costs are 10-15x higher. Prompt stratification would also matter more with large system prompts (1000+ tokens).

### 2. Stochastic LLM tool-calling variance dominates per-turn costs

- Same prompt, same model → different tool strategies between runs
- T9: V2 used `file_read` + `shell` (2 tools, $0.0257) vs V1's single `shell` ($0.0165) — 55% more expensive from pure randomness
- T16: V2 hit a shell syntax error (process substitution `>()` in `/bin/sh`) requiring retry — 37% more expensive
- **Next step**: Run benchmarks 3-5x and report median. Single-run benchmarks are statistically noisy. If provider supports `seed` parameter, use it to eliminate stochastic variance.

### 3. Output caps (max_tool_output_chars) never triggered

- No tool output was large enough to hit the 15,000 char cap
- Current benchmark tasks produce small outputs (directory listings, short file contents)
- **Next step**: Add benchmark turns that generate large tool output (>10KB): `find / -name "*.py"`, `cat` on large files, verbose build output.

### 4. No Complex tier tasks tested

- All tool tasks classified as Standard. No tasks triggered Complex classification.
- Missing coverage: multi-file analysis, architecture questions, debugging scenarios
- **Next step**: Add 5+ Complex tier tasks (e.g., "analyze this codebase structure", "debug why X fails") to test Complex tier with `max_iterations=10` and Full prompt.

---

## What Worked Well

### 1. Compound task optimization — primary value driver

- V2's DONE-criteria injection + iteration limits lead to more efficient multi-step execution
- T14 (create dir + 3 files + list): V2 used 2 tool rounds vs V1's 3 → **22% savings**
- T17 (write script + run + verify): V2 used 2 tool rounds vs V1's 4 → **36% savings**
- The LLM learns to batch operations into fewer shell commands when constrained

### 2. Rule-based classification — zero overhead, 100% accuracy

- 100% correct across all 20 turns
- Zero additional LLM calls for classification
- Clear tier boundaries: greeting patterns → Trivial, question marks + short msgs → Simple, tool keywords → Standard
- No false positives (simple task routed through expensive pipeline)

### 3. Parallel benchmark execution

- Running v1/v2 simultaneously with isolated workspace dirs (`workspace_v1/`, `workspace_v2/`)
- WORKSPACE template substitution in prompts keeps prompts structurally identical
- Halves wall-clock time with no data contamination

---

## Pitfalls Learned

### Session history pollution — critical

- `~/.temm1e/memory.db` retains conversation history across benchmark runs
- First benchmark was polluted: 76 restored Gemini messages inflated base context from ~3,100 to ~9,000 tokens
- Broke Simple tier classification (history depth penalty in classifier)
- **Rule**: ALWAYS clean both `~/.temm1e/memory.db` AND per-version `memory.db` before any benchmark

### Gemini incompatibility with tool benchmarks

- Gemini 3 Flash rejects tool result feedback through OpenAI-compat endpoint
- Error: `Function call is missing a thought_signature in functionCall parts`
- Tool execution works (command runs), but follow-up API call with tool results fails
- **Rule**: Cannot benchmark tool usage on Gemini via compat provider. Need native Gemini SDK.

### Log sanitization is mandatory

- API keys, home directory paths, hostnames all appear in raw terminal logs
- Built post-run regex pipeline: `AIza*`, `sk-*`, `sk-ant-*`, `sk-or-*`, home dir, hostname
- **Rule**: Always sanitize before committing or sharing logs

---

## Future Experiment Roadmap

| Priority | Improvement | Expected Impact |
|----------|-------------|-----------------|
| **P0** | Run 3-5x, report median | Eliminates stochastic noise, trustworthy numbers |
| **P0** | Test with expensive models (Claude Opus, GPT-4o) | Amplifies per-token savings, shows true dollar impact |
| **P1** | Add large-output tool tasks (>10KB output) | Tests max_tool_output_chars cap — currently untested |
| **P1** | Add Complex tier tasks | Tests full pipeline with max_iterations=10 |
| **P1** | Measure latency, not just cost | V2 should be faster on trivial/simple (skip tool loop) |
| **P2** | Test with tool-aggressive models | Shows Trivial/Simple tier savings when model over-uses tools |
| **P2** | Increase to 50+ turns | More data points, better statistical significance |
| **P2** | Use provider seed parameter | Eliminates stochastic variance for apples-to-apples comparison |
| **P3** | Test with large system prompts (1000+ tokens) | Amplifies prompt stratification savings |

---

*Insights from TEMM1E Tem's Mind v2.0 A/B benchmark experiment*
*Date: 2026-03-10*
