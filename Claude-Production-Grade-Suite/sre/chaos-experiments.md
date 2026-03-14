# TEMM1E Chaos Experiment Plan

> Chaos engineering experiments to validate SLO resilience and failure recovery.
> Owner: SRE | Last updated: 2026-03-08
> Prerequisite: All experiments must be approved by the on-call SRE and run in a staging environment first.

---

## Principles

1. **Steady state first:** Define and verify the steady state before injecting failure.
2. **Minimize blast radius:** Start with the smallest possible injection scope.
3. **Abort on unexpected cascading failures:** Every experiment has explicit abort conditions.
4. **Automate rollback:** Injection must be reversible within 30 seconds.
5. **Document everything:** Record observations, metrics snapshots, and deviations from expected behavior.

---

## Experiment 1: Gateway Crash Recovery

**Target SLO:** Gateway Uptime >= 99.9% (43.2 min/month budget)

### Steady State Hypothesis

- `temm1e_gateway_up == 1`
- `/health` returns HTTP 200 with `{"status":"ok"}` within 10ms p99
- All channels are receiving and processing messages normally
- Cold start completes in < 50ms

### Injection Method

```bash
# Method A: Kill the process
kill -9 $(pgrep temm1e)

# Method B: Block the listening port (simulate network partition)
iptables -A INPUT -p tcp --dport 8080 -j DROP

# Method C: Exhaust file descriptors
# (Use a test harness that opens connections without closing them)
for i in $(seq 1 1024); do
  exec {fd}<>/dev/tcp/localhost/8080 &
done
```

### Expected Behavior

| Phase | Expected Outcome | Metric to Verify |
|-------|-----------------|-----------------|
| Injection (kill) | Process terminates immediately | `up{job="temm1e"} == 0` |
| Detection | `GatewayDown` alert fires within 1 minute | PagerDuty incident created |
| Recovery (systemd) | systemd restarts process automatically | `up{job="temm1e"} == 1` within 5s of restart |
| Cold start | New process binds port and serves `/health` in < 50ms | `temm1e_gateway_cold_start_seconds < 0.050` |
| Steady state restored | All channels reconnect and resume processing | `temm1e_message_processing_duration_seconds_count` incrementing |

### Abort Conditions

- Process does not restart after 60 seconds (systemd restart loop).
- Cold start exceeds 5 seconds (configuration or dependency issue).
- Data corruption detected in SQLite or vault after restart.

### Observations to Record

- Time from kill to `GatewayDown` alert firing
- Time from kill to process restart
- Cold start duration
- Number of in-flight messages lost
- Session state recovery (were sessions lost?)
- Any error spike on channels after restart

---

## Experiment 2: Provider Failure and Fallback

**Target SLO:** Provider Availability >= 99.0% (7.2 h/month budget)

### Steady State Hypothesis

- `temm1e_provider_health_check_success == 1` for all configured providers
- `temm1e_provider_request_total{status="success"}` rate is stable
- Provider p99 latency < 30s
- Messages are processed with provider completions returning valid responses

### Injection Method

```bash
# Method A: Block provider API at network level
iptables -A OUTPUT -d api.anthropic.com -j DROP

# Method B: Redirect provider API to a mock returning 500
# In /etc/hosts or DNS override:
echo "127.0.0.1 api.anthropic.com" >> /etc/hosts
# Run mock server returning 500 on port 443

# Method C: Invalidate API key in vault
# Store an invalid key
echo -n "sk-ant-INVALID" | temm1e vault store anthropic-api-key
# Restart to pick up the new key

# Method D: Inject latency via tc (simulate slow responses)
tc qdisc add dev eth0 root netem delay 35000ms
```

### Expected Behavior

| Phase | Expected Outcome | Metric to Verify |
|-------|-----------------|-----------------|
| Injection | Provider requests begin failing | `temm1e_provider_request_total{status="error"}` incrementing |
| Detection | `ProviderDown` alert fires within 2 minutes | Health check gauge drops to 0 |
| Error handling | `Temm1eError::Provider` returned to callers | Error responses sent to users (not crashes) |
| Fallback (if configured) | Traffic shifts to fallback provider | `temm1e_provider_request_total{provider="openai_compat"}` incrementing |
| Budget tracking | Error budget burn rate reflected in SLO dashboard | Budget < 50% triggers deployment freeze |
| Recovery | After removing injection, health check recovers within 30s | `temm1e_provider_health_check_success == 1` |

### Abort Conditions

- Process crashes due to unhandled provider error (should never happen).
- Vault or memory backends affected by provider injection (blast radius too wide).
- Fallback provider also fails (cascading failure).

### Observations to Record

- Time from injection to first error response to user
- Time from injection to `ProviderDown` alert
- Whether fallback provider activated (if configured)
- Error budget burn rate during injection
- Recovery time after removing injection
- Any messages that were silently dropped (vs. error response)

---

## Experiment 3: Vault Key Corruption

**Target SLO:** Vault Operations >= 99.99% (4.3 min/month budget), Zero decryption failures

### Steady State Hypothesis

- `temm1e_vault_decryption_failures_total == 0`
- `temm1e_vault_operation_total{status="success"}` rate is stable
- Vault operation p99 < 10ms
- `vault.key` is 32 bytes with permissions 0600

### Injection Method

**WARNING: This experiment must use a test vault, never production keys.**

```bash
# Method A: Corrupt vault.key (append extra bytes)
cp ~/.temm1e/vault.key ~/.temm1e/vault.key.backup
echo "CORRUPT" >> ~/.temm1e/vault.key

# Method B: Corrupt vault.enc (invalid JSON)
cp ~/.temm1e/vault.enc ~/.temm1e/vault.enc.backup
echo "INVALID{{{" >> ~/.temm1e/vault.enc

# Method C: Change vault.key permissions
chmod 644 ~/.temm1e/vault.key

# Method D: Replace vault.key with wrong key (valid 32 bytes but different)
cp ~/.temm1e/vault.key ~/.temm1e/vault.key.backup
dd if=/dev/urandom of=~/.temm1e/vault.key bs=32 count=1
```

### Expected Behavior

| Phase | Expected Outcome | Metric to Verify |
|-------|-----------------|-----------------|
| Injection (key corrupt) | Next vault read fails with "vault key must be exactly 32 bytes" | `temm1e_vault_operation_total{status="error"}` increments |
| Injection (wrong key) | Decryption fails with ChaCha20-Poly1305 AEAD error | `temm1e_vault_decryption_failures_total` increments |
| Detection | `VaultDecryptionFailure` alert fires immediately (any increment) | PagerDuty P1 incident |
| Cascade | Provider calls fail because API key cannot be resolved from vault | `temm1e_provider_request_total{status="error"}` incrementing |
| Injection (perms) | `VaultKeyPermissionDrift` warning fires within 1 minute | `temm1e_vault_key_permissions != 384` |
| Recovery (backup restore) | Restoring backup key immediately fixes vault operations | `temm1e_vault_decryption_failures_total` stops incrementing |

### Abort Conditions

- Any data loss or corruption beyond the intentionally corrupted test files.
- Process enters an unrecoverable state requiring manual intervention beyond simple file restoration.
- Production API keys are affected (this experiment must only touch test environments).

### Observations to Record

- Whether `LocalVault::read_key()` validates key size before use
- Whether `LocalVault::load()` handles corrupt JSON gracefully
- Time from injection to `VaultDecryptionFailure` alert
- Cascade depth: which other services fail?
- Recovery time after restoring backup key
- Whether the process needs restart or recovers dynamically

---

## Experiment 4: Memory Backend Saturation

**Target SLO:** Memory Operations >= 99.9% (search p99 < 50ms, store p99 < 20ms)

### Steady State Hypothesis

- `temm1e_memory_operation_total{status="success"}` rate is stable
- Search p99 < 50ms, store p99 < 20ms
- `temm1e_memory_entries_total` is below 100k
- `temm1e_memory_pool_active_connections` < 4 (pool not saturated)

### Injection Method

```bash
# Method A: Flood the memory store with entries to push past 100k
python3 -c "
import sqlite3, random, string
conn = sqlite3.connect('$HOME/.temm1e/memory.db')
cursor = conn.cursor()
for i in range(100000):
    content = ''.join(random.choices(string.ascii_letters, k=500))
    cursor.execute('INSERT INTO memories (session_id, role, content, created_at) VALUES (?, ?, ?, datetime(\"now\"))',
                   (f'flood-{i%100}', 'user', content))
    if i % 10000 == 0:
        conn.commit()
        print(f'Inserted {i} entries')
conn.commit()
conn.close()
"

# Method B: Lock the SQLite database to simulate contention
sqlite3 ~/.temm1e/memory.db "BEGIN EXCLUSIVE; SELECT sleep(30);"
# (This blocks all other connections for 30s)

# Method C: Fill disk to prevent writes
dd if=/dev/zero of=/tmp/fillfile bs=1M count=10000
# (Careful: this fills the disk)

# Method D: Saturate connection pool
# Send concurrent requests that each perform slow queries
```

### Expected Behavior

| Phase | Expected Outcome | Metric to Verify |
|-------|-----------------|-----------------|
| Injection (100k entries) | Search p99 approaches 50ms SLO boundary | `temm1e_memory_operation_duration_seconds{operation="search"}` |
| Detection | `MemoryEntriesHigh` warning fires after 1 hour | Entry count > 100k |
| Injection (DB lock) | Memory operations queue behind lock | `temm1e_memory_pool_active_connections >= 5` |
| Detection | `MemoryBackendDown` fires if error rate > 10% for 2 minutes | Pool saturation -> timeouts -> errors |
| Cascade | Message processing slows (memory store/search in critical path) | `MessageProcessingLatencyHigh` may fire |
| Recovery (unlock) | Operations resume immediately after lock released | Latencies return to baseline |
| Recovery (cleanup) | Deleting flood entries restores search performance | `temm1e_memory_entries_total` decreases |

### Abort Conditions

- Database file corruption (as opposed to slow performance).
- Process crash due to unhandled database error.
- Disk full condition affecting vault or other components.

### Observations to Record

- Search latency curve as entry count increases (1k, 10k, 50k, 100k)
- Connection pool behavior under contention (queue depth, timeout behavior)
- Whether `hybrid_search()` degrades gracefully
- Impact on message processing latency (cascade measurement)
- Time to recover after clearing flood entries and running VACUUM

---

## Experiment 5: Session Exhaustion and Memory Pressure

**Target SLO:** Session operation p99 < 5ms, Process RSS < 100 MB

### Steady State Hypothesis

- `temm1e_active_sessions` < 10
- `process_resident_memory_bytes` < 20 MB (idle)
- Session operations (get_or_create, update, remove) complete in < 5ms p99
- `SessionManager` RwLock contention is negligible

### Injection Method

```bash
# Method A: Create many concurrent sessions via multiple channels
# Simulate 200 unique users sending messages simultaneously
for i in $(seq 1 200); do
  curl -X POST http://localhost:8080/webhook/telegram \
    -H "Content-Type: application/json" \
    -d "{\"chat_id\": \"flood-$i\", \"user_id\": \"user-$i\", \"text\": \"$(head -c 1000 /dev/urandom | base64)\"}" &
done
wait

# Method B: Create sessions with very long histories
# Send 200 messages to the same session to grow history
for i in $(seq 1 200); do
  curl -X POST http://localhost:8080/webhook/telegram \
    -H "Content-Type: application/json" \
    -d "{\"chat_id\": \"heavy-session\", \"user_id\": \"user-1\", \"text\": \"Message $i with substantial content padding: $(head -c 2000 /dev/urandom | base64)\"}"
done

# Method C: Simulate memory leak by disabling session cleanup
# (Requires code modification -- use in staging only)
```

### Expected Behavior

| Phase | Expected Outcome | Metric to Verify |
|-------|-----------------|-----------------|
| Injection (200 sessions) | Sessions created, RSS grows proportionally | `temm1e_active_sessions == 200` |
| Memory impact | RSS increases by ~8-16 MB (200 x 40-80 KB) | `process_resident_memory_bytes` |
| Detection | `SessionCountHigh` fires if count > 50 | Warning alert |
| LRU eviction | At `MAX_SESSIONS = 1000`, oldest sessions are evicted | Log messages: "Evicted LRU session" |
| History truncation | At `MAX_HISTORY_PER_SESSION = 200`, oldest messages trimmed | Session history length capped |
| RwLock contention | Under heavy concurrent access, write lock may cause brief stalls | `temm1e_session_operation_duration_seconds` |
| Recovery | After traffic stops, sessions persist but memory is stable | RSS stabilizes |

### Abort Conditions

- `ProcessMemoryHigh` critical alert fires (RSS > 100 MB) during a test that was not expected to reach that level.
- OOM kill triggered.
- RwLock deadlock (should be impossible with async RwLock, but verify).
- Session data corruption.

### Observations to Record

- RSS per session at different history depths (10, 50, 100, 200 messages)
- LRU eviction behavior: does it correctly evict the oldest accessed session?
- Session operation latency under contention (50, 100, 200 concurrent sessions)
- History truncation behavior: are the most recent messages preserved?
- Time for RSS to stabilize after session flood stops

---

## Experiment 6: Tool Execution Failure and Sandbox Bypass

**Target SLO:** Tool success rate >= 98.0%, Sandbox violation rate < 0.1%

### Steady State Hypothesis

- `temm1e_tool_execution_total{status="success"}` rate is stable
- `temm1e_tool_execution_total{status="sandbox_violation"} == 0`
- `temm1e_tool_rounds_per_message` p95 < 8
- Tool execution p99 < 30s

### Injection Method

```bash
# Method A: Remove a tool dependency
mv /usr/bin/git /usr/bin/git.hidden
# (Git tool calls will fail)

# Method B: Make workspace directory read-only
chmod 000 /path/to/workspace

# Method C: Craft a message that triggers maximum tool rounds
# Send a prompt designed to cause the agent to iterate through all 10 tool rounds
curl -X POST http://localhost:8080/webhook/cli \
  -H "Content-Type: application/json" \
  -d '{"text": "Run shell commands to create 10 different files, one at a time, checking each exists before creating the next."}'

# Method D: Attempt path traversal via tool input
# (Test that validate_sandbox() correctly rejects)
```

### Expected Behavior

| Phase | Expected Outcome | Metric to Verify |
|-------|-----------------|-----------------|
| Injection (missing dep) | Tool execution returns `Temm1eError::Tool` | `temm1e_tool_execution_total{status="error"}` |
| Detection | `ToolHighFailureRate` fires if rate > 5% for 10 minutes | Warning alert |
| Injection (read-only dir) | File write tools fail, read tools succeed | Error rate increase for write-dependent tools only |
| Injection (max rounds) | Agent hits `MAX_TOOL_ROUNDS = 10` and stops | `temm1e_tool_rounds_per_message` histogram shows value at 10 |
| Detection | `ExcessiveToolRounds` fires if p95 > 8 | Warning alert |
| Injection (sandbox) | `validate_sandbox()` rejects path traversal | `temm1e_tool_execution_total{status="sandbox_violation"}` |
| Detection | `SandboxViolations` fires immediately | Warning alert |

### Abort Conditions

- Sandbox bypass succeeds (tool accesses files outside workspace). This is a security incident.
- Tool execution causes process crash.
- Agent enters infinite loop (does not respect MAX_TOOL_ROUNDS).

### Observations to Record

- Error messages returned to user for each failure type (are they user-friendly?)
- Whether the agent gracefully degrades when a tool is unavailable
- Sandbox enforcement: list of paths tested and rejection results
- MAX_TOOL_ROUNDS enforcement: does the agent stop and respond to the user?
- Resource consumption during maximum tool rounds (memory, CPU)

---

## Experiment 7: File Transfer Under Stress

**Target SLO:** Transfer success rate >= 99.0%, p99 < 5s for files < 10 MB

### Steady State Hypothesis

- `temm1e_file_transfer_total{status="success"}` rate is stable
- Transfer p99 < 5s for files under 10 MB
- No file transfer errors in logs

### Injection Method

```bash
# Method A: Send files exceeding size limits
dd if=/dev/urandom of=/tmp/large_file.bin bs=1M count=100
# Upload via channel

# Method B: Inject network latency on file transfer paths
tc qdisc add dev eth0 root netem delay 2000ms 500ms

# Method C: Fill disk to prevent file saves
dd if=/dev/zero of=/tmp/diskfiller bs=1M count=10000

# Method D: Send concurrent file transfers
for i in $(seq 1 20); do
  dd if=/dev/urandom of=/tmp/test_$i.bin bs=1M count=5
  # Upload all simultaneously
done
```

### Expected Behavior

| Phase | Expected Outcome | Metric to Verify |
|-------|-----------------|-----------------|
| Injection (oversize) | `max_file_size()` check rejects the file | Rejection counter increments, not counted as SLO error |
| Injection (latency) | Transfer duration increases, may breach p99 SLO | `temm1e_file_transfer_duration_seconds` |
| Injection (disk full) | `save_received_file()` fails with I/O error | `temm1e_file_transfer_total{status="error"}` |
| Detection | `FileTransferHighErrors` fires if rate > 5% for 10 minutes | Warning alert |
| Concurrent transfers | All transfers complete (possibly slower) | Throughput metrics |
| Recovery | Removing latency/freeing disk restores normal operation | Latency returns to baseline |

### Abort Conditions

- Path traversal rejection from `save_received_file()` is bypassed (security issue).
- File transfer causes process crash or memory exhaustion (large file in memory).
- Disk full condition cascades to SQLite or vault (shared filesystem).

### Observations to Record

- Maximum file size accepted per channel
- Transfer latency at different file sizes (1 MB, 5 MB, 10 MB, 50 MB)
- Streaming transfer behavior (`send_file_stream`) under latency
- Error messages returned for oversize files vs. I/O errors
- Disk space consumed by received files (cleanup behavior)

---

## Experiment 8: Full Cascade Failure

**Target SLO:** All SLOs simultaneously

### Steady State Hypothesis

All eight SLO categories are within budget:
- Message Processing: 99.5%
- Provider Availability: 99.0%
- Gateway Uptime: 99.9%
- Memory Operations: 99.9%
- Vault Operations: 99.99%
- File Transfer: 99.0%
- Session Management: 99.9%
- Tool Execution: 98.0%

### Injection Method

Inject a realistic failure scenario that cascades across components:

```bash
# Scenario: Provider outage + high session load + memory pressure

# Step 1: Create 100 concurrent sessions (simulate traffic spike)
for i in $(seq 1 100); do
  curl -X POST http://localhost:8080/webhook/telegram \
    -d "{\"chat_id\": \"cascade-$i\", \"user_id\": \"user-$i\", \"text\": \"Hello\"}" &
done
wait

# Step 2: Block provider API (all completions will fail)
iptables -A OUTPUT -d api.anthropic.com -j DROP

# Step 3: Observe cascade for 10 minutes

# Step 4: Restore provider access
iptables -D OUTPUT -d api.anthropic.com -j DROP
```

### Expected Behavior

| Phase | Expected Cascade | Metrics to Watch |
|-------|-----------------|-----------------|
| Session flood | RSS increases, `SessionCountHigh` fires | `temm1e_active_sessions`, `process_resident_memory_bytes` |
| Provider block | All completions fail, error rate spikes | `temm1e_provider_health_check_success == 0` |
| Combined impact | `MessageProcessingHighErrors` fires, users get errors | `temm1e_message_processing_duration_seconds_count{status="error"}` |
| Gateway stays up | Gateway continues serving `/health` even during provider/message errors | `temm1e_gateway_up == 1` |
| Error budget burn | Multiple SLO budgets burning simultaneously | Dashboard shows multi-SLO breach |
| Recovery | After provider restored, error rate returns to normal | All health check gauges return to 1 |
| Session drain | Sessions remain, RSS stays elevated until idle timeout | `temm1e_active_sessions` slowly decreases |

### Abort Conditions

- Process crashes (OOM or panic) during cascade.
- Gateway becomes unreachable (blast radius exceeds expected scope).
- Data corruption in any backend (SQLite, vault).
- Recovery does not complete within 5 minutes of removing injection.

### Observations to Record

- Complete cascade timeline: which alerts fire in what order?
- Maximum error budget burn rate across all SLOs
- Whether the gateway remained available throughout
- Recovery sequence: which components recover first?
- Total RSS consumed during the combined scenario
- Any unexpected interactions between failure modes

---

## Experiment Schedule

| Experiment | Frequency | Environment | Estimated Duration |
|-----------|-----------|-------------|-------------------|
| 1. Gateway Crash Recovery | Monthly | Staging, then Production | 15 minutes |
| 2. Provider Failure/Fallback | Monthly | Staging, then Production | 30 minutes |
| 3. Vault Key Corruption | Quarterly | Staging ONLY | 30 minutes |
| 4. Memory Backend Saturation | Quarterly | Staging | 45 minutes |
| 5. Session Exhaustion | Quarterly | Staging | 30 minutes |
| 6. Tool Execution Failure | Quarterly | Staging | 30 minutes |
| 7. File Transfer Stress | Quarterly | Staging | 30 minutes |
| 8. Full Cascade | Semi-annually | Staging ONLY | 60 minutes |

---

## Pre-Experiment Checklist

- [ ] Experiment approved by on-call SRE
- [ ] Staging environment matches production configuration
- [ ] All monitoring dashboards are accessible and displaying real-time data
- [ ] Backup of vault.key, vault.enc, and memory.db taken
- [ ] Rollback procedure tested and documented
- [ ] Communication sent to team: experiment window, expected alerts, abort contact
- [ ] PagerDuty silenced for staging environment alerts (not production)

## Post-Experiment Checklist

- [ ] All injections removed and verified
- [ ] Steady state restored and verified across all SLOs
- [ ] Metrics snapshots saved for comparison
- [ ] Observations documented in experiment log
- [ ] Unexpected findings filed as action items
- [ ] Dashboard showing clean state before concluding
