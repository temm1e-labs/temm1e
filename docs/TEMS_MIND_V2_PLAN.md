# TEMM1E Tem's Mind v2.0 — Implementation Plan

**Author:** Tung + Claude (harmonized from RFC + codebase analysis + Claude Code ecosystem research)
**Date:** 2026-03-10
**Status:** Approved Plan — No Code Yet
**Scope:** `temm1e-agent`, `temm1e-core`, `temm1e-tools`, `temm1e-memory`, `temm1e-skills`, `temm1e-channels`

---

## Two Non-Negotiable Goals

### Goal 1: Token Efficiency Without Losing Capability or Quality

Every optimization must pass this test: "Does the user notice any degradation?" If yes, the optimization is rejected. We reduce waste (tokens spent on overhead, re-sent history, bloated prompts), never reduce capability.

### Goal 2: Resilience — Zero Dead-Ends, Zero Silent Failures

The Tem's Mind must be self-sustaining. Every failure path must have a recovery path. No operation can leave the system in an unrecoverable state. If something fails, the user gets told, the system recovers, and the next message works. This extends TEMM1E's existing 4-layer resilience architecture (source elimination, catch_unwind, dead worker detection, global panic hook) into the agentic decision layer.

---

## Part 1: Token Optimization Architecture

### What Already Works (Do Not Replace)

These existing systems are battle-tested in production. The optimizations build ON TOP of them, never replace them:

| System | Location | Status |
|---|---|---|
| Priority-based context budgeting | `context.rs:68-382` | Keep as-is |
| Tool output compression (heuristic) | `output_compression.rs` | Enhance, don't replace |
| Zero-cost verification prompt injection | `runtime.rs:638-652` | Keep as-is |
| FailureTracker + strategy rotation | `runtime.rs:575-595` | Enhance with structured types |
| Memory budget allocation (15%/5%/2000) | `context.rs:124-260` | Keep as-is, revisit at scale |
| Rule-based complexity classification | `model_router.rs:55-198` | Extend with Trivial tier |
| Compound task detection | `done_criteria.rs` | Keep as-is |
| Budget enforcement | `budget.rs` | Keep as-is |

### Optimization 1: Trivial Fast-Path (Extend Existing Classification)

**Problem:** "hi", "thanks", "what can you do?" run through the full tool loop.

**Solution:** Add a `Trivial` tier to the existing `model_router.rs` classifier. Short-circuit before the tool loop.

**Classification rules (rule-based, zero LLM cost):**

```
Trivial if ALL of:
  - Message < 50 chars
  - No action verbs (find, create, run, deploy, read, write, search, build, fix, etc.)
  - No file paths, URLs, or code blocks
  - No tool-trigger keywords (shell, browser, file, fetch, etc.)
  - History depth < 3 turns OR message matches greeting/farewell patterns
```

**Runtime behavior:**
- Build Minimal system prompt (identity + safety only, ~300 tokens)
- Single provider.complete() call
- Skip LEARN phase entirely
- Skip tool loop entirely
- Return response directly

**Resilience rule:** If the provider response contains tool_use blocks despite Trivial classification, escalate to Simple and re-enter the normal loop. Never drop tool calls.

**What we explicitly reject from the RFC:**
- LLM-based classification (wasteful, adds latency)
- 5-tier system (CRITICAL is not meaningfully different from COMPLEX)
- Two-phase THINK (extra API call for Standard+ is worse than one slightly larger call)
- Parsing `[COMPLEXITY: X]` tags from LLM output (fragile, model-dependent)

**Final classification tiers:** Trivial, Simple, Standard, Complex (4 tiers, extending existing 3)

### Optimization 2: System Prompt Stratification

**Problem:** Full system prompt (~2000 tokens) sent on every LLM call, including trivial messages.

**Solution:** Extend `SystemPromptBuilder` in `prompt_optimizer.rs` with a `PromptTier` parameter.

**Tier definitions:**

| Tier | Tokens | Contents | Used By |
|---|---|---|---|
| Minimal | ~300 | Identity + safety rules | Trivial |
| Basic | ~800 | Minimal + tool names (no schemas) + basic guidelines | Simple |
| Standard | ~2000 | Basic + full tool schemas + memory context + verification + DONE criteria | Standard |
| Full | ~2500 | Standard + planning instructions + delegation protocol + learning protocol | Complex |

**How it integrates:**
1. `model_router.rs` classifies complexity (existing logic + Trivial addition)
2. Classification maps to PromptTier via `ExecutionProfile`
3. `build_system_prompt()` in `context.rs` receives tier parameter
4. `SystemPromptBuilder` conditionally includes sections based on tier

**Resilience rule:** If any tier fails to produce a valid prompt (empty, encoding error), fall back to Standard tier. Standard is always the safe default.

### Optimization 3: Complexity-Aware Tool Output Caps

**Problem:** 30K char hard cap is one-size-fits-all. Simple tasks waste tokens on verbose output they don't need.

**Solution:** Scale the existing `MAX_TOOL_OUTPUT_CHARS` by complexity tier.

| Tier | Max Output Chars | Rationale |
|---|---|---|
| Simple | 5,000 | Single tool call, outcome is pass/fail |
| Standard | 15,000 | Multi-step, needs detail but not everything |
| Complex | 30,000 | Full context for planning and debugging |

**Integration:** Pass complexity to `output_compression.rs`. The existing per-tool-type compression logic stays — it already handles error extraction, head+tail truncation, and smart summarization. We just tighten the cap for simpler tasks.

**What we explicitly reject from the RFC:**
- `SmartTruncate` with regex priority patterns (expensive, existing heuristic compression already does this)
- Token-based budgeting with `estimate_tokens()` (4 chars/token heuristic is +/- 30%, false precision)
- Per-tool-type base budgets (over-engineered, char caps per complexity are sufficient)

**Resilience rule:** If compressed output is empty (compression bug), return first 1000 chars of raw output with `[COMPRESSION_FALLBACK]` marker. Never return empty tool results.

### Optimization 4: Structured Failure Types

**Problem:** Verification failures are free-text, verbose when re-injected into history. Strategy rotation is pattern-less — retries N times regardless of failure type.

**Solution:** Add structured failure types. Feed compact failure signals into history instead of verbose text.

**New types in `temm1e-core`:**

```
VerifyFailure {
    kind: FailureKind,        // ToolError | WrongOutput | Incomplete | ServiceDown | NeedsInput | Timeout | AuthError
    brief: String,            // Max 50 tokens, one-line description
    suggestion: Option<String>, // Max 30 tokens, actionable hint
    retryable: Retryability,  // RetryDirect | RetryDifferent | NeedsHuman | Impossible
}

RecoveryAction {
    RetryDirect { max_retries, backoff },
    RetryModified { hint },
    AskUser { question },
    SkipStep { reason },
    Abort { explanation },
}
```

**Integration with existing FailureTracker:**
1. When a tool fails (non-zero exit, HTTP error, timeout), construct `VerifyFailure` from the error
2. Classify recovery action via a single `match` statement (not trait-object chain):
   - HTTP 429 / 5xx / timeout -> `RetryDirect`
   - HTTP 401/403 -> `AskUser`
   - "command not found" / "permission denied" -> `RetryModified`
   - "no space left" / "disk full" -> `AskUser`
   - Connection refused -> `RetryDirect`
   - Max retries exceeded -> `Abort`
3. Inject `failure.to_context_string()` (~50 tokens) instead of full verification block (~200 tokens)
4. Replace current strategy rotation with recovery-action dispatch

**What we explicitly reject from the RFC:**
- `FailureEscalator` with `Vec<Box<dyn RuleClassifier>>` (over-abstracted for ~5 rules)
- LLM fallback classifier (200 tokens per failure, rarely needed)
- Separate `Replan` action (depends on plan infrastructure not yet built)

**Resilience rule:** If failure classification itself panics, default to `RetryDirect { max_retries: 2 }`. If a `NeedsHuman` action gets no reply within 300s, escalate to `Abort` with explanation sent to user.

### Optimization 5: Relevance-Scored Memory — DEFERRED

**Status:** Skip for v2.0. Revisit when memory store exceeds 500 entries per user.

**Reason:** Memory store is young (v1.7.0). Existing priority-based budgets (15% memory, 5% learnings, 2000 knowledge) are adequate. TF-IDF on <100 entries is noisy. Adding an embedder is new infrastructure for marginal benefit at current scale.

**Trigger for implementation:** When production logs show irrelevant learnings confusing THINK, or when per-user learning count exceeds 500.

### Optimization 6: Plan Generation for Complex Tasks

**Problem:** Complex multi-step tasks accumulate full conversation history per step. No visibility into progress, no checkpointing, no resume on failure.

**Solution:** For Complex tasks, generate an explicit plan, execute step-by-step with summary-only context.

**Plan types in `temm1e-core`:**

```
TaskPlan {
    id, order_summary, steps: Vec<PlanStep>,
    dependencies: Vec<Dependency>,
    estimated_total_tokens, created_at,
}

PlanStep {
    index, description, tool: ToolKind,
    expected_output, status: StepStatus,
    checkpoint: Option<StepCheckpoint>,
}

StepStatus = Pending | InProgress | Completed(summary) | Failed(VerifyFailure) | Skipped(reason)
```

**The critical token optimization — summary-only step context:**

Instead of accumulating full conversation history from all previous THINK/ACTION/VERIFY cycles, each step receives only:
- Task summary (~100 tokens)
- Completed step summaries (~50 tokens each)
- Current step description

For a 7-step task: ~18K tokens total vs ~56K without checkpointing (67% reduction).

**Execution model:**
1. `model_router.rs` classifies as Complex
2. Single LLM call to generate plan (structured JSON output)
3. Present plan to user as informational message (non-blocking, no approval gate)
4. Execute step by step, saving checkpoints to SQLite
5. On step failure: classify via RecoveryAction, handle accordingly
6. On resume: load checkpoint, skip completed steps

**What we explicitly reject from the RFC:**
- Blocking `channel.wait_for_reply()` for human gates (channel architecture doesn't support blocking waits — channels are async mpsc-based)
- CRITICAL tier separate from COMPLEX (the "human gate" is a per-step flag, not a tier)
- Per-step approval dialogs (adds complexity without proportional value at current scale)

**What we defer:**
- Replan on strategy failure (build after plan infrastructure is stable)
- Parallel step execution (build after sequential is proven)
- Multi-agent delegation (build after sub-agent system exists)

**Resilience rules:**
- If plan generation fails (invalid JSON, provider error), fall back to Standard loop (no plan). Never block on a bad plan.
- If a checkpoint save fails, continue execution (volatile progress is better than halted execution). Log the failure.
- If step N fails and recovery is `Impossible`, complete remaining independent steps (check dependency graph), report partial completion.

### Token Savings Summary

| Optimization | Mechanism | Savings | Frequency | Net Impact |
|---|---|---|---|---|
| OPT 1: Trivial fast-path | Skip tool loop + LEARN | 3-5K tokens | ~40% of messages | ~15-20% total |
| OPT 2: Prompt stratification | Smaller prompt per tier | 1-1.5K tokens | ~65% of messages | ~8-12% total |
| OPT 3: Tighter output caps | Less re-sent tool output | 1-3K tokens per iteration | ~25% of messages | ~3-5% total |
| OPT 4: Structured failures | Compact retry feedback | 500-1.5K tokens per retry | ~15% of messages | ~2-4% total |
| OPT 6: Plan checkpoints | Summary-only step context | 30-40K per Complex task | ~8% of messages | ~3-5% total |
| **Combined** | | | | **~25-40% total** |

---

## Part 2: Resilience Architecture

### Design Principle: Every Path Must Recover

The Tem's Mind has three categories of failure, each with a mandatory recovery strategy:

### Layer 1: Infrastructure Failures (Provider, Network, System)

| Failure | Current Handling | v2.0 Enhancement |
|---|---|---|
| Provider timeout | Circuit breaker (exists) | + Automatic fallback to secondary provider |
| Provider 429 (rate limit) | Retry (exists) | + Exponential backoff with jitter, cap at 60s |
| Provider 500+ | Circuit breaker (exists) | + Try next provider in rotation before breaking |
| Network partition | Error propagated | + Queue message for retry, notify user of delay |
| SQLite write failure | Error propagated | + In-memory fallback for session state, persist on recovery |
| Disk full | Error propagated | + Detect proactively, warn user before critical |

**Provider Failover Chain:**
```
Primary provider (user-configured)
  -> Secondary provider (if configured in temm1e.toml)
    -> Degraded mode (respond with "I'm temporarily unable to process this, retrying in N seconds")
      -> Never: silent failure
```

**New rule:** No message may be silently dropped. If all providers fail, the user gets an explicit error message with a retry suggestion. The message stays in the session queue for automatic retry when a provider recovers.

### Layer 2: Agentic Logic Failures (Classification, Planning, Tool Execution)

| Failure | Recovery |
|---|---|
| Complexity misclassification (too low) | Escalation: if Simple task needs >2 tool calls, auto-promote to Standard |
| Complexity misclassification (too high) | No harm — runs with more capability than needed, slightly higher cost |
| Plan generation produces invalid JSON | Fall back to Standard loop (no plan). Log for debugging. |
| Plan step fails with Impossible | Skip step, continue independent steps, report partial completion |
| Tool execution panics | catch_unwind (exists), session rollback (exists), inject error into history |
| LLM returns empty response | Retry once. If still empty, inject "I couldn't generate a response" as reply. |
| LLM returns malformed tool_use | Skip malformed call, log it, continue with valid calls in same response |
| Max iterations reached | Stop loop, compile best response from accumulated context, inform user |
| Budget exceeded mid-task | Stop gracefully, return partial results, inform user of budget limit |

**Escalation chain for classification:**
```
Trivial -> if response contains tool_use -> promote to Simple
Simple -> if rule-based verify fails -> promote to Standard
Standard -> if max_iterations hit -> promote to Complex (enable planning)
Complex -> if plan fails -> fall back to Standard loop
```

Every promotion is logged for learning. Over time, these logs improve classification accuracy.

### Layer 3: State Corruption (Memory, History, Checkpoints)

| Failure | Recovery |
|---|---|
| Session history corrupted (malformed JSON) | Discard corrupted entries, keep what parses, inject summary of gap |
| Memory backend unreachable | Continue without memory injection, log warning |
| Checkpoint file corrupted | Start plan from step 0, log the corruption |
| Learning extraction fails | Skip LEARN phase for this turn, don't corrupt memory store |
| Config file invalid | Use compiled-in defaults, warn user on next message |

**Golden rule:** State corruption never prevents message processing. The system degrades gracefully: first drop optional context (learnings, memory), then degrade features (no planning, no verification), but ALWAYS respond to the user.

### Resilience Invariants (Must Hold Under All Conditions)

1. **No silent death:** Every failure path produces a user-visible message. If a panic is caught, if a provider is down, if a tool crashes — the user is told.
2. **No infinite loops:** Every retry has a cap. Every escalation has a ceiling (Complex). Every wait has a timeout.
3. **No state corruption on failure:** Session history is rolled back if catch_unwind triggers (exists). Plan checkpoints are atomic (write-then-rename). Memory writes are idempotent.
4. **No cascading failures:** Circuit breakers isolate provider failures. Tool failures don't kill the loop. Plan step failures don't abort the whole plan unless dependencies require it.
5. **Graceful degradation order:** Planning -> Learning -> Memory -> Verification -> Tool Use -> Response. Strip features in this order if resources are constrained. Response is NEVER stripped.

---

## Part 3: Skill & Plugin System (Claude Code Aligned)

### Design Philosophy

TEMM1E adopts the [Agent Skills open standard](https://agentskills.io) used by Claude Code, Cursor, Gemini CLI, and Codex CLI. This means:

- Skills are `SKILL.md` files with YAML frontmatter — the same format works across ecosystems
- Progressive disclosure: metadata always loaded, instructions on-demand, resources as-needed
- Skills can be installed from Claude Code's marketplace and community repositories
- TEMM1E-specific extensions are additive (extra frontmatter fields), never breaking

### Skill Format (Compatible with Claude Code + Agent Skills Standard)

```
my-skill/
  SKILL.md           # Required: frontmatter + instructions
  reference.md       # Optional: detailed docs, loaded on-demand
  examples/          # Optional: example outputs
  scripts/           # Optional: executable scripts
    validate.sh
    helper.py
```

**SKILL.md format:**

```yaml
---
# === Agent Skills Standard Fields (cross-platform compatible) ===
name: deploy-to-cloud          # Lowercase, hyphens, max 64 chars
description: >                 # Required. When to use this skill.
  Deploy the application to cloud infrastructure. Use when the user
  asks to deploy, ship, push to production, or go live.

# === Claude Code Compatible Fields ===
disable-model-invocation: false  # If true, only user can trigger via /name
user-invocable: true             # If false, hidden from / menu, agent-only
allowed-tools: Shell, FileRead   # Restrict which tools the skill can use
argument-hint: "[environment]"   # Shown in autocomplete

# === TEMM1E Extension Fields ===
temm1e-complexity: standard     # Hint for classification: trivial|simple|standard|complex
temm1e-max-budget-usd: 0.50    # Max spend when this skill is active
temm1e-require-approval: true   # Require user confirmation before execution
temm1e-channels: [telegram, discord]  # Restrict to specific channels (empty = all)
temm1e-timeout-seconds: 300     # Max execution time
---

# Deploy to Cloud

Instructions for Claude/TEMM1E to follow when this skill is active.

## Steps
1. Verify the build is clean
2. Run the test suite
3. Build the release artifact
4. Deploy to $ARGUMENTS environment
5. Verify health check passes

## Rollback
If deployment fails, roll back to the previous version and notify the user.

## Additional Resources
- For cloud provider details, see [reference.md](reference.md)
```

### Three-Level Progressive Loading (Matching Claude Code Architecture)

**Level 1 — Metadata (always loaded at startup):**
- `name` and `description` from YAML frontmatter
- Cost: ~100 tokens per skill
- Loaded into system prompt so the agent knows what's available
- Budget: 2% of context window (matching Claude Code's default)

**Level 2 — Instructions (loaded when skill is triggered):**
- Full SKILL.md body
- Cost: typically under 5K tokens
- Loaded when user invokes `/skill-name` or agent auto-matches based on description

**Level 3 — Resources (loaded as-needed):**
- Additional files in the skill directory
- Scripts executed via shell, output returned (script code never enters context)
- Reference docs read only when SKILL.md references them
- Cost: effectively unlimited, only relevant portions loaded

**Token efficiency impact:** Without progressive loading, 20 installed skills at 2K tokens each = 40K tokens in every prompt. With progressive loading, the same 20 skills cost ~2K tokens (metadata only) until one is triggered.

### Skill Discovery & Installation

**Where skills live (priority order, highest wins):**

| Location | Scope | Priority |
|---|---|---|
| `~/.temm1e/skills/<name>/SKILL.md` | User-level (all projects) | 1 (highest) |
| `.temm1e/skills/<name>/SKILL.md` | Project-level | 2 |
| Plugin skills (installed) | Per-plugin namespace | 3 |
| Built-in skills | Ship with TEMM1E binary | 4 (lowest) |

**Cross-compatibility with Claude Code:**

TEMM1E reads skills from `.claude/skills/` as a fallback path. This means:
- A project with Claude Code skills in `.claude/skills/` works in TEMM1E out of the box
- TEMM1E's own skills in `.temm1e/skills/` take precedence if both exist
- Claude Code marketplace plugins can be manually copied to `~/.temm1e/plugins/` (not all features will work, but SKILL.md + scripts will)

**Skill installation (CLI):**

```bash
# Install from local directory
temm1e skill install ./path/to/skill/

# Install from Git repository
temm1e skill install https://github.com/user/skill-repo

# List installed skills
temm1e skill list

# Remove a skill
temm1e skill remove deploy-to-cloud
```

### Skill Invocation

**Three invocation paths:**

1. **User explicit:** User types `/deploy-to-cloud staging` in any channel (Telegram, Discord, CLI)
2. **Agent auto-match:** Agent reads skill descriptions in system prompt, decides to load a skill based on the user's message. Only for skills with `disable-model-invocation: false` (default).
3. **Skill chaining:** One skill's instructions reference another skill by name. Agent loads the referenced skill.

**Invocation flow:**
```
User message arrives
  -> Agent sees skill metadata in system prompt (Level 1)
  -> Agent decides to use skill OR user explicitly invokes /name
  -> Runtime loads SKILL.md body (Level 2)
  -> Skill instructions become part of the agent's context for this turn
  -> Agent follows instructions, using allowed tools
  -> If instructions reference supporting files, agent reads them (Level 3)
  -> Skill completes, results returned to user
```

### Skill Authoring by Users (In-Chat Skill Creation)

Users should be able to create skills conversationally:

```
User: "Create a skill that checks my website uptime every hour"

TEMM1E:
1. Generates SKILL.md with appropriate frontmatter
2. Creates supporting scripts if needed
3. Saves to ~/.temm1e/skills/check-uptime/
4. Confirms: "Created /check-uptime skill. Try it with /check-uptime https://mysite.com"
```

This is a built-in capability of the agent, not a separate feature. The agent uses its existing file-write tools to create skill files. The skill format is simple enough that the agent can author it correctly.

### Plugin System

**Plugin = a bundle of skills + agents + hooks + config.**

**Plugin structure (Claude Code compatible):**

```
my-plugin/
  .claude-plugin/
    plugin.json         # Manifest: name, version, description, author
  skills/
    skill-one/
      SKILL.md
    skill-two/
      SKILL.md
  agents/               # Sub-agent definitions (future)
    researcher.md
  hooks/
    hooks.json          # Lifecycle hooks
  scripts/              # Shared scripts used by skills
    common.sh
```

**Plugin manifest (`plugin.json`):**

```json
{
  "name": "cloud-deploy-suite",
  "description": "Cloud deployment skills for AWS, GCP, and Azure",
  "version": "1.0.0",
  "author": { "name": "TEMM1E Community" },
  "homepage": "https://github.com/example/cloud-deploy-suite",
  "license": "MIT",
  "temm1e": {
    "min_version": "2.0.0",
    "required_tools": ["shell"],
    "required_features": ["browser"]
  }
}
```

**Plugin namespacing:** Plugin skills are prefixed: `/cloud-deploy-suite:deploy-aws`. This prevents conflicts between plugins with same-named skills.

**Plugin installation:**

```bash
# Install from Git
temm1e plugin install https://github.com/user/my-plugin

# Install from local directory
temm1e plugin install ./my-plugin

# List installed plugins
temm1e plugin list

# Remove plugin
temm1e plugin remove cloud-deploy-suite
```

**Plugin storage:** `~/.temm1e/plugins/<plugin-name>/`

### TemHub Marketplace (Future — Post v2.0)

The existing TemHub concept (signed skill marketplace with ed25519 signatures) evolves to support the plugin format:

- Registry at `hub.temm1e.dev` (future)
- Skills/plugins are signed with author's ed25519 key
- `temm1e hub search "deploy"` — search the registry
- `temm1e hub install deploy-suite` — install from registry
- Signature verification on install (existing vault crypto)
- Community ratings and reviews

**Cross-ecosystem compatibility goal:** Any Claude Code skill from `anthropics/skills` or community repositories that follows the Agent Skills standard should work in TEMM1E with zero modification. TEMM1E-specific extensions (budget limits, channel restrictions, approval gates) are purely additive frontmatter fields that other tools ignore.

### Built-In Skills (Ship with TEMM1E)

| Skill | Description | Complexity Hint |
|---|---|---|
| `/help` | Show available commands and skills | trivial |
| `/addkey` | Add or rotate an API key | simple |
| `/keys` | List configured API keys | simple |
| `/removekey` | Remove an API key | simple |
| `/model` | Switch active model | simple |
| `/status` | Show agent status, budget, uptime | trivial |
| `/debug` | Analyze recent errors from logs | standard |
| `/learn` | Show what the agent has learned | simple |
| `/forget` | Clear specific learnings | simple |
| `/skill` | Manage installed skills (list/install/remove) | simple |

### Hooks System (Claude Code Aligned)

**Hook events (lifecycle points where user scripts can execute):**

| Event | When | Use Case |
|---|---|---|
| `PreToolUse` | Before a tool executes | Validate commands, block dangerous ops |
| `PostToolUse` | After a tool executes | Auto-format, lint, log |
| `SessionStart` | When a conversation starts | Load context, set up environment |
| `SessionStop` | When a conversation ends | Cleanup, save state |
| `PreMessage` | Before agent processes a message | Content filtering, rate limiting |
| `PostMessage` | After agent responds | Logging, analytics, notifications |

**Hook configuration (in `temm1e.toml`):**

```toml
[hooks.PostToolUse]
matcher = "Shell"
command = "./scripts/auto-lint.sh"
timeout_ms = 5000

[hooks.PreToolUse]
matcher = "Shell"
command = "./scripts/validate-command.sh"
timeout_ms = 3000
```

**Hook input/output:** JSON on stdin (matching Claude Code format), exit codes control behavior:
- Exit 0: proceed normally
- Exit 1: hook error (log warning, proceed anyway)
- Exit 2: block the operation (tool call is rejected, error fed back to agent)

**Resilience rule:** Hook failures (timeout, crash, non-zero exit other than 2) NEVER block the agent. They are logged as warnings. Only exit code 2 is an intentional block.

---

## Part 4: Sub-Agent Architecture (Future — Post v2.0)

This section documents the target architecture for sub-agents, inspired by Claude Code's model. Not implemented in v2.0, but the skill and hook infrastructure in v2.0 is designed to support it.

### Sub-Agent Concept

A sub-agent is a specialized agent instance that:
- Runs in its own context window (isolated from main conversation)
- Has a custom system prompt focused on one domain
- Has restricted tool access (e.g., read-only for research agents)
- Returns a summary to the main agent when done

**Built-in sub-agent types (matching Claude Code):**

| Agent | Model | Tools | Purpose |
|---|---|---|---|
| Explore | Fast/cheap model | Read-only | Codebase search, file discovery |
| Plan | Inherits | Read-only | Research for planning mode |
| General | Inherits | All | Complex multi-step tasks |

**Custom sub-agents:** Defined as Markdown files in `.temm1e/agents/` (same format as Claude Code's `.claude/agents/`):

```yaml
---
name: security-reviewer
description: Reviews code for security vulnerabilities
tools: Shell, FileRead, Grep
model: sonnet
---

You are a security reviewer. Analyze code for OWASP top 10 vulnerabilities...
```

**Why defer to post-v2.0:** Sub-agents require message routing between isolated context windows, which is architectural work in `temm1e-gateway`. The v2.0 skill system provides 80% of the value (specialized behavior per task) without the complexity of isolated contexts.

---

## Part 5: Implementation Phases

### Phase 1 — Foundation (Week 1-2)

**Goal:** Trivial fast-path + prompt stratification. Immediate 20-25% token savings.

| Task | Crate | Files | Risk |
|---|---|---|---|
| Add `Trivial` tier to complexity classifier | temm1e-agent | `model_router.rs` | Low |
| Add `PromptTier` enum | temm1e-core | `types/mod.rs` | Low |
| Add `ExecutionProfile` struct | temm1e-core | `types/mod.rs` | Low |
| Implement tiered prompt building | temm1e-agent | `prompt_optimizer.rs` | Low |
| Wire tier into `build_system_prompt()` | temm1e-agent | `context.rs` | Medium |
| Trivial fast-path in runtime loop | temm1e-agent | `runtime.rs` | Medium |
| Escalation: Trivial -> Simple if tool_use detected | temm1e-agent | `runtime.rs` | Low |

**Validation:**
- All 1141 existing tests pass
- 10-turn CLI self-test protocol passes
- Trivial messages ("hi", "thanks") respond faster and cheaper
- Standard/Complex tasks behave identically to v1.7.0

### Phase 2 — Token Optimization (Week 3-4)

**Goal:** Structured failures + tighter output caps. Additional 5-10% token savings.

| Task | Crate | Files | Risk |
|---|---|---|---|
| Add `VerifyFailure`, `FailureKind`, `Retryability` types | temm1e-core | `types/error.rs` | Low |
| Add `RecoveryAction` enum | temm1e-core | `types/error.rs` | Low |
| Complexity-aware tool output caps | temm1e-agent | `runtime.rs`, `output_compression.rs` | Medium |
| Structured failure construction from tool errors | temm1e-agent | `runtime.rs` | Medium |
| Recovery action dispatch (replace strategy rotation) | temm1e-agent | `runtime.rs` | Medium |
| Compact failure injection into history | temm1e-agent | `runtime.rs` | Medium |

**Validation:**
- Strategy rotation tests updated for new recovery actions
- Tool output compression tests verify per-tier caps
- Failure scenarios (timeout, auth, not-found) produce correct RecoveryAction
- 10-turn CLI self-test passes with identical quality

### Phase 3 — Skill System (Week 5-7)

**Goal:** SKILL.md support, progressive loading, built-in skills, user skill creation.

| Task | Crate | Files | Risk |
|---|---|---|---|
| SKILL.md parser (YAML frontmatter + markdown body) | temm1e-skills | New: `parser.rs` | Medium |
| Skill discovery (scan directories, build metadata index) | temm1e-skills | New: `discovery.rs` | Medium |
| Skill registry (load, list, match by description) | temm1e-skills | New: `registry.rs` | Medium |
| Skill invocation (load Level 2 on trigger, inject into context) | temm1e-agent | `runtime.rs`, `context.rs` | High |
| `/skill` slash command (list, install, remove) | temm1e-skills | New: `commands.rs` | Medium |
| Migrate existing built-in commands to SKILL.md format | temm1e-skills | New: `skills/` directory | Medium |
| Skill argument substitution (`$ARGUMENTS`, `$0`, `$1`) | temm1e-skills | `parser.rs` | Low |
| Claude Code `.claude/skills/` fallback path | temm1e-skills | `discovery.rs` | Low |
| Dynamic context injection (`!`command`` syntax) | temm1e-skills | `parser.rs` | Medium |

**Validation:**
- Create 5+ test skills, verify discovery and invocation
- Verify Claude Code skills from `anthropics/skills` repo load correctly
- Verify skill metadata budget stays within 2% of context window
- Built-in commands (/addkey, /model, /keys) work as skills
- User can create a skill via chat and invoke it

### Phase 4 — Plan Generation (Week 8-10)

**Goal:** Complex task planning with checkpointed execution.

| Task | Crate | Files | Risk |
|---|---|---|---|
| Add `TaskPlan`, `PlanStep`, `StepCheckpoint` types | temm1e-core | New: `types/plan.rs` | Low |
| Plan generation prompt + JSON parsing | temm1e-agent | New: `planner.rs` | High |
| Summary-only step context builder | temm1e-agent | `context.rs` | High |
| Checkpointed step execution | temm1e-agent | `runtime.rs` | High |
| Checkpoint persistence (SQLite) | temm1e-memory | `sqlite.rs` | Medium |
| Plan presentation via channel | temm1e-channels | Channel trait extension | Medium |
| Progress updates per step | temm1e-channels | Channel trait extension | Low |
| Fallback: plan failure -> Standard loop | temm1e-agent | `runtime.rs` | Medium |
| Feature flag: `[agent] enable_plan_generation = false` | temm1e-core | `types/config.rs` | Low |

**Validation:**
- Complex tasks (multi-step file operations, research + write) produce plans
- Plans execute with checkpoint saves visible in SQLite
- If plan generation fails, task completes via Standard loop
- Token usage for planned Complex tasks is measurably lower than v1.7.0
- Feature flag OFF: behavior identical to v1.7.0

### Phase 5 — Plugin & Hooks (Week 11-13)

**Goal:** Plugin packaging, hooks lifecycle, marketplace preparation.

| Task | Crate | Files | Risk |
|---|---|---|---|
| Plugin manifest parser (`plugin.json`) | temm1e-skills | New: `plugin.rs` | Medium |
| Plugin installation (git clone + validate) | temm1e-skills | `plugin.rs` | Medium |
| Plugin namespacing (prefix skill names) | temm1e-skills | `registry.rs` | Medium |
| Hook event system (PreToolUse, PostToolUse, etc.) | temm1e-core | New: `types/hooks.rs` | Medium |
| Hook executor (spawn command, pass JSON stdin) | temm1e-agent | New: `hooks.rs` | Medium |
| Hook configuration in `temm1e.toml` | temm1e-core | `types/config.rs` | Low |
| Skill-scoped hooks (from SKILL.md frontmatter) | temm1e-skills | `parser.rs` | Medium |

**Validation:**
- Install a multi-skill plugin, verify all skills discoverable
- Hook on PostToolUse fires correctly, exit code 2 blocks tool
- Hook timeout/crash doesn't block agent
- Plugin removal cleanly uninstalls all skills

---

## Part 6: Migration & Compatibility

### Backwards Compatibility Guarantees

1. **All v1.7.0 behavior is the default.** New features are additive or behind feature flags.
2. **Existing `temm1e.toml` configs work unchanged.** New config fields have sane defaults.
3. **Existing conversations/sessions are preserved.** Memory DB schema extensions are additive (new columns with defaults).
4. **Provider integrations untouched.** Token optimization happens at the agent layer, not the provider layer.

### Claude Code Ecosystem Compatibility Matrix

| Feature | Claude Code | TEMM1E v2.0 | Compatibility |
|---|---|---|---|
| SKILL.md with YAML frontmatter | Full support | Full support | 100% |
| Skill progressive loading (3 levels) | Full support | Full support | 100% |
| `$ARGUMENTS` substitution | Full support | Full support | 100% |
| `!`command`` dynamic injection | Full support | Full support | 100% |
| `context: fork` (sub-agent) | Full support | Deferred | Parsed, ignored gracefully |
| `agent:` field | Full support | Deferred | Parsed, ignored gracefully |
| `allowed-tools` | Full support | Full support | Mapped to TEMM1E tool names |
| `disable-model-invocation` | Full support | Full support | 100% |
| `user-invocable` | Full support | Full support | 100% |
| `model:` field | Full support | Partial | Mapped to TEMM1E provider/model pairs |
| `hooks:` in SKILL.md | Full support | Phase 5 | Parsed, ignored until Phase 5 |
| Plugin marketplace install | Full support | Phase 5+ | Manual install first, marketplace later |
| `.claude/skills/` path | Native | Fallback | TEMM1E reads as secondary path |
| `.claude/agents/` | Full support | Deferred | Not read until sub-agent phase |

**Graceful degradation for unsupported fields:** When TEMM1E encounters a SKILL.md with Claude Code fields it doesn't yet support (e.g., `context: fork`), it logs a debug message and ignores the field. The skill still works — it just runs in the main context instead of a fork. This ensures Claude Code community skills are usable immediately, with full feature parity coming in later phases.

---

## Appendix A: Key Decisions Log

| Decision | Rationale |
|---|---|
| 4 tiers not 5 | CRITICAL adds no meaningful behavior over COMPLEX with per-step approval flags |
| Rule-based classification not LLM | Zero cost, handles 90%+ of cases, escalation covers misses |
| Single THINK call not two-phase | Extra API call for Standard+ costs more than slightly larger prompt |
| Enhance FailureTracker not replace | Production-tested code, structured types add to it |
| Defer memory scoring | Store too small, existing budgets sufficient |
| Agent Skills standard for skills | Cross-ecosystem compatibility, massive existing skill library |
| `.temm1e/` primary, `.claude/` fallback | Own namespace, but leverage existing Claude Code project skills |
| No blocking human gates | Channel architecture is async mpsc, blocking waits need architectural work |
| Feature flag for plan generation | High-risk feature needs controlled rollout to live users |
| Hooks never block on failure | Resilience invariant: user-defined code cannot kill the agent |

## Appendix B: Files Touched Per Phase

```
Phase 1 (Foundation):
  MODIFY: crates/temm1e-core/src/types/mod.rs         (PromptTier, ExecutionProfile)
  MODIFY: crates/temm1e-agent/src/model_router.rs      (Trivial classification)
  MODIFY: crates/temm1e-agent/src/prompt_optimizer.rs   (tiered prompt building)
  MODIFY: crates/temm1e-agent/src/context.rs            (tier-aware prompt construction)
  MODIFY: crates/temm1e-agent/src/runtime.rs            (fast-path, escalation)

Phase 2 (Token Optimization):
  MODIFY: crates/temm1e-core/src/types/error.rs         (VerifyFailure, RecoveryAction)
  MODIFY: crates/temm1e-agent/src/runtime.rs            (structured failure, recovery dispatch)
  MODIFY: crates/temm1e-agent/src/output_compression.rs (complexity-aware caps)

Phase 3 (Skill System):
  CREATE: crates/temm1e-skills/src/parser.rs            (SKILL.md parser)
  CREATE: crates/temm1e-skills/src/discovery.rs         (directory scanning)
  CREATE: crates/temm1e-skills/src/registry.rs          (load, list, match)
  CREATE: crates/temm1e-skills/src/commands.rs          (CLI commands)
  CREATE: .temm1e/skills/*/SKILL.md                     (built-in skills)
  MODIFY: crates/temm1e-agent/src/runtime.rs            (skill invocation)
  MODIFY: crates/temm1e-agent/src/context.rs            (skill metadata injection)

Phase 4 (Plan Generation):
  CREATE: crates/temm1e-core/src/types/plan.rs          (TaskPlan types)
  CREATE: crates/temm1e-agent/src/planner.rs            (plan generation)
  MODIFY: crates/temm1e-agent/src/runtime.rs            (checkpointed execution)
  MODIFY: crates/temm1e-agent/src/context.rs            (summary-only step context)
  MODIFY: crates/temm1e-memory/src/sqlite.rs            (checkpoint persistence)
  MODIFY: crates/temm1e-core/src/types/config.rs        (feature flag)

Phase 5 (Plugins & Hooks):
  CREATE: crates/temm1e-skills/src/plugin.rs            (plugin manifest, install)
  CREATE: crates/temm1e-core/src/types/hooks.rs         (hook event types)
  CREATE: crates/temm1e-agent/src/hooks.rs              (hook executor)
  MODIFY: crates/temm1e-skills/src/registry.rs          (plugin namespacing)
  MODIFY: crates/temm1e-core/src/types/config.rs        (hook config)
```

## Appendix C: Research Sources

- [Anthropic Agent Skills Repository](https://github.com/anthropics/skills)
- [Agent Skills Standard (agentskills.io)](https://agentskills.io)
- [Claude Code Skills Documentation](https://code.claude.com/docs/en/skills)
- [Claude Code Sub-Agents Documentation](https://code.claude.com/docs/en/sub-agents)
- [Claude Code Hooks Reference](https://code.claude.com/docs/en/hooks)
- [Claude Code Plugins Documentation](https://code.claude.com/docs/en/plugins)
- [Agent Skills API Overview](https://platform.claude.com/docs/en/agents-and-tools/agent-skills/overview)
- [Claude Code Best Practices (Anthropic Engineering)](https://www.anthropic.com/engineering/claude-code-best-practices)
