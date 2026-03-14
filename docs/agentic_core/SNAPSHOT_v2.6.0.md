# Agentic Core Snapshot — v2.6.0

> Exact implementation snapshot of `crates/temm1e-agent/src/runtime.rs` as of v2.6.0.
> This document captures the full `process_message()` flow so future conversations
> can reference the architecture without re-reading 1500+ lines of code.

## Entry Point

```
AgentRuntime::process_message(
    msg: &InboundMessage,
    session: &mut SessionContext,
    interrupt: Option<Arc<AtomicBool>>,
    pending: Option<PendingMessages>,
    reply_tx: Option<mpsc::UnboundedSender<OutboundMessage>>,
    status_tx: Option<watch::Sender<AgentTaskStatus>>,
    cancel: Option<CancellationToken>,
) -> Result<(OutboundMessage, TurnUsage), Temm1eError>
```

## Phase 1: Message Intake (lines 258-405)

1. **User text extraction** — prioritizes `msg.text`, falls back to attachment descriptions, returns early for empty messages.
2. **Credential detection** — `temm1e_vault::detect_credentials()` scans for API keys. Detected but not stored in plain text.
3. **Vision attachment loading** — reads image files from workspace, base64-encodes, creates `ContentPart::Image` parts.
4. **Vision capability check** — if model doesn't support vision (`model_supports_vision()`), strips images and prepends notice to user text.
5. **History push** — user message appended to `session.history` (as Text or Parts with images).

## Phase 2: Classification (lines 407-556)

1. **Blueprint categories** — `fetch_available_categories()` queries memory for grounded vocabulary.
2. **LLM classifier** — `classify_message()` makes one fast LLM call that classifies AND responds:
   - **Chat** → pushes reply to history, returns immediately (1 API call total).
   - **Stop** → pushes ack to history, returns immediately.
   - **Order** → extracts `blueprint_hint`, sends early ack via `reply_tx`, continues to pipeline.
3. **Fallback** — on classifier error, `ModelRouter::classify_complexity()` provides rule-based classification.
4. **Execution profile** — difficulty maps to `ExecutionProfile` (prompt tier, max iterations, tool output cap, verify mode, use_learn).

## Phase 3: Pre-Loop Setup (lines 558-637)

1. **DONE criteria** — compound tasks get a DONE verification prompt injected as System message.
2. **Task queue** — creates persistent task entry, marks Running.
3. **Blueprint matching** — `fetch_by_category()` using classifier's `blueprint_hint`. `select_best_blueprint()` picks best fit within 10% budget.
4. **Self-correction engine** — `FailureTracker` initialized with `max_consecutive_failures` (default: 2).
5. **Prompted mode setup** — fallback state for models without native tool calling.
6. **Flags** — `send_message_used = false`, `rounds = 0`, `interrupted = false`.

## Phase 4: Tool Loop (lines 638-1444)

The core loop runs until: final text reply, interruption, max rounds, max duration, or budget exhaustion.

### Each iteration:

1. **Guard checks** — interrupt flag, duration limit, round limit.
2. **Context building** — `build_context()` assembles system prompt + history + tools + blueprints + memory + learnings within token budget.
3. **Personality mode injection** — reads `shared_mode` (PLAY/WORK) and prepends mode block.
4. **Prompted mode** — if active, moves tool definitions from API body into system prompt text.
5. **Budget check** — `budget.check_budget()` returns error string if exceeded.
6. **Circuit breaker** — `can_execute()` gates provider calls; tracks success/failure rate.
7. **Provider call** — `provider.complete(request)`.
   - On error with tools: checks if it's a tool-unsupported error → switches to `prompted_mode`, retries.
   - On other errors: records circuit breaker failure, returns error.
8. **Usage tracking** — accumulates `turn_api_calls`, `turn_input/output_tokens`, `turn_cost_usd`.
9. **Response parsing** — separates `ContentPart::Text` and `ContentPart::ToolUse`.
10. **Prompted mode parsing** — if active and no native tool_uses, parses JSON tool calls from text via `parse_tool_call_json()`.

### If no tool calls (final reply):

1. Joins text parts into `reply_text`.
2. **send_message suppression** — if `send_message_used`, clears `reply_text` to avoid duplication.
3. **DONE verification** — appends verification prompt for compound tasks.
4. **History push** — skips if `reply_text` is empty (send_message case).
5. **Cross-task learning** — extracts learnings (skipped for trivial/simple tasks), persists to memory.
6. **Blueprint authoring** — if task warrants it, spawns background `author_blueprint()` call + appends notification to reply.
7. **Blueprint refinement** — if blueprint was loaded, spawns background `refine_blueprint()` call + appends notification.
8. **Task queue** — marks completed.
9. Returns `(OutboundMessage, TurnUsage)`.

### If tool calls present:

1. Records assistant message in history (Parts for native mode, Text for prompted mode).
2. **Tool execution loop** — for each `(tool_use_id, tool_name, arguments)`:
   a. `execute_tool()` runs the tool.
   b. Tracks `send_message_used` flag.
   c. Output capping — V2 uses `compress_tool_output()`, V1 uses safe UTF-8 truncation.
   d. **Self-correction** — tracks failures, injects strategy rotation prompt after N consecutive failures.
   e. **Structured failure** — V2 classifies errors with `classify_tool_failure()`.
   f. Pushes `ContentPart::ToolResult` to `tool_result_parts`.
   g. **Vision injection** — `take_last_image()` from tool, pushes `ContentPart::Image` if model supports vision.
3. **Pending message injection** — drains pending queue for this chat_id, appends to last `ToolResult` (uses `rfind` to skip Image parts).
4. **Verification engine** — appends verification hint to last `ToolResult` (uses `rfind`). Skipped for Trivial/Simple tasks.
5. Pushes tool results to history (as Role::Tool Parts for native, Role::User Text for prompted).
6. **Checkpoint** — persists session history to task queue.

### Loop exit (fallback):

- Interrupted → "Task stopped."
- Max rounds → "I reached the maximum number of tool execution steps."

## Supporting Systems

### AgentRuntime Fields

| Field | Type | Default | Purpose |
|-------|------|---------|---------|
| `max_turns` | usize | 200 | Max conversation turns |
| `max_context_tokens` | usize | 30,000 | Token budget (auto-capped to model limits) |
| `max_tool_rounds` | usize | 200 | Max tool loop iterations |
| `max_task_duration` | Duration | 1800s | Wall-clock task timeout |
| `verification_enabled` | bool | true | Post-action verification hints |
| `max_consecutive_failures` | usize | 2 | Failure threshold for strategy rotation |
| `v2_optimizations` | bool | true | Complexity classification, prompt tiers |
| `parallel_phases` | bool | false | Blueprint DAG parallelism |
| `shared_mode` | Option\<SharedMode\> | None | PLAY/WORK personality |

### model_supports_vision()

Deny-list approach: known text-only models return `false`, unknown models default to `true`.
- GLM: only `v`-suffix models have vision
- MiniMax: all text-only
- GPT-3.x: text-only
- Everything else (Claude, GPT-4+, Gemini, Grok): vision-capable

### Blueprint Authoring/Refinement

Both run in `tokio::spawn` (fire-and-forget):
- `author_blueprint()` — single LLM call with temperature 0.3, max_tokens 4096. LLM can output "SKIP" to decline.
- `refine_blueprint()` — single LLM call, updates body in-place, increments version/execution counters.

### Personality Modes

`mode_prompt_block()` generates PLAY or WORK voice rules:
- **PLAY**: energetic, warm, :3 sparingly, CAPITALIZE for emphasis
- **WORK**: sharp, precise, >:3 strategically, no fluff

## v2.6.0 Changes (this release)

1. **Vision-based browser interaction** — `take_last_image()` on Tool trait, vision injection after tool execution (line 1308-1335), `ToolOutputImage` struct in core.
2. **send_message deduplication** — `send_message_used` flag (line 637, 1221, 951), suppresses redundant final reply.
3. **Empty history skip** — won't push empty assistant messages when send_message cleared reply (line 965-972).
4. **Blueprint notification** — appends italic text to reply after blueprint spawn/refine (lines 1071, 1133).
5. **rfind for injection ordering** — pending messages and verification use `rfind` for ToolResult to skip Image parts (lines 1360, 1383).
6. **Interceptor unlimited output** — `max_tokens: None` on interceptor LLM call (main.rs dispatcher).
7. **Pending queue scoping** — push to pending only when `is_busy=true` (main.rs dispatcher).
8. **Tem identity** — system prompt includes "Your personal nickname is Tem."
