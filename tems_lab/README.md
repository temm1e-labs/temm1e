# Tem's Lab

Where TEMM1E's cognitive systems are researched, built, and proven.

Every feature in Tem's Mind starts here — as a theory, gets stress-tested against real models and real conversations, and only ships when the data says it works. No vaporware. Every claim has a benchmark behind it.

---

## λ-Memory

**Memory that fades, not disappears.**

Current AI agents delete old messages or summarize them into oblivion. Both permanently lose information. λ-Memory takes a different approach: memories decay through an exponential function (`score = importance × e^(−λt)`) but never truly disappear. The agent sees old memories at progressively lower fidelity — full text → summary → essence → hash — and can recall any memory by hash to restore full detail.

Three features no other system has ([competitive analysis](LAMBDA_MEMORY_RESEARCH.md)):
- **Hash-based recall** from compressed memory — the agent sees the shape of what it forgot and can pull it back
- **Dynamic skull budgeting** — same algorithm adapts from 16K to 2M context windows without overflow
- **Pre-computed fidelity layers** — full/summary/essence written once at creation, selected at read time by decay score

### Benchmarked: 1,200+ API calls across GPT-5.2 and Gemini Flash

| Test | λ-Memory | Echo Memory | Naive Summary |
|------|----------|-------------|---------------|
| [Single-session recall](LAMBDA_BENCH_GPT52_REPORT.md) (GPT-5.2) | 81.0% | **86.0%** | 65.0% |
| [Multi-session recall](LAMBDA_BENCH_MULTISESSION_REPORT.md) (GPT-5.2, 5 sessions) | **95.0%** | 58.8% | 23.8% |
| [Cross-model](LAMBDA_BENCH_REPORT.md) (Gemini Flash) | 67.0% | 76.0% | 48.5% |

Echo Memory (keyword search over recent context) wins when everything fits in one session. The moment context resets between sessions — which is how real users interact with agents — λ-Memory achieves **95% recall where Echo drops to 59%** and naive summarization collapses to 24%.

Naive rolling summarization is the worst strategy in every test. It destroys information from early turns as new summaries overwrite old ones. [Full data](LAMBDA_EFFECTIVENESS_REPORT.md).

### Documents

| | |
|---|---|
| [Research Paper](LAMBDA_RESEARCH_PAPER.md) | The complete story — problem, architecture, 1,200+ API calls of benchmarks, per-question scoring, cross-model analysis, conclusions |
| [Design Doc](LAMBDA_MEMORY.md) | Decay function, skull model, dynamic budget, adaptive pressure thresholds, full dry run with exact numbers |
| [Competitive Research](LAMBDA_MEMORY_RESEARCH.md) | Landscape review of Letta/MemGPT, Mem0, Zep, FadeMem, Kore, LangMem, CrewAI — what exists, what's novel, honest gaps |
| [Implementation Guide](LAMBDA_MEMORY_IMPLEMENTATION.md) | Every file, function, SQL statement — the bone for implementation |
| [Multi-Session Benchmark](LAMBDA_BENCH_MULTISESSION_REPORT.md) | 5 sessions over 7 simulated days, context reset between each, 20-question recall exam |
| [GPT-5.2 Single-Session](LAMBDA_BENCH_GPT52_REPORT.md) | 100 turns, 3 strategies, per-question breakdown, cross-model comparison |
| [Final Report](LAMBDA_FINAL_REPORT.md) | Consolidated analysis across all runs with recommendations |

**Status:** Implemented in Rust. 1,509 tests pass. Ships with `/memory` command for hot-switching between λ-Memory and Echo Memory mid-conversation.

---

## Tem's Mind v2.0

**The agentic loop that knows what kind of task you're asking before it starts working.**

v1 treats every message the same — full system prompt, full tool pipeline, same iteration limits whether you say "thanks" or ask to debug a codebase. v2 classifies each message into a complexity tier (Trivial → Simple → Standard → Complex) **before** calling the LLM, using zero-cost rule-based heuristics.

### Benchmarked: 30 turns across Gemini Flash and GPT-5.2

| Metric | v1 | v2 | Delta |
|--------|----|----|-------|
| [Cost per successful turn](TEMS_MIND_V2_BENCHMARK.md) (Gemini Flash) | $0.00159 | $0.00145 | **-9.3%** |
| [Compound task savings](TEMS_MIND_V2_BENCHMARK_TOOLS.md) (GPT-5.2) | baseline | -12.2% | **-12.2%** |
| Tool call failures | 2 turns | 1 turn | **-50%** |
| Classification accuracy | N/A | 100% (30/30) | Zero LLM overhead |

The savings come from compound multi-step tasks where fewer API rounds mean less cumulative context. Trivial turns (greetings, acknowledgments) skip the tool pipeline entirely.

### Documents

| | |
|---|---|
| [Architecture](TEMS_MIND_ARCHITECTURE.md) | Full component map — runtime, context manager, executor, self-correction, circuit breaker, 15 subsystems |
| [Implementation Plan](TEMS_MIND_V2_PLAN.md) | Token optimization architecture, resilience design, blueprint system, prompt stratification |
| [Benchmark: Gemini Flash](TEMS_MIND_V2_BENCHMARK.md) | 10-turn A/B — 9.3% cost reduction, 50% fewer provider errors |
| [Benchmark: GPT-5.2](TEMS_MIND_V2_BENCHMARK_TOOLS.md) | 20-turn tool-heavy A/B — 4.8% total, 12.2% on compound, 100% classification accuracy |
| [Experiment Insights](TEMS_MIND_V2_EXPERIMENT_INSIGHTS.md) | Where v2 saves, where it doesn't, what the 4.8% number undersells |
| [Release Notes](TEMS_MIND_V2_RELEASE.md) | User-facing changelog |

**Status:** Shipped. Running in production.

---

## Eigen-Tune (Self-Tuning)

**An AI agent that fine-tunes itself. Zero added LLM cost.**

Every LLM call is a training example. Eigen-Tune captures them, scores quality from user behavior, curates datasets, trains local models, and graduates them through statistically rigorous gates — all with zero user intervention beyond `/eigentune on` and zero added LLM cost.

The bet: open-source models will only get better. Our job is to have the best domain-specific training data ready when they do. **The data is the moat. The model is a commodity.**

### Architecture

A 7-stage closed-loop pipeline with per-tier state machines governed by statistical tests. **Default: $0 added LLM cost.** Optional Teacher Mode for users who want to pay for stronger guarantees.

| Stage | Method | Cost |
|-------|--------|------|
| Collect | Fire-and-forget hook on every provider call | $0 |
| Score | Beta-Binomial model from user behavior signals | $0 |
| Curate | Shannon entropy + Thompson sampling | $0 |
| Train | QLoRA via Unsloth/MLX → GGUF → Ollama | $0 (local compute) |
| Evaluate | Embedding similarity (local) + Wilson score (99% CI) | $0 |
| Shadow | User behavior → SPRT (Wald, 1945) | $0 |
| Monitor | User behavior → CUSUM (Page, 1954) | $0 |
| *Teacher (opt-in)* | *LLM-as-judge with position debiasing* | *LLM API cost* |

Each complexity tier (Simple, Standard, Complex) graduates independently. Simple first, complex last. Cloud is always the fallback. The user IS the judge — their behavior (continue, retry, reject) drives graduation decisions.

### Benchmarked: Real fine-tuning on Apple M2, 16GB RAM

| Metric | Result |
|--------|--------|
| Data collected | 10 conversations, 3 tiers, 8 domains |
| Training loss | 2.450 → 1.242 (49% reduction) |
| Peak memory | 0.509 GB training, 0.303 GB inference |
| Speed | ~28 it/sec training, ~200 tok/sec inference |
| Base model 72°F | "150°C" (wrong) |
| **Fine-tuned 72°F** | **"21.2°C" (close to 22.2°C)** |
| Statistical tests | 128 tests, all passing |

The base model made a fundamental arithmetic error. Ten training examples fixed it. This is knowledge distillation working at consumer scale. [Research paper →](eigen/RESEARCH_PAPER.md) · [Full pipeline log →](eigen/PIPELINE_PROOF_LOG.txt)

### Documents

| | |
|---|---|
| [Research Paper](eigen/RESEARCH_PAPER.md) | Full paper: architecture, math, real M2 results, economics, limitations |
| [Design Doc](eigen/DESIGN.md) | Formal state machine, mathematical formulas (SPRT, CUSUM, Wilson, Beta-Binomial, Shannon entropy), zero-cost evaluation architecture, data model, risk assessment |
| [Implementation Plan](eigen/IMPLEMENTATION.md) | Phase-by-phase build guide — every struct, function, file, and test |
| [Technical Reference](eigen/TECHNICAL_REFERENCE.md) | Ollama API endpoints, training scripts (Unsloth + MLX), embedding judge, two-tier behavior detection, ChatML format, codebase hook locations |
| [Pipeline Proof Log](eigen/PIPELINE_PROOF_LOG.txt) | Unedited output: data → training → inference on Apple M2, 2026-03-18 |
| [Setup Guide](eigen/SETUP.md) | User-facing: install Ollama + MLX/Unsloth, enable, choose model, troubleshooting |

**Status:** Implemented and proven. 136 tests, real fine-tuning on M2.

---

## Research Philosophy

Every system in Tem's Lab follows the same process:

1. **Theory** — what's the problem, what's the hypothesis
2. **Landscape** — what exists, what's been tried, what failed
3. **Design** — architecture with exact math, not hand-waving
4. **Implement** — in Rust, in the actual codebase, not a prototype
5. **Benchmark** — against alternatives, on real models, with scoring rubrics
6. **Ship or kill** — if the data says it doesn't work, it doesn't ship

No feature ships without a benchmark. No benchmark ships without a scoring rubric. No claim is made without data behind it.

---

*TEMM1E's Lab — where Tem's mind is built*
