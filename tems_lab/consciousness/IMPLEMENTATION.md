# Tem Aware: Implementation Plan

**Date:** 2026-03-29
**Research:** [RESEARCH_PAPER.md](RESEARCH_PAPER.md)
**Branch:** `consciousness`

---

## Overview

Tem Aware is implemented as an additive observer that hooks into the existing agent runtime via the status channel + a new observation struct. No existing runtime logic is modified — consciousness is a new module that reads state and optionally writes context.

---

## Phase 1: Core Observer

### 1.1 TurnObservation Struct

**New file:** `crates/temm1e-agent/src/awareness.rs`

Define the `TurnObservation` struct as specified in the research paper Section 3.3. This is a pure data struct — no logic, just fields collected from existing runtime state.

Also define:
- `ConsciousnessIntervention` enum: `NoAction`, `Whisper(String)`, `Redirect { memory_query: String }`, `Override { block_tool: String, reason: String }`
- `AwarenessConfig` — enabled flag, confidence threshold, max interventions per conversation, observation model override

### 1.2 Observation Collector

**New function in** `crates/temm1e-agent/src/awareness.rs`:

`collect_observation(runtime_state, turn_number, session_id) -> TurnObservation`

Collects data from:
- `AgentTaskStatus` (from status channel) — phase, tokens, cost
- `BudgetTracker` — cumulative spend, budget remaining
- Classification result — category, difficulty (captured during process_message)
- Tool execution results — tool names, success/failure, retry count
- Context metrics — token counts per section
- Memory recalls — IDs of recalled memories
- Circuit breaker state

All of this data already exists in the runtime. The collector just reads it into one struct.

### 1.3 Awareness Engine

**New file:** `crates/temm1e-agent/src/awareness_engine.rs`

The core logic:

```rust
pub struct AwarenessEngine {
    config: AwarenessConfig,
    memory: Arc<dyn Memory>,        // Consciousness's own λ-Memory
    provider: Arc<dyn Provider>,    // LLM for consciousness reasoning
    session_notes: Vec<String>,     // Notes from this session
    intervention_count: u32,        // Count this session
}

impl AwarenessEngine {
    pub async fn observe(
        &mut self,
        observation: &TurnObservation,
    ) -> ConsciousnessIntervention {
        // 1. Check rule-based triggers first (no LLM call needed)
        if let Some(intervention) = self.check_rules(observation) {
            return intervention;
        }

        // 2. If rules don't trigger, check if LLM observation is warranted
        if !self.should_observe_deeply(observation) {
            return ConsciousnessIntervention::NoAction;
        }

        // 3. Deep observation: call LLM with turn observation
        let prompt = self.build_observation_prompt(observation);
        let response = self.provider.complete(prompt).await;
        self.parse_intervention(response)
    }
}
```

**Rule-based triggers (no LLM cost):**
- `consecutive_failures > 2` → Whisper about alternative approach
- `budget_percent_used > 80` → Whisper about efficiency
- Destructive tool name detected (`rm`, `delete`, `drop`) without confirmation context → Override

**Deep observation triggers (requires LLM call):**
- Classification confidence < 0.6
- Turn number > 5 (multi-turn conversation, worth observing trajectory)
- Tool failure occurred this turn
- No rule-based trigger but consciousness memory has relevant notes

This two-tier system means most turns are evaluated by rules only ($0 cost). Only complex situations trigger the LLM call.

### 1.4 Integration Point in Runtime

**Modified:** `crates/temm1e-agent/src/runtime.rs`

After `process_message()` completes and before returning the response, call the awareness engine:

```rust
// At the end of process_message(), after response is ready:
if let Some(awareness) = &mut self.awareness {
    let observation = awareness::collect_observation(
        &self, turn_number, &session_id
    );
    let intervention = awareness.observe(&observation).await;

    match intervention {
        ConsciousnessIntervention::Whisper(note) => {
            // Store note — will be injected into next turn's system prompt
            self.consciousness_note = Some(note);
        }
        ConsciousnessIntervention::Redirect { memory_query } => {
            // Trigger targeted memory recall for next turn
            self.consciousness_memory_recall = Some(memory_query);
        }
        ConsciousnessIntervention::Override { block_tool, reason } => {
            // Block the tool call (only applies to next turn)
            self.consciousness_block = Some((block_tool, reason));
        }
        ConsciousnessIntervention::NoAction => {}
    }
}
```

**Injection into next turn:** At the start of the NEXT `process_message()`, if `self.consciousness_note` is set, prepend it to the system prompt:

```
{{consciousness}}
[Note from your awareness layer — consider this context for your response]
{note_content}
{{/consciousness}}
```

This is ephemeral — the note is consumed and cleared after one use.

---

## Phase 2: Config and Wiring

### 2.1 AwarenessConfig

**Modified:** `crates/temm1e-core/src/types/config.rs`

```toml
[awareness]
enabled = false                    # Off by default
confidence_threshold = 0.7        # Only inject on high confidence
max_interventions_per_session = 10 # Prevent runaway injection
observe_model = ""                 # Empty = use user's model
observation_mode = "rules_first"   # "rules_first", "always_llm", "rules_only"
```

### 2.2 Wire into AgentRuntime

**Modified:** `crates/temm1e-agent/src/runtime.rs`

Add optional `AwarenessEngine` field to `AgentRuntime`. Initialize from config during runtime construction. Feature-gated: `#[cfg(feature = "awareness")]` — but since it's purely additive and off-by-default, we can include it without a feature gate.

### 2.3 Wire into main.rs

**Modified:** `src/main.rs`

When constructing the agent runtime, if `config.awareness.enabled`, create an `AwarenessEngine` with:
- Its own λ-Memory instance (separate SQLite table or separate DB file)
- A provider instance (same as user's, or override model if configured)
- Config parameters

---

## Phase 3: Testing

### 3.1 Unit Tests

| Test | What it verifies |
|------|-----------------|
| `test_collect_observation` | TurnObservation correctly populated from mock runtime state |
| `test_rule_retry_trigger` | consecutive_failures > 2 produces Whisper |
| `test_rule_budget_trigger` | budget > 80% produces Whisper |
| `test_rule_destructive_trigger` | rm/delete tool produces Override |
| `test_no_action_default` | Normal turn produces NoAction |
| `test_whisper_injection` | consciousness_note is prepended to next system prompt |
| `test_whisper_ephemeral` | consciousness_note is cleared after one use |
| `test_config_defaults` | AwarenessConfig defaults are sensible |
| `test_max_interventions` | Stops injecting after max_interventions_per_session |

### 3.2 Live A/B Test (Post-Implementation)

As specified in research paper Section 5. 50 conversations, 7 metrics, 4 success criteria.

---

## File Change Summary

### New Files

| File | Description |
|------|-------------|
| `crates/temm1e-agent/src/awareness.rs` | TurnObservation struct, collect_observation(), intervention types |
| `crates/temm1e-agent/src/awareness_engine.rs` | AwarenessEngine, rule-based + LLM observation, intervention logic |

### Modified Files

| File | Change |
|------|--------|
| `crates/temm1e-agent/src/runtime.rs` | Add optional AwarenessEngine, call observe() after process_message, inject consciousness notes |
| `crates/temm1e-agent/src/lib.rs` | Add `pub mod awareness; pub mod awareness_engine;` |
| `crates/temm1e-core/src/types/config.rs` | Add AwarenessConfig struct |
| `src/main.rs` | Initialize AwarenessEngine when config.awareness.enabled |

### No New Crates

Unlike Tem Gaze (which needed a new crate for desktop-specific deps), Tem Aware lives entirely within `temm1e-agent`. It uses existing dependencies: `temm1e-core` (traits, types), the Provider trait (for LLM calls), and the Memory trait (for consciousness's own λ-Memory). No new external dependencies.

---

## Dependencies

Zero new dependencies. Tem Aware uses:
- `temm1e-core::Provider` — for consciousness LLM calls
- `temm1e-core::Memory` — for consciousness's own λ-Memory
- `tokio` — for async
- `serde` / `serde_json` — for observation serialization
- `tracing` — for logging

All already in the workspace.

---

## Risk Mitigation

| Risk | Mitigation |
|------|-----------|
| Bad intervention derails agent | Confidence threshold (0.7), max_interventions limit (10/session) |
| Latency per turn | Consciousness runs AFTER response delivery, injects into NEXT turn (not current) |
| Token cost | Rule-based triggers are free; LLM observation only on complex turns |
| Context pollution | Whispers are ephemeral — consumed after one turn, not in history |
| Feature breaks existing behavior | Off by default. When disabled, zero code paths change. |
