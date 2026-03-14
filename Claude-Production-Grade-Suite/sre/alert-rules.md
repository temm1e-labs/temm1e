# TEMM1E Alert Rules

> Prometheus-compatible alerting rules for TEMM1E runtime.
> Owner: SRE | Last updated: 2026-03-08

---

## Alert Severity Levels

| Severity | Response Time | Notification | Action |
|----------|--------------|--------------|--------|
| **Critical** | < 5 min | PagerDuty + Slack #incidents | Immediate investigation required |
| **Warning** | < 30 min | Slack #alerts | Investigate within shift |
| **Info** | Next business day | Slack #monitoring | Review in daily standup |

---

## 1. Gateway Health Alerts

```yaml
groups:
  - name: temm1e_gateway
    rules:

      # CRITICAL: Gateway is down
      - alert: GatewayDown
        expr: up{job="temm1e"} == 0
        for: 1m
        labels:
          severity: critical
          service: gateway
        annotations:
          summary: "TEMM1E gateway is unreachable"
          description: "The /health endpoint has been unreachable for 1 minute. Process may have crashed or port 8080 is not bound."
          runbook: "Check process status, restart if needed. Verify TcpListener bind on configured host:port."

      # CRITICAL: Gateway health endpoint returning non-200
      - alert: GatewayUnhealthy
        expr: probe_http_status_code{job="temm1e-health"} != 200
        for: 2m
        labels:
          severity: critical
          service: gateway
        annotations:
          summary: "TEMM1E /health returning non-200"
          description: "Health endpoint is responding but returning status {{ $value }}."
          runbook: "Check AppState components. Review HealthResponse.status field."

      # WARNING: Gateway latency elevated
      - alert: GatewayHighLatency
        expr: |
          histogram_quantile(0.99,
            rate(temm1e_gateway_http_request_duration_seconds_bucket{path="/health"}[5m])
          ) > 0.010
        for: 5m
        labels:
          severity: warning
          service: gateway
        annotations:
          summary: "Gateway /health p99 latency > 10ms"
          description: "p99 latency is {{ $value | humanizeDuration }}. SLO target is < 10ms."

      # WARNING: High error rate on HTTP endpoints
      - alert: GatewayHighErrorRate
        expr: |
          (
            sum(rate(temm1e_gateway_http_requests_total{status_code=~"5.."}[5m]))
            /
            sum(rate(temm1e_gateway_http_requests_total[5m]))
          ) > 0.01
        for: 5m
        labels:
          severity: warning
          service: gateway
        annotations:
          summary: "Gateway 5xx error rate > 1%"
          description: "Current error rate is {{ $value | humanizePercentage }}."

      # INFO: Gateway cold start exceeded target
      - alert: GatewayColdStartSlow
        expr: temm1e_gateway_cold_start_seconds > 0.050
        labels:
          severity: info
          service: gateway
        annotations:
          summary: "Gateway cold start exceeded 50ms target"
          description: "Cold start took {{ $value | humanizeDuration }}. Target is < 50ms."
```

---

## 2. Provider Error Alerts

```yaml
  - name: temm1e_provider
    rules:

      # CRITICAL: Provider completely failing
      - alert: ProviderDown
        expr: temm1e_provider_health_check_success == 0
        for: 2m
        labels:
          severity: critical
          service: provider
        annotations:
          summary: "AI provider {{ $labels.provider }} health check failing"
          description: "Provider::health_check() has returned false for 2 minutes."
          runbook: "Verify API key via vault. Check upstream provider status page. Consider switching to fallback provider."

      # CRITICAL: Provider error rate exceeds SLO budget burn
      - alert: ProviderHighErrorRate
        expr: |
          (
            sum by (provider) (rate(temm1e_provider_request_total{status="error"}[5m]))
            /
            sum by (provider) (rate(temm1e_provider_request_total[5m]))
          ) > 0.05
        for: 5m
        labels:
          severity: critical
          service: provider
        annotations:
          summary: "Provider {{ $labels.provider }} error rate > 5%"
          description: "Error rate is {{ $value | humanizePercentage }}. SLO budget will burn in < 6 hours at this rate."

      # WARNING: Provider latency elevated
      - alert: ProviderHighLatency
        expr: |
          histogram_quantile(0.99,
            sum by (provider, le) (rate(temm1e_provider_request_duration_seconds_bucket[5m]))
          ) > 30
        for: 5m
        labels:
          severity: warning
          service: provider
        annotations:
          summary: "Provider {{ $labels.provider }} p99 latency > 30s"
          description: "p99 latency is {{ $value | humanizeDuration }}."

      # WARNING: Provider timeout rate elevated
      - alert: ProviderTimeouts
        expr: |
          (
            sum by (provider) (rate(temm1e_provider_request_total{status="timeout"}[10m]))
            /
            sum by (provider) (rate(temm1e_provider_request_total[10m]))
          ) > 0.02
        for: 5m
        labels:
          severity: warning
          service: provider
        annotations:
          summary: "Provider {{ $labels.provider }} timeout rate > 2%"
          description: "Requests timing out at {{ $value | humanizePercentage }}."

      # INFO: Provider rate limiting detected
      - alert: ProviderRateLimited
        expr: |
          sum by (provider) (rate(temm1e_provider_request_total{status="rate_limited"}[5m])) > 0
        for: 2m
        labels:
          severity: info
          service: provider
        annotations:
          summary: "Provider {{ $labels.provider }} returning rate limit responses"
          description: "Consider reducing concurrency or upgrading API tier."
```

---

## 3. Message Processing / Latency Spike Alerts

```yaml
  - name: temm1e_message_processing
    rules:

      # CRITICAL: Message processing completely stalled
      - alert: MessageProcessingStalled
        expr: |
          rate(temm1e_message_processing_duration_seconds_count[5m]) == 0
          and
          sum(rate(temm1e_gateway_http_requests_total{path="/status"}[5m])) > 0
        for: 10m
        labels:
          severity: critical
          service: agent
        annotations:
          summary: "No messages processed in 10 minutes while gateway is up"
          description: "Agent runtime may be deadlocked or all channels disconnected."

      # CRITICAL: Message processing error rate breaching SLO
      - alert: MessageProcessingHighErrors
        expr: |
          (
            sum by (channel) (rate(temm1e_message_processing_duration_seconds_count{status="error"}[5m]))
            /
            sum by (channel) (rate(temm1e_message_processing_duration_seconds_count[5m]))
          ) > 0.02
        for: 5m
        labels:
          severity: critical
          service: agent
        annotations:
          summary: "Message processing error rate > 2% on channel {{ $labels.channel }}"
          description: "Current error rate: {{ $value | humanizePercentage }}. SLO target: 99.5% success."

      # WARNING: Message processing latency spike (excluding provider time)
      - alert: MessageProcessingLatencyHigh
        expr: |
          histogram_quantile(0.99,
            sum by (channel, le) (rate(temm1e_message_processing_duration_seconds_bucket[5m]))
          ) > 0.100
        for: 5m
        labels:
          severity: warning
          service: agent
        annotations:
          summary: "Message processing p99 > 100ms on channel {{ $labels.channel }}"
          description: "p99 latency is {{ $value | humanizeDuration }}. SLO target: < 100ms (excl. provider)."

      # WARNING: Too many tool rounds per message
      - alert: ExcessiveToolRounds
        expr: |
          histogram_quantile(0.95,
            sum by (le) (rate(temm1e_tool_rounds_per_message_bucket[15m]))
          ) > 8
        for: 10m
        labels:
          severity: warning
          service: agent
        annotations:
          summary: "95th percentile tool rounds > 8 (max 10)"
          description: "Agent is frequently hitting near MAX_TOOL_ROUNDS. Review system prompt or tool design."

      # INFO: Channel-specific error rate elevated
      - alert: ChannelErrorsElevated
        expr: |
          (
            sum by (channel) (rate(temm1e_message_processing_duration_seconds_count{status="error"}[30m]))
            /
            sum by (channel) (rate(temm1e_message_processing_duration_seconds_count[30m]))
          ) > 0.005
        for: 15m
        labels:
          severity: info
          service: channel
        annotations:
          summary: "Channel {{ $labels.channel }} error rate elevated"
          description: "Error rate {{ $value | humanizePercentage }} over 30m window."
```

---

## 4. Memory Limits Alerts

```yaml
  - name: temm1e_memory
    rules:

      # CRITICAL: Memory backend unreachable
      - alert: MemoryBackendDown
        expr: |
          (
            sum(rate(temm1e_memory_operation_total{status="error"}[5m]))
            /
            sum(rate(temm1e_memory_operation_total[5m]))
          ) > 0.10
        for: 2m
        labels:
          severity: critical
          service: memory
        annotations:
          summary: "Memory backend error rate > 10%"
          description: "SQLite/Postgres may be unreachable or corrupt. Error rate: {{ $value | humanizePercentage }}."
          runbook: "Check SQLite file integrity. For Postgres, check connection pool (max_connections=5)."

      # WARNING: Memory search latency breaching SLO
      - alert: MemorySearchSlow
        expr: |
          histogram_quantile(0.99,
            sum by (backend, le) (rate(temm1e_memory_operation_duration_seconds_bucket{operation="search"}[5m]))
          ) > 0.050
        for: 5m
        labels:
          severity: warning
          service: memory
        annotations:
          summary: "Memory search p99 > 50ms (SLO target)"
          description: "p99 search latency: {{ $value | humanizeDuration }}. Check entry count and query patterns."

      # WARNING: Memory store latency elevated
      - alert: MemoryStoreSlow
        expr: |
          histogram_quantile(0.99,
            sum by (backend, le) (rate(temm1e_memory_operation_duration_seconds_bucket{operation="store"}[5m]))
          ) > 0.020
        for: 5m
        labels:
          severity: warning
          service: memory
        annotations:
          summary: "Memory store p99 > 20ms"
          description: "p99 store latency: {{ $value | humanizeDuration }}."

      # WARNING: Memory entry count growing large
      - alert: MemoryEntriesHigh
        expr: temm1e_memory_entries_total > 100000
        for: 1h
        labels:
          severity: warning
          service: memory
        annotations:
          summary: "Memory entry count exceeds 100k"
          description: "Current count: {{ $value }}. Search performance may degrade. Consider archiving old entries."

      # INFO: SQLite connection pool saturation
      - alert: MemoryPoolSaturation
        expr: temm1e_memory_pool_active_connections >= 4
        for: 5m
        labels:
          severity: info
          service: memory
        annotations:
          summary: "SQLite pool near saturation (4/5 connections active)"
          description: "Pool approaching max_connections=5 limit."

      # CRITICAL: Process RSS exceeding idle target significantly
      - alert: ProcessMemoryHigh
        expr: process_resident_memory_bytes > 104857600
        for: 5m
        labels:
          severity: critical
          service: runtime
        annotations:
          summary: "Process RSS > 100 MB"
          description: "Current RSS: {{ $value | humanize1024 }}B. Idle target is < 20 MB. Possible memory leak."

      # WARNING: Process RSS elevated above idle target
      - alert: ProcessMemoryElevated
        expr: process_resident_memory_bytes > 52428800
        for: 10m
        labels:
          severity: warning
          service: runtime
        annotations:
          summary: "Process RSS > 50 MB"
          description: "Current RSS: {{ $value | humanize1024 }}B. Idle target is < 20 MB."
```

---

## 5. Vault Failure Alerts

```yaml
  - name: temm1e_vault
    rules:

      # CRITICAL: Vault decryption failures (potential key corruption)
      - alert: VaultDecryptionFailure
        expr: increase(temm1e_vault_decryption_failures_total[5m]) > 0
        labels:
          severity: critical
          service: vault
        annotations:
          summary: "Vault decryption failure detected"
          description: "ChaCha20-Poly1305 decryption failed. Possible key corruption or vault.enc tampering."
          runbook: "1. Check vault.key file integrity (32 bytes, permissions 0600). 2. Check vault.enc JSON validity. 3. Restore from backup if corrupt."

      # CRITICAL: Vault operation failure rate
      - alert: VaultHighErrorRate
        expr: |
          (
            sum(rate(temm1e_vault_operation_total{status="error"}[5m]))
            /
            sum(rate(temm1e_vault_operation_total[5m]))
          ) > 0.001
        for: 2m
        labels:
          severity: critical
          service: vault
        annotations:
          summary: "Vault error rate > 0.1%"
          description: "SLO target is 99.99%. Current error rate: {{ $value | humanizePercentage }}."

      # WARNING: Vault operation latency elevated
      - alert: VaultLatencyHigh
        expr: |
          histogram_quantile(0.99,
            sum by (le) (rate(temm1e_vault_operation_duration_seconds_bucket[5m]))
          ) > 0.010
        for: 5m
        labels:
          severity: warning
          service: vault
        annotations:
          summary: "Vault operation p99 > 10ms"
          description: "p99 latency: {{ $value | humanizeDuration }}. May indicate disk I/O issues for vault.enc flush."

      # WARNING: Vault key file permission drift
      - alert: VaultKeyPermissionDrift
        expr: temm1e_vault_key_permissions != 384
        for: 1m
        labels:
          severity: warning
          service: vault
        annotations:
          summary: "Vault key file permissions are not 0600"
          description: "vault.key file permissions have drifted. Current mode: {{ $value }} (expected 384 = 0o600)."
          runbook: "Run: chmod 600 ~/.temm1e/vault.key"

      # INFO: Large number of vault keys stored
      - alert: VaultKeyCountHigh
        expr: temm1e_vault_keys_total > 500
        for: 1h
        labels:
          severity: info
          service: vault
        annotations:
          summary: "Vault contains > 500 keys"
          description: "Consider key rotation and cleanup of unused secrets."
```

---

## 6. Tool Execution Alerts

```yaml
  - name: temm1e_tools
    rules:

      # WARNING: Sandbox violations detected
      - alert: SandboxViolations
        expr: increase(temm1e_tool_execution_total{status="sandbox_violation"}[15m]) > 0
        labels:
          severity: warning
          service: tools
        annotations:
          summary: "Tool sandbox violation detected"
          description: "Tool {{ $labels.tool_name }} attempted access outside workspace. Review tool declarations."
          runbook: "Check executor.rs validate_sandbox(). Review PathAccess declarations for the tool."

      # WARNING: Tool execution failure rate high
      - alert: ToolHighFailureRate
        expr: |
          (
            sum by (tool_name) (rate(temm1e_tool_execution_total{status="error"}[15m]))
            /
            sum by (tool_name) (rate(temm1e_tool_execution_total[15m]))
          ) > 0.05
        for: 10m
        labels:
          severity: warning
          service: tools
        annotations:
          summary: "Tool {{ $labels.tool_name }} failure rate > 5%"
          description: "Current failure rate: {{ $value | humanizePercentage }}."

      # INFO: Tool execution latency elevated
      - alert: ToolExecutionSlow
        expr: |
          histogram_quantile(0.99,
            sum by (tool_name, le) (rate(temm1e_tool_execution_duration_seconds_bucket[15m]))
          ) > 30
        for: 10m
        labels:
          severity: info
          service: tools
        annotations:
          summary: "Tool {{ $labels.tool_name }} p99 > 30s"
          description: "Shell or browser tools may be legitimately slow. Review if unexpected."
```

---

## 7. File Transfer Alerts

```yaml
  - name: temm1e_file_transfer
    rules:

      # WARNING: File transfer failure rate
      - alert: FileTransferHighErrors
        expr: |
          (
            sum by (channel) (rate(temm1e_file_transfer_total{status="error"}[15m]))
            /
            sum by (channel) (rate(temm1e_file_transfer_total[15m]))
          ) > 0.05
        for: 10m
        labels:
          severity: warning
          service: file_transfer
        annotations:
          summary: "File transfer error rate > 5% on {{ $labels.channel }}"
          description: "Current error rate: {{ $value | humanizePercentage }}."

      # INFO: Large file transfers
      - alert: LargeFileTransfer
        expr: |
          histogram_quantile(0.99,
            sum by (channel, le) (rate(temm1e_file_transfer_size_bytes_bucket[1h]))
          ) > 52428800
        labels:
          severity: info
          service: file_transfer
        annotations:
          summary: "p99 file size > 50MB on {{ $labels.channel }}"
          description: "Large files may cause latency issues. Check max_file_size() per channel."
```

---

## 8. Session Alerts

```yaml
  - name: temm1e_sessions
    rules:

      # WARNING: Session count approaching capacity
      - alert: SessionCountHigh
        expr: temm1e_active_sessions > 50
        for: 5m
        labels:
          severity: warning
          service: sessions
        annotations:
          summary: "Active session count > 50"
          description: "Current sessions: {{ $value }}. HashMap + RwLock may become a bottleneck."

      # INFO: Session count growing
      - alert: SessionCountGrowing
        expr: delta(temm1e_active_sessions[1h]) > 20
        labels:
          severity: info
          service: sessions
        annotations:
          summary: "20+ new sessions in the last hour"
          description: "Session growth rate may indicate a usage spike. Monitor memory."
```

---

## Alert Rule Summary

| Alert Name | Severity | Service | Condition |
|-----------|----------|---------|-----------|
| GatewayDown | Critical | gateway | up == 0 for 1m |
| GatewayUnhealthy | Critical | gateway | /health non-200 for 2m |
| GatewayHighLatency | Warning | gateway | /health p99 > 10ms |
| GatewayHighErrorRate | Warning | gateway | 5xx rate > 1% |
| GatewayColdStartSlow | Info | gateway | cold start > 50ms |
| ProviderDown | Critical | provider | health_check == 0 for 2m |
| ProviderHighErrorRate | Critical | provider | error rate > 5% |
| ProviderHighLatency | Warning | provider | p99 > 30s |
| ProviderTimeouts | Warning | provider | timeout rate > 2% |
| ProviderRateLimited | Info | provider | rate limit responses detected |
| MessageProcessingStalled | Critical | agent | 0 messages for 10m |
| MessageProcessingHighErrors | Critical | agent | error rate > 2% |
| MessageProcessingLatencyHigh | Warning | agent | p99 > 100ms |
| ExcessiveToolRounds | Warning | agent | p95 rounds > 8 |
| ChannelErrorsElevated | Info | channel | error rate > 0.5% |
| MemoryBackendDown | Critical | memory | error rate > 10% |
| MemorySearchSlow | Warning | memory | search p99 > 50ms |
| MemoryStoreSlow | Warning | memory | store p99 > 20ms |
| MemoryEntriesHigh | Warning | memory | entries > 100k |
| MemoryPoolSaturation | Info | memory | 4/5 connections |
| ProcessMemoryHigh | Critical | runtime | RSS > 100 MB |
| ProcessMemoryElevated | Warning | runtime | RSS > 50 MB |
| VaultDecryptionFailure | Critical | vault | any decrypt failure |
| VaultHighErrorRate | Critical | vault | error rate > 0.1% |
| VaultLatencyHigh | Warning | vault | p99 > 10ms |
| VaultKeyPermissionDrift | Warning | vault | perms != 0600 |
| VaultKeyCountHigh | Info | vault | keys > 500 |
| SandboxViolations | Warning | tools | any violation |
| ToolHighFailureRate | Warning | tools | failure rate > 5% |
| ToolExecutionSlow | Info | tools | p99 > 30s |
| FileTransferHighErrors | Warning | file_transfer | error rate > 5% |
| LargeFileTransfer | Info | file_transfer | p99 size > 50MB |
| SessionCountHigh | Warning | sessions | count > 50 |
| SessionCountGrowing | Info | sessions | delta > 20/h |

**Total: 34 alert rules** (8 Critical, 14 Warning, 12 Info)
