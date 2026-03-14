# TEMM1E Code Audit Report

**Version:** 1.0
**Date:** 2026-03-08
**Auditor:** Security Engineer (T6c)
**Scope:** All source files in `crates/*/src/*.rs` plus `src/main.rs`
**Files Audited:** 54 (53 in crates/ + 1 main.rs)
**Methodology:** OWASP Top 10, Rust-specific security, cryptographic review, STRIDE verification

---

## Executive Summary

The TEMM1E codebase demonstrates good Rust security practices overall: zero `unsafe` blocks, parameterized SQL queries via sqlx, and a reasonable sandbox enforcement model. However, the audit identified **16 findings** across Critical (2), High (4), Medium (6), and Low (4) severities. The most critical issues are the lack of key material zeroization in the vault subsystem and the absence of prompt-injection guardrails in the agent runtime, both of which were predicted by the STRIDE threat model and confirmed by this code-level audit.

---

## Findings Table

### Critical Findings

| ID | File:Line | Severity | Category | Description | Recommendation |
|----|-----------|----------|----------|-------------|----------------|
| CA-01 | `temm1e-vault/src/local.rs:105-114` | **CRITICAL** | Cryptographic / Memory Safety | **Vault key not zeroized after use.** `read_key()` returns `[u8; 32]` which is then used to construct a cipher. Neither the key bytes nor the cipher are zeroized after cryptographic operations. The key material persists in memory indefinitely and may be observable via core dumps, swap, or memory forensics. The same applies to `store_secret()` (line 215-216) and `get_secret()` (line 265-266) where `raw_key` is a stack variable that is never wiped. | Add `zeroize` crate as a dependency. Use `Zeroizing<[u8; 32]>` wrapper for the key, or manually call `zeroize()` on the key bytes after cipher construction. Implement `Drop` with zeroization for any struct holding key material. |
| CA-02 | `temm1e-agent/src/runtime.rs:139-157`, `executor.rs:12-53` | **CRITICAL** | Injection / Elevation of Privilege | **No prompt-injection mitigation for tool execution.** The agent runtime blindly executes whatever tool calls the AI provider returns (line 145: `execute_tool(tool_name, arguments.clone(), &self.tools, session)`). An attacker who can influence the user message (or inject into the conversation context) can cause the AI to invoke tools with arbitrary arguments. The sandbox only checks pre-declared file paths in tool *declarations*, not the actual *arguments* passed at runtime. For example, a shell tool could receive any command string and the sandbox would not inspect it. | Implement argument-level validation in the executor: validate file path arguments against workspace scope, restrict shell command patterns, add a confirmation step for destructive operations. Consider adding a tool allowlist per-session and rate-limiting tool invocations. |

### High Findings

| ID | File:Line | Severity | Category | Description | Recommendation |
|----|-----------|----------|----------|-------------|----------------|
| CA-03 | `temm1e-channels/src/telegram.rs:189-196` | **HIGH** | Information Disclosure | **Bot token exposed in file download URLs.** When downloading file attachments, the bot token is interpolated directly into the URL: `format!("https://api.telegram.org/file/bot{}/{}",  self.token, tg_file.path)`. This URL is passed to `reqwest::get()` which may log it on error, and the token is visible in any network trace or error message. If the URL is ever included in a log, memory dump, or error response, the bot token is compromised. | Use the teloxide built-in file download methods instead of constructing the URL manually. If manual URL construction is necessary, ensure the URL is never logged or included in error messages. Redact the token in any debug/error output. |
| CA-04 | `temm1e-channels/src/telegram.rs:66-79, 288-296` | **HIGH** | Spoofing / Broken Authentication | **Telegram allowlist uses spoofable usernames.** The `check_allowed()` method (line 66-79) and `handle_telegram_message()` (line 288-296) both accept username-based matching: `a.trim_start_matches('@') == uname`. Telegram usernames can be changed by the user at any time, so a blocked user can simply change their username to match an allowed username. Only the numeric user ID (`u.id.0`) is stable and unforgeable. | Remove username-based matching from the allowlist. Only match on numeric Telegram user IDs. Document that allowlist entries must be numeric IDs. Provide a helper command to look up a user's numeric ID. |
| CA-05 | `temm1e-core/src/types/config.rs:81-87` | **HIGH** | Sensitive Data Exposure | **`ProviderConfig` derives `Debug` and `Serialize` with plaintext `api_key` field.** `#[derive(Debug, Clone, Serialize, Deserialize, Default)]` on `ProviderConfig` means that `{:?}` formatting and serialization will emit the API key in cleartext. The `config show` command in `main.rs:141` does `toml::to_string_pretty(&config)` which serializes the entire config including `api_key` to stdout. Any debug log that prints the config struct will also leak the key. | Implement a custom `Debug` for `ProviderConfig` that redacts `api_key`. Use `#[serde(skip_serializing)]` or a custom serializer that masks the key. Redact sensitive fields in the `config show` command. |
| CA-06 | `temm1e-agent/src/executor.rs:56-96` | **HIGH** | Broken Access Control / Sandbox Bypass | **Sandbox validation is based on static declarations, not runtime arguments.** `validate_sandbox()` only inspects `tool.declarations().file_access` -- the *declared* paths a tool says it needs. It does not inspect the actual `arguments` JSON passed to the tool at runtime. A tool that declares access to `"."` (current directory) could receive arguments pointing to `"/etc/passwd"` and the sandbox would not catch it because it only validated the declaration. Furthermore, `canonicalize()` on line 82-84 uses `unwrap_or(abs_path)` which means non-existent paths bypass canonicalization, potentially enabling TOCTOU race conditions with symlinks. | Validate actual file path arguments from the `arguments` JSON, not just declarations. Reject non-existent paths that cannot be canonicalized. Consider using `std::fs::canonicalize` before the path check and rejecting paths that fail canonicalization. Add runtime argument validation hooks to the Tool trait. |

### Medium Findings

| ID | File:Line | Severity | Category | Description | Recommendation |
|----|-----------|----------|----------|-------------|----------------|
| CA-07 | `temm1e-memory/src/sqlite.rs:96` | **MEDIUM** | Injection (SQL) | **Search query string interpolated into LIKE pattern without escaping.** `format!("%{query}%")` on line 96 constructs the LIKE pattern. While the pattern is bound via `?` parameterization (safe from SQL injection), the `query` string is not escaped for LIKE wildcards (`%`, `_`). A user searching for `%` would match all entries, and `_` would match any single character, leading to unexpected search behavior. This is not a SQL injection but a LIKE injection. | Escape LIKE special characters (`%` -> `\%`, `_` -> `\_`) in the query string before constructing the pattern, and add `ESCAPE '\'` to the LIKE clause. |
| CA-08 | `temm1e-vault/src/local.rs:92-98` | **MEDIUM** | Security Misconfiguration | **Vault key file permissions set on best-effort basis with `let _ =`.** On Unix, `set_permissions` is called with `let _ =`, meaning permission-setting failures are silently ignored. If the key file is created with world-readable permissions (e.g., on a filesystem that doesn't support Unix permissions, or due to umask issues), the encryption key is exposed. Furthermore, no permissions are set on the vault file itself (`vault.enc`). | Use `map_err` instead of `let _ =` and log a warning on permission failure. Set restrictive permissions on both `vault.key` and `vault.enc`. Consider using `umask(0o077)` before file creation. Verify permissions on startup and warn if they are too permissive. |
| CA-09 | `temm1e-gateway/src/session.rs:59` | **MEDIUM** | Security Misconfiguration | **Fallback workspace path defaults to `/tmp` or current directory.** `std::env::current_dir().unwrap_or_else(\|_\| "/tmp".into())` means if `current_dir()` fails, the workspace falls back to `/tmp`. This is a world-writable directory, which means the sandbox would allow tools to read/write anywhere under `/tmp`, including files owned by other users. | Require an explicit workspace path in the configuration. If a default is needed, use a user-specific directory like `~/.temm1e/workspace`. Never fall back to `/tmp`. |
| CA-10 | `temm1e-gateway/src/server.rs:56-68` | **MEDIUM** | Security Misconfiguration | **No TLS enforcement despite config support.** The `GatewayConfig` has `tls`, `tls_cert`, and `tls_key` fields, but `server.rs` uses plain `TcpListener::bind` and `axum::serve` without TLS. The TLS configuration is accepted but silently ignored. The default config has `tls: false` and `host: "127.0.0.1"`, which is safe for localhost, but if deployed on `0.0.0.0` without TLS, all traffic (including API keys in provider requests) is unencrypted. | Implement TLS support using `tokio-rustls` when `config.tls == true`. If TLS is configured but cert/key are missing, return a startup error. Log a warning if binding to a non-loopback address without TLS. |
| CA-11 | `temm1e-gateway/src/health.rs:37-62` | **MEDIUM** | Information Disclosure | **Status endpoint exposes internal architecture details without authentication.** `GET /status` returns provider name, channel names, tool names, and memory backend type. This endpoint has no authentication and reveals the system's internal component topology to any network client. An attacker can use this information for targeted attacks. | Add authentication to the `/status` endpoint (e.g., require an API key header). The `/health` endpoint can remain unauthenticated. Consider moving detailed status information behind an admin endpoint. |
| CA-12 | `temm1e-gateway/src/session.rs:10-85` | **MEDIUM** | Denial of Service | **Unbounded session storage with no eviction.** `SessionManager` stores sessions in a `HashMap` with no maximum capacity and no TTL/eviction. An attacker can create unlimited sessions by sending messages from different user_id/chat_id combinations, eventually exhausting memory. Session history also grows unboundedly within each session. | Add a maximum session count with LRU eviction. Add a TTL for inactive sessions (e.g., 1 hour). Limit conversation history length per session. Consider periodic cleanup of stale sessions. |

### Low Findings

| ID | File:Line | Severity | Category | Description | Recommendation |
|----|-----------|----------|----------|-------------|----------------|
| CA-13 | `temm1e-core/src/config/env.rs:4-11` | **LOW** | Information Disclosure | **Environment variable expansion is unbounded.** `expand_env_vars()` replaces `${VAR}` patterns with environment variable values. If a config file contains `${PATH}` or `${HOME}`, these system-level values are expanded and potentially logged or serialized. No allowlist restricts which env vars can be expanded. | Add an allowlist of permitted environment variable prefixes (e.g., `TEMM1E_*`). Log a warning when expanding non-allowlisted variables. |
| CA-14 | `temm1e-vault/src/local.rs:173-183` | **LOW** | Data Integrity | **Non-atomic vault file writes.** `flush()` writes the entire vault contents to `vault.enc` using `tokio::fs::write()`. If the process crashes during the write, the vault file may be truncated or corrupted. There is no write-ahead log or atomic rename pattern. | Use atomic file writes: write to a temporary file first, then `rename()` it to `vault.enc`. This ensures the vault file is always in a consistent state. |
| CA-15 | `temm1e-vault/src/local.rs:20` | **LOW** | Information Disclosure | **`StoredSecret` derives `Debug` and `Clone`.** `#[derive(Debug, Clone, Serialize, Deserialize)]` on `StoredSecret` means the nonce and ciphertext can appear in debug output. While this is not directly exploitable (the data is encrypted), it increases the surface area for information leakage. | Remove `Debug` derive from `StoredSecret` or implement a custom `Debug` that only shows the key name, not the encrypted data. |
| CA-16 | `temm1e-channels/src/telegram.rs:27-28` | **LOW** | Sensitive Data Exposure | **Bot token stored as plaintext `String` in `TelegramChannel` struct.** The `token: String` field holds the Telegram bot token in memory for the lifetime of the channel. Combined with the `Debug` derive being absent (which is good), this is lower risk, but the token should ideally be resolved from the vault at use-time rather than cached. | Consider resolving the bot token from the vault on each use, or using a `Zeroizing<String>` wrapper. At minimum, ensure the struct never derives `Debug`. |

---

## STRIDE Verification

The following STRIDE findings from the threat model were verified in code:

| STRIDE ID | Verified? | Code Location | Notes |
|-----------|-----------|---------------|-------|
| I-V01 (Vault key not zeroized) | **CONFIRMED** | `local.rs:105-114, 215-216, 265-266` | `read_key()` returns raw `[u8; 32]`; no `zeroize` crate in dependencies. See CA-01. |
| E-A01 (Prompt injection -> tool execution) | **CONFIRMED** | `runtime.rs:139-157`, `executor.rs:12-53` | Tool calls are executed without argument-level validation. See CA-02. |
| I-C01 (Telegram bot token in URLs) | **CONFIRMED** | `telegram.rs:189-196` | Token interpolated into download URL. See CA-03. |
| S-C01 (Username-based allowlist bypass) | **CONFIRMED** | `telegram.rs:66-79, 288-296` | Username matching present alongside user_id matching. See CA-04. |

---

## Rust-Specific Security Assessment

### Unsafe Code
- **Result:** Zero `unsafe` blocks across all 53 crate source files. This is excellent.

### Panic Paths
- **Result:** All `.unwrap()` and `.expect()` calls are confined to:
  - Test code (`#[cfg(test)]` modules) -- acceptable
  - Static regex compilation in `detector.rs` (lines 38-103) via `LazyLock` -- these are compile-time-verifiable patterns and will only panic at program start, which is acceptable
  - `env.rs:5` -- one `.expect("invalid regex")` for a static regex, acceptable
- **Non-test production code uses `?` operator and `map_err()` consistently.** This is well done.

### Integer Overflow
- **Result:** No manual arithmetic on user-controlled integers. The `file.size as usize` casts in `telegram.rs` (lines 340, 352, etc.) are from Telegram API responses and could theoretically overflow on 32-bit platforms, but are low risk.

### Memory Safety
- **Result:** No raw pointer manipulation. All cryptographic key material is handled through `[u8; 32]` arrays which are stack-allocated and will be overwritten eventually, but not deterministically (see CA-01).

---

## Cryptographic Review

### ChaCha20-Poly1305 Implementation (`local.rs`)

| Aspect | Assessment | Details |
|--------|-----------|---------|
| Algorithm choice | **Good** | ChaCha20-Poly1305 is an IETF-standard AEAD cipher, appropriate for local secret storage |
| Nonce generation | **Good** | Random 12-byte nonces via `OsRng.fill_bytes()` (line 124-125). Random nonces are safe for ChaCha20 as long as the key is not reused across >2^32 encryptions |
| Key generation | **Good** | 32-byte keys generated via `OsRng.fill_bytes()` (line 86) |
| Key storage | **Adequate** | Key file at `~/.temm1e/vault.key` with 0o600 permissions on Unix (best-effort, see CA-08) |
| AEAD tag verification | **Good** | Handled implicitly by the `chacha20poly1305` crate's `decrypt()` method which returns an error on tag mismatch |
| Key zeroization | **MISSING** | See CA-01. Critical gap. |
| Nonce reuse prevention | **Good** | Random nonces per encryption operation. Collision probability is negligible at normal usage volumes |
| Atomic writes | **Missing** | See CA-14. Non-atomic flush could corrupt vault file |

---

## OWASP Top 10 Assessment Summary

| OWASP Category | Risk Level | Notes |
|----------------|-----------|-------|
| A01 - Broken Access Control | **High** | Sandbox checks declarations not arguments (CA-06); username spoofing (CA-04) |
| A02 - Cryptographic Failures | **Critical** | Key material not zeroized (CA-01) |
| A03 - Injection | **Critical** | Prompt injection to tool execution (CA-02); LIKE injection (CA-07) |
| A04 - Insecure Design | **Medium** | No TLS enforcement (CA-10); unbounded sessions (CA-12) |
| A05 - Security Misconfiguration | **Medium** | Best-effort file permissions (CA-08); /tmp fallback (CA-09) |
| A06 - Vulnerable Components | **Low** | See dependency-audit.md. No known CVEs in locked versions. |
| A07 - Auth Failures | **High** | Username-based allowlist (CA-04); unauthenticated status endpoint (CA-11) |
| A08 - Data Integrity Failures | **Low** | Non-atomic vault writes (CA-14) |
| A09 - Logging & Monitoring | **Medium** | API key in config serialization (CA-05); bot token in URLs (CA-03) |
| A10 - SSRF | **Low** | No user-controlled URLs in server-side requests (provider URLs are config-controlled) |

---

## Positive Security Observations

1. **All SQL queries use parameterized binds** via sqlx `?` placeholders -- no string interpolation into SQL.
2. **File path traversal prevention** in `file_transfer.rs:18-22` strips directory components from received filenames.
3. **Proper error handling** throughout -- `?` operator and `map_err()` used consistently in production code.
4. **No `unsafe` code** anywhere in the codebase.
5. **Sandbox exists and is tested** -- the executor has unit tests for workspace-relative paths, absolute path rejection, and path traversal rejection.
6. **Default config is security-conscious** -- `sandbox: "mandatory"`, `host: "127.0.0.1"`, `file_scanning: true`.
7. **TLS via rustls** (not OpenSSL) is in the dependency tree, avoiding C-library attack surface.
8. **reqwest configured with `rustls-tls`** and `default-features = false` -- no OpenSSL dependency.
