# TEMM1E Capacity Baseline

> Resource requirements, scaling triggers, and bottleneck analysis for TEMM1E runtime.
> Owner: SRE | Last updated: 2026-03-08

---

## 1. Binary Footprint

| Metric | Target | Measured (Expected) | Notes |
|--------|--------|-------------------|-------|
| Binary size | < 10 MB | ~5-8 MB (release, stripped) | Single statically-linked binary |
| Cold start | < 50 ms | ~15-30 ms | TcpListener bind + SQLite init + vault key load |
| Idle RSS | < 20 MB | ~8-15 MB | axum runtime + SQLite pool + vault cache |

### Cold Start Breakdown
| Phase | Expected Duration | Component |
|-------|------------------|-----------|
| Process spawn | 1-2 ms | OS |
| Config load (TOML parse) | 1-3 ms | `config::loader` |
| SQLite connect + schema init | 5-10 ms | `SqliteMemory::new()` |
| Vault key load + cache init | 2-5 ms | `LocalVault::with_dir()` |
| axum Router build | 1-2 ms | `SkyGate::build_router()` |
| TcpListener bind | 1-3 ms | `SkyGate::start()` |
| **Total** | **~11-25 ms** | Well within 50 ms target |

---

## 2. Resource Per Session

Each active session (`SessionContext` in the `SessionManager` HashMap) consumes:

| Resource | Estimate | Formula |
|----------|----------|---------|
| Base struct | ~256 bytes | session_id + channel + chat_id + user_id + workspace_path |
| History (per message) | ~1-4 KB | `ChatMessage` with text or `ContentPart` variants |
| History (typical session, 20 msgs) | ~40-80 KB | 20 messages x 2-4 KB average |
| History (heavy session, 100 msgs) | ~200-400 KB | 100 messages with tool results |
| HashMap overhead | ~64 bytes | Key + bucket pointer |
| **Total (typical)** | **~40-80 KB** | Per active session |

### Concurrent Session Scaling

| Sessions | Estimated Memory | Notes |
|----------|-----------------|-------|
| 1 | 15-16 MB | Idle + 1 session |
| 5 (target) | 16-18 MB | Within 20 MB idle target |
| 10 | 17-20 MB | Marginal increase |
| 50 | 22-28 MB | History accumulation dominates |
| 100 | 30-50 MB | May need session eviction |
| 500 | 80-200 MB | Requires session sharding or persistence |

### Session Memory Limit Recommendation
- Implement session history truncation at 50 messages (FIFO) to cap per-session memory at ~200 KB.
- Implement session idle timeout at 30 minutes to prevent unbounded HashMap growth.
- For > 100 concurrent sessions, persist session history to SQLite and load on demand.

---

## 3. Per-Component Resource Profile

### 3.1 Gateway (axum)

| Resource | Idle | Per Request | Notes |
|----------|------|-------------|-------|
| Memory | ~2 MB | ~1-10 KB | axum allocations per request |
| CPU | negligible | ~0.1 ms | Route matching + JSON serialization |
| File descriptors | 1 (listener) | 1 per connection | TCP sockets |
| Tokio tasks | 1 (accept loop) | 1 per request | Short-lived |

**Concurrency limit:** Bounded by tokio runtime thread pool (default: CPU cores) and file descriptor limit.

### 3.2 Provider (HTTPS Client)

| Resource | Idle | Per Request | Notes |
|----------|------|-------------|-------|
| Memory | ~1 MB (connection pool) | ~10 KB - 1 MB | Depends on response size |
| CPU | negligible | ~1-5 ms | TLS handshake (first), JSON parse |
| Network | 0 | ~1-100 KB out, 1 KB-1 MB in | Prompt + response sizes |
| Duration | N/A | 1-30 s typical | Upstream dependent |

**Connection pool:** HTTP/2 with keep-alive. Single connection per provider handles multiple concurrent requests via multiplexing.

### 3.3 Memory (SQLite)

| Resource | Idle | Per Operation | Notes |
|----------|------|---------------|-------|
| Memory | ~3-5 MB | ~1-50 KB | Page cache + WAL |
| Disk | Schema only (~4 KB) | ~0.5-2 KB/entry | Depends on content length |
| File descriptors | 2-3 | 0 (pooled) | DB file + WAL + SHM |
| CPU | negligible | ~0.1-5 ms | LIKE search is O(n) |

**Pool capacity:** `max_connections = 5`. At 5 concurrent operations, new requests block on pool acquisition.

**Disk growth estimate:**
| Entries | DB Size | Search Latency |
|---------|---------|---------------|
| 1,000 | ~1 MB | < 5 ms |
| 10,000 | ~10 MB | < 20 ms |
| 100,000 | ~100 MB | 20-50 ms (SLO boundary) |
| 1,000,000 | ~1 GB | > 100 ms (SLO breach) |

### 3.4 Vault (LocalVault)

| Resource | Idle | Per Operation | Notes |
|----------|------|---------------|-------|
| Memory | ~10-100 KB | ~1-5 KB | In-memory HashMap cache |
| Disk | 32 bytes (key) + vault.enc | ~100-500 bytes per secret | JSON-encoded ciphertexts |
| CPU | negligible | ~0.1-1 ms | ChaCha20-Poly1305 encrypt/decrypt |
| File I/O | 0 | 1 write per mutation | Full vault.enc flush on every store/delete |

**Vault.enc flush bottleneck:** Every `store_secret` and `delete_secret` rewrites the entire `vault.enc` file. At > 500 keys, this becomes a latency concern.

### 3.5 Channels

| Channel | Memory (Idle) | Memory (Active) | Network | Notes |
|---------|--------------|-----------------|---------|-------|
| Telegram | ~1 MB | ~2-5 MB | Long-poll connection | `getUpdates` long-poll |
| Discord | ~2 MB | ~3-8 MB | WebSocket | Persistent gateway connection |
| Slack | ~1 MB | ~2-5 MB | WebSocket | Socket Mode |
| WhatsApp | ~1 MB | ~2-5 MB | Webhook | Inbound HTTP |
| CLI | ~0.1 MB | ~0.5 MB | None | stdin/stdout |

### 3.6 Tool Execution

| Tool | Memory | CPU | I/O | Duration |
|------|--------|-----|-----|----------|
| Shell | ~1-10 MB (child process) | Unbounded | subprocess | 0.1 s - 60 s |
| File ops | ~1-5 MB | Low | Disk | < 1 s |
| Browser | ~50-200 MB (headless) | High | Network | 1-30 s |
| Git | ~5-20 MB | Medium | Disk + network | 0.5-10 s |
| HTTP | ~1-5 MB | Low | Network | 0.1-30 s |

**Browser tool** is the single largest resource consumer. Limit concurrent browser sessions to 2.

---

## 4. Scaling Triggers

### Vertical Scaling (Single Instance)

| Trigger | Threshold | Action |
|---------|-----------|--------|
| RSS > 50 MB sustained 10m | Warning | Investigate session history growth, enable truncation |
| RSS > 100 MB sustained 5m | Critical | Restart with session persistence; investigate leak |
| CPU > 80% sustained 5m | Warning | Reduce concurrent tool executions |
| SQLite search p99 > 50 ms | Warning | Add FTS5 full-text index; consider Postgres migration |
| SQLite DB > 500 MB | Warning | Archive old entries; implement retention policy |
| Open FDs > 80% of limit | Warning | Review connection leaks; increase ulimit |
| Active sessions > 50 | Warning | Enable session idle timeout eviction |

### Horizontal Scaling (Multi-Instance / Cloud Mode)

| Trigger | Threshold | Action |
|---------|-----------|--------|
| Active sessions > 100 | Scale | Deploy additional instances behind load balancer |
| Message rate > 50/s sustained | Scale | Add instances; shard by channel |
| Provider rate limiting | Scale | Distribute across multiple API keys |
| SQLite contention (pool saturation) | Migrate | Switch to PostgreSQL backend |
| Vault.enc flush latency > 10 ms | Migrate | Switch to external KMS (AWS KMS, HashiCorp Vault) |

### Mode-Specific Considerations

| Scenario | Local Mode (127.0.0.1) | Cloud Mode (0.0.0.0, TLS) |
|----------|----------------------|--------------------------|
| Typical sessions | 1-3 | 10-100+ |
| Memory backend | SQLite | PostgreSQL |
| Vault backend | local-chacha20 | AWS KMS / external |
| Scaling strategy | Vertical only | Horizontal with state externalization |
| Session storage | In-memory HashMap | Redis / PostgreSQL |
| File storage | Local filesystem | S3 / GCS |

---

## 5. Bottleneck Analysis

### 5.1 Identified Bottlenecks (Ordered by Severity)

#### B1: Provider Latency (External, Dominant)
- **Impact:** 1-30 s per completion call dominates end-to-end latency.
- **Mitigation:** Provider latency is excluded from message processing SLO. Streaming reduces perceived latency. Provider fallback reduces error impact.
- **Cannot be optimized** within TEMM1E; upstream dependency.

#### B2: Session History Memory Growth (Internal)
- **Impact:** Unbounded history in `SessionContext.history: Vec<ChatMessage>` grows linearly with conversation length.
- **Mitigation:** Implement sliding window (50 messages) with summarization. Persist to memory backend.
- **Risk:** Without mitigation, a single 500-message session could consume ~2 MB.

#### B3: SQLite LIKE-Based Search (Internal)
- **Impact:** `search()` uses `WHERE content LIKE ?` which is O(n) full table scan.
- **Current v0.1:** Acceptable for < 100k entries (< 50 ms).
- **Mitigation path:** Add FTS5 extension for full-text search. Layer in vector similarity when ready.
- **Risk:** At > 100k entries, search SLO will be breached.

#### B4: Vault Full-File Flush (Internal)
- **Impact:** Every `store_secret` / `delete_secret` serializes and writes the entire `vault.enc`.
- **Current:** Acceptable for < 500 keys (< 10 ms).
- **Mitigation:** Implement append-only log with periodic compaction, or switch to SQLite-backed vault.
- **Risk:** At > 1000 keys, flush latency may breach SLO.

#### B5: SessionManager RwLock Contention (Internal)
- **Impact:** Single `RwLock<HashMap>` for all sessions. Write-lock during `get_or_create_session` blocks all readers.
- **Current:** Acceptable for < 50 concurrent sessions.
- **Mitigation:** Shard by channel (one HashMap per channel), or use `dashmap` for concurrent access.
- **Risk:** At > 100 sessions with high churn, contention becomes measurable.

#### B6: Browser Tool Memory (Internal)
- **Impact:** Headless browser consumes 50-200 MB per instance.
- **Mitigation:** Limit concurrent browser tool sessions to 2. Implement tool execution queue.
- **Risk:** Uncontrolled browser tool usage can push RSS well above 100 MB.

#### B7: Connection Pool Saturation (Internal)
- **Impact:** `SqlitePoolOptions::max_connections(5)` limits concurrent DB operations.
- **Current:** Sufficient for 5+ concurrent channels.
- **Mitigation:** Increase pool size for cloud mode. Switch to Postgres for higher concurrency.
- **Risk:** Under burst load, requests will queue waiting for pool connections.

### 5.2 Bottleneck Priority Matrix

| Bottleneck | Probability | Impact | Effort to Fix | Priority |
|-----------|-------------|--------|---------------|----------|
| B1: Provider Latency | Certain | High | Cannot fix | Accept + SLO exclusion |
| B2: Session Memory | High | Medium | Low | P1 - Implement truncation |
| B3: SQLite LIKE Search | Medium | High | Medium | P2 - Add FTS5 |
| B4: Vault Full Flush | Low | Medium | Medium | P3 - After 500+ keys |
| B5: RwLock Contention | Low | Medium | Low | P3 - After 100+ sessions |
| B6: Browser Tool RSS | Medium | High | Low | P1 - Add concurrency limit |
| B7: Pool Saturation | Low | Medium | Low | P3 - Increase for cloud |

---

## 6. Capacity Planning Checklist

### Pre-Deployment
- [ ] Binary size verified < 10 MB (`ls -la target/release/temm1e`)
- [ ] Cold start verified < 50 ms (measure spawn-to-health)
- [ ] Idle RSS verified < 20 MB (no active sessions)
- [ ] SQLite connection pool sized appropriately (5 for local, 20 for cloud)
- [ ] Vault.key permissions set to 0600
- [ ] File descriptor limit reviewed (`ulimit -n`, recommend >= 1024)

### Weekly Review
- [ ] Memory entry count trending (target: < 100k without retention)
- [ ] Vault key count (target: < 500)
- [ ] Peak session count vs. target (5+ local, scaling triggers for cloud)
- [ ] Provider error budget burn rate
- [ ] SQLite DB file size

### Monthly Review
- [ ] SLO error budget burn across all services
- [ ] Capacity trend projections (linear regression on entry count, session count)
- [ ] Browser tool usage patterns (RSS impact)
- [ ] Disk usage growth rate
