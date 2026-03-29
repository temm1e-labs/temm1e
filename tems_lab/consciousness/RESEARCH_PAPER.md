# Tem Aware: Consciousness as a Separate Observer Sub-Agent for Agentic AI

> **Authors:** Quan Duong, Tem (TEMM1E Labs)
> **Date:** March 2026
> **Status:** Pre-implementation. Hypotheses only. No empirical evidence.
> **Branch:** `consciousness`

---

## Abstract

We propose Tem Aware, a metacognitive observer layer for TEMM1E — a consciousness sub-agent that watches the main agent's full internal state and selectively injects context to improve outcomes. Unlike self-correction approaches (Reflexion, Self-Refine) where the same model reflects on its own output, Tem Aware is a **separate entity** with its own memory, its own reasoning, and real-time visibility into internal state that the main agent cannot see from its own context window: classification confidence, budget consumption trajectory, tool retry patterns, cross-session behavioral trends, and context composition ratios.

We ground our architecture in Global Workspace Theory (Baars, 1988) and address the critical finding from Huang et al. (ICLR 2024) that intrinsic self-correction without external feedback degrades performance. Our key argument: Tem Aware succeeds where self-correction fails because the observer provides **structurally external feedback** — information derived from system-level instrumentation, not from re-prompting the same model.

We define a formal intervention framework with three levels (whisper, redirect, override), a cost model predicting net-negative token spend through waste prevention, and a 50-conversation A/B experiment protocol with 7 metrics and 4 success criteria. All claims are hypotheses. We will ship or kill based on data.

---

## 1. Introduction

### 1.1 The Blind-Forward Problem

Every agentic AI loop — including TEMM1E's — operates blind-forward. Each LLM call receives its immediate context (system prompt, conversation history, tool results) and produces its immediate output. No entity in the system watches the trajectory across calls. No entity notices when the agent has drifted from the user's intent over 8 turns. No entity remembers that the same tool failure was solved differently in a previous session.

This produces four documented failure modes in TEMM1E's production deployment:

**F1 — Retry loops.** The agent retries a failing tool approach 3-5 times before the circuit breaker terminates the loop. Each retry costs tokens and time. A trajectory-aware observer would detect the pattern after attempt 2 and suggest an alternative.

**F2 — Intent drift.** Over multi-turn complex tasks, the agent's focus gradually shifts from the user's original goal. Each turn is locally coherent, but the trajectory diverges. An observer holding the original intent in persistent memory would detect the drift.

**F3 — Missed cross-session knowledge.** The agent solved an identical problem in a previous session. The solution exists in λ-Memory, but the current turn's keywords don't trigger recall. An observer with its own memory of past solutions would bridge the gap.

**F4 — Unacknowledged risk.** The agent is about to execute a destructive or irreversible action without user confirmation. An observer evaluating action safety would intervene.

### 1.2 Why Self-Correction Doesn't Solve This

The most relevant literature on agent self-improvement establishes a critical constraint:

**Intrinsic self-correction (same model, no external feedback) does not improve and often degrades performance** (Huang et al., ICLR 2024; Kamoi et al., TACL 2024). The fundamental problem: "If an LLM possesses the ability to self-correct, why doesn't it simply offer the correct answer in its initial attempt?"

Self-Refine (Madaan et al., NeurIPS 2023) achieves ~20% improvement but uses the same model to critique itself — it works for stylistic refinement but not for reasoning errors. Reflexion (Shinn et al., NeurIPS 2023) achieves +22% on AlfWorld but reflects retrospectively (after failure), not proactively (before commitment).

**The key condition for successful self-correction: external feedback is available** (Kamoi et al., 2024). CRITIC (Gou et al., ICLR 2024) succeeds precisely because it uses external tools to verify outputs. Code agents succeed at self-correction because unit tests provide ground-truth feedback.

### 1.3 Our Thesis

Tem Aware provides **structurally external feedback** to the agent loop. The observer is a separate process with access to system-level instrumentation that the main agent cannot see from its context window:

| Information | Visible to main agent? | Visible to Tem Aware? |
|---|---|---|
| Conversation history | Yes | Yes |
| Tool call results | Yes | Yes |
| Classification confidence score | **No** | Yes |
| Budget consumption (% spent) | **No** | Yes |
| Consecutive tool retry count | **No** | Yes |
| Context composition (% system/tools/history/memory) | **No** | Yes |
| Cross-session patterns (same error seen before) | **No** (unless recalled) | Yes (own memory) |
| Circuit breaker state | **No** | Yes |
| Time elapsed on current task | **No** | Yes |

This is not the same model "thinking again." This is a separate entity reporting measurements from instrumentation the main agent has no access to. By the criteria of Huang et al. and Kamoi et al., this qualifies as external feedback.

### 1.4 Theoretical Grounding

**Global Workspace Theory (Baars, 1988):** Consciousness is a broadcast mechanism. Specialized modules (perception, language, motor) process information locally. Information becomes "conscious" when selected by attention and broadcast to all modules via a global workspace.

Mapping to TEMM1E:
- **Specialized modules** = classifier, context assembler, tool executor, budget tracker, memory system
- **Global workspace** = Tem Aware, with read access to all modules
- **Broadcast** = selective context injection back into the agent loop

Tem Aware is the global workspace. It doesn't do the thinking — the agent loop does. It selects the most salient cross-module information and makes it available where it's needed.

**Higher-Order Thought Theory:** A mental state is conscious when there is a higher-order representation of it. Tem Aware IS the higher-order process — it represents (observes) the agent's first-order cognitive states.

---

## 2. Related Work

### 2.1 Self-Reflection Systems

| System | Architecture | Observer | Timing | Key Result |
|--------|-------------|----------|--------|------------|
| Reflexion (Shinn, 2023) | Same model reflects on failure | Self (intrinsic) | Post-hoc (after failure) | +22% AlfWorld |
| Self-Refine (Madaan, 2023) | Same model critiques and revises | Self (intrinsic) | Post-generation | ~20% avg improvement |
| LATS (Zhou, 2024) | Tree search with LLM as value function | Self (3 roles) | During search | 92.7% HumanEval |
| CRITIC (Gou, 2024) | LLM + external tools verify | External (tools) | Post-generation | +7.7 F1 on QA |
| ExpeL (Zhao, 2024) | Extract insights from trial-and-error | Self + memory | Cross-episode | Learns from experience |
| Galaxy (Bao, 2025) | KoRa agent + Kernel meta-agent | Separate (Kernel) | Cross-task | Self-evolution |
| **Tem Aware (ours)** | **Separate sub-agent + system instrumentation** | **External (separate process)** | **Real-time, proactive** | **Hypothesis: +5% completion, net-negative cost** |

### 2.2 The Critical Gap

No existing system combines all four properties:

1. **Separate observer** (not the same model reflecting on itself)
2. **Internal-state visibility** (sees classification, budget, retry count — not just outputs)
3. **Proactive intervention** (injects before the agent commits, not after failure)
4. **Persistent observer memory** (the observer itself has λ-Memory across sessions)

Reflexion is retrospective. Self-Refine is intrinsic. CRITIC uses external tools but not internal state. CrewAI's manager sees outputs, not internals. Galaxy's Kernel is the closest — a meta-agent that enables self-evolution — but does not observe real-time internal state of the primary agent.

### 2.3 Anthropic's Introspection Research

Anthropic's "Emergent Introspective Awareness in LLMs" (October 2025) found that Claude models can detect artificially injected concepts in their own activations, but only ~20% of the time. This low introspective reliability argues FOR an external observer rather than relying on the model to monitor itself.

---

## 3. Architecture

### 3.1 Functional Definition

**Consciousness = a separate observer entity that can see the full internal machinations of a mind and has full control to alter its course.**

Three requirements:
1. **Separation.** The observer is a distinct process with its own memory and reasoning. Not a prompt prefix. Not a self-reflection step. A separate LLM call with separate context.
2. **Full visibility.** The observer sees every observable state in the agent loop: classification result, context composition, tool calls, tool results, budget state, retry count, circuit breaker state, memory recalls.
3. **Full control.** The observer can inject context (whisper), modify the conversation state (redirect), or block an action (override).

### 3.2 System Architecture

```
                         ┌──────────────────────────────────┐
                         │          TEM AWARE                │
                         │     (Consciousness Sub-Agent)     │
                         │                                    │
                         │  Own λ-Memory (observer identity)  │
                         │  Own reasoning (separate LLM call) │
                         │  Confidence threshold (≥ 0.7)      │
                         │                                    │
                         │  Input: TurnObservation struct      │
                         │  Output: Intervention | NoAction    │
                         └──────────────┬─────────────────────┘
                                        │
                            observes ↓  │  ↑ injects
                                        │
  ┌─────────────────────────────────────┼───────────────────────────────┐
  │                              THE MIND                               │
  │                        (Agent Runtime Loop)                         │
  │                                                                      │
  │  Message → Classify → Context Assembly → Provider.complete()         │
  │     → Tool Execution → Self-Correction → Response → Blueprint       │
  │                                                                      │
  │  Observable state at each step via status channel + instrumentation  │
  └──────────────────────────────────────────────────────────────────────┘
```

### 3.3 Observation Protocol

At the completion of each agent turn (after response delivery, before the next user message), Tem Aware receives a `TurnObservation`:

```rust
struct TurnObservation {
    // Turn metadata
    turn_number: u32,
    session_id: String,
    timestamp: i64,

    // User input
    user_message_preview: String,    // First 200 chars

    // Classification
    category: String,                // Chat, Order, Stop
    difficulty: String,              // Simple, Standard, Complex
    classification_confidence: f64,  // 0.0-1.0

    // Context composition
    context_tokens_total: u32,
    context_tokens_system: u32,
    context_tokens_tools: u32,
    context_tokens_history: u32,
    context_tokens_memory: u32,
    context_budget_remaining: u32,

    // Provider call
    model_used: String,
    input_tokens: u32,
    output_tokens: u32,
    cost_usd: f64,

    // Budget
    cumulative_cost_usd: f64,
    budget_limit_usd: f64,          // 0 = unlimited
    budget_percent_used: f64,

    // Tool execution
    tools_called: Vec<String>,
    tool_results: Vec<String>,      // "success" or error message
    consecutive_failures: u32,
    strategy_rotations: u32,

    // Memory
    memory_recalls: Vec<String>,    // Recalled memory IDs
    memory_recalls_useful: bool,    // Did the response use them?

    // Response
    response_preview: String,       // First 200 chars
    response_tokens: u32,

    // Circuit breaker
    circuit_breaker_state: String,  // Closed, Open, HalfOpen

    // Previous consciousness notes (from this session)
    previous_notes: Vec<String>,
}
```

### 3.4 Intervention Levels

**Level 1 — Whisper (inject context).**
Add a `{{consciousness}}` block to the system prompt of the next LLM call. The mind sees this as additional context. Ephemeral — not persisted in conversation history.

Example: *"Note from your awareness layer: the user's original request was to fix the login bug. The last 3 turns focused on file permissions. Consider whether this is the root cause or a tangent."*

**Level 2 — Redirect (modify context).**
Trigger a targeted λ-Memory recall and inject it into the next turn's context. Modify the context composition (e.g., increase memory allocation, decrease tool output allocation).

Example: *Inject memory recall: "In session 7, the same permission error was solved by checking the .env file path, not by changing permissions."*

**Level 3 — Override (block action).**
Block a planned tool call. Reserved for preventing irreversible harm.

Example: *Block `shell(rm -rf)` — "This destructive command was not explicitly requested by the user. Require confirmation."*

### 3.5 Decision Logic

```
observe(turn) → {
    // Check each intervention trigger
    if consecutive_failures > 1 AND same_tool_retried:
        WHISPER("Tool {X} has failed {N} times. Consider: {alternative}")

    if intent_drift_detected(turn, session_history):
        WHISPER("Original user intent: {X}. Current trajectory: {Y}. Realign?")

    if memory_match_found(turn, consciousness_memory):
        REDIRECT(inject_memory_recall(matched_memory))

    if destructive_action_without_confirmation(turn):
        OVERRIDE(block_tool_call, reason)

    if budget_percent_used > 80 AND task_not_near_completion:
        WHISPER("Budget at {X}%. Consider a more efficient approach.")

    if classification_confidence < 0.5:
        WHISPER("Classification was uncertain ({conf}). The task may be more complex than estimated.")

    // Default: no intervention
    return NO_ACTION
}
```

### 3.6 The Cheap Model Question

The blog post proposed using a cheap model (Haiku-class) for consciousness while the main agent uses the user's configured model. We rejected multi-model routing for Tem Gaze because grounding outputs are user-facing. Consciousness outputs are NOT user-facing — they're internal context injection. Using a cheap model for internal observation while keeping the expensive model for user interaction is architecturally clean.

**However:** We will start with the user's configured model for consciousness too. Reason: the consciousness sub-agent needs to understand the user's domain, the agent's behavior, and the conversation context. A model too weak to understand these will produce bad interventions. We'll test cheap-model consciousness as an optimization after proving the architecture works.

---

## 4. Cost Model

### 4.1 Cost Per Turn

**Observation cost:** 1 LLM call per turn with the TurnObservation as input (~500-800 tokens). At Sonnet 4.6 pricing ($3/M input, $15/M output): ~$0.003 per observation.

**Intervention cost:** When consciousness injects, the `{{consciousness}}` block adds ~100-200 tokens to the next LLM call: ~$0.0005 additional.

**Average per turn:** ~$0.003 (observation) + ~$0.0002 (intervention, amortized — most turns have no injection).

### 4.2 Cost Savings Hypothesis

| Prevented failure | Estimated savings per occurrence | Frequency (per 20-turn conversation) |
|---|---|---|
| 3 avoided retries | $0.03 | ~0.5 (every other conversation) |
| Intent drift correction | $0.05 (5 wasted turns) | ~0.3 |
| Memory recall shortcut | $0.08 (8 turns of rediscovery) | ~0.2 |
| Destructive action prevention | Incalculable (user trust) | ~0.05 |

**Estimated savings per conversation:** ~$0.03
**Estimated consciousness cost per conversation:** ~$0.06 (20 turns x $0.003)
**Net cost impact:** +$0.03 per conversation (consciousness costs more than it saves)

**Honest assessment:** Consciousness is NOT net-negative on cost in the typical case. It's a quality improvement that costs ~$0.03/conversation. The question is whether the quality improvement (fewer failures, better intent preservation, cross-session learning) justifies this cost.

For a $0.10-0.25 conversation, $0.03 is a 12-30% overhead. Significant but not prohibitive if outcomes measurably improve.

---

## 5. Experiment Protocol

### 5.1 Design

**A/B test.** 50 multi-turn conversations (10-20 turns each), each run twice: once without consciousness (baseline), once with consciousness (treatment). Same tasks, same provider (Gemini Flash), same model.

**Task categories:**
- 10 simple tasks (single-step, chat-like)
- 15 standard tasks (multi-step, tool use)
- 15 complex tasks (multi-tool, multi-step, requires planning)
- 10 adversarial tasks (designed to trigger failure modes: ambiguous intent, known tool failures, destructive actions)

### 5.2 Metrics

| Metric | Measurement | Success threshold |
|--------|------------|-------------------|
| Task completion rate | Human judge: did the agent accomplish the goal? | +5% improvement |
| Total token cost | Sum of all API calls including consciousness | No more than +30% increase |
| Retry count | Tool retries before success or failure | Reduction |
| Intent preservation | Human judge (1-5 scale): did the agent stay on track? | +0.5 point improvement |
| Memory utilization | Useful λ-Memory recalls per conversation | Increase |
| Intervention accuracy | Human judge: was each consciousness injection helpful? | ≥70% helpful |
| Latency per turn | Wall clock time, message to response | ≤3 second increase |

### 5.3 Success Criteria

ALL FOUR must be met:
1. Task completion rate improves by ≥5%
2. Total token cost increases by no more than 30%
3. Intervention accuracy ≥70%
4. Latency increase ≤3 seconds per turn

If all four: ship. If any fails: analyze, iterate, re-test. If all fail: kill.

---

## 6. Limitations and Risks

1. **Bad interventions.** The observer is an LLM. It can be wrong. A bad whisper could derail a working trajectory. Mitigation: confidence threshold, log-only mode for low-confidence observations.

2. **Latency.** +1-3 seconds per turn. Acceptable for messaging-first, noticeable for CLI. Mitigation: parallel execution — consciousness runs alongside the next user input wait.

3. **Context pollution.** Consciousness injections add tokens. Over long conversations, these accumulate. Mitigation: injections are ephemeral (next turn only), not persisted in history.

4. **The null result.** Consciousness might not help. The failure modes might be rare enough that interventions don't trigger often enough to matter. This is the most honest risk.

5. **Observer gaming.** If the main agent can detect consciousness injections, it might learn to game them. This is the CoT monitoring risk identified by OpenAI. Mitigation: consciousness uses the same voice as the system prompt, not a distinct persona.

---

## 7. References

### Metacognition and Self-Reflection

[1] Shinn, N. et al. "Reflexion: Language Agents with Verbal Reinforcement Learning." NeurIPS 2023. arXiv:2303.11366

[2] Madaan, A. et al. "Self-Refine: Iterative Refinement with Self-Feedback." NeurIPS 2023. arXiv:2303.17651

[3] Zhou, A. et al. "Language Agent Tree Search Unifies Reasoning, Acting, and Planning in Language Models." ICML 2024. arXiv:2310.04406

[4] Gou, Z. et al. "CRITIC: Large Language Models Can Self-Correct with Tool-Interactive Critiquing." ICLR 2024. arXiv:2305.11738

[5] Zhao, A. et al. "ExpeL: LLM Agents Are Experiential Learners." AAAI 2024. arXiv:2308.10144

[6] Park, J.S. et al. "Generative Agents: Interactive Simulacra of Human Behavior." UIST 2023. arXiv:2304.03442

[7] Sumers, T.R. et al. "Cognitive Architectures for Language Agents." TMLR 2024. arXiv:2309.02427

[8] Bao, Y. et al. "Galaxy: A Multi-Agent Framework for Self-Evolution." 2025. arXiv:2508.03991

### Self-Correction Limitations

[9] Huang, J. et al. "Large Language Models Cannot Self-Correct Reasoning Yet." ICLR 2024. arXiv:2310.01798

[10] Kamoi, R. et al. "When Can LLMs Actually Correct Their Own Mistakes?" TACL 2024.

[11] Song, K. et al. "Mind the Gap: Examining the Self-Improvement Capabilities of Large Language Models." ICLR 2025. arXiv:2412.02674

### Cognitive Science

[12] Baars, B.J. "A Cognitive Theory of Consciousness." Cambridge University Press, 1988. (Global Workspace Theory)

[13] Jian, Y. et al. "Truly Self-Improving Agents Require Intrinsic Metacognitive Learning." 2025. arXiv:2506.05109

### AI Safety and Introspection

[14] Bai, Y. et al. "Constitutional AI: Harmlessness from AI Feedback." Anthropic, 2022. arXiv:2212.08073

[15] Anthropic. "Emergent Introspective Awareness in Large Language Models." October 2025. transformer-circuits.pub

[16] OpenAI. "CoT Monitoring: Reasoning Model Safety." 2025.

### Agent Architectures

[17] Huang, W. et al. "Inner Monologue: Embodied Reasoning through Planning with Language Models." CoRL 2022. arXiv:2207.05608

[18] Irving, G. et al. "AI Safety via Debate." 2018. arXiv:1805.00899
