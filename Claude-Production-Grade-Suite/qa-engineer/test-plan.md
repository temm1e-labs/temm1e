# TEMM1E Test Plan

## 1. Strategy

### Test Pyramid

| Layer        | Scope                                      | Tooling                | Target Coverage |
|--------------|--------------------------------------------|------------------------|-----------------|
| Unit         | Individual functions, structs, serde        | `#[test]`, `#[tokio::test]` | >= 80%   |
| Integration  | Cross-crate interactions, DB ops, file I/O  | `tests/` dirs, tmpdir  | >= 60%          |
| E2E          | Full gateway boot, message routing          | axum `TestClient`      | >= 40%          |
| Performance  | Boot time, latency, search speed            | `std::time::Instant`   | Pass/Fail gates |

### Coverage Targets per Crate

| Crate              | Unit | Integration | Notes                                        |
|---------------------|------|-------------|----------------------------------------------|
| temm1e-core        | 90%  | N/A         | Config parsing, type serde, env expansion     |
| temm1e-gateway     | 70%  | 80%         | Health endpoints, session manager CRUD        |
| temm1e-agent       | 80%  | 60%         | Context assembly, sandbox validation          |
| temm1e-providers   | 85%  | 40%         | Request/response serde, SSE parsing (no network) |
| temm1e-channels    | 75%  | 50%         | File transfer sanitization, allowlist logic   |
| temm1e-memory      | 85%  | 80%         | SQLite CRUD, markdown read/write, search scoring |
| temm1e-vault       | 90%  | 90%         | Already 16 tests; extend minimally            |

## 2. Test Categories Mapped to User Stories

### US-1: Multi-channel messaging
- **Unit**: Channel allowlist enforcement (telegram `check_allowed`)
- **Unit**: File transfer filename sanitization
- **Unit**: MIME type detection from extension
- **Integration**: CLI channel file receive/send

### US-2: AI provider integration
- **Unit**: Anthropic request body serialization
- **Unit**: OpenAI-compat request body serialization
- **Unit**: SSE event parsing (Anthropic text delta, tool_use, message_delta)
- **Unit**: SSE event parsing (OpenAI-compat text delta, tool_calls, [DONE])
- **Unit**: Message conversion (both directions)
- **Unit**: Provider factory (`create_provider`)

### US-3: Agent runtime
- **Unit**: Context builder (system prompt, tool defs, memory injection)
- **Unit**: Sandbox violation detection (path traversal, out-of-workspace)
- **Unit**: Tool lookup by name (found, not found)
- **Integration**: Full message processing loop (mock provider + mock memory)

### US-4: Persistent memory
- **Integration**: SQLite store/get/delete/list_sessions/session_history
- **Integration**: Markdown store/get/delete/parse roundtrip
- **Unit**: Hybrid search scoring (TF-IDF, empty query, keyword weight)
- **Unit**: Entry type string conversion roundtrip
- **Performance**: Search over 1000 entries < 50ms

### US-5: Secret management
- **Unit** (existing): Encrypt/decrypt roundtrip, vault URI parsing
- **Unit** (existing): Credential detection (6 provider patterns + generic)
- **Unit**: Vault URI `is_vault_uri` predicate

### US-6: Configuration
- **Unit**: TOML parsing with defaults
- **Unit**: YAML parsing compatibility
- **Unit**: Env variable expansion (`${VAR}`)
- **Unit**: Default config (no file) produces valid struct
- **Unit**: Config serde roundtrip (serialize -> deserialize == identity)

### US-7: Gateway
- **Unit**: Health handler returns 200 + JSON
- **Unit**: Session manager create/get/update/remove/count
- **Unit**: Session key determinism

### Performance Gates (Non-functional)
- **Perf**: Boot time < 50ms (config load + in-memory DB init)
- **Perf**: Memory search < 50ms over 1000 entries
- **Perf**: Message routing latency < 100ms (mock provider)

## 3. Priority Matrix

| Priority | Category                              | Risk if untested           | Est. tests |
|----------|---------------------------------------|----------------------------|------------|
| P0       | Vault encrypt/decrypt, credential det | Data breach                | 16 (done)  |
| P0       | Sandbox violation detection           | Remote code execution      | 4          |
| P0       | File transfer sanitization            | Path traversal attack      | 3          |
| P0       | Config parsing (TOML/YAML/env)        | Startup failure            | 7          |
| P1       | Provider serde (Anthropic, OpenAI)    | API integration failure    | 8          |
| P1       | SSE stream parsing                    | Streaming broken           | 6          |
| P1       | SQLite CRUD                           | Data loss                  | 6          |
| P1       | Session management                    | Lost conversations         | 5          |
| P1       | Gateway health endpoint               | Monitoring blind spot      | 2          |
| P2       | Markdown memory                       | OpenClaw compat broken     | 4          |
| P2       | Hybrid search scoring                 | Poor retrieval quality     | 3 (done)   |
| P2       | Context assembly                      | Bad prompts                | 3          |
| P2       | Type serde roundtrips                 | Serialization bugs         | 5          |
| P3       | Performance gates                     | SLA violations             | 3          |

**Total planned: ~75 test cases across 13 test files**

## 4. Test Utilities

A workspace-level `temm1e-test-utils` crate provides:
- `MockProvider` — returns canned `CompletionResponse`, tracks call count
- `MockMemory` — in-memory `Vec<MemoryEntry>` with basic search
- `MockChannel` — records sent messages, configurable allowlist
- `TestConfigBuilder` — fluent API to build `Temm1eConfig` for tests
- `make_test_entry()` — factory for `MemoryEntry` with sensible defaults
- `make_inbound_msg()` — factory for `InboundMessage`

## 5. Execution

```bash
# Run all tests
cargo test --workspace

# Run with feature gates (no telegram, no discord)
cargo test --workspace --no-default-features

# Run only unit tests (fast)
cargo test --workspace --lib

# Run specific crate
cargo test -p temm1e-core
cargo test -p temm1e-memory
```
