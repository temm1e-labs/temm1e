# Operations Guide: Monitoring

TEMM1E provides structured logging, metrics collection, health endpoints, and optional OpenTelemetry export for production observability.

## Health Check Endpoint

The gateway exposes a health check at:

```
GET http://<host>:<port>/health
```

Response when healthy:

```json
{
  "status": "Healthy",
  "components": [
    { "name": "gateway", "status": "Healthy", "message": null },
    { "name": "provider", "status": "Healthy", "message": null },
    { "name": "memory", "status": "Healthy", "message": null },
    { "name": "vault", "status": "Healthy", "message": null }
  ]
}
```

Possible status values: `Healthy`, `Degraded`, `Unhealthy`.

The health endpoint is used by:
- Docker `HEALTHCHECK` (every 30s)
- Fly.io HTTP checks (every 30s)
- Kubernetes liveness/readiness probes
- Load balancer health checks

## Structured Logging

TEMM1E uses the `tracing` crate for structured, async-aware logging. All logs are emitted as JSON.

### Configuration

```toml
[observability]
log_level = "info"
```

Or via environment variable:

```bash
RUST_LOG=info              # Global level
RUST_LOG=temm1e=debug     # Debug for TEMM1E only
RUST_LOG=temm1e_agent=trace,temm1e_gateway=debug,info  # Per-module
```

### Log Levels

| Level | Use |
|-------|-----|
| `error` | Unrecoverable failures, security violations |
| `warn` | Recoverable issues, deprecated config, rate limiting |
| `info` | Startup, shutdown, channel connections, message processing |
| `debug` | Request/response details, config resolution, session management |
| `trace` | Wire-level data, token-by-token streaming, full request bodies |

### Log Format

Logs are emitted as JSON lines (one JSON object per line):

```json
{
  "timestamp": "2025-01-15T10:30:00.123Z",
  "level": "INFO",
  "target": "temm1e_gateway::router",
  "message": "Message routed",
  "channel": "telegram",
  "chat_id": "123456",
  "session_id": "sess-abc-123",
  "span": {
    "name": "process_message"
  }
}
```

### Correlation IDs

Every inbound message is assigned a correlation ID that propagates through all log entries for that request:

```json
{"correlation_id": "req-7f3a-4b2c", "message": "Message received", ...}
{"correlation_id": "req-7f3a-4b2c", "message": "Context assembled", ...}
{"correlation_id": "req-7f3a-4b2c", "message": "Provider call started", ...}
{"correlation_id": "req-7f3a-4b2c", "message": "Tool executed: shell", ...}
{"correlation_id": "req-7f3a-4b2c", "message": "Reply sent", ...}
```

Use the correlation ID to trace a single message through the entire pipeline.

## Metrics

TEMM1E collects the following metrics via the `Observable` trait:

### Counters

| Metric | Labels | Description |
|--------|--------|-------------|
| `temm1e_messages_total` | `channel`, `direction` (inbound/outbound) | Total messages processed |
| `temm1e_tool_calls_total` | `tool_name`, `status` (success/error) | Total tool executions |
| `temm1e_vault_access_total` | `operation` (read/write/delete) | Vault operations |
| `temm1e_file_transfers_total` | `channel`, `direction` | File transfers |
| `temm1e_auth_attempts_total` | `channel`, `result` (allowed/denied) | Authentication attempts |

### Histograms

| Metric | Labels | Description |
|--------|--------|-------------|
| `temm1e_provider_latency_seconds` | `provider`, `model` | AI provider response latency |
| `temm1e_message_processing_seconds` | `channel` | End-to-end message processing time |
| `temm1e_tool_execution_seconds` | `tool_name` | Tool execution duration |
| `temm1e_memory_search_seconds` | `backend` | Memory search latency |

### Gauges

| Metric | Labels | Description |
|--------|--------|-------------|
| `temm1e_active_sessions` | `channel` | Currently active sessions |
| `temm1e_channel_connected` | `channel` | Channel connection status (1 = connected) |

## OpenTelemetry Integration

TEMM1E supports exporting traces and metrics via OpenTelemetry.

### Configuration

```toml
[observability]
log_level = "info"
otel_enabled = true
otel_endpoint = "http://otel-collector:4317"
```

The endpoint expects an OpenTelemetry Collector running gRPC on port 4317.

### Traces

When OpenTelemetry is enabled, TEMM1E exports distributed traces covering:

- Message receipt and routing
- Context assembly (memory search, skill loading)
- Provider API calls (with model and token count attributes)
- Tool executions (with tool name and duration)
- Response streaming

Each trace spans the full lifecycle of a message, from receipt to reply.

### Collector Setup

Example OpenTelemetry Collector configuration:

```yaml
# otel-collector-config.yaml
receivers:
  otlp:
    protocols:
      grpc:
        endpoint: 0.0.0.0:4317

processors:
  batch:
    timeout: 5s

exporters:
  # Send to Grafana Cloud, Datadog, Honeycomb, etc.
  otlp/grafana:
    endpoint: "tempo-us-central1.grafana.net:443"
    headers:
      authorization: "Basic <token>"

service:
  pipelines:
    traces:
      receivers: [otlp]
      processors: [batch]
      exporters: [otlp/grafana]
    metrics:
      receivers: [otlp]
      processors: [batch]
      exporters: [otlp/grafana]
```

Docker Compose with collector:

```yaml
services:
  temm1e:
    image: ghcr.io/temm1e/temm1e:latest
    environment:
      TEMM1E_OBSERVABILITY__OTEL_ENABLED: "true"
      TEMM1E_OBSERVABILITY__OTEL_ENDPOINT: "http://otel-collector:4317"

  otel-collector:
    image: otel/opentelemetry-collector-contrib:latest
    volumes:
      - ./otel-collector-config.yaml:/etc/otelcol-contrib/config.yaml
    ports:
      - "4317:4317"
```

## Dashboard Recommendations

### Grafana

Recommended panels for a TEMM1E Grafana dashboard:

1. **Message throughput** -- `rate(temm1e_messages_total[5m])` by channel and direction
2. **Provider latency (p50/p95/p99)** -- `histogram_quantile(0.95, temm1e_provider_latency_seconds)`
3. **Tool execution rate** -- `rate(temm1e_tool_calls_total[5m])` by tool name
4. **Error rate** -- `rate(temm1e_tool_calls_total{status="error"}[5m]) / rate(temm1e_tool_calls_total[5m])`
5. **Active sessions** -- `temm1e_active_sessions` by channel
6. **Memory search latency** -- `histogram_quantile(0.95, temm1e_memory_search_seconds)`
7. **Health status** -- poll `/health` endpoint, alert on non-Healthy

### Key SLIs

| SLI | Target | Measurement |
|-----|--------|-------------|
| Message processing latency (excl. model) | < 100 ms p95 | `temm1e_message_processing_seconds` |
| Memory search latency | < 50 ms p95 | `temm1e_memory_search_seconds` |
| Tool execution success rate | > 99% | `temm1e_tool_calls_total` success/total |
| Channel availability | > 99.9% | `temm1e_channel_connected` |
| Cold start time | < 50 ms | Measure time from process start to first health check pass |

## Alert Rules

### Critical

| Alert | Condition | Action |
|-------|-----------|--------|
| Gateway down | `/health` returns non-200 for > 1 min | Restart container, check logs |
| Provider unreachable | `health_check()` fails for > 5 min | Verify API key, check provider status page |
| Vault access failure | `temm1e_vault_access_total{status="error"}` > 0 | Check vault key file, disk space |
| Sandbox violation | Any `SandboxViolation` error in logs | Investigate the triggering request |

### Warning

| Alert | Condition | Action |
|-------|-----------|--------|
| High provider latency | `provider_latency_seconds` p95 > 30s | Check provider status, consider switching models |
| Rate limiting active | `temm1e_auth_attempts_total{result="denied"}` increasing | Adjust rate limits or investigate abuse |
| Memory search slow | `memory_search_seconds` p95 > 500ms | Consider vacuuming SQLite or indexing |
| Disk usage high | Volume usage > 80% | Rotate logs, clean up old files |

## Log Aggregation

### Docker

View logs from a running container:

```bash
docker logs temm1e --follow --tail 100
```

### Fly.io

```bash
fly logs --app temm1e
```

### Centralized Logging

For production, forward JSON logs to a log aggregation service:

```yaml
# Docker Compose with Loki
services:
  temm1e:
    logging:
      driver: loki
      options:
        loki-url: "http://loki:3100/loki/api/v1/push"
        loki-batch-size: "400"
```

Or use any log shipper that reads JSON lines from stdout (Fluentd, Filebeat, Vector, etc.).

## Audit Log

When `security.audit_log = true` (default), TEMM1E logs all security-relevant events:

- Tool executions (tool name, arguments, workspace path)
- Vault access (key name, operation type)
- File transfers (file name, size, direction, channel)
- Authentication attempts (user ID, channel, result)
- Sandbox violations (attempted path, blocked reason)

Audit entries are emitted at `INFO` level with an `"audit": true` field in the JSON:

```json
{
  "timestamp": "2025-01-15T10:35:00.456Z",
  "level": "INFO",
  "audit": true,
  "event": "tool_execution",
  "tool": "shell",
  "command": "ls -la",
  "workspace": "/var/lib/temm1e/workspaces/default",
  "session_id": "sess-abc-123",
  "user_id": "telegram:456"
}
```

Filter audit events in your log aggregation system using the `"audit": true` field.
