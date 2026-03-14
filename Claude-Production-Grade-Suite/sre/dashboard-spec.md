# TEMM1E Dashboard Specification

> Grafana-compatible dashboard panels for TEMM1E runtime monitoring.
> Owner: SRE | Last updated: 2026-03-08
> Dashboard UID: `temm1e-overview`

---

## Dashboard Layout

The dashboard is organized into 7 rows, each collapsible. Default time range: Last 6 hours. Auto-refresh: 30s.

### Variables (Template Variables)

```json
{
  "templating": {
    "list": [
      {
        "name": "channel",
        "type": "query",
        "query": "label_values(temm1e_message_processing_duration_seconds_count, channel)",
        "multi": true,
        "includeAll": true,
        "current": { "text": "All", "value": "$__all" }
      },
      {
        "name": "provider",
        "type": "query",
        "query": "label_values(temm1e_provider_request_total, provider)",
        "multi": true,
        "includeAll": true
      },
      {
        "name": "backend",
        "type": "query",
        "query": "label_values(temm1e_memory_operation_total, backend)",
        "multi": false,
        "current": { "text": "sqlite", "value": "sqlite" }
      },
      {
        "name": "tool_name",
        "type": "query",
        "query": "label_values(temm1e_tool_execution_total, tool_name)",
        "multi": true,
        "includeAll": true
      }
    ]
  }
}
```

---

## Row 1: Overview / Golden Signals

### Panel 1.1: Gateway Status (Stat)
```
Type: stat
Title: Gateway Status
Query: up{job="temm1e"}
Mappings: 1 = "UP" (green), 0 = "DOWN" (red)
Size: 2x4
```

### Panel 1.2: Active Sessions (Gauge)
```
Type: gauge
Title: Active Sessions
Query: temm1e_active_sessions
Thresholds: 0-5 green, 5-20 yellow, 20+ orange, 50+ red
Size: 2x4
```

### Panel 1.3: Uptime (Stat)
```
Type: stat
Title: Uptime
Query: time() - process_start_time_seconds{job="temm1e"}
Unit: seconds (duration)
Size: 2x4
```

### Panel 1.4: Process RSS (Gauge)
```
Type: gauge
Title: Memory (RSS)
Query: process_resident_memory_bytes{job="temm1e"}
Unit: bytes
Thresholds: 0-20MB green, 20-50MB yellow, 50-100MB red
Max: 104857600
Size: 2x4
```

### Panel 1.5: Binary Size (Stat)
```
Type: stat
Title: Binary Size
Query: temm1e_binary_size_bytes
Unit: bytes
Thresholds: 0-10MB green, 10MB+ red
Size: 2x4
```

### Panel 1.6: Error Budget Remaining (Bar Gauge)
```
Type: bargauge
Title: Error Budget Remaining (30d)
Queries:
  - "Message Processing": 1 - (sum(increase(temm1e_message_processing_duration_seconds_count{status="error"}[30d])) / sum(increase(temm1e_message_processing_duration_seconds_count[30d]))) / 0.005
  - "Provider": 1 - (sum(increase(temm1e_provider_request_total{status="error"}[30d])) / sum(increase(temm1e_provider_request_total[30d]))) / 0.01
  - "Gateway": 1 - (1 - avg_over_time(up{job="temm1e"}[30d])) / 0.001
  - "Memory": 1 - (sum(increase(temm1e_memory_operation_total{status="error"}[30d])) / sum(increase(temm1e_memory_operation_total[30d]))) / 0.001
  - "Vault": 1 - (sum(increase(temm1e_vault_operation_total{status="error"}[30d])) / sum(increase(temm1e_vault_operation_total[30d]))) / 0.0001
Thresholds: 0-25% red, 25-50% orange, 50-100% green
Unit: percentunit
Size: 12x4
```

---

## Row 2: Message Processing (RED Method)

### Panel 2.1: Request Rate (Time Series)
```
Type: timeseries
Title: Message Rate by Channel
Query: sum by (channel) (rate(temm1e_message_processing_duration_seconds_count{channel=~"$channel"}[5m]))
Legend: {{ channel }}
Unit: reqps
Size: 4x6
```

### Panel 2.2: Error Rate (Time Series)
```
Type: timeseries
Title: Message Error Rate by Channel
Query: |
  sum by (channel) (rate(temm1e_message_processing_duration_seconds_count{status="error", channel=~"$channel"}[5m]))
  /
  sum by (channel) (rate(temm1e_message_processing_duration_seconds_count{channel=~"$channel"}[5m]))
Legend: {{ channel }}
Unit: percentunit
Thresholds: line at 0.005 (SLO)
Size: 4x6
```

### Panel 2.3: Duration Heatmap (Heatmap)
```
Type: heatmap
Title: Message Processing Duration
Query: sum by (le) (increase(temm1e_message_processing_duration_seconds_bucket{channel=~"$channel"}[5m]))
Color: scheme "spectral"
yAxis: unit seconds
Size: 4x6
```

### Panel 2.4: Latency Percentiles (Time Series)
```
Type: timeseries
Title: Message Processing Latency (p50/p95/p99)
Queries:
  - p50: histogram_quantile(0.50, sum by (le) (rate(temm1e_message_processing_duration_seconds_bucket{channel=~"$channel"}[5m])))
  - p95: histogram_quantile(0.95, sum by (le) (rate(temm1e_message_processing_duration_seconds_bucket{channel=~"$channel"}[5m])))
  - p99: histogram_quantile(0.99, sum by (le) (rate(temm1e_message_processing_duration_seconds_bucket{channel=~"$channel"}[5m])))
Unit: seconds
Thresholds: line at 0.100 (SLO p99)
Size: 6x6
```

### Panel 2.5: Tool Rounds Distribution (Histogram)
```
Type: histogram
Title: Tool Rounds per Message
Query: sum by (le) (increase(temm1e_tool_rounds_per_message_bucket[5m]))
Thresholds: line at 10 (MAX_TOOL_ROUNDS)
Size: 6x6
```

---

## Row 3: AI Provider (RED Method)

### Panel 3.1: Provider Request Rate (Time Series)
```
Type: timeseries
Title: Provider Request Rate
Query: sum by (provider) (rate(temm1e_provider_request_total{provider=~"$provider"}[5m]))
Legend: {{ provider }}
Unit: reqps
Size: 4x6
```

### Panel 3.2: Provider Error Rate (Time Series)
```
Type: timeseries
Title: Provider Error Rate
Query: |
  sum by (provider) (rate(temm1e_provider_request_total{status=~"error|timeout", provider=~"$provider"}[5m]))
  /
  sum by (provider) (rate(temm1e_provider_request_total{provider=~"$provider"}[5m]))
Legend: {{ provider }}
Unit: percentunit
Thresholds: line at 0.01 (SLO)
Size: 4x6
```

### Panel 3.3: Provider Latency (Time Series)
```
Type: timeseries
Title: Provider Latency (p50/p95/p99)
Queries:
  - p50: histogram_quantile(0.50, sum by (provider, le) (rate(temm1e_provider_request_duration_seconds_bucket{provider=~"$provider"}[5m])))
  - p95: histogram_quantile(0.95, sum by (provider, le) (rate(temm1e_provider_request_duration_seconds_bucket{provider=~"$provider"}[5m])))
  - p99: histogram_quantile(0.99, sum by (provider, le) (rate(temm1e_provider_request_duration_seconds_bucket{provider=~"$provider"}[5m])))
Legend: {{ provider }} p{{ quantile }}
Unit: seconds
Thresholds: line at 30 (SLO p99)
Size: 4x6
```

### Panel 3.4: Provider Health Check (Stat with Sparkline)
```
Type: stat
Title: Provider Health
Query: temm1e_provider_health_check_success
Mappings: 1 = "Healthy" (green), 0 = "Unhealthy" (red)
GraphMode: area
Size: 4x3
```

### Panel 3.5: Provider Request Breakdown (Pie)
```
Type: piechart
Title: Provider Request Status Breakdown
Query: sum by (status) (increase(temm1e_provider_request_total{provider=~"$provider"}[1h]))
Size: 4x3
```

---

## Row 4: Memory & Vault Operations (RED Method)

### Panel 4.1: Memory Operation Rate (Time Series)
```
Type: timeseries
Title: Memory Operation Rate
Query: sum by (operation) (rate(temm1e_memory_operation_total{backend=~"$backend"}[5m]))
Legend: {{ operation }}
Unit: ops
Size: 4x6
```

### Panel 4.2: Memory Operation Latency (Time Series)
```
Type: timeseries
Title: Memory Operation Latency (p99)
Query: |
  histogram_quantile(0.99,
    sum by (operation, le) (rate(temm1e_memory_operation_duration_seconds_bucket{backend=~"$backend"}[5m]))
  )
Legend: {{ operation }}
Unit: seconds
Thresholds: line at 0.050 (search SLO), line at 0.020 (store SLO)
Size: 4x6
```

### Panel 4.3: Memory Entry Count (Time Series)
```
Type: timeseries
Title: Total Memory Entries
Query: temm1e_memory_entries_total
Unit: short
Thresholds: line at 100000
Size: 4x6
```

### Panel 4.4: Vault Operation Rate (Time Series)
```
Type: timeseries
Title: Vault Operation Rate
Query: sum by (operation) (rate(temm1e_vault_operation_total[5m]))
Legend: {{ operation }}
Unit: ops
Size: 4x6
```

### Panel 4.5: Vault Operation Latency (Time Series)
```
Type: timeseries
Title: Vault Operation Latency (p99)
Query: |
  histogram_quantile(0.99,
    sum by (operation, le) (rate(temm1e_vault_operation_duration_seconds_bucket[5m]))
  )
Legend: {{ operation }}
Unit: seconds
Thresholds: line at 0.010 (SLO)
Size: 4x6
```

### Panel 4.6: Vault Decryption Failures (Stat)
```
Type: stat
Title: Vault Decryption Failures
Query: increase(temm1e_vault_decryption_failures_total[24h])
Thresholds: 0 = green, 1+ = red
Size: 4x6
```

---

## Row 5: Tool Execution (RED Method)

### Panel 5.1: Tool Execution Rate (Time Series)
```
Type: timeseries
Title: Tool Execution Rate
Query: sum by (tool_name) (rate(temm1e_tool_execution_total{tool_name=~"$tool_name"}[5m]))
Legend: {{ tool_name }}
Unit: ops
Size: 4x6
```

### Panel 5.2: Tool Execution Errors (Time Series)
```
Type: timeseries
Title: Tool Execution Errors
Query: sum by (tool_name, status) (rate(temm1e_tool_execution_total{status=~"error|sandbox_violation", tool_name=~"$tool_name"}[5m]))
Legend: {{ tool_name }} ({{ status }})
Unit: ops
Size: 4x6
```

### Panel 5.3: Tool Execution Duration (Time Series)
```
Type: timeseries
Title: Tool Execution Duration (p99)
Query: |
  histogram_quantile(0.99,
    sum by (tool_name, le) (rate(temm1e_tool_execution_duration_seconds_bucket{tool_name=~"$tool_name"}[5m]))
  )
Legend: {{ tool_name }}
Unit: seconds
Size: 4x6
```

### Panel 5.4: Sandbox Violations (Stat)
```
Type: stat
Title: Sandbox Violations (24h)
Query: sum(increase(temm1e_tool_execution_total{status="sandbox_violation"}[24h]))
Thresholds: 0 = green, 1+ = red
Size: 4x3
```

### Panel 5.5: Enabled Tools (Table)
```
Type: table
Title: Registered Tools
Query: temm1e_tool_registered{tool_name=~"$tool_name"}
Columns: tool_name, enabled
Size: 4x3
```

---

## Row 6: Channel Status & File Transfer

### Panel 6.1: Channel Status Matrix (Status History)
```
Type: status-history
Title: Channel Status
Query: temm1e_channel_connected{channel=~"$channel"}
Mappings: 1 = "Connected" (green), 0 = "Disconnected" (red)
Size: 12x3
```

### Panel 6.2: Messages per Channel (Bar Chart)
```
Type: barchart
Title: Messages per Channel (1h)
Query: sum by (channel) (increase(temm1e_message_processing_duration_seconds_count{channel=~"$channel"}[1h]))
Unit: short
Size: 4x6
```

### Panel 6.3: File Transfer Rate (Time Series)
```
Type: timeseries
Title: File Transfer Rate
Query: sum by (channel, direction) (rate(temm1e_file_transfer_total{channel=~"$channel"}[5m]))
Legend: {{ channel }} {{ direction }}
Unit: ops
Size: 4x6
```

### Panel 6.4: File Transfer Size Distribution (Heatmap)
```
Type: heatmap
Title: File Transfer Size Distribution
Query: sum by (le) (increase(temm1e_file_transfer_size_bytes_bucket{channel=~"$channel"}[15m]))
yAxis: unit bytes
Size: 4x6
```

### Panel 6.5: File Transfer Bytes (Time Series)
```
Type: timeseries
Title: File Transfer Throughput
Query: sum by (channel, direction) (rate(temm1e_file_transfer_bytes_total{channel=~"$channel"}[5m]))
Legend: {{ channel }} {{ direction }}
Unit: Bps
Size: 6x6
```

### Panel 6.6: File Transfer Errors (Time Series)
```
Type: timeseries
Title: File Transfer Errors
Query: sum by (channel) (rate(temm1e_file_transfer_total{status="error", channel=~"$channel"}[5m]))
Legend: {{ channel }}
Unit: ops
Size: 6x6
```

---

## Row 7: Resource Usage

### Panel 7.1: CPU Usage (Time Series)
```
Type: timeseries
Title: CPU Usage
Query: rate(process_cpu_seconds_total{job="temm1e"}[5m])
Unit: percentunit
Size: 4x6
```

### Panel 7.2: Memory RSS (Time Series)
```
Type: timeseries
Title: Resident Memory (RSS)
Query: process_resident_memory_bytes{job="temm1e"}
Unit: bytes
Thresholds: line at 20971520 (20MB idle target), line at 52428800 (50MB warning)
Size: 4x6
```

### Panel 7.3: Open File Descriptors (Time Series)
```
Type: timeseries
Title: Open File Descriptors
Query: process_open_fds{job="temm1e"}
Max: process_max_fds{job="temm1e"}
Unit: short
Size: 4x6
```

### Panel 7.4: Goroutines / Tokio Tasks (Time Series)
```
Type: timeseries
Title: Tokio Active Tasks
Query: temm1e_tokio_active_tasks
Unit: short
Size: 4x6
```

### Panel 7.5: SQLite Pool Connections (Time Series)
```
Type: timeseries
Title: SQLite Pool Connections
Queries:
  - Active: temm1e_memory_pool_active_connections
  - Idle: temm1e_memory_pool_idle_connections
  - Max: 5 (constant line)
Unit: short
Size: 4x6
```

### Panel 7.6: Network I/O (Time Series)
```
Type: timeseries
Title: Network I/O
Queries:
  - Received: rate(temm1e_network_receive_bytes_total[5m])
  - Transmitted: rate(temm1e_network_transmit_bytes_total[5m])
Unit: Bps
Size: 4x6
```

---

## Panel Summary

| Row | Panels | Focus |
|-----|--------|-------|
| 1. Overview | 6 | Golden signals, status, error budgets |
| 2. Message Processing | 5 | RED method for message pipeline |
| 3. AI Provider | 5 | RED method for provider calls |
| 4. Memory & Vault | 6 | RED method for storage operations |
| 5. Tool Execution | 5 | RED method for sandboxed tools |
| 6. Channel & Files | 6 | Channel connectivity, file transfer |
| 7. Resource Usage | 6 | CPU, memory, FDs, tasks, pool, network |
| **Total** | **39** | |
