# Runbook: Provider Unreachable

> **Alert:** `ProviderDown` / `ProviderHighErrorRate`
> **Severity:** Critical
> **Service:** provider
> **Response Time:** < 5 minutes
> **Last Updated:** 2026-03-08

---

## Symptoms and Detection

### Triggering Alerts

| Alert | Condition | Duration |
|-------|-----------|----------|
| `ProviderDown` | `temm1e_provider_health_check_success == 0` | 2 minutes |
| `ProviderHighErrorRate` | Provider error rate > 5% | 5 minutes |

### Related Warning Alerts

| Alert | Condition | Duration |
|-------|-----------|----------|
| `ProviderHighLatency` | Provider p99 > 30s | 5 minutes |
| `ProviderTimeouts` | Timeout rate > 2% | 5 minutes |
| `ProviderRateLimited` | Rate limit responses detected | 2 minutes |

### Observable Symptoms

- PagerDuty incident fires with `severity=critical, service=provider`.
- Users report that the agent is not responding or responding with errors.
- `temm1e_provider_health_check_success` gauge is 0 for the affected provider.
- `temm1e_provider_request_total{status="error"}` counter is incrementing rapidly.
- `temm1e_provider_request_total{status="timeout"}` counter increasing (requests exceeding 60s timeout).
- Error messages in logs: `Temm1eError::Provider("Anthropic request failed: ...")` or `Temm1eError::Auth(...)`.

---

## Impact Assessment

| Dimension | Impact |
|-----------|--------|
| **User-facing** | Agent cannot generate responses. Messages are received but processing fails at the provider completion step. |
| **SLO burn** | Provider Availability SLO (99.0%) has a 7.2 h/month budget. At 100% failure, budget exhausts in 7.2 hours. |
| **Blast radius** | All channels are affected since all share the same provider. If a fallback provider is configured, only primary traffic is impacted. |
| **Data loss risk** | None. Messages are received and sessions are maintained. Failed completions return errors but do not corrupt state. |

---

## Step-by-Step Diagnosis

### Step 1: Identify the affected provider

```bash
# Check which provider is configured
curl http://localhost:8080/status | jq .provider

# Check provider metrics
curl http://localhost:8080/metrics | grep temm1e_provider_health_check_success
curl http://localhost:8080/metrics | grep temm1e_provider_request_total
```

### Step 2: Verify API key validity

```bash
# Check if the API key is in the vault
temm1e status

# Test the API key directly
# For Anthropic:
curl -s -o /dev/null -w "%{http_code}" \
  -H "x-api-key: $(temm1e vault get anthropic-api-key)" \
  -H "anthropic-version: 2023-06-01" \
  https://api.anthropic.com/v1/messages \
  -X HEAD
# 405 = reachable (HEAD not supported but server responds)
# 401 = invalid API key
# 000 = network unreachable
```

### Step 3: Check upstream provider status

| Provider | Status Page |
|----------|-------------|
| Anthropic | https://status.anthropic.com |
| OpenAI-compatible | Provider-specific |

```bash
# Quick connectivity check to Anthropic
curl -s -o /dev/null -w "HTTP %{http_code}, time %{time_total}s\n" \
  https://api.anthropic.com/v1/messages
```

### Step 4: Check application logs for error details

```bash
# Filter provider-related logs
journalctl -u temm1e --since "10 minutes ago" | grep -i "provider\|anthropic\|complete\|stream"

# Look for specific error patterns:
# - "Anthropic request failed" = network/connection error
# - "API error (401)" = authentication failure (Temm1eError::Auth)
# - "API error (429)" = rate limiting (Temm1eError::RateLimited)
# - "API error (500/502/503)" = upstream server error
# - "Health check failed" = health_check() HEAD request failed
```

### Step 5: Check network connectivity

```bash
# DNS resolution
dig api.anthropic.com

# TCP connectivity
nc -zv api.anthropic.com 443 -w 5

# TLS handshake
openssl s_client -connect api.anthropic.com:443 -servername api.anthropic.com </dev/null 2>&1 | head -20

# Check for proxy/firewall issues
curl -v https://api.anthropic.com/v1/messages 2>&1 | head -30
```

### Step 6: Check rate limiting

```bash
# Check rate limit metrics
curl http://localhost:8080/metrics | grep "rate_limited"

# Review recent request volume
curl http://localhost:8080/metrics | grep temm1e_provider_request_total
```

---

## Remediation

### Remediation A: Upstream Provider Outage

1. Confirm outage on the provider's status page.

2. If a fallback provider is configured, enable it:
   ```toml
   # temm1e.toml
   [provider]
   name = "openai_compat"  # Switch to fallback
   # or configure automatic fallback:
   fallback = "openai_compat"
   ```

3. Restart with the fallback provider:
   ```bash
   systemctl restart temm1e
   ```

4. Monitor fallback provider health:
   ```bash
   curl http://localhost:8080/metrics | grep temm1e_provider_health_check_success
   ```

5. When the primary provider recovers, switch back and verify.

### Remediation B: Invalid or Expired API Key

1. Verify the key is present and readable:
   ```bash
   temm1e vault list
   ```

2. If the key is missing or corrupt, re-store it:
   ```bash
   # Store a new API key in the vault
   echo -n "sk-ant-api03-..." | temm1e vault store anthropic-api-key
   ```

3. If using `vault://temm1e/anthropic-api-key` URI resolution, verify the URI in config matches the vault key name.

4. Restart to pick up the new key:
   ```bash
   systemctl restart temm1e
   ```

### Remediation C: Rate Limiting

1. Check current request rate and concurrency:
   ```bash
   curl http://localhost:8080/metrics | grep temm1e_provider_request
   ```

2. Reduce concurrency by throttling incoming messages or implementing request queuing.

3. If persistent, upgrade the API tier with the provider or distribute load across multiple API keys.

4. The `Temm1eError::RateLimited` variant is intentionally excluded from SLO error counting, but it still impacts user experience.

### Remediation D: Network/DNS Issues

1. If DNS fails:
   ```bash
   # Try alternative DNS
   dig @8.8.8.8 api.anthropic.com
   # Add to /etc/hosts as temporary workaround if needed
   ```

2. If firewall/proxy is blocking:
   ```bash
   # Check proxy settings
   env | grep -i proxy
   # Ensure HTTPS traffic to provider API is allowed
   ```

3. If TLS issues:
   ```bash
   # Update CA certificates
   update-ca-certificates  # Debian/Ubuntu
   ```

### Remediation E: Provider Timeout Issues

1. The health check in `AnthropicProvider::health_check()` sends a HEAD request to `/v1/messages`. A 405 response is considered healthy (server is reachable).

2. If completions time out (> 60s as defined in SLO):
   - Check if the model being used is experiencing elevated latency.
   - Consider switching to a faster model (e.g., Haiku instead of Opus).
   - Check for unusually large prompt sizes.

---

## Prevention Measures

1. **API key rotation:** Schedule quarterly key rotation. Store backup keys in the vault.

2. **Fallback provider configuration:** Always configure at least one fallback provider in production:
   ```toml
   [provider]
   name = "anthropic"
   fallback = "openai_compat"
   ```

3. **Health check monitoring:** The `Provider::health_check()` runs every 30 seconds via heartbeat. Ensure metrics are being scraped.

4. **Error budget alerting:** Set up SLO-based alerts. When provider error budget < 50% (3.6 hours remaining), enable automatic fallback.

5. **Rate limit headroom:** Monitor `ProviderRateLimited` info alerts. Proactively upgrade API tier before rate limits become a regular issue.

6. **Network monitoring:** Monitor DNS resolution and TLS certificate expiry for provider endpoints.

7. **Model selection:** Default to a model that balances latency and quality. Avoid using the slowest models for time-sensitive interactions.

---

## Related Runbooks

- [High Error Rate](./high-error-rate.md) -- provider errors cascade to message processing errors
- [Gateway Down](./gateway-down.md) -- if provider failure causes process panic
- [Vault Failure](./vault-failure.md) -- if API key retrieval from vault is the root cause
- [Incident Response](./incident-response.md) -- escalation procedures
