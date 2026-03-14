# Blueprint System — Procedural Memory for TEMM1E Agents

> **Status**: Design Proposal
> **Author**: TEMM1E Core Team
> **Date**: 2026-03-12

## 1. The Problem

TEMM1E's current post-task learning system (`learning.rs`) extracts **declarative summaries** after task completion:

```
[OK] shell+browser: Task type 'shell+browser' succeeded using: shell → browser → web_fetch.
Errors encountered: rate limit exceeded. Strategy rotation was triggered.
```

This tells the agent *what happened*. It does not tell the agent *how to do it again*.

For complex, multi-step tasks — the kind that take 20+ tool calls, involve authentication flows, timing-sensitive operations, platform-specific patterns, and nuanced decision-making — the gap between "I learned something" and "I can replicate it" is enormous.

### Real example

A user asks: *"Go on Reddit, log in, find subreddits relevant to TEMM1E, and engage naturally — 3 genuine comments for every 1 that mentions TEMM1E."*

After completing this task, the current system produces:

```
Task type 'browser+web' succeeded using: browser → web_fetch.
```

That's a **caption**, not knowledge. The next time the agent sees this task, it starts from zero. Every hard-won insight — the login flow, the 2FA wait pattern, which subreddits ban self-promotion, what "natural Reddit tone" actually means, the timing between posts — is lost.

## 2. The Vision: Blueprints

A **Blueprint** is a replayable, structured procedure that the agent writes after completing a complex task. It captures not just *what* was done, but *how* to do it again — with enough precision that a future agent instance (or the same agent in a new session) can load it, follow it, and achieve the same outcome with minimal trial-and-error.

### Blueprints vs. Learnings

| Dimension | Learning (current) | Blueprint (proposed) |
|---|---|---|
| **Nature** | Declarative — "I know that..." | Procedural — "Here's how to do X" |
| **Granularity** | One sentence per task | Full procedure: phases, steps, decision points, failure modes |
| **Trigger** | All non-trivial tasks | Complex tasks only (compound, multi-tool, multi-phase) |
| **Storage** | JSON blob in memory | Structured document with YAML frontmatter |
| **Context injection** | Flat text in 5% budget | Loaded as operational guide when task matches |
| **Evolution** | Append-only, never updated | Updated after each execution (new failure modes, refined steps) |
| **Analogy** | Post-it note | Engineering drawing |

### Blueprints vs. Skills

Skills are **authored instructions** — human-written, version-controlled, distributed via TemHub. They tell the agent what a skill *is* and how to *generally* use it.

Blueprints are **earned knowledge** — auto-generated from real execution, specific to observed conditions, refined through repetition. They tell the agent what *actually worked* and what to watch out for.

A skill says: "You can use the browser tool to interact with web pages."
A blueprint says: "Reddit login: navigate to reddit.com/login, fill username field `#loginUsername`, fill password field `#loginPassword`, click submit, wait 3s for redirect, check for 2FA modal — if present, alert user and poll every 5s for up to 2 minutes."

**They are complementary.** A skill provides the capability. A blueprint provides the operational memory.

## 3. Blueprint Anatomy

A Blueprint is a Markdown document with YAML frontmatter, stored in the memory backend and optionally on disk.

### 3.1 Schema

```yaml
---
# Identity
id: "bp-reddit-organic-engagement"
name: "Reddit Organic Engagement Campaign"
version: 3                          # Incremented on each refinement
created: "2026-03-10T14:30:00Z"
updated: "2026-03-12T09:15:00Z"

# Matching
trigger_patterns:
  - "reddit"
  - "subreddit"
  - "organic engagement"
  - "natural comments"
  - "guerilla marketing"
task_signature: "browser+web_fetch+shell"  # Tool combination fingerprint
semantic_tags:
  - "social-media"
  - "marketing"
  - "community-engagement"
  - "reddit"

# Fitness
times_executed: 3
times_succeeded: 2
times_failed: 1
success_rate: 0.67
avg_tool_calls: 47
avg_duration_secs: 1200

# Scope
owner_user_id: "12345678"           # User who generated this blueprint
scope: "user"                       # "user" | "global" | "team"
---
```

### 3.2 Body Structure

The body follows a fixed structure with mandatory sections. The agent writes this in natural language with structured headers — not code, not pseudocode, but precise operational prose.

```markdown
## Objective

One-sentence description of what this blueprint accomplishes.

## Prerequisites

What must be true before starting. Credentials, tools, permissions, state.

- Reddit account credentials (username + password)
- Browser tool available
- User has specified: target product/project, subreddit criteria, comment ratio

## Phases

### Phase 1: Authentication

**Goal**: Establish authenticated Reddit session.

**Steps**:
1. Navigate to `reddit.com/login`
2. Enter credentials in login form
3. Submit and wait for redirect (up to 10s)
4. If 2FA prompt appears:
   - Alert user: "Reddit is requesting 2FA. Please provide the code."
   - Poll user response every 5s, timeout after 2 minutes
   - Enter 2FA code and submit
5. Verify login: check for username display in top-right nav

**Decision point**: If login fails with "incorrect password", do NOT retry —
ask user to verify credentials. Reddit locks accounts after 5 failed attempts.

**Failure modes**:
- Rate limited → wait 60s, retry once
- CAPTCHA → alert user, cannot proceed automatically
- Account suspended → abort, inform user

### Phase 2: Reconnaissance

**Goal**: Identify target subreddits and calibrate engagement approach.

**Steps**:
1. Search Reddit for subreddits matching product domain keywords
2. For each candidate (check up to 15):
   - Read subscriber count — skip if < 5,000 (low impact)
   - Read sidebar rules — flag any that explicitly ban self-promotion
   - Read top 20 posts from past week — note dominant tone, topics, formatting
   - Check if product/competitor is already discussed (search within subreddit)
3. Rank by: relevance (0.5) + activity (0.3) + promotion-friendliness (0.2)
4. Select top 5 subreddits

**Decision point**: If no subreddit scores above threshold, report to user
and suggest alternative platforms or broader search terms.

**Quality gate**: At least 3 viable subreddits identified before proceeding.

### Phase 3: Tone Calibration

**Goal**: Internalize the communication style of each target subreddit.

**Steps**:
1. For each selected subreddit, read 30 recent comments from hot threads
2. Extract patterns:
   - Average sentence length
   - Slang/jargon frequency (e.g., Reddit-specific: "ngl", "imo", "based")
   - Emoji usage (most subreddits: minimal; some: heavy)
   - Formatting conventions (markdown, plain text, lists)
   - Self-reference style ("I", "we", impersonal)
3. Note what gets upvoted vs. downvoted — the community rewards/punishes

**Output**: Mental model of each subreddit's voice. No document produced,
but this calibration MUST inform Phase 4 content.

### Phase 4: Content Creation & Posting

**Goal**: Execute the engagement campaign at the specified ratio.

**Steps**:
For each subreddit, in order:
1. **Organic comments first** (3x per the ratio):
   - Find threads where you have genuine, useful input
   - Write comment that matches subreddit tone exactly
   - Comment must be substantive (>2 sentences, adds value)
   - Reference specific details from the post (proves you read it)
   - Post the comment
   - Wait 5-15 minutes (randomized, NOT uniform intervals)
2. **Promotional comment** (1x per the ratio):
   - Find a thread where the product is genuinely relevant to the discussion
   - Frame as personal experience: "I've been using [product] for..."
   - Lead with the problem, not the product
   - Include organic imperfections (casual phrasing, minor tangent)
   - NEVER: direct link as first thing, exclamation marks, marketing language
   - Post the comment
   - Wait 10-20 minutes before moving to next subreddit

**Timing rule**: Total campaign should span 2-4 hours minimum. Compressed
posting triggers spam detection.

**Quality gate**: Before posting each comment, re-read it and ask:
"Would I click this person's profile and get suspicious?" If yes, rewrite.

## Failure Recovery

| Scenario | Detection | Response |
|---|---|---|
| Rate limited | HTTP 429 or "you are doing that too much" | Wait indicated time (usually 10 min), continue |
| Shadowbanned | Posts visible when logged in, invisible in incognito | STOP immediately, inform user |
| Comment removed by mod | Check comment URL returns 404 or [removed] | Note subreddit as hostile, skip further posts there |
| Session expired | Redirect to login page during action | Re-authenticate (Phase 1), resume from last step |
| 2FA required mid-session | 2FA modal appears unexpectedly | Alert user, wait for code, continue |

## Verification

How to confirm the blueprint executed correctly:
- [ ] All organic comments are visible (check in incognito)
- [ ] Promotional comments are visible and not flagged
- [ ] Comment ratio matches specification (3:1 in this case)
- [ ] No account warnings or restrictions triggered
- [ ] Spacing between comments is non-uniform and > 5 minutes

## Execution Log

### Run 1 — 2026-03-10
- Subreddits: r/selfhosted, r/homelab, r/opensource, r/DevOps
- Result: SUCCESS (11 organic, 4 promotional, 0 removed)
- Duration: 2h 15m, 43 tool calls
- Note: r/DevOps has strict automod — comments under 50 chars get removed

### Run 2 — 2026-03-11
- Subreddits: r/selfhosted, r/homelab, r/sysadmin, r/cloudcomputing
- Result: PARTIAL (9 organic, 3 promotional, 1 removed by r/sysadmin automod)
- Duration: 1h 50m, 38 tool calls
- Note: r/sysadmin requires flair on all posts; comments on unflair'd posts get nuked
- **Refinement**: Added r/sysadmin flair requirement to Phase 2 checklist

### Run 3 — 2026-03-12
- Subreddits: r/selfhosted, r/homelab, r/opensource, r/cloudcomputing
- Result: SUCCESS (12 organic, 4 promotional, 0 removed)
- Duration: 2h 30m, 51 tool calls
- Note: Slower pacing (randomized 8-18 min gaps) yielded zero flags
- **Refinement**: Updated Phase 4 timing rule from 5-15 min to 8-18 min
```

## 4. Lifecycle

### 4.1 Creation — When does a Blueprint get written?

Not every task deserves a blueprint. A blueprint is created when ALL of the following are true:

1. **The task was compound** — detected by `is_compound_task()` or the task used 3+ distinct tools
2. **The task took significant effort** — 10+ tool calls or 5+ minutes of execution
3. **The task succeeded** (or partially succeeded with recoverable failures)
4. **No existing blueprint matched** — this was a novel execution, not a replay

After the agent loop produces a final response (the DONE state), the runtime enters the **Blueprint Authoring Phase**: it sends the full conversation history to the LLM with a structured prompt asking it to distill the execution into a Blueprint following the schema above.

This is a **separate LLM call** — not part of the user-facing conversation. The user sees the final response. The blueprint is written asynchronously in the background.

### 4.2 Matching — How does a Blueprint get found?

When a new task arrives, before the agent loop begins execution, the runtime runs **Blueprint Matching**:

```
Incoming task text
  → Extract semantic tags (LLM or keyword-based)
  → Search blueprints by:
      1. Trigger pattern keyword overlap (fast, first pass)
      2. Semantic tag similarity (second pass)
      3. Task signature match (tool combination fingerprint)
  → Rank candidates by:
      - Relevance score (keyword + semantic)  × 0.5
      - Success rate                          × 0.3
      - Recency (prefer recently-updated)     × 0.2
  → If top candidate scores above threshold (0.6):
      → Load blueprint into context
```

**Matching relies on the model's judgment.** We trust the model to select the right tool from a toolset — we can trust it to select the right blueprint from a blueprint set. The matching system proposes candidates; the model decides whether to follow one. If it picks wrong, the existing self-correction loop (`self_correction.rs`) catches the divergence and the agent adapts. This is the same trust model we apply to tool selection and task decomposition.

### 4.3 Injection — How does the agent use a Blueprint?

When a blueprint is matched, it's injected into the context as a **System message** positioned between the system prompt and the conversation history:

```
[System prompt]
[Blueprint: Reddit Organic Engagement Campaign v3]
  ... full blueprint body ...
[End Blueprint]
[Memory/Knowledge context]
[Conversation history]
```

The system prompt includes an instruction like:

> A Blueprint has been loaded for this task. Use it as your operational guide.
> Follow the phases in order. Deviate only when conditions differ from what
> the blueprint describes — and document what was different. After completing
> the task, report any refinements that should be applied to the blueprint.

The blueprint gets a dedicated context budget (separate from the 5% learning budget and 15% memory budget). Recommended: **10% of total context budget**, capped at the blueprint's actual size.

### 4.4 Refinement — How does a Blueprint evolve?

Blueprints are **living documents with a CRUD lifecycle**:

```
Create  → First successful execution of a novel complex task
Read    → Matched and loaded into context on future task
Update  → Refined after each execution (new failure modes, adjusted steps)
Delete  → User removes it, or auto-retired after sustained low success rate
```

After each execution that used a loaded blueprint, the runtime enters the **Blueprint Refinement Phase**:

1. Compare actual execution against blueprint steps
2. Identify deviations:
   - Steps that were skipped (no longer needed?)
   - Steps that were added (new requirement?)
   - Failure modes encountered that weren't documented
   - Timing/parameters that were adjusted
3. Send a refinement prompt to the LLM with the original blueprint + execution diff
4. LLM produces an updated blueprint with:
   - Version incremented
   - New failure modes added
   - Steps adjusted based on latest execution
   - Execution log appended
   - Fitness metrics updated (times_executed, success_rate, etc.)
5. Updated blueprint replaces the previous version in storage via `Memory::store()` (same ID, updated content — this is an Update, not a Create)

**Blueprints self-heal through use.** If a web platform changes its UI and the blueprint's steps fail, the agent adapts in real-time using its existing self-correction capabilities, completes the task, and the refinement phase updates the blueprint with the new selectors/flow. The next execution uses the corrected version automatically. Staleness is not a permanent problem — it's a one-execution cost that the refinement loop fixes.

A blueprint that's been executed 10 times and refined each time is dramatically more reliable than one generated from a single execution.

### 4.5 Retirement — When does a Blueprint get archived?

A blueprint should be retired when:
- Success rate drops below 0.3 over 5+ executions (the procedure no longer works)
- It hasn't been matched in 90 days (the task is no longer relevant)
- The user explicitly deletes it

Retired blueprints are moved to an archive, not deleted — they may contain useful failure mode documentation.

## 5. Storage Architecture

### 5.1 Memory Backend (Primary)

Blueprints are stored as `MemoryEntry` records with a new entry type:

```rust
pub enum MemoryEntryType {
    Conversation,
    LongTerm,
    DailyLog,
    Skill,
    Knowledge,
    Blueprint,     // NEW
}
```

The `content` field holds the full Markdown body. The `metadata` field holds the YAML frontmatter as JSON (for fast querying without parsing Markdown).

```rust
MemoryEntry {
    id: "blueprint:bp-reddit-organic-engagement",
    content: "## Objective\n...",  // Full markdown body
    metadata: json!({
        "type": "blueprint",
        "name": "Reddit Organic Engagement Campaign",
        "version": 3,
        "trigger_patterns": ["reddit", "subreddit", "organic engagement"],
        "task_signature": "browser+web_fetch+shell",
        "semantic_tags": ["social-media", "marketing"],
        "times_executed": 3,
        "success_rate": 0.67,
        "owner_user_id": "12345678",
        "scope": "user",
    }),
    entry_type: MemoryEntryType::Blueprint,
    ..
}
```

### 5.2 Filesystem (Optional Cache)

For fast local access, blueprints can also be cached as `.md` files under `~/.temm1e/blueprints/`:

```
~/.temm1e/blueprints/
  bp-reddit-organic-engagement.md
  bp-deploy-docker-vps.md
  bp-github-issue-triage.md
```

The filesystem copy is a cache — the memory backend is the source of truth. This allows:
- Human inspection and editing of blueprints
- Version control (users can commit their blueprints)
- Offline access when the memory backend is unavailable

## 6. Context Budget — The Token Economics Argument

### Upfront cost vs. wasted cost

A blueprint costs **2,000-4,000 tokens** to load. That sounds expensive — 10% of context.

But consider what happens *without* a blueprint: the agent figures things out from scratch. It tries an approach, fails, rotates strategy, retries. A complex task that the blueprint handles in 25 tool calls might take 47 tool calls without one — each call consuming tokens for the request, the response, and the tool output stored in history.

**The blueprint's context cost is an investment with negative net cost.** It spends 3,000 tokens upfront to save 15,000+ tokens of dead-end exploration, failed attempts, strategy rotations, and self-correction loops. The agent without a blueprint also spends tokens figuring things out — it just spends them *badly*.

### Budget allocation

The context builder (`context.rs`) currently allocates budget as:

| Category | Budget |
|---|---|
| System prompt | Always included |
| Tool definitions | Always included |
| Recent messages | Always kept (30-60) |
| Memory search | 15% |
| Cross-task learnings | 5% |
| Older history | Remainder |

With Blueprints:

| Category | Budget |
|---|---|
| System prompt | Always included |
| Tool definitions | Always included |
| **Active Blueprint** | **Up to 10%** |
| Recent messages | Always kept (30-60) |
| Memory search | 15% |
| Cross-task learnings | **5% (unchanged)** |
| Older history | Remainder |

Learnings budget stays at 5% — they are not reduced. As discussed in Section 7, Learnings serve as ambient breadcrumbs even when a Blueprint is loaded. The 10% Blueprint budget comes from the older history pool, which is the lowest-priority category anyway.

## 7. Relationship to Existing Systems

### Blueprints + Learnings — Two-Layer Procedural Memory

**Both systems always fire. Learnings are never suppressed.**

```
Simple task  → Learning only (quick signal, no Blueprint warranted)
Complex task → Learning (breadcrumb) + Blueprint (full procedure)
```

Learnings and Blueprints serve different roles in the agent's memory:

| | Learning | Blueprint |
|---|---|---|
| **Role** | Ambient signal — always present, cheap | Targeted guide — loaded only on confident match |
| **Context cost** | ~50 tokens per entry (5% budget) | ~1,000-4,000 tokens (10% budget) |
| **Matching** | No matching — all recent learnings are injected | Selective — only loads if task matches above threshold |
| **Value** | "You've done something *like* this before" | "Here's exactly how to do it again" |

**Why keep both on complex tasks?** The Learning acts as a **breadcrumb** even when the
Blueprint doesn't match a future task. Example:

- Agent completes Reddit organic engagement → Blueprint created + Learning created
- Later, user asks "post on Hacker News naturally"
- The Reddit Blueprint doesn't match (different platform, below threshold)
- But the Learning fragment is there: `[OK] browser+web: succeeded using browser → web_fetch → shell. Strategy rotation triggered.`
- That fragment tells the agent: "you've done social media engagement before, browser+web combo worked, and your first approach probably won't — try alternatives early"

The Learning is a low-cost, always-present hint. The Blueprint is the full engineering drawing that only gets pulled when the job is a close match. **They are complementary layers, not redundant.**

### Blueprints + Skills

Skills define **capabilities**. Blueprints document **procedures that use those capabilities**. A blueprint might reference skills:

> "Phase 3 requires the `browser` tool with screenshot capability.
> If the `browser-advanced` skill is loaded, use visual comparison
> for tone calibration instead of text-only analysis."

### Blueprints + Done Criteria

Done criteria define **what DONE looks like**. Blueprints define **how to get there**. They complement each other perfectly:

- Done criteria: "All organic comments visible, ratio matches spec, no account warnings"
- Blueprint: "Here's the 4-phase procedure to achieve that"

A blueprint's **Verification** section can auto-populate Done criteria.

### Blueprints + Task Decomposition

Task decomposition (`task_decomposition.rs`) breaks compound tasks into sub-tasks. When a blueprint is loaded, the decomposition step can **skip the LLM call** and use the blueprint's phases directly as the task graph. This saves tokens and ensures the decomposition matches proven execution patterns.

## 8. The Authoring Prompt

When the agent needs to write a new blueprint, the following prompt is sent with the full conversation history:

```
You have just completed a complex task. Write a Blueprint — a structured,
replayable procedure document — so that a future agent can execute the same
type of task by following your blueprint.

Write the blueprint in Markdown following this exact structure:

## Objective
[One sentence: what does this blueprint accomplish?]

## Prerequisites
[What must be true before starting? Credentials, tools, permissions, state.]

## Phases
[Break the procedure into sequential phases. Each phase has:]
### Phase N: [Name]
**Goal**: [What this phase achieves]
**Steps**: [Numbered, specific, actionable steps]
**Decision points**: [Where choices must be made, and what to choose]
**Failure modes**: [What can go wrong, how to detect it, how to recover]
**Quality gates**: [Conditions that must be true before moving to next phase]

## Failure Recovery
[Table: Scenario | Detection | Response]

## Verification
[Checklist of conditions that prove the task completed correctly]

## Execution Log
### Run 1 — [today's date]
[What happened in this execution: subreddits targeted, results, duration,
tool call count, anything surprising]

CRITICAL RULES:
- Be SPECIFIC. "Navigate to the login page" is useless. "Navigate to
  reddit.com/login, fill #loginUsername, fill #loginPassword, click .login-btn"
  is a blueprint.
- Include TIMING. If you waited between actions, say how long and why.
- Include FAILURE MODES you actually encountered, not theoretical ones.
- Include DECISION POINTS where you had to choose between approaches.
- The goal is REPLAYABILITY. Another agent reading this should be able to
  execute the same task with the same quality, without trial and error.
```

Also provide YAML frontmatter for the header:

```
Suggest appropriate values for:
- id (kebab-case, descriptive)
- name (human-readable title)
- trigger_patterns (5-10 keywords that would identify this type of task)
- task_signature (tool combination used, e.g., "browser+web_fetch+shell")
- semantic_tags (3-5 domain categories)
```

## 9. Implementation Roadmap

### Phase 1: Foundation (MVP)
- Add `Blueprint` entry type to `MemoryEntryType`
- Create `blueprint.rs` module in `temm1e-agent` with:
  - `Blueprint` struct (parsed from Markdown + frontmatter)
  - `should_create_blueprint()` — threshold heuristics
  - `create_blueprint_prompt()` — authoring prompt generator
  - `parse_blueprint()` — Markdown+YAML parser
  - `match_blueprint()` — keyword-based matching
- Wire into runtime: blueprint authoring after DONE (async, non-blocking)
- Wire into context builder: blueprint injection with dedicated budget
- Storage: use existing `Memory::store()` with `MemoryEntryType::Blueprint`

### Phase 2: Refinement Loop
- Add `refine_blueprint_prompt()` — takes original blueprint + execution diff
- Track execution metadata (tool calls, duration, outcome) per blueprint run
- Implement version incrementing and execution log appending
- Add fitness metrics (success_rate, avg_tool_calls)

### Phase 3: Smart Matching
- Semantic similarity matching (embed trigger patterns + task text)
- Task signature matching (tool combination fingerprinting)
- Confidence scoring with configurable threshold
- User confirmation: "I found a blueprint for this task. Follow it? [Y/n]"

### Phase 4: Composability
- Blueprint fragments: reusable sub-procedures (e.g., "Reddit Auth" can be used by multiple Reddit blueprints)
- Blueprint chaining: one blueprint can reference another as a prerequisite phase
- Blueprint inheritance: a specific blueprint can extend a general one

### Phase 5: Distribution
- Blueprint sharing via TemHub (opt-in, anonymized)
- Community-curated blueprints for common tasks
- Blueprint ratings and trust scoring

## 10. Design Principles

1. **Blueprints are earned, not authored.** They come from real execution, not imagination. Every step in a blueprint was actually performed and verified.

2. **Specificity over generality.** A blueprint for "deploy Node.js app to AWS EC2 via Docker" is better than one for "deploy app to cloud." The more specific, the more replayable.

3. **Living documents.** Blueprints are never "done." Every execution is an opportunity to refine. A 10-execution blueprint is dramatically better than a 1-execution blueprint.

4. **Conservative matching.** A wrong blueprint wastes context and misleads the agent. No match is better than a bad match. Keep the threshold high.

5. **Additive to the system.** Blueprints don't replace learnings, skills, or done criteria. They complement them. Each system has its role in the agent's cognitive architecture.

6. **The user owns their blueprints.** Blueprints are personal operational knowledge. They can inspect, edit, export, and delete them. They are never shared without explicit consent.

## 11. Success Metrics

How to measure whether the Blueprint system is working:

- **First-attempt success rate**: Tasks with a matched blueprint should succeed on first attempt more often than tasks without
- **Tool call reduction**: Repeated tasks should require fewer tool calls over time as blueprints mature
- **Time reduction**: Repeated tasks should complete faster
- **Failure mode coverage**: Blueprints should accumulate failure modes over executions, reducing surprise failures
- **User satisfaction**: Users should report that their agent "remembers how to do things"

## 12. Resolved Design Decisions

These questions were raised during design and resolved:

1. **Blueprint context cost** — Resolved: a blueprint costs 2,000-4,000 tokens upfront but *saves* 10,000-20,000 tokens of dead-end exploration. The net token cost is negative. See Section 6 for the full economics argument.

2. **Matching false positives** — Resolved: we trust the model's judgment, same as we trust it for tool selection and task decomposition. The self-correction loop handles bad matches. See Section 4.2.

3. **Blueprint staleness** — Resolved: blueprints self-heal through the CRUD refinement loop. If a step fails because the target platform changed, the agent adapts via self-correction, completes the task, and the refinement phase updates the blueprint. One-execution cost, then fixed. See Section 4.4.

4. **Learning overlap** — Resolved: both systems always fire (Option B). Learnings serve as ambient breadcrumbs even when no Blueprint matches. No suppression, no wasted effort. See Section 7.

## 13. Open Questions

1. **LLM authoring quality**: The blueprint is only as good as the LLM's ability to distill procedure from conversation history. How do we validate blueprint quality before storing? (Possible: dry-run validation pass, or quality gate based on specificity heuristics.)

2. **Multi-user blueprints**: If multiple users of a shared TEMM1E instance perform similar tasks, should their blueprints merge? How to handle conflicting procedures?

3. **Privacy**: Blueprints may contain sensitive operational details (login flows, internal URLs, API patterns). How do we handle blueprint storage encryption and access control?

4. **Partial loading**: For very long blueprints (>4,000 tokens), should we support loading only the current phase? This would reduce context cost but requires phase-tracking state.
