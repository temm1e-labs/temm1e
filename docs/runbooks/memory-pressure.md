# Runbook: Memory Pressure

> **Alert:** `ProcessMemoryHigh` / `MemoryBackendDown` / `SessionCountHigh`
> **Severity:** Critical / Warning
> **Service:** runtime / memory / sessions
> **Response Time:** < 5 minutes (Critical), < 30 minutes (Warning)
> **Last Updated:** 2026-03-08

---

## Symptoms and Detection

### Triggering Alerts

| Alert | Condition | Duration | Severity |
|-------|-----------|----------|----------|
| `ProcessMemoryHigh` | Process RSS > 100 MB | 5 minutes | Critical |
| `MemoryBackendDown` | Memory backend error rate > 10% | 2 minutes | Critical |
| `ProcessMemoryElevated` | Process RSS > 50 MB | 10 minutes | Warning |
| `MemorySearchSlow` | Search p99 > 50ms | 5 minutes | Warning |
| `MemoryStoreSlow` | Store p99 > 20ms | 5 minutes | Warning |
| `MemoryEntriesHigh` | Memory entries > 100k | 1 hour | Warning |
| `MemoryPoolSaturation` | 4/5 SQLite pool connections active | 5 minutes | Info |
| `SessionCountHigh` | Active sessions > 50 | 5 minutes | Warning |

### Observable Symptoms

- PagerDuty incident fires for `ProcessMemoryHigh` (Critical).
- Slack #alerts for elevated memory or session warnings.
- `process_resident_memory_bytes` gauge is above threshold.
- Application responses become slow due to GC pressure or swap.
- OOM killer may terminate the process (check `dmesg`).
- SQLite operations slow down due to connection pool contention.
- Session operations become slow due to `RwLock` contention.

---

## Impact Assessment

| Dimension | Impact |
|-----------|--------|
| **User-facing** | Increased latency on all operations. In severe cases, OOM kill causes full outage. |
| **SLO burn** | Memory Operations SLO (99.9%, p99 < 50ms) directly threatened. Gateway Uptime SLO (99.9%) at risk if OOM kills the process. |
| **Blast radius** | All channels and operations are affected. Memory is a shared resource. |
| **Data loss risk** | Moderate. If OOM kills the process, all in-memory session state (SessionManager HashMap) is lost. Persisted data (SQLite, vault.enc) is safe. |

---

## Step-by-Step Diagnosis

### Step 1: Measure current memory usage

```bash
# Process RSS (Resident Set Size)
ps -o rss= -p $(pgrep temm1e) | awk '{print $1/1024 " MB"}'

# Detailed memory map
cat /proc/$(pgrep temm1e)/smaps_rollup

# Prometheus metric
curl http://localhost:8080/metrics | grep process_resident_memory_bytes
```

Reference thresholds from capacity baseline:
- Idle target: < 20 MB
- Warning: > 50 MB
- Critical: > 100 MB

### Step 2: Identify the memory consumer

Check each component's contribution:

#### a) Session count and history size

```bash
# Active sessions
curl http://localhost:8080/metrics | grep temm1e_active_sessions

# Estimate: each session with 20 messages = ~40-80 KB
# 50 sessions x 80 KB = ~4 MB
# 100 sessions x 80 KB = ~8 MB
# 100 sessions x 400 KB (heavy) = ~40 MB
```

If `temm1e_active_sessions` is high (> 50), session history accumulation is likely the cause.

#### b) Memory backend (SQLite) usage

```bash
# Database file size
ls -lh ~/.temm1e/memory.db

# Entry count
sqlite3 ~/.temm1e/memory.db "SELECT COUNT(*) FROM memories;"

# SQLite memory usage (page cache)
sqlite3 ~/.temm1e/memory.db "PRAGMA page_count; PRAGMA page_size;"

# Connection pool utilization
curl http://localhost:8080/metrics | grep temm1e_memory_pool_active_connections
```

Reference: SQLite page cache typically uses 3-5 MB, but can grow with large databases.

#### c) Browser tool memory

```bash
# Check for headless browser processes
ps aux | grep -i "chrom\|firefox\|headless" | grep -v grep

# Each browser instance: 50-200 MB
```

Browser tool is the single largest memory consumer per the capacity baseline.

#### d) Vault cache size

```bash
# Vault key count (each cached in-memory HashMap)
curl http://localhost:8080/metrics | grep temm1e_vault_keys_total

# Vault file size
ls -lh ~/.temm1e/vault.enc
```

Vault cache is typically < 100 KB unless storing many large secrets.

#### e) Provider HTTP client connection pool

```bash
# HTTP/2 connections consume ~1 MB per provider
curl http://localhost:8080/metrics | grep provider
```

### Step 3: Check for memory leaks

```bash
# Track RSS over time (sample every 10s for 2 minutes)
for i in $(seq 1 12); do
  echo "$(date +%T) $(ps -o rss= -p $(pgrep temm1e) | awk '{print $1/1024 " MB"}')"
  sleep 10
done

# If RSS grows steadily without corresponding session/entry growth, suspect a leak
```

### Step 4: Check system-level memory pressure

```bash
# System memory
free -m

# Swap usage (if swapping, performance will degrade severely)
swapon --show
vmstat 1 5

# OOM score
cat /proc/$(pgrep temm1e)/oom_score
cat /proc/$(pgrep temm1e)/oom_score_adj
```

### Step 5: Check SQLite performance

```bash
# Memory operation latencies
curl http://localhost:8080/metrics | grep temm1e_memory_operation_duration_seconds

# Entry count (affects search performance)
curl http://localhost:8080/metrics | grep temm1e_memory_entries_total

# Search is O(n) LIKE scan. At 100k entries, p99 approaches 50ms SLO boundary
```

---

## Remediation

### Remediation A: Session History Overflow

If active session count is high and histories are large:

1. The `SessionManager` enforces `MAX_HISTORY_PER_SESSION = 200` messages and `MAX_SESSIONS = 1000` with LRU eviction. Verify these limits are active:
   ```bash
   # Check for eviction log messages
   journalctl -u temm1e | grep "Evicted LRU session"
   ```

2. If sessions are within limits but still consuming too much memory, reduce the history limit:
   ```toml
   # temm1e.toml (if configurable)
   [sessions]
   max_history = 50  # Reduce from 200
   max_sessions = 100  # Reduce from 1000
   ```

3. Force session cleanup:
   ```bash
   # Restart will clear all in-memory sessions
   systemctl restart temm1e
   ```

### Remediation B: Browser Tool Memory

If headless browser processes are consuming memory:

1. Kill orphaned browser processes:
   ```bash
   pkill -f "chromium.*headless"
   pkill -f "chrome.*headless"
   ```

2. Limit concurrent browser tool sessions to 2 (per capacity baseline recommendation).

3. If browser tool is not needed, disable it in the tool configuration.

### Remediation C: SQLite Memory Backend

If the memory entry count is > 100k:

1. Archive old entries:
   ```bash
   # Back up first
   cp ~/.temm1e/memory.db ~/.temm1e/memory.db.bak

   # Delete entries older than 90 days
   sqlite3 ~/.temm1e/memory.db "DELETE FROM memories WHERE created_at < datetime('now', '-90 days');"
   sqlite3 ~/.temm1e/memory.db "VACUUM;"
   ```

2. If search latency is degraded (p99 > 50ms), add FTS5 index:
   ```sql
   -- Future optimization
   CREATE VIRTUAL TABLE IF NOT EXISTS memories_fts USING fts5(content, content=memories);
   ```

3. If pool saturation is the issue (4-5/5 connections active), investigate long-running queries:
   ```bash
   sqlite3 ~/.temm1e/memory.db ".timer on"
   sqlite3 ~/.temm1e/memory.db "SELECT * FROM memories WHERE content LIKE '%test%' LIMIT 10;"
   ```

### Remediation D: Process Memory Leak

If RSS grows continuously without corresponding session/entry growth:

1. Collect a heap profile if available:
   ```bash
   # If jemalloc profiling is enabled
   MALLOC_CONF="prof:true,prof_prefix:/tmp/temm1e" systemctl restart temm1e
   # After some time, trigger dump
   kill -USR2 $(pgrep temm1e)
   jeprof /tmp/temm1e.* --svg > /tmp/heap.svg
   ```

2. As immediate mitigation, schedule periodic restarts:
   ```bash
   # Temporary: restart every 6 hours
   # Add to crontab or systemd timer
   ```

3. Escalate to engineering with heap profile and RSS growth data.

### Remediation E: OOM Kill Recovery

If the process was OOM killed:

1. Confirm:
   ```bash
   dmesg | grep -i "oom.*temm1e\|killed.*temm1e"
   journalctl -k | grep -i oom
   ```

2. Restart immediately:
   ```bash
   systemctl restart temm1e
   ```

3. Increase memory limit to prevent immediate recurrence:
   ```bash
   # Kubernetes
   kubectl patch deployment temm1e -p '{"spec":{"template":{"spec":{"containers":[{"name":"temm1e","resources":{"limits":{"memory":"512Mi"}}}]}}}}'

   # Docker
   docker update --memory 512m temm1e

   # systemd
   systemctl edit temm1e
   # Add: MemoryMax=512M
   ```

4. Investigate root cause using Steps 2-3 above.

### Remediation F: SQLite Connection Pool Exhaustion

If `MemoryPoolSaturation` or `MemoryBackendDown` fired:

1. Check for stuck queries:
   ```bash
   sqlite3 ~/.temm1e/memory.db ".timeout 1000"
   # If this hangs, the database may be locked
   ```

2. Restart the service to reset the connection pool:
   ```bash
   systemctl restart temm1e
   ```

3. For cloud mode, increase pool size:
   ```toml
   [memory]
   backend = "sqlite"
   max_connections = 10  # Increase from 5
   ```

4. If pool saturation is recurring, plan migration to PostgreSQL.

---

## Prevention Measures

1. **Session limits:** The `SessionManager` enforces `MAX_SESSIONS = 1000` and `MAX_HISTORY_PER_SESSION = 200`. For local mode, consider lowering to 100 sessions / 50 messages per session to keep RSS under 50 MB.

2. **Memory retention policy:** Implement automated cleanup of memory entries older than 90 days. Monitor `temm1e_memory_entries_total` gauge weekly.

3. **Browser tool limits:** Limit concurrent headless browser instances to 2. Implement a tool execution queue with memory-aware admission control.

4. **RSS monitoring:** The `ProcessMemoryElevated` warning at 50 MB gives early warning before the 100 MB critical threshold. Use this window to investigate.

5. **Connection pool sizing:**
   - Local mode: 5 connections (default)
   - Cloud mode: 20 connections
   - Monitor `MemoryPoolSaturation` info alert as a leading indicator.

6. **Capacity planning:** Review the capacity baseline document quarterly:
   - 5 sessions = ~16-18 MB (within 20 MB idle target)
   - 50 sessions = ~22-28 MB
   - 100 sessions = ~30-50 MB (warning zone)
   - 500 sessions = ~80-200 MB (requires session sharding)

7. **Graceful degradation:** When error budget < 50%, investigate query plans (SQLite) or connection pool exhaustion. When exhausted, trigger memory backend migration review.

---

## Related Runbooks

- [Gateway Down](./gateway-down.md) -- OOM kill causes gateway down
- [High Error Rate](./high-error-rate.md) -- memory pressure causes elevated error rates
- [Vault Failure](./vault-failure.md) -- disk space exhaustion affects both vault and memory
- [Incident Response](./incident-response.md) -- escalation procedures
