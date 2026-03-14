# Cross-Provider History Sanitization — Design Document

**Date:** 2026-03-13
**Status:** Approved design, pending implementation
**Affected crates:** `temm1e-agent` (context.rs, history_pruning.rs), `temm1e-providers` (openai_compat.rs)

---

## Problem Statement

When a user switches AI providers mid-conversation (e.g., Anthropic → Gemini → OpenAI), the persisted conversation history contains tool_call/tool_result messages formatted for the original provider. These cause failures:

1. **Gemini**: `function_response.name: Name cannot be empty` — tool result messages lack the `name` field
2. **Gemini**: `function call turn comes immediately after a user turn or after a function response turn` — strict message ordering violated
3. **Anthropic → OpenAI**: Field name mismatches (`tool_use_id` vs `tool_call_id` semantics)
4. **Any switch**: Old tool execution context is stale and semantically meaningless to the new provider

### Root Cause

- `ChatMessage` is stored as JSON in SQLite via `serde_json::to_string()`
- No provider info is stored per message — can't tell which provider created it
- No timestamp per message — only Vec ordering
- History is restored as-is on reconnect, then passed to whichever provider is currently active
- The canonical format (`ContentPart::ToolUse/ToolResult`) is provider-agnostic, but each provider's API has different structural requirements for tool messages

---

## Solution: Strip Tool Messages from Older History

### Core Insight

Tool call/result pairs are **ephemeral execution artifacts**, not conversation content. What matters to the LLM is what the user asked and what the assistant concluded — not the raw protocol-level messages from previous sessions.

### Architecture

The codebase already splits history into two buckets in `context.rs`:

```
[older_history]   → history[0..recent_start]       (everything before last 30-60 messages)
[recent_messages] → history[recent_start..]         (last 30-60 messages, ALWAYS kept)
```

**The change:** Strip tool messages from `older_history` only. Recent messages (current session's tool interactions) are untouched.

### What Gets Stripped (from older_history only)

1. `Role::Tool` messages — removed entirely
2. `ContentPart::ToolUse` parts — removed from Assistant messages
3. `ContentPart::ToolResult` parts — removed from any message
4. Assistant messages that become empty after stripping — removed entirely
5. Assistant messages with mixed Text + ToolUse — **Text preserved**, ToolUse stripped, message flattened

### What Is NOT Stripped

- All `recent_messages` (last 30-60) — untouched, full tool protocol preserved
- `Role::User` messages — always kept
- `Role::Assistant` text content — always kept
- `ContentPart::Image` parts — handled separately by existing image stripping
- Chat digest — already skips tool messages by design
- Memory entries, learnings, blueprints — separate system, not affected

---

## Data Flow (with change marked)

```
User message arrives
  │
  ▼
main.rs:2285-2319
  Restore history from SQLite (JSON → Vec<ChatMessage>)
  Full fidelity: all ContentParts preserved (ToolUse, ToolResult, Text, Image)
  │
  ▼
context.rs:204-214
  Split into two buckets:
    older_history = history[0..recent_start]
    recent_messages = history[recent_start..]    ← UNTOUCHED
  │
  ▼
context.rs:377
  group_into_turns(&older_history)
  Tool_call + tool_result paired as ATOMIC units
  Budget pruning drops/keeps entire turns
  │
  ▼
context.rs:398-402
  kept_older = surviving older messages
  │
  ▼
★ NEW: strip_tool_messages_from_older(&mut kept_older)     ← THE CHANGE
  Remove Role::Tool messages
  Remove ToolUse/ToolResult parts from Assistant messages
  Flatten mixed messages (keep Text, strip tool parts)
  Remove empty Assistant messages
  │
  ▼
context.rs:417-424
  build_chat_digest() — already skips tool messages
  │
  ▼
context.rs:426-438
  Assemble final message list:
    summary + digest + blueprints + knowledge + memory + learnings + kept_older + recent
  │
  ▼
context.rs:477-503
  Strip images for non-vision models (existing, unaffected)
  │
  ▼
context.rs:509
  remove_orphaned_tool_results() — safety net for recent messages
  │
  ▼
Provider converts ChatMessage → API format
  Anthropic: cleaner input, fewer rejection risks
  OpenAI-compat: sanitize_tool_ordering() becomes pure safety net
```

---

## Edge Case Analysis

### Case 1: Tool_call in older, tool_result in recent (boundary split)

**Scenario:** Message 29 (older) has ToolUse, Message 30 (recent) has ToolResult.

**After stripping:** Message 29 loses its ToolUse part. Message 30's ToolResult references a tool_use_id that no longer exists.

**Resolution:** `remove_orphaned_tool_results()` at line 509 catches this — removes the orphaned tool_result from recent. This is identical to today's behavior when budget pruning drops old turns.

**Risk: ZERO**

### Case 2: Tool_result in older, tool_call in recent

**Impossible.** Tool_calls always precede tool_results in temporal order.

**Risk: ZERO**

### Case 3: Both tool_call and tool_result in older

**After stripping:** Both removed. No orphans created. Chat digest captures the text summary of what happened.

**Risk: ZERO**

### Case 4: Both tool_call and tool_result in recent

**Untouched.** Recent window is never modified by this change.

**Risk: ZERO**

### Case 5: User says "use the same command as before" (referencing old tools)

**After stripping:** The Assistant's Text parts are preserved: "I ran `ls -la /tmp` and found 3 files." The LLM reads natural language, not raw tool protocol. Chat digest also captures the exchange.

**Risk: ZERO**

### Case 6: Multi-step task spanning the boundary

**Analysis:** Recent window (30-60 messages) covers the active task's tool interactions. Older history is stale context from previous sessions. Chat digest preserves the narrative. This is identical behavior to today when budget pruning drops old turns.

**Risk: ZERO**

### Case 7: Assistant message with Text + ToolUse (mixed content)

**Scenario:**
```rust
ChatMessage {
    role: Assistant,
    content: Parts([
        Text { text: "Let me check that for you" },  // KEEP
        ToolUse { id: "t1", name: "shell", ... },     // STRIP
    ])
}
```

**After stripping:**
```rust
ChatMessage {
    role: Assistant,
    content: Text("Let me check that for you"),  // Flattened
}
```

**Implementation:** Follow the exact same pattern as image stripping (context.rs:477-503):
1. `parts.retain(|p| !matches!(p, ContentPart::ToolUse { .. } | ContentPart::ToolResult { .. }))`
2. If only one Text part remains, flatten `Parts([Text{...}])` → `Text(...)`
3. If no parts remain, mark message for removal

**Risk: ZERO** (pattern already battle-tested in image stripping)

### Case 8: Learning extraction from tool results

**Analysis:** Learnings are extracted by `runtime.rs` DURING execution and stored as separate `MemoryEntry` objects with `entry_type: Learning`. They are injected as System messages in context.rs:317-355. They do NOT come from old history tool messages.

**Risk: ZERO**

### Case 9: Anthropic provider receiving stripped history

**Analysis:** Anthropic gets cleaner history — fewer tool messages means fewer chances for the API to reject malformed tool pairs. The Anthropic provider has NO sanitization of its own (passes tool messages as-is). Stripping stale tool messages is strictly safer.

**Risk: ZERO**

### Case 10: OpenAI-compat provider receiving stripped history

**Analysis:** Cleaner input. The `sanitize_tool_ordering()` fix (added 2026-03-13) becomes a pure safety net for recent messages rather than the primary defense against cross-provider history.

**Risk: ZERO**

### Case 11: Older history becomes empty after stripping

**Analysis:** If all older messages were tool messages, `kept_older` becomes empty. Chat digest still captures user/assistant text. Summary marker still injected if messages were dropped by budget pruning (line 404-415). The final message list has: summary + digest + recent. No structural issue.

**Risk: ZERO**

### Case 12: Image parts in tool-bearing messages

**Analysis:** Image stripping (line 477-503) runs AFTER the proposed change. Images live in User messages, tool parts live in Assistant/Tool messages. No interaction.

**Risk: ZERO**

---

## Implementation Location

**File:** `crates/temm1e-agent/src/context.rs`
**Insert point:** Between lines 402 and 417 (after `kept_older` is built, before chat digest)

```rust
// Strip tool messages from older history — they're stale execution artifacts
// that cause cross-provider format issues and waste tokens. Text parts preserved.
// This follows the same pattern as image stripping (lines 477-503).
strip_tool_messages_from_older(&mut kept_older);
```

### Function Signature

```rust
/// Strip tool execution artifacts from older history messages.
///
/// Removes:
/// - `Role::Tool` messages entirely
/// - `ContentPart::ToolUse` and `ContentPart::ToolResult` parts from other messages
/// - Messages that become empty after part removal
///
/// Preserves:
/// - `ContentPart::Text` parts (the assistant's natural language)
/// - `ContentPart::Image` parts (handled separately by vision stripping)
/// - All `Role::User` messages
fn strip_tool_messages_from_older(messages: &mut Vec<ChatMessage>) {
    // 1. Remove Role::Tool messages entirely
    // 2. Strip ToolUse/ToolResult parts from remaining messages
    // 3. Flatten Parts([Text{...}]) → Text(...) when only text remains
    // 4. Remove messages that become empty
}
```

---

## Defense in Depth (Existing + New)

| Layer | Location | What It Does | Status |
|-------|----------|-------------|--------|
| **Prevention** | context.cs: `strip_tool_messages_from_older()` | Strips stale tool messages before any provider sees them | NEW (this change) |
| **Turn grouping** | context.rs: `group_into_turns()` | Keeps tool_call/result pairs atomic during budget pruning | EXISTING |
| **Orphan removal** | context.rs: `remove_orphaned_tool_results()` | Removes tool_results with missing tool_use_ids | EXISTING |
| **Provider sanitization** | openai_compat.rs: `sanitize_tool_ordering()` | Removes unpaired tool messages at provider level | EXISTING (added 2026-03-13) |
| **Tool name injection** | openai_compat.rs: `build_tool_name_map()` | Adds `name` field to tool results (Gemini requirement) | EXISTING (added 2026-03-13) |
| **Classifier isolation** | llm_classifier.rs | Strips all tool messages from classifier context | EXISTING (added 2026-03-13) |

---

## Testing Strategy

### Unit Tests for `strip_tool_messages_from_older()`

1. **Basic stripping**: Role::Tool messages removed
2. **Mixed assistant message**: Text preserved, ToolUse stripped, message flattened
3. **Pure tool_use assistant message**: Entire message removed (no text to preserve)
4. **User messages untouched**: Role::User with any content parts preserved
5. **Empty input**: No panic on empty Vec
6. **No tool messages**: Messages without tool parts pass through unchanged
7. **Image parts preserved**: ToolUse stripped but Image kept in same message

### Integration Test

1. Build context with history containing old tool messages + recent tool messages
2. Verify old tool messages are stripped
3. Verify recent tool messages are intact
4. Verify `remove_orphaned_tool_results()` handles boundary correctly

---

## Related Changes (Already Implemented 2026-03-13)

These provider-level fixes were implemented as immediate mitigations before the generic solution:

1. **`openai_compat.rs` — `build_tool_name_map()`**: Pre-scans messages, adds `name` to tool results
2. **`openai_compat.rs` — `sanitize_tool_ordering()`**: Removes orphaned tool messages at provider level
3. **`llm_classifier.rs`**: Strips Role::Tool and tool-only assistant messages from classifier history

These remain as defense-in-depth after the generic solution is implemented.

---

## Decision Log

| Date | Decision | Rationale |
|------|----------|-----------|
| 2026-03-13 | Strip from older_history only, not recent | Recent messages contain active session tool interactions that are structurally valid |
| 2026-03-13 | Follow image stripping pattern | Battle-tested pattern in same file, same data structures |
| 2026-03-13 | Keep provider-level sanitization | Defense in depth — generic solution handles 99%, provider level catches edge cases |
| 2026-03-13 | No provider metadata per ChatMessage | Would require schema migration, not worth it when stripping is sufficient |
