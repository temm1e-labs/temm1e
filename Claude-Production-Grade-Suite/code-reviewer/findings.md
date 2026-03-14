# TEMM1E Code Review Findings

**Reviewer:** Code Reviewer (T6b)
**Date:** 2026-03-08
**Files reviewed:** 42 source files across 13 crates (7 implemented + 5 stubs + 1 binary)

---

## F-01: `temm1e-core` config module makes core heavier than intended

**Severity:** Medium
**Category:** Dependency direction
**Location:** `crates/temm1e-core/Cargo.toml`, `crates/temm1e-core/src/config/`

The ADR-001 states core should have "zero external deps beyond serde/async-trait." However, core includes the config loading module (`config/loader.rs`, `config/env.rs`), which pulls in `toml`, `serde_yaml`, `regex`, and `dirs` as dependencies. The config loading logic (file discovery, TOML/YAML parsing, env-var expansion) is operational code, not trait/type definitions.

**Recommendation:** Extract `config/loader.rs` and `config/env.rs` into a separate crate (e.g., the root binary or a new `temm1e-config` crate), keeping only the `Temm1eConfig` struct and related types in core. This would reduce core's dependency count from 13 to approximately 8.

---

## F-02: `temm1e-gateway` depends on `temm1e-agent`, violating strict "impl depends on core only"

**Severity:** Medium
**Category:** Dependency direction
**Location:** `crates/temm1e-gateway/Cargo.toml`

`temm1e-gateway` imports `temm1e-agent` for `AgentRuntime`. This creates a cross-impl-crate dependency:
```
gateway -> agent -> core
```

While this is architecturally reasonable (the gateway routes messages through the agent), it violates the ADR-002 rule that "impl crates depend on core only." This coupling means gateway cannot be compiled without agent, and changes to agent's internal API will break gateway.

**Recommendation:** Define an `AgentRuntime`-like trait in `temm1e-core` that the gateway depends on. The actual `AgentRuntime` struct would implement this trait in `temm1e-agent`, and the binary crate would wire them together via dependency injection.

---

## F-03: Blocking `std::fs` calls in async context

**Severity:** High
**Category:** Async patterns
**Location:**
- `crates/temm1e-core/src/config/loader.rs:29,34` -- `std::fs::read_to_string`
- `crates/temm1e-core/src/config/loader.rs:33` -- `path.exists()` (blocking stat)
- `crates/temm1e-channels/src/cli.rs:89` -- `std::fs::metadata`
- `crates/temm1e-vault/src/local.rs:96` -- `std::fs::Permissions::from_mode` (not blocking itself, but the pattern around it)

The `load_config` function uses synchronous `std::fs::read_to_string` and `path.exists()`. While `load_config` is not itself async, it is called from `main` which runs inside the tokio runtime. In the current call pattern (called once at startup before entering the async event loop), this is acceptable. However, the `cli.rs` line 89 uses `std::fs::metadata` inside an `async move` block that runs on the tokio executor, which is a genuine blocking call in async context.

**Recommendation:**
- `cli.rs:89`: Replace `std::fs::metadata(path)` with `tokio::fs::metadata(path).await` (requires making the closure async-compatible at that point, or moving the check outside the hot path).
- `config/loader.rs`: Either mark it clearly as startup-only (add doc comment), or convert to async using `tokio::fs`.

---

## F-04: `unwrap()` calls in library code (non-test)

**Severity:** Medium
**Category:** Error handling
**Location:**
- `crates/temm1e-core/src/config/env.rs:5` -- `Regex::new(...).expect("invalid regex")`
- `crates/temm1e-vault/src/detector.rs:38-103` -- Multiple `Regex::new(...).unwrap()` inside `LazyLock`
- `crates/temm1e-vault/src/detector.rs:121,135,136` -- `caps.get(N).unwrap()` in regex capture processing

The `expect`/`unwrap` calls on `Regex::new` inside `LazyLock` are acceptable -- these are compile-time-known patterns that cannot fail. However, `caps.get(1).unwrap()` in `detector.rs:121,135,136` operates on user input. While regex capture group 1 is guaranteed to exist if `captures_iter` yields a match, this relies on an implicit invariant. A `?` or explicit error would be more robust.

**Recommendation:**
- The `Regex::new().unwrap()` inside `LazyLock` is acceptable (standard Rust pattern). Consider adding a brief comment explaining why it is safe.
- For `caps.get(N).unwrap()`, replace with `.ok_or_else(|| ...)` or use `if let Some(m) = caps.get(N)` to avoid any risk from regex pattern changes.

---

## F-05: `#[allow(dead_code)]` blanket suppression

**Severity:** Low
**Category:** Code quality
**Location:** `crates/temm1e-providers/src/lib.rs:7` -- `#![allow(dead_code)]`

The crate-level `#![allow(dead_code)]` suppresses all dead-code warnings for the entire `temm1e-providers` crate. This hides legitimate unused code and reduces Rust's ability to detect stale code over time.

**Recommendation:** Remove the crate-level attribute. If specific types or helper functions are intentionally unused (e.g., SSE structs used only in deserialization), annotate them individually with `#[allow(dead_code)]` and a comment explaining why.

---

## F-06: Missing `Default` implementation for `SessionManager`

**Severity:** Low
**Category:** Code quality / API surface
**Location:** `crates/temm1e-gateway/src/session.rs:87-91`

`SessionManager` implements `Default` manually to call `Self::new()`. This is correct but the implementation exists at the bottom of the file. The `new()` method and `Default` impl are equivalent, which is clean. No issue here -- just noting it for completeness.

**Actual finding:** `SessionManager::get_or_create_session` at line 59 uses `std::env::current_dir().unwrap_or_else(|_| "/tmp".into())` for the workspace path. This hardcodes `/tmp` as a fallback, which is platform-specific (Windows has no `/tmp`). Since the project targets macOS/Linux, this is a Low concern.

---

## F-07: `Temm1eError` does not implement `From` for `sqlx::Error`

**Severity:** Medium
**Category:** Error handling
**Location:** `crates/temm1e-core/src/types/error.rs`

`Temm1eError` implements `From<serde_json::Error>` and `From<std::io::Error>` via `#[from]`, but does not have a `From<sqlx::Error>` variant. This means every sqlx call in `temm1e-memory` requires a manual `.map_err()`. This is not inherently wrong (it keeps core independent of sqlx), but it leads to verbose and repetitive error mapping throughout `sqlite.rs`.

**Recommendation:** This is acceptable for keeping `temm1e-core` free of sqlx dependency. Consider a helper function in `temm1e-memory` (e.g., `fn sql_err(e: sqlx::Error) -> Temm1eError`) to reduce boilerplate.

---

## F-08: Credential detection not wired into message pipeline

**Severity:** High
**Category:** Trait implementation correctness / feature completeness
**Location:** `crates/temm1e-vault/src/detector.rs`, `crates/temm1e-agent/src/runtime.rs`

ADR-004 specifies credential detection must run "on every incoming message." The `detect_credentials` function is implemented and well-tested in `temm1e-vault`, but it is never called from:
- `AgentRuntime::process_message`
- `route_message` in gateway
- `handle_telegram_message` in channels
- Any other message processing code

Credentials sent via chat messages will pass through the system undetected and unencrypted.

**Recommendation:** Integrate `detect_credentials` into the message processing pipeline, likely in `AgentRuntime::process_message` or `route_message`, before passing the message to the provider. Detected credentials should be stored via the `Vault` trait and stripped/masked from the message before logging.

---

## F-09: Dual `entry_type_to_str` / `str_to_entry_type` helper functions

**Severity:** Low
**Category:** Code quality (DRY violation)
**Location:**
- `crates/temm1e-memory/src/sqlite.rs:228-246`
- `crates/temm1e-memory/src/markdown.rs:300-319`

Both `sqlite.rs` and `markdown.rs` define identical private helper functions `entry_type_to_str` and `str_to_entry_type`. This is a minor DRY violation.

**Recommendation:** Move these helpers to a shared location within `temm1e-memory` (e.g., the `lib.rs` or a new `util.rs` module), or consider adding `Display` / `FromStr` implementations on `MemoryEntryType` in `temm1e-core`.

---

## F-10: OpenAI-compat Tool message conversion only handles first ToolResult

**Severity:** Medium
**Category:** Trait implementation correctness
**Location:** `crates/temm1e-providers/src/openai_compat.rs:237-253`

When converting `MessageContent::Parts` for a `Role::Tool` message, the code handles only the first `ToolResult` part it finds:
```rust
for part in parts {
    if let ContentPart::ToolResult { tool_use_id, content, .. } = part {
        return Ok(serde_json::json!({
            "role": "tool",
            "tool_call_id": tool_use_id,
            "content": content,
        }));
    }
}
```

The OpenAI API expects one `tool` message per tool_call_id. If the agent runtime sends multiple `ToolResult` parts in a single `ChatMessage`, only the first will be sent to the API. The agent runtime (`runtime.rs:160-163`) does in fact bundle multiple tool results into a single `ChatMessage` with `MessageContent::Parts(tool_result_parts)`.

**Recommendation:** In `convert_message_to_openai`, when encountering a `Role::Tool` message with multiple `ToolResult` parts, either:
1. Return a `Vec<serde_json::Value>` and flatten into the messages array (requires changing the function signature), or
2. Change the agent runtime to emit one `ChatMessage` per tool result.

---

## F-11: Telegram `msg.date` timestamp may not have timezone

**Severity:** Low
**Category:** Trait implementation correctness
**Location:** `crates/temm1e-channels/src/telegram.rs:318`

`InboundMessage.timestamp` is typed as `chrono::DateTime<chrono::Utc>`, but `msg.date` from teloxide is `chrono::DateTime<Utc>`, so this should be fine. No actual issue -- the types align.

---

## F-12: `Serialization` error variant uses `#[from]` for `serde_json::Error`

**Severity:** Low
**Category:** Error handling
**Location:** `crates/temm1e-core/src/types/error.rs:44-45`

This means any `serde_json::Error` anywhere in the codebase can be auto-converted to `Temm1eError::Serialization` via `?`. This is convenient but slightly imprecise -- a JSON error during provider response parsing would show up as "Serialization error" rather than "Provider error". The current code correctly uses `.map_err()` in most places where a more specific error is appropriate, so this is just a minor ergonomics note.

---

## F-13: `types` module re-exports sub-modules but not their contents

**Severity:** Low
**Category:** Public API surface
**Location:** `crates/temm1e-core/src/types/mod.rs`, `crates/temm1e-core/src/lib.rs`

`lib.rs` does `pub use types::*;` which re-exports the sub-modules (`message`, `file`, `config`, `session`, `error`) but not their individual types. This means consumers must write `temm1e_core::types::error::Temm1eError` or `temm1e_core::error::Temm1eError` rather than `temm1e_core::Temm1eError`.

The `traits/mod.rs` does `pub use provider::*;` etc., so trait names are available at `temm1e_core::Provider`. But type names require the module path.

This is inconsistent: traits are importable as `temm1e_core::Provider` but types require `temm1e_core::types::error::Temm1eError`. Some crates use `temm1e_core::error::Temm1eError` (memory), others use `temm1e_core::types::error::Temm1eError` (agent, gateway, providers, channels, vault).

**Recommendation:** Add `pub use error::Temm1eError;` and similar re-exports in `types/mod.rs` to allow `temm1e_core::Temm1eError` directly. Alternatively, standardize the import path across all consuming crates.

---

## F-14: `main.rs` Commands are mostly stubs (TODO markers)

**Severity:** Low
**Category:** Code quality
**Location:** `src/main.rs:95-153`

The `Commands::Start`, `Commands::Chat`, `Commands::Migrate`, and all `SkillCommands` contain TODO comments and placeholder `println!` statements. The wiring between the binary and the impl crates is not yet connected -- `temm1e-gateway::SkyGate` is not instantiated, providers/channels are not created from config, etc.

This is expected at v0.1, but the presence of `println!` for user-facing output (instead of structured tracing or proper CLI output) should be cleaned up before release.

---

## F-15: Naming conventions follow Rust standards

**Severity:** N/A (no finding)
**Category:** Naming conventions

All struct names use PascalCase. All function/method names use snake_case. Module names use snake_case. Crate names use kebab-case (Cargo standard). Constants use SCREAMING_SNAKE_CASE. Enum variants use PascalCase. No violations found.

---

## F-16: `ProviderConfig.api_key` stored as plaintext `Option<String>`

**Severity:** Medium
**Category:** Code quality / security concern
**Location:** `crates/temm1e-core/src/types/config.rs:82-87`

`ProviderConfig` has `pub api_key: Option<String>`. The config loader reads this from disk (or env vars) and holds it in memory as plaintext. The ADR-005 says "plaintext secrets never touch disk." If the TOML config file contains `api_key = "sk-ant-..."`, it is plaintext on disk.

The vault URI scheme (`vault://temm1e/key`) exists to address this, but nothing in the config loading path resolves vault URIs. A user who sets `api_key = "vault://temm1e/anthropic_api_key"` would send the literal string "vault://..." as their API key.

**Recommendation:** Add vault URI resolution in the provider creation path. When `api_key` starts with `vault://`, resolve it via the Vault trait before using it. (Note: not reviewing security depth per T6a scope, but this is a code-quality/architecture gap.)

---

## F-17: `telegram.rs` empty allowlist means "allow all"

**Severity:** Medium
**Category:** Code quality (ADR-005 conformance nuance)
**Location:** `crates/temm1e-channels/src/telegram.rs:67-79`, `crates/temm1e-channels/src/telegram.rs:288-297`

ADR-005 states "Empty allowlist = deny all. No exceptions." However, `TelegramChannel::check_allowed` returns `true` if the allowlist is empty:
```rust
if self.allowlist.is_empty() {
    return true;
}
```

The same logic exists in `handle_telegram_message`:
```rust
if !allowlist.is_empty() { ... check ... }
```

This means an empty allowlist permits all users, contradicting the ADR. The `ChannelConfig.allowlist` also defaults to an empty `Vec<String>`.

**Recommendation:** Invert the logic: if `allowlist.is_empty()`, deny by default. This requires users to explicitly configure at least one allowed user/chat. Add a special sentinel value (e.g., `"*"`) if "allow all" is intentionally desired for development.

---

## Findings Summary

| Severity | Count | IDs |
|----------|-------|-----|
| Critical | 0 | -- |
| High | 2 | F-03, F-08 |
| Medium | 6 | F-01, F-02, F-07, F-10, F-16, F-17 |
| Low | 5 | F-05, F-06, F-09, F-13, F-14 |
| **Total** | **13** | |

### Key Themes

1. **Integration gaps** (F-08, F-16): Core features (credential detection, vault URI resolution) are implemented but not wired into the runtime pipeline.
2. **Dependency direction** (F-01, F-02): Core is slightly heavier than ADR intended; gateway has a cross-impl dependency on agent.
3. **Async discipline** (F-03): One genuine blocking-in-async call; config loading is startup-only but should be documented.
4. **Error handling** is generally strong -- `Temm1eError` is used consistently, `unwrap()` in lib code is limited to known-safe patterns (regex literals) and regex captures.
5. **Code quality** is high -- naming conventions, documentation, module organization, and API design all follow Rust idioms well.
