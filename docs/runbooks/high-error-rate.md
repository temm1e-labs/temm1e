# Runbook: High Error Rate

> **Alert:** `MessageProcessingHighErrors` / `MessageProcessingStalled` / `GatewayHighErrorRate`
> **Severity:** Critical
> **Service:** agent / gateway
> **Response Time:** < 5 minutes
> **Last Updated:** 2026-03-08

---

## Symptoms and Detection

### Triggering Alerts

| Alert | Condition | Duration |
|-------|-----------|----------|
| `MessageProcessingHighErrors` | Message error rate > 2% per channel | 5 minutes |
| `MessageProcessingStalled` | Zero messages processed for 10 minutes while gateway is up | 10 minutes |
| `GatewayHighErrorRate` | HTTP 5xx error rate > 1% | 5 minutes (Warning) |

### Related Warning/Info Alerts

| Alert | Condition | Duration |
|-------|-----------|----------|
| `MessageProcessingLatencyHigh` | Message p99 > 100ms (excl. provider) | 5 minutes |
| `ExcessiveToolRounds` | p95 tool rounds > 8 (max 10) | 10 minutes |
| `ChannelErrorsElevated` | Channel error rate > 0.5% over 30m | 15 minutes |
| `ToolHighFailureRate` | Tool failure rate > 5% | 10 minutes |
| `MemoryBackendDown` | Memory backend error rate > 10% | 2 minutes |

### Observable Symptoms

- PagerDuty incident fires with `severity=critical, service=agent`.
- `temm1e_message_processing_duration_seconds_count{status="error"}` incrementing.
- Users report agent failures, error responses, or complete silence.
- Dashboard shows error rate spike across one or more channels.
- Message Processing SLO (99.5% success) error budget burning rapidly.

---

## Impact Assessment

| Dimension | Impact |
|-----------|--------|
| **User-facing** | Users receive error responses or no response. Degraded experience across affected channels. |
| **SLO burn** | Message Processing SLO (99.5%) has ~2.16 h/month budget. At 2% error rate, budget burns 4x faster than allowed. |
| **Blast radius** | May affect all channels (systemic issue) or a single channel (channel-specific bug). Check `channel` label in metrics. |
| **Data loss risk** | Low. Failed messages are not persisted to memory. Session state is preserved but may contain partial history. |

---

## Step-by-Step Diagnosis

### Step 1: Determine scope -- systemic or channel-specific

```bash
# Check error rates per channel
curl http://localhost:8080/metrics | grep 'temm1e_message_processing_duration_seconds_count{.*status="error"}'

# Compare with total request counts per channel
curl http://localhost:8080/metrics | grep 'temm1e_message_processing_duration_seconds_count'
```

- If all channels have elevated errors: systemic issue (provider, memory, vault, or agent runtime).
- If only one channel: channel-specific issue (channel adapter, webhook, or API).

### Step 2: Identify the error type from logs

```bash
# Recent error logs
journalctl -u temm1e --since "15 minutes ago" --priority err

# Filter by error variants
journalctl -u temm1e --since "15 minutes ago" | grep -oP 'Temm1eError::\w+'  | sort | uniq -c | sort -rn
```

Map `Temm1eError` variants to root causes:

| Error Variant | Root Cause | Next Step |
|---------------|-----------|-----------|
| `Temm1eError::Provider(...)` | AI provider failure | See [provider-unreachable.md](./provider-unreachable.md) |
| `Temm1eError::Memory(...)` | Memory backend failure | Step 4 below |
| `Temm1eError::Vault(...)` | Vault operation failure | See [vault-failure.md](./vault-failure.md) |
| `Temm1eError::Channel(...)` | Channel adapter failure | Step 5 below |
| `Temm1eError::Tool(...)` | Tool execution failure | Step 6 below |
| `Temm1eError::Auth(...)` | Authentication failure | Check API key in vault |
| `Temm1eError::SandboxViolation(...)` | Tool sandbox breach | Check tool declarations |
| `Temm1eError::Internal(...)` | Runtime bug | Collect logs and escalate |

### Step 3: Check if processing is stalled (MessageProcessingStalled)

If the `MessageProcessingStalled` alert fired (zero messages for 10 minutes while gateway is up):

```bash
# Verify gateway is responding
curl http://localhost:8080/health

# Check if messages are being received at the gateway level
curl http://localhost:8080/metrics | grep temm1e_gateway_http_requests_total

# Check for deadlocks or stuck tasks
# Look for tasks that have been running for an unusually long time
journalctl -u temm1e --since "15 minutes ago" | grep -i "timeout\|deadlock\|stuck"
```

Possible causes:
- Agent runtime deadlock (SessionManager RwLock contention).
- All channels disconnected (no inbound messages reaching the router).
- Provider calls hanging indefinitely (timeout not enforced).
- Tokio runtime starvation (blocking operations on async runtime).

### Step 4: Diagnose memory backend errors

```bash
# Check memory operation metrics
curl http://localhost:8080/metrics | grep temm1e_memory_operation_total

# Check SQLite file health
sqlite3 ~/.temm1e/memory.db "PRAGMA integrity_check;"
sqlite3 ~/.temm1e/memory.db "PRAGMA journal_mode;"  # Should be "wal"

# Check pool saturation
curl http://localhost:8080/metrics | grep temm1e_memory_pool_active_connections
```

If the memory backend error rate is > 10% (`MemoryBackendDown`), the database may be corrupted or the connection pool exhausted.

### Step 5: Diagnose channel-specific errors

```bash
# Check channel health per channel
curl http://localhost:8080/metrics | grep 'temm1e_message_processing.*channel='

# For Telegram: check webhook/long-poll status
# For Discord: check WebSocket connection
# For Slack: check Socket Mode connection
```

Channel-specific issues:
- Telegram: Bot token expired or revoked. Webhook URL changed.
- Discord: Gateway WebSocket disconnected. Bot permissions changed.
- Slack: Socket Mode token expired. App reinstallation needed.
- WhatsApp: Webhook verification failed. Business API issues.

### Step 6: Diagnose tool execution errors

```bash
# Check tool failure rates
curl http://localhost:8080/metrics | grep temm1e_tool_execution_total

# Check for sandbox violations
curl http://localhost:8080/metrics | grep sandbox_violation

# Check specific tool errors
journalctl -u temm1e --since "15 minutes ago" | grep -i "tool.*error\|sandbox\|execute_tool"
```

### Step 7: Check overall system health

```bash
# Process health
ps aux | grep temm1e
cat /proc/$(pgrep temm1e)/status | grep -E "VmRSS|Threads"

# System resources
free -m
uptime  # load average
df -h
```

---

## Remediation

### Remediation A: Provider-Caused Errors

If the dominant error type is `Temm1eError::Provider`:

1. Follow the [Provider Unreachable](./provider-unreachable.md) runbook.
2. Enable fallback provider if available.
3. Provider errors are partially outside TEMM1E's control -- the provider SLO (99.0%) has a separate, larger error budget.

### Remediation B: Memory Backend Failure

1. If SQLite integrity check fails:
   ```bash
   # Back up corrupted database
   cp ~/.temm1e/memory.db ~/.temm1e/memory.db.corrupted

   # Attempt recovery
   sqlite3 ~/.temm1e/memory.db ".recover" | sqlite3 ~/.temm1e/memory_recovered.db
   mv ~/.temm1e/memory_recovered.db ~/.temm1e/memory.db

   systemctl restart temm1e
   ```

2. If connection pool is saturated (5/5 connections active):
   ```bash
   # For cloud mode, increase pool size in config
   # For local mode, investigate long-running queries
   ```

### Remediation C: Processing Stalled / Deadlock

1. Collect diagnostic information:
   ```bash
   # Thread dump (if supported)
   kill -USR1 $(pgrep temm1e)  # If signal handler is configured

   # Check tokio runtime state via metrics
   curl http://localhost:8080/metrics | grep tokio
   ```

2. Restart the service:
   ```bash
   systemctl restart temm1e
   ```

3. If stall recurs, it indicates a bug. Collect core dump and escalate:
   ```bash
   # Enable core dumps
   ulimit -c unlimited
   # Reproduce and collect
   ```

### Remediation D: Channel-Specific Errors

1. Identify the failing channel from metrics labels.

2. Check channel-specific credentials and configuration:
   ```bash
   temm1e config show | grep -A5 "\[channel\]"
   ```

3. For webhook-based channels, verify the webhook endpoint is reachable from the external service.

4. Restart only the affected channel if possible, or restart the entire service.

### Remediation E: Tool Execution Errors

1. If sandbox violations are the cause, review tool `PathAccess` declarations in `executor.rs`.

2. If a specific tool is failing consistently:
   - Check tool dependencies (e.g., `git`, `node`, `chromium` installed and in PATH).
   - Check workspace directory permissions.
   - Temporarily disable the failing tool if it is non-critical.

### Remediation F: Emergency Error Rate Reduction

If error rate is burning through SLO budget rapidly:

1. If errors are concentrated on non-critical channels, temporarily disable those channels.
2. If errors are in tool execution, reduce `MAX_TOOL_ROUNDS` or disable complex tools.
3. If errors are systemic, initiate a rollback to the last known good version.

---

## Prevention Measures

1. **Error categorization:** Ensure all `Temm1eError` variants are properly instrumented with labels in metrics to enable fast triage.

2. **Canary deployments:** Deploy to a single channel first and monitor error rates before rolling out to all channels.

3. **Circuit breakers:** Implement circuit breakers on provider calls and tool executions to fail fast rather than accumulate timeout errors.

4. **Error budget alerting:** Configure SLO burn-rate alerts:
   - Budget < 50% remaining: halt non-critical deployments, page on-call.
   - Budget < 25% remaining: freeze all changes, dedicate engineering to reliability.

5. **Automated testing:** Ensure integration tests cover the `route_message` -> `process_message` -> provider path for each channel type.

6. **Timeout enforcement:** Verify that all external calls (provider, channel APIs) have timeouts configured. Provider timeout is 60s per the SLO definitions.

7. **Note on RateLimited:** `Temm1eError::RateLimited` is intentionally excluded from SLO error counting. Do not treat rate limiting as an error for SLO purposes, but do investigate if it impacts user experience.

---

## Related Runbooks

- [Provider Unreachable](./provider-unreachable.md) -- most common cause of high error rates
- [Gateway Down](./gateway-down.md) -- if gateway itself is failing
- [Vault Failure](./vault-failure.md) -- if secrets resolution is the root cause
- [Memory Pressure](./memory-pressure.md) -- if resource exhaustion is causing errors
- [Incident Response](./incident-response.md) -- escalation procedures
