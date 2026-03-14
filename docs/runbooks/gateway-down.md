# Runbook: Gateway Down

> **Alert:** `GatewayDown` / `GatewayUnhealthy`
> **Severity:** Critical
> **Service:** gateway
> **Response Time:** < 5 minutes
> **Last Updated:** 2026-03-08

---

## Symptoms and Detection

### Triggering Alerts

| Alert | Condition | Duration |
|-------|-----------|----------|
| `GatewayDown` | `up{job="temm1e"} == 0` | 1 minute |
| `GatewayUnhealthy` | `/health` endpoint returning non-200 | 2 minutes |

### Observable Symptoms

- PagerDuty incident fires with `severity=critical, service=gateway`.
- Slack #incidents channel receives notification.
- External probes (Blackbox Exporter) report `/health` endpoint unreachable.
- All channels (Telegram, Discord, Slack, WhatsApp, CLI) stop receiving responses.
- `temm1e_gateway_up` gauge drops to 0.
- No new entries in `temm1e_gateway_http_requests_total`.

---

## Impact Assessment

| Dimension | Impact |
|-----------|--------|
| **User-facing** | Complete service outage. No messages are processed on any channel. |
| **SLO burn** | Gateway Uptime SLO (99.9%) has a 43.2 min/month budget. Every minute of downtime directly consumes budget. |
| **Blast radius** | All channels, all users. Provider and memory backends may be healthy but inaccessible. |
| **Data loss risk** | None for persisted data (vault, memory). In-flight messages and session state in the `SessionManager` HashMap are lost if the process crashed. |

---

## Step-by-Step Diagnosis

### Step 1: Verify the process is running

```bash
# Check if the temm1e process is alive
pgrep -f temm1e
ps aux | grep temm1e

# If running in a container
docker ps | grep temm1e
kubectl get pods -l app=temm1e
```

If the process is not running, skip to [Remediation: Process Crash](#remediation-a-process-crash).

### Step 2: Check if the port is bound

```bash
# Verify the configured port (default 8080) is being listened on
ss -tlnp | grep 8080
lsof -i :8080

# If in a container, exec into it
docker exec <container> ss -tlnp | grep 8080
```

If the port is not bound but the process is running, the `TcpListener::bind()` in `SkyGate::start()` failed. Check logs for `Failed to bind to <addr>`.

### Step 3: Check application logs

```bash
# View recent logs (structured JSON output)
journalctl -u temm1e --since "5 minutes ago" --no-pager

# If in Docker
docker logs --tail 200 --timestamps temm1e

# If in Kubernetes
kubectl logs -l app=temm1e --tail=200 --timestamps
```

Look for:
- `Temm1eError::Internal("Failed to bind to ...")` -- port conflict
- `Temm1eError::Internal("Server error: ...")` -- axum runtime failure
- Panic backtraces -- unexpected crash
- OOM killer messages in `dmesg`

### Step 4: Check the health endpoint manually

```bash
# Direct health check
curl -v http://localhost:8080/health

# Expected response:
# HTTP/1.1 200 OK
# {"status":"ok","version":"<version>","uptime_seconds":<n>}

# Check status endpoint for component health
curl -v http://localhost:8080/status
```

If `/health` returns non-200, the `HealthResponse.status` field is not "ok" -- this indicates an `AppState` initialization problem (channels, agent, or config failed to load).

### Step 5: Check system resources

```bash
# Memory
free -m
cat /proc/$(pgrep temm1e)/status | grep VmRSS

# File descriptors
ls /proc/$(pgrep temm1e)/fd | wc -l
ulimit -n

# Disk (SQLite, vault files)
df -h ~/.temm1e/
ls -la ~/.temm1e/
```

### Step 6: Check for port conflicts

```bash
# See what else is using the port
lsof -i :8080
netstat -tlnp | grep 8080
```

---

## Remediation

### Remediation A: Process Crash

1. Check for core dumps or OOM kill:
   ```bash
   dmesg | grep -i "oom\|killed" | tail -20
   coredumpctl list | tail -5
   ```

2. Restart the service:
   ```bash
   systemctl restart temm1e
   # or
   docker restart temm1e
   # or
   kubectl rollout restart deployment/temm1e
   ```

3. Verify recovery:
   ```bash
   curl http://localhost:8080/health
   ```

4. If the process crashes again immediately, check configuration:
   ```bash
   temm1e config validate
   ```

### Remediation B: Port Binding Failure

1. Identify the conflicting process:
   ```bash
   lsof -i :8080
   ```

2. Either stop the conflicting process or change TEMM1E's port:
   ```bash
   # In temm1e.toml
   [gateway]
   host = "127.0.0.1"
   port = 8081  # Change to available port
   ```

3. Restart TEMM1E:
   ```bash
   systemctl restart temm1e
   ```

### Remediation C: Health Endpoint Returning Non-200

1. Check `/status` for component-level health:
   ```bash
   curl http://localhost:8080/status | jq .
   ```

2. The `StatusResponse` includes `provider`, `channels`, `tools`, and `memory_backend` fields. Identify which component reports unhealthy.

3. If provider is the issue, see [provider-unreachable.md](./provider-unreachable.md).

4. If memory backend is the issue, check SQLite file integrity:
   ```bash
   sqlite3 ~/.temm1e/memory.db "PRAGMA integrity_check;"
   ```

### Remediation D: Resource Exhaustion

1. If OOM killed, increase memory limit:
   ```bash
   # Kubernetes
   kubectl edit deployment temm1e
   # Increase resources.limits.memory

   # Docker
   docker update --memory 512m temm1e
   ```

2. If file descriptor exhaustion:
   ```bash
   ulimit -n 65535  # Increase in service unit or Dockerfile
   ```

### Remediation E: Emergency Rollback

If the failure correlates with a recent deployment:

```bash
# Docker
docker run -d <previous-image-tag>

# Kubernetes
kubectl rollout undo deployment/temm1e
```

---

## Prevention Measures

1. **Pre-deployment health checks:** CI pipeline must verify `temm1e config validate` and a startup/shutdown cycle before promoting a build.

2. **Readiness probes:** Configure Kubernetes readiness probe on `/health` to prevent traffic routing to unhealthy instances.

3. **Resource limits:** Set memory limits with headroom (256 MB for local mode, 512 MB for cloud mode) based on the capacity baseline (idle RSS ~8-15 MB, peak ~50-100 MB).

4. **Blue-green deployments:** When Gateway Uptime error budget < 50%, only use blue-green deployment strategy with automatic rollback.

5. **Port conflict detection:** Startup script should pre-check port availability before launching.

6. **Graceful shutdown:** Ensure `ctrl_c` signal handler in `main.rs` is properly draining in-flight requests.

7. **Monitoring:** Ensure external prober (Blackbox Exporter) is hitting `/health` every 15 seconds as specified in the SLO definitions.

---

## Related Runbooks

- [High Error Rate](./high-error-rate.md) -- if gateway is up but returning 5xx
- [Memory Pressure](./memory-pressure.md) -- if OOM is the root cause
- [Provider Unreachable](./provider-unreachable.md) -- if `/status` shows provider issues
- [Incident Response](./incident-response.md) -- escalation procedures
