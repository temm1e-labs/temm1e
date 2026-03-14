# Cognitive Architecture — The Finite Brain Model

## The Central Thesis

An LLM is a thinking brain with a fixed-size skull.

The context window is not a buffer. It is not a queue. It is not a log file you keep appending to until something breaks. It is **working memory** — the total cognitive capacity available to the intelligence at any given moment. Every token consumed is a neuron recruited. Every token wasted is a thought the brain can no longer have.

Most agent frameworks treat context as an implementation detail. They stuff history in until it overflows, then truncate from the front or summarize into mush. TEMM1E treats context as the **primary architectural constraint** — the one that shapes every other decision in the system.

## The Problem With Fuzzy Summarization

The standard approach to long-running agents is summarization. When context grows too large, compress the history into a summary. This is catastrophically wrong for procedural tasks.

Consider what happens when an agent successfully deploys an application to production — a 25-step procedure involving Docker builds, registry pushes, SSH connections, config file edits, service restarts, and health checks. The agent executed this perfectly. What does summarization preserve?

> "Previously deployed the application to production using Docker and SSH."

This is useless. It's a newspaper headline about surgery written by someone who's never held a scalpel. The agent that reads this summary will repeat every mistake, re-discover every dead end, and re-invent every workaround that the original execution already solved. The knowledge was there. The system threw it away.

Summarization destroys the three things that matter most for procedural replay:
1. **Ordering** — which step comes before which, and why
2. **Decision points** — where choices were made and what informed them
3. **Failure modes** — what went wrong, how it was detected, how it was recovered

These are exactly the structures that make the difference between an agent that stumbles through a task and one that executes with surgical precision.

## Blueprints: Concrete Procedural Memory

TEMM1E's answer is the Blueprint system — structured, replayable procedure documents that capture the full execution graph, not a lossy summary of it.

A Blueprint is not a description of what happened. It is a **recipe for what to do**. The distinction matters:

| Summarization | Blueprint |
|---------------|-----------|
| "Deployed the app using Docker" | Phase 1: Build → `docker build -t app:v2 .` with Dockerfile at `/deploy/Dockerfile`. Phase 2: Push → `docker push registry.io/app:v2`, verify with `docker manifest inspect`. Phase 3: Deploy → SSH to `prod-01`, `docker pull`, `docker-compose up -d`, verify health at `/health` returns 200 within 30s. |
| Loses structure after compression | Preserves exact sequence, commands, verification steps |
| Agent must re-derive the procedure | Agent follows the procedure directly |
| Each execution is exploration | Each execution is replication |

### The Cognitive Stack

TEMM1E's memory is not one system — it's four layers, each serving a different cognitive function:

```
Skills      → what the agent CAN do        (capabilities)
Blueprints  → what the agent KNOWS HOW     (procedures)
Learnings   → what the agent NOTICED       (signals)
Memory      → what the agent REMEMBERS     (facts)
```

**Skills** are tool definitions — the agent's hands. They define what actions are possible.

**Blueprints** are procedural memory — the agent's muscle memory. They define how to combine actions into effective sequences. A Blueprint doesn't just say "use the shell tool." It says "run `docker build` first, check the exit code, if it fails check for missing dependencies in the Dockerfile, then push to the registry and verify the manifest before proceeding to deployment."

**Learnings** are ambient signals — the agent's intuition. "Last time we deployed on Friday, the CDN cache took 2 hours to clear." These are breadcrumbs, not procedures. They inform decisions within a procedure but don't define the procedure itself.

**Memory** is factual recall — the agent's notebook. API keys, user preferences, project configuration. Raw facts, no procedure attached.

These layers are complementary. A Blueprint uses Skills (tools). It's informed by Learnings (signals from past executions). It references Memory (credentials, endpoints). But the Blueprint itself is the **executable plan** — the thing that turns capability into competence.

### Why Blueprints Self-Heal

Blueprints are not static documents. They evolve through a CRUD refinement loop:

1. **Create** — After a complex task, the agent authors a Blueprint capturing the procedure.
2. **Match** — On a similar future task, the Blueprint is loaded into context.
3. **Execute** — The agent follows the Blueprint, adapting as needed.
4. **Refine** — After execution, the Blueprint is updated with what changed. New failure modes are added. Timing is recalibrated. Steps that were modified are rewritten.

This means stale Blueprints fix themselves. A deployment procedure that was correct six months ago but now requires an extra migration step will fail on the first post-change execution. The agent adapts, completes the task, and the Blueprint is refined to include the new step. The next execution succeeds without adaptation.

This is how human expertise works. A surgeon doesn't re-learn appendectomy from first principles every time. They follow a practiced procedure and refine it based on new cases. The procedure is concrete and actionable. It's not a summary of "things I've done with scalpels."

## The Finite Brain Constraint

### Every Resource Declares Its Cost

In TEMM1E, nothing enters the context window without a price tag. Every Blueprint, every tool definition, every memory entry, every learning — all carry pre-computed token counts stored in their metadata. This is computed once at authoring time, not estimated at injection time.

This is the metabolic cost model. In biology, every organ, every thought, every movement costs energy. The body tracks these costs and allocates resources accordingly. An agent must do the same with tokens.

### The Brain Sees Its Own Budget

Every context rebuild injects a **Resource Budget Dashboard** into the system prompt:

```
=== CONTEXT BUDGET ===
Model: claude-sonnet-4-6 | Limit: 200,000 tokens
Used: 34,200 tokens
  System:     2,100
  Tools:      3,400
  Blueprint:  1,247
  Memory:     1,200
  Learnings:    450
  History:   25,803
Available: 165,800 tokens
Blueprint budget: 18,753 / 20,000 remaining
=== END BUDGET ===
```

The LLM sees exactly how much context it has consumed and how much remains. This is not decorative. It enables the intelligence to make **resource-aware decisions**:

- "I have 165K tokens remaining — I can afford to load a detailed Blueprint for this task."
- "I'm down to 20K available — I should be concise in my tool outputs and skip the optional verification step."
- "The Blueprint I need is 50K tokens — that's too large, I'll work from the outline instead."

A brain that doesn't know the size of its own skull will keep trying to think bigger thoughts until it crashes. A brain that sees its limits allocates wisely.

### Graceful Degradation Over Failure

When a Blueprint is too large for the context budget, TEMM1E doesn't crash. It doesn't silently overflow. It degrades gracefully through a three-tier system:

| Scenario | Action |
|----------|--------|
| Blueprint fits in 10% of budget | Inject full body — the agent gets the complete procedure |
| Blueprint > 10% but < 25% of context | Inject outline — objective + phase headers only. Agent knows the structure but must fill in details. |
| Blueprint > 25% of total context | Reject body entirely. Show catalog only — name, description, token cost. Agent works without procedural guidance. |

This is the same principle as biological triage. A brain under resource pressure doesn't attempt everything and fail catastrophically. It drops the least critical functions first, preserving core capability. Peripheral vision goes before central vision. Color perception goes before shape recognition. The system degrades gracefully rather than failing completely.

## Zero Extra LLM Calls — Upstream Feeds Downstream

### The Cost of Intelligence

Every LLM call costs time and money. More importantly, every LLM call is a **latency checkpoint** in the agent loop. A user sends a message. The system classifies it. Then it searches for a matching Blueprint. Then it builds context. Then it executes. Each LLM call in this chain adds 1-3 seconds of latency.

The naive approach to Blueprint matching is a dedicated LLM call: "Here are 5 Blueprints. Which one best matches this task?" This adds latency, cost, and another failure point.

TEMM1E's v2 matching architecture eliminates this call entirely by leveraging **upstream information to serve downstream decisions**.

### The Classifier Hint

The message classifier already runs on every inbound message. It determines whether the message is chat, an actionable order, or a stop request. This LLM call is unavoidable — it's the gateway to the agent loop.

Adding one field to the classifier's JSON response costs approximately zero additional tokens:

```json
{
  "category": "order",
  "chat_text": "On it, scanning the directory now.",
  "difficulty": "standard",
  "blueprint_hint": "filesystem"
}
```

The `blueprint_hint` field is the classifier's opinion about which category of Blueprint might be relevant. It picks from the **grounded set** — the actual categories stored in the database, not invented ones.

This single field, added to an existing LLM call, replaces an entire dedicated matching call. The downstream Blueprint fetcher uses it to query by category. No extra LLM call. No extra latency. No extra cost.

### Grounded Vocabularies

A subtle but critical design decision: the classifier must pick from categories that **actually exist** in the database.

Before the classifier runs, the system executes a simple metadata query:

```sql
SELECT DISTINCT semantic_tags FROM memory_entries WHERE entry_type = 'blueprint'
```

This returns the actual stored categories: `["filesystem", "deployment", "web-scraping"]`. The classifier prompt includes these as the allowed set:

```
Pick from ["filesystem", "deployment", "web-scraping"] or omit the field.
```

This prevents the **free-form matching problem**: two LLM calls producing independent free-form strings that need to agree. If the classifier invents a category "file-ops" but the stored Blueprint uses "filesystem", the match fails silently.

Grounded vocabularies ensure that the classifier's output is always a valid key into the Blueprint store. The system can only recommend what it actually has. Hallucinated categories are impossible by construction.

### The Information Cascade

The full flow, with zero extra LLM calls for Blueprint matching:

```
1. User message arrives

2. Pre-classifier: SQL query for distinct semantic_tags
   → ["filesystem", "deployment", "reporting"]
   Cost: ~1ms database query

3. Classifier (existing LLM call, +1 field):
   Input: user message + history + "pick from [filesystem, ...] or null"
   Output: { category: "order", blueprint_hint: "filesystem", ... }
   Cost: ~0 extra tokens

4. Blueprint fetch by category:
   SELECT FROM memory WHERE semantic_tags CONTAINS "filesystem"
   → Returns N blueprints with pre-computed token counts
   Cost: ~1ms database query

5. Context builder with budget enforcement:
   - Injects Resource Budget Dashboard
   - Shows compact catalog (name + description + token cost)
   - Auto-loads best blueprint body if it fits within 10% budget
   - Falls back to outline if too large
   Cost: computed at build time, no LLM call

6. Main LLM executes (existing tool loop):
   - Sees budget dashboard, knows its limits
   - Sees blueprint body/outline, follows the procedure
   - No extra calls — this was always going to happen
```

Steps 2 and 4 are database queries (~1ms each). Step 3 adds ~20 tokens to an existing LLM call. Steps 5 and 6 are computation and an existing LLM call. Total extra cost for Blueprint matching: approximately 2ms and 20 tokens.

Compare this to v1's approach of a dedicated matching LLM call: 1-3 seconds, 2,000+ tokens, an additional failure point, and the free-form matching problem. The v2 architecture is faster, cheaper, more reliable, and more correct.

## Design Principles — Summary

### 1. The LLM Is a Finite Brain

The context window is working memory. It has a hard limit. Every injection must be measured, budgeted, and justified. The brain must see its own resource consumption. Silent overflow is a system failure.

### 2. Procedures Over Summaries

Summarization destroys procedural structure. Blueprints preserve it. A summary tells you what happened. A Blueprint tells you what to do. For an agent that needs to replicate procedures, the Blueprint is the only representation that matters.

### 3. Self-Healing Through Use

Blueprints are refined after every execution. Stale procedures fix themselves. Wrong steps get corrected. New failure modes get captured. The system gets better with use, not worse with time.

### 4. Upstream Feeds Downstream

Never make an extra LLM call when an upstream call can carry the information. Add a field to an existing JSON response. Use grounded vocabularies from the database. Design the data flow so each stage enriches the next without additional intelligence calls.

### 5. Grounded Vocabularies

When two stages need to agree on a value, one stage must be constrained to pick from values the other stage actually has. Never let two independent LLM calls produce free-form strings that need to match. Ground the vocabulary in the actual data store.

### 6. Graceful Degradation

Truncate before reject. Reject before crash. Show an outline when the body is too large. Show a catalog when the outline is too large. Fall back to rule-based classification when the LLM classifier fails. The system should always do the best it can with the resources it has — never fail catastrophically because one subsystem hits a limit.

### 7. Measure Once, Use Forever

Token costs are computed at authoring time and stored in metadata. Budget dashboards are built from pre-computed values. No estimation happens at request time for pre-existing resources. The metabolic cost of every resource is known before it enters the context.

---

*This document describes the architectural philosophy behind TEMM1E's Blueprint system and context management. For implementation details, see `BLUEPRINT_SYSTEM.md` (vision), `BLUEPRINT_IMPLEMENTATION.md` (step-by-step plan), and `BLUEPRINT_MATCHING_V2.md` (matching architecture).*
