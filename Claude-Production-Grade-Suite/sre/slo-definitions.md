# TEMM1E SLO Definitions

> Service Level Objectives for the TEMM1E cloud-native Rust AI agent runtime.
> Owner: SRE | Last updated: 2026-03-08 | Review cadence: quarterly

---

## 1. Message Processing

### SLI
**Metric:** `temm1e_message_processing_duration_seconds`
- Histogram measuring the time from `InboundMessage` receipt at the gateway router (`route_message`) to `OutboundMessage` return, **excluding** upstream AI provider latency.
- Labels: `channel` (telegram, discord, slack, whatsapp, cli), `status` (success, error), `tool_rounds` (0..10).

### SLO
| Target | Window | Budget |
|--------|--------|--------|
| p50 < 50 ms | 30-day rolling | N/A |
| p99 < 100 ms | 30-day rolling | 1% of requests may exceed |
| Success rate >= 99.5% | 30-day rolling | 0.5% error budget (~2.16 h/month) |

### Error Budget Policy
- Budget < 50% remaining: halt non-critical deployments; page on-call.
- Budget < 25% remaining: freeze all changes; dedicate engineering to reliability.
- Budget exhausted: incident review required before any new feature work resumes.

### Measurement
- Source: `Observable::observe_histogram` calls in `AgentRuntime::process_message`.
- Provider call time is subtracted using a nested span (`provider_complete_duration_seconds`).
- Errors counted: any `Temm1eError` variant returned from `route_message` except `Temm1eError::RateLimited` (which is intentional throttling).

---

## 2. AI Provider Availability

### SLI
**Metric:** `temm1e_provider_request_total` (counter) with labels `provider` (anthropic, openai_compat), `status` (success, error, timeout).

**Metric:** `temm1e_provider_request_duration_seconds` (histogram) with labels `provider`, `model`.

**Metric:** `temm1e_provider_health_check_success` (gauge, 0 or 1) from `Provider::health_check()`.

### SLO
| Target | Window | Budget |
|--------|--------|--------|
| Provider success rate >= 99.0% | 30-day rolling | 1% error budget (~7.2 h/month) |
| p99 provider latency < 30 s | 30-day rolling | 1% |
| Health check pass rate >= 99.5% | 7-day rolling | 0.5% |

### Error Budget Policy
- Provider errors are partially outside TEMM1E's control (upstream dependency).
- When budget < 50%: enable automatic provider fallback if a secondary provider is configured.
- When budget exhausted: raise to upstream provider support; consider switching default model.

### Measurement
- Source: wrapper around `Provider::complete()` and `Provider::stream()`.
- Timeout: any request exceeding 60 s is counted as an error.
- `health_check()` runs every 30 s via heartbeat loop; result published as gauge.

---

## 3. Gateway Uptime

### SLI
**Metric:** `temm1e_gateway_up` (gauge, 0 or 1) derived from synthetic `/health` endpoint probes.

**Metric:** `temm1e_gateway_http_request_duration_seconds` (histogram) with labels `method`, `path`, `status_code`.

**Metric:** `temm1e_gateway_http_requests_total` (counter) with labels `method`, `path`, `status_code`.

### SLO
| Target | Window | Budget |
|--------|--------|--------|
| Availability >= 99.9% | 30-day rolling | 0.1% (~43.2 min/month) |
| `/health` p99 < 10 ms | 30-day rolling | 1% |
| Cold start < 50 ms | Per-startup | Hard requirement |

### Error Budget Policy
- Budget < 50%: page on-call, enable blue-green deployment rollback.
- Budget exhausted: mandatory post-incident review; no deploys until root cause resolved.

### Measurement
- External prober (e.g., Blackbox Exporter) hits `GET /health` every 15 s.
- `HealthResponse.status == "ok"` and HTTP 200 counts as success.
- Cold start measured from process spawn to first successful `/health` response.
- The `SkyGate::start()` bind-to-listen transition time is instrumented.

---

## 4. Memory Operations

### SLI
**Metric:** `temm1e_memory_operation_duration_seconds` (histogram) with labels `backend` (sqlite, postgres), `operation` (store, search, get, delete, list_sessions, get_session_history).

**Metric:** `temm1e_memory_operation_total` (counter) with labels `backend`, `operation`, `status` (success, error).

**Metric:** `temm1e_memory_entries_total` (gauge) -- total entries in the memory store.

### SLO
| Target | Window | Budget |
|--------|--------|--------|
| Search p99 < 50 ms | 30-day rolling | 1% |
| Store/Get/Delete p99 < 20 ms | 30-day rolling | 1% |
| Success rate >= 99.9% | 30-day rolling | 0.1% |

### Error Budget Policy
- Budget < 50%: investigate query plans (SQLite) or connection pool exhaustion (Postgres).
- Budget exhausted: trigger memory backend migration review.

### Measurement
- Source: wrapper around `Memory` trait methods in `SqliteMemory` and future Postgres implementation.
- `SqlitePoolOptions::max_connections(5)` -- pool saturation is a leading indicator.
- Hybrid search scoring via `hybrid_search()` is included in search latency.

---

## 5. Vault Operations

### SLI
**Metric:** `temm1e_vault_operation_duration_seconds` (histogram) with labels `backend` (local-chacha20, aws-kms), `operation` (store_secret, get_secret, delete_secret, list_keys, resolve_uri).

**Metric:** `temm1e_vault_operation_total` (counter) with labels `backend`, `operation`, `status` (success, error).

**Metric:** `temm1e_vault_decryption_failures_total` (counter) -- specifically tracks ChaCha20-Poly1305 decryption failures which may indicate key corruption.

### SLO
| Target | Window | Budget |
|--------|--------|--------|
| Operation p99 < 10 ms | 30-day rolling | 1% |
| Success rate >= 99.99% | 30-day rolling | 0.01% (~4.3 min/month) |
| Zero decryption failures due to key corruption | Continuous | Zero tolerance |

### Error Budget Policy
- Any decryption failure: immediate P1 incident; potential key rotation required.
- Budget < 50%: audit vault.key file permissions (must be 0600) and vault.enc integrity.
- Budget exhausted: disable vault writes until investigation completes.

### Measurement
- Source: wrapper around `Vault` trait methods in `LocalVault`.
- Decrypt failures are specifically instrumented separately from general errors.
- `vault.key` file permission checks run on startup and via heartbeat.

---

## 6. File Transfer

### SLI
**Metric:** `temm1e_file_transfer_duration_seconds` (histogram) with labels `channel`, `direction` (receive, send), `mime_type`.

**Metric:** `temm1e_file_transfer_bytes_total` (counter) with labels `channel`, `direction`.

**Metric:** `temm1e_file_transfer_total` (counter) with labels `channel`, `direction`, `status` (success, error).

**Metric:** `temm1e_file_transfer_size_bytes` (histogram) -- file size distribution.

### SLO
| Target | Window | Budget |
|--------|--------|--------|
| Transfer success rate >= 99.0% | 30-day rolling | 1% |
| p99 transfer time < 5 s (files < 10 MB) | 30-day rolling | 1% |
| p99 transfer time < 30 s (files < 50 MB) | 30-day rolling | 1% |

### Error Budget Policy
- Budget < 50%: review channel-specific file size limits via `FileTransfer::max_file_size()`.
- Budget exhausted: disable file transfers on failing channels; route through filestore fallback.

### Measurement
- Source: `FileTransfer::receive_file()`, `FileTransfer::send_file()`, `FileTransfer::send_file_stream()`.
- Path traversal rejections from `save_received_file()` are counted as security events, not SLO errors.
- Streaming transfers (`send_file_stream`) track progress and duration.

---

## 7. Session Management

### SLI
**Metric:** `temm1e_active_sessions` (gauge) -- current session count from `SessionManager::session_count()`.

**Metric:** `temm1e_session_operation_duration_seconds` (histogram) with labels `operation` (get_or_create, update, remove).

### SLO
| Target | Window | Budget |
|--------|--------|--------|
| Concurrent sessions >= 5 without degradation | Continuous | Hard requirement |
| Session operation p99 < 5 ms | 30-day rolling | 1% |

### Measurement
- `SessionManager` RwLock contention is a leading indicator.
- Memory usage per session tracked via `process_resident_memory_bytes`.

---

## 8. Tool Execution

### SLI
**Metric:** `temm1e_tool_execution_duration_seconds` (histogram) with labels `tool_name`, `status` (success, error, sandbox_violation).

**Metric:** `temm1e_tool_execution_total` (counter) with same labels.

**Metric:** `temm1e_tool_rounds_per_message` (histogram) -- number of tool-use rounds per message (max 10 per `MAX_TOOL_ROUNDS`).

### SLO
| Target | Window | Budget |
|--------|--------|--------|
| Tool success rate >= 98.0% | 30-day rolling | 2% |
| Sandbox violation rate < 0.1% | 30-day rolling | 0.1% |
| p99 single tool execution < 30 s | 30-day rolling | 1% |

### Measurement
- Source: `execute_tool()` in `temm1e-agent/src/executor.rs`.
- Sandbox violations from `validate_sandbox()` are counted separately.
- `MAX_TOOL_ROUNDS` (10) exhaustion is tracked as a warning event.

---

## Summary Table

| Service | Availability SLO | Latency SLO (p99) | Error Budget (30d) |
|---------|-----------------|-------------------|-------------------|
| Message Processing | 99.5% | < 100 ms | 0.5% |
| Provider Availability | 99.0% | < 30 s | 1.0% |
| Gateway Uptime | 99.9% | < 10 ms | 0.1% |
| Memory Operations | 99.9% | < 50 ms (search) | 0.1% |
| Vault Operations | 99.99% | < 10 ms | 0.01% |
| File Transfer | 99.0% | < 5 s | 1.0% |
| Session Management | 99.9% | < 5 ms | 0.1% |
| Tool Execution | 98.0% | < 30 s | 2.0% |
