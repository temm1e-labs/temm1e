# TEMM1E v2.0 — Smarter Tem's Mind

TEMM1E v2.0 introduces a smarter Tem's Mind that understands **what kind of task you're asking** before it starts working. The result: fewer unnecessary API calls, lower costs on complex tasks, and the same response quality you're used to.

---

## What Changed

v1 treats every message the same — full system prompt, full tool pipeline, same iteration limits whether you say "thanks" or ask it to build and run a script.

v2 classifies each message into a complexity tier **before** calling the LLM:

| You say... | v2 understands | What it skips |
|------------|---------------|---------------|
| "Hi" / "Thanks" / "Ok" | **Trivial** — no work needed | Skips tool pipeline entirely |
| "What is HTTP?" / "Capital of France?" | **Simple** — just answer | Limits tool iterations, uses lighter prompt |
| "Create these files, then verify" | **Standard** — real work | Full pipeline, optimized execution |
| "Debug this codebase" | **Complex** — deep work | Full pipeline, extended iterations |

Classification is instant and rule-based — it costs nothing. No extra LLM call, no added latency.

---

## What You Get

### Lower cost on complex tasks

Multi-step tasks that require multiple tool calls are where v2 shines. In our 20-turn benchmark against v1:

| Task type | Cost change | Why |
|-----------|------------|-----|
| Greetings, simple Q&A | ~Same | LLM already handles these efficiently |
| Single tool tasks | ~Same | One tool call is one tool call |
| **Multi-step compound tasks** | **12% cheaper** | Fewer API rounds to complete the same work |
| Best case (script + run + verify) | **36% cheaper** | 2 tool rounds instead of 4 |

**Overall across all task types: 4.8% cost reduction.** The savings concentrate on expensive multi-step operations — exactly where cost matters most.

### Fewer API calls

v2 completed the same 20-turn benchmark with:
- **39 API calls** vs v1's 41
- **19 tool executions** vs v1's 22 (14% fewer)
- **5.6% fewer input tokens** overall

Fewer calls means lower latency on compound tasks, not just lower cost.

### Same quality, same reliability

- 20/20 turns successful in both versions
- Conversation memory works identically (correctly recalled first message after 20 turns)
- Error handling works identically (graceful reporting on missing files/paths)
- Code generation, factual answers, and tool usage are indistinguishable in quality

---

## How It Works

```
User message arrives
    ↓
[Rule-based classifier] → Trivial? Simple? Standard? Complex?
    ↓
Selects: prompt tier, tool loop (on/off), max iterations, output caps
    ↓
LLM processes with optimized configuration
    ↓
Same response, less overhead
```

The classifier uses pattern matching — greeting patterns, question structure, tool keywords, compound task markers. It runs in microseconds with zero token cost.

---

## Benchmark Details

Tested with OpenAI GPT-5.2, 20 turns covering:
- 3 trivial turns (greetings)
- 3 simple turns (factual questions)
- 7 single-tool turns (file ops, shell commands, code generation)
- 4 multi-step compound turns (create + verify, script + run + cleanup)
- 2 error handling turns (missing files/paths)
- 1 memory recall turn

Both versions ran in parallel with isolated workspaces, fresh state, identical prompts.

Full technical report: [Benchmark Report](TEMS_MIND_V2_BENCHMARK_TOOLS.md)
Terminal logs: [v1](benchmark_v2_tools_v1_log.txt) | [v2](benchmark_v2_tools_v2_log.txt)

---

## Configuration

v2 is opt-in. Add to your `temm1e.toml`:

```toml
[agent]
v2_optimizations = true
```

When `false` or absent, TEMM1E behaves exactly like v1. No migration needed, no breaking changes.

---

## What's Next

- Testing with premium models (Claude Opus, GPT-4o) where per-token savings are amplified
- Output cap optimization for tasks with large tool output (>10KB)
- Latency benchmarks — v2 should be noticeably faster on trivial/simple messages
- Complex tier testing with deep multi-file analysis tasks

---

*TEMM1E v2.0 — same intelligence, less waste.*
