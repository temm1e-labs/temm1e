# TEMM1E Incident Response Playbook

> Incident response procedures for the TEMM1E AI agent runtime.
> Owner: SRE | Last updated: 2026-03-08
> Review cadence: quarterly

---

## Severity Levels

### SEV1 -- Critical Service Outage

| Attribute | Detail |
|-----------|--------|
| **Definition** | Complete loss of service affecting all users, or security breach involving data exposure. |
| **Examples** | Gateway down (all channels unreachable). Vault key compromise. Data breach. |
| **Triggering Alerts** | `GatewayDown`, `GatewayUnhealthy`, `VaultDecryptionFailure` (if security-related) |
| **Response Time** | Immediate (< 5 minutes) |
| **Communication** | Slack #incidents, PagerDuty, email to engineering leadership |
| **Staffing** | Incident Commander (IC) + at least one SRE + relevant domain engineer |
| **Update Cadence** | Every 15 minutes until mitigated |
| **Target Resolution** | < 1 hour to mitigate, < 4 hours to resolve |

### SEV2 -- Major Degradation

| Attribute | Detail |
|-----------|--------|
| **Definition** | Significant degradation affecting a large portion of users or a critical subsystem. |
| **Examples** | Provider completely down (no AI completions). Memory backend failure. Error rate > 5%. |
| **Triggering Alerts** | `ProviderDown`, `ProviderHighErrorRate`, `MemoryBackendDown`, `MessageProcessingHighErrors`, `ProcessMemoryHigh` |
| **Response Time** | < 15 minutes |
| **Communication** | Slack #incidents, PagerDuty |
| **Staffing** | On-call SRE + relevant domain engineer |
| **Update Cadence** | Every 30 minutes until mitigated |
| **Target Resolution** | < 2 hours to mitigate, < 8 hours to resolve |

### SEV3 -- Minor Degradation

| Attribute | Detail |
|-----------|--------|
| **Definition** | Partial degradation affecting a subset of users or non-critical functionality. |
| **Examples** | Single channel errors elevated. Tool execution failures on one tool. Latency SLO breach. File transfer errors on one channel. |
| **Triggering Alerts** | `GatewayHighErrorRate`, `GatewayHighLatency`, `ProviderHighLatency`, `ProviderTimeouts`, `MemorySearchSlow`, `VaultLatencyHigh`, `VaultKeyPermissionDrift`, `SandboxViolations`, `ToolHighFailureRate`, `FileTransferHighErrors`, `ProcessMemoryElevated`, `SessionCountHigh` |
| **Response Time** | < 30 minutes |
| **Communication** | Slack #alerts |
| **Staffing** | On-call SRE |
| **Update Cadence** | Every hour until resolved |
| **Target Resolution** | < 4 hours to mitigate, < 24 hours to resolve |

### SEV4 -- Minor Issue

| Attribute | Detail |
|-----------|--------|
| **Definition** | Cosmetic issue or early warning indicator. No immediate user impact. |
| **Examples** | Cold start slow. Memory entry count growing. Rate limiting from provider. Large file transfer detected. Session count growing. |
| **Triggering Alerts** | `GatewayColdStartSlow`, `ChannelErrorsElevated`, `ProviderRateLimited`, `MemoryEntriesHigh`, `MemoryPoolSaturation`, `VaultKeyCountHigh`, `ToolExecutionSlow`, `LargeFileTransfer`, `SessionCountGrowing` |
| **Response Time** | Next business day |
| **Communication** | Slack #monitoring |
| **Staffing** | Reviewed in daily standup |
| **Update Cadence** | As needed |
| **Target Resolution** | Within current sprint |

---

## Escalation Matrix

### On-Call Rotation

| Role | Responsibility | Contact Method |
|------|---------------|----------------|
| Primary On-Call SRE | First responder for all alerts. Triages severity. Starts incident response. | PagerDuty auto-page |
| Secondary On-Call SRE | Backup if primary does not acknowledge within 5 minutes. | PagerDuty escalation |
| Incident Commander (IC) | Leads SEV1/SEV2 response. Coordinates communication. Makes rollback decisions. | PagerDuty escalation / direct page |
| Engineering Lead | Approves production changes during incidents. Provides domain expertise. | Slack @mention / phone |

### Escalation Triggers

| Condition | Escalation Action |
|-----------|------------------|
| Alert not acknowledged within 5 minutes | Auto-escalate to secondary on-call |
| SEV1 declared | Auto-page IC and engineering lead |
| SEV2 not mitigated within 30 minutes | Escalate to IC |
| SEV3 not mitigated within 2 hours | Escalate to secondary on-call |
| Security-related incident (any SEV) | Page security team immediately |
| Error budget exhausted for any SLO | Page engineering lead for deployment freeze decision |
| Provider outage lasting > 1 hour | Escalate to provider account manager |

### Service-Specific Escalation

| Service | Primary Expert | Runbook |
|---------|---------------|---------|
| Gateway (axum, health, routing) | Platform SRE | [gateway-down.md](./gateway-down.md) |
| Provider (Anthropic, OpenAI) | AI/ML Engineer | [provider-unreachable.md](./provider-unreachable.md) |
| Vault (ChaCha20, key management) | Security Engineer | [vault-failure.md](./vault-failure.md) |
| Memory (SQLite, search) | Backend Engineer | [memory-pressure.md](./memory-pressure.md) |
| Channels (Telegram, Discord, Slack) | Integration Engineer | [high-error-rate.md](./high-error-rate.md) |
| Tools (shell, browser, sandbox) | Platform Engineer | [high-error-rate.md](./high-error-rate.md) |

---

## Incident Response Procedure

### Phase 1: Detection and Triage (0-5 minutes)

1. **Acknowledge the alert** in PagerDuty within 5 minutes.

2. **Assess severity** using the definitions above. If uncertain, start at one level higher and downgrade.

3. **Open incident channel:**
   ```
   Slack: Create thread in #incidents
   Title: [SEV<N>] <Brief description>
   Example: [SEV2] Provider Anthropic returning 500 errors
   ```

4. **Post initial assessment** using the template below (see Communication Templates).

5. **Assign roles:**
   - SEV1/SEV2: Identify IC, communications lead, and technical lead.
   - SEV3/SEV4: On-call SRE handles all roles.

### Phase 2: Diagnosis (5-30 minutes)

1. **Open the relevant runbook** for the primary alert that fired.

2. **Check the dashboard** for the affected service (refer to `dashboard-spec.md`).

3. **Correlate with recent changes:**
   ```bash
   # Check recent deployments
   git log --since="2 hours ago" --oneline

   # Check recent config changes
   git diff HEAD~5 config/

   # Check deployment history
   kubectl rollout history deployment/temm1e
   ```

4. **Narrow the blast radius:**
   - Which channels are affected?
   - Which SLOs are breaching?
   - Is this a regression or a new failure mode?

5. **Post diagnosis update** every 15 minutes (SEV1) or 30 minutes (SEV2).

### Phase 3: Mitigation (15 minutes - 2 hours)

1. **Apply the remediation steps** from the relevant runbook.

2. **If the cause is a recent deployment, rollback:**
   ```bash
   # Kubernetes
   kubectl rollout undo deployment/temm1e

   # Docker
   docker run -d <previous-image-tag>

   # Verify rollback
   curl http://localhost:8080/health
   curl http://localhost:8080/status
   ```

3. **If the cause is a configuration change, revert:**
   ```bash
   git checkout HEAD~1 -- config/temm1e.toml
   systemctl restart temm1e
   ```

4. **If the cause is external (provider outage, network issue):**
   - Enable fallback provider if applicable.
   - Notify users of degraded service via appropriate channels.
   - Monitor provider status page for updates.

5. **Confirm mitigation:**
   - Affected metrics return to baseline.
   - Error rate drops below SLO threshold.
   - Health check returns 200.
   - Test end-to-end message flow.

6. **Post mitigation update** using the template below.

### Phase 4: Resolution (Hours - Days)

1. **Identify root cause** (may require code review, log analysis, or trace analysis).

2. **Implement permanent fix** (bug fix, configuration hardening, architecture improvement).

3. **Verify fix in staging** before deploying to production.

4. **Deploy fix** following standard deployment procedure with extra monitoring.

5. **Close the incident** in PagerDuty and Slack.

6. **Schedule post-incident review** within 5 business days.

---

## Communication Templates

### Initial Assessment (Phase 1)

```
## [SEV<N>] <Brief Title>

**Status:** Investigating
**Impact:** <Who is affected and how>
**Start Time:** <YYYY-MM-DD HH:MM UTC>
**On-Call:** <Name>
**IC:** <Name> (SEV1/SEV2 only)

### What We Know
- Alert(s) firing: <Alert names>
- Affected service(s): <Service names>
- Affected channel(s): <Channel names or "all">
- Error rate: <Current error rate>

### What We're Doing
- <Current investigation step>
- <Next planned action>

### Timeline
- HH:MM - Alert fired: <Alert name>
- HH:MM - Acknowledged by <Name>
```

### Status Update (Phase 2/3)

```
## Update <N> - [SEV<N>] <Brief Title>

**Status:** Investigating / Mitigating / Mitigated
**Time:** <HH:MM UTC>

### Progress
- <What we've learned since last update>
- <Actions taken>

### Current Theory
- <Root cause hypothesis>

### Next Steps
- <Planned action 1>
- <Planned action 2>

### Metrics
- Error rate: <Current> (was <Previous>)
- Affected users: <Estimate>
- Error budget impact: <X minutes consumed>
```

### Mitigation Confirmation (Phase 3)

```
## Mitigated - [SEV<N>] <Brief Title>

**Status:** Mitigated
**Duration:** <Start time> to <Mitigation time> (<Duration>)
**Mitigated By:** <Name>

### Summary
- <What happened>
- <What we did to mitigate>
- <Current state of the system>

### Remaining Work
- [ ] Root cause analysis
- [ ] Permanent fix implementation
- [ ] Post-incident review scheduled for <Date>

### Error Budget Impact
- <SLO name>: <X minutes> consumed of <Y minutes> monthly budget (<Z%>)
```

### Resolution and Closure

```
## Resolved - [SEV<N>] <Brief Title>

**Status:** Resolved
**Total Duration:** <Duration>
**Root Cause:** <Brief description>
**Fix:** <What was deployed/changed>

### Impact
- Users affected: <Count/estimate>
- Error budget consumed: <Minutes per SLO>
- Messages failed: <Count>
- Data loss: <None / description>

### Action Items
- [ ] <Action item 1> - Owner: <Name> - Due: <Date>
- [ ] <Action item 2> - Owner: <Name> - Due: <Date>

### Post-Incident Review
- Scheduled: <Date/Time>
- Document: <Link to PIR doc>
```

---

## Post-Incident Review (PIR) Checklist

Post-incident reviews are required for all SEV1 and SEV2 incidents, and recommended for SEV3 incidents that reveal systemic issues.

### PIR Scheduling

| Severity | PIR Required | Deadline |
|----------|-------------|----------|
| SEV1 | Mandatory | Within 3 business days |
| SEV2 | Mandatory | Within 5 business days |
| SEV3 | Recommended | Within 10 business days |
| SEV4 | Optional | As needed |

### PIR Preparation

- [ ] Incident timeline assembled from alerts, logs, and Slack history
- [ ] Metrics snapshots captured (dashboards, error rates, latency graphs)
- [ ] All participants (responders, IC, stakeholders) invited
- [ ] PIR document pre-populated with facts (no blame, no speculation)

### PIR Meeting Agenda (60 minutes)

1. **Timeline review (15 min):** Walk through events chronologically.
2. **Detection analysis (10 min):** How quickly was the issue detected? Could monitoring be improved?
3. **Response analysis (10 min):** Was the response efficient? Were runbooks helpful?
4. **Root cause analysis (15 min):** What was the underlying cause? Use 5 Whys methodology.
5. **Action items (10 min):** Define concrete, time-bound action items with owners.

### PIR Document Template

```
# Post-Incident Review: [SEV<N>] <Title>

**Date:** <YYYY-MM-DD>
**Duration:** <Start> to <End> (<Duration>)
**Severity:** SEV<N>
**IC:** <Name>
**Author:** <Name>

## Summary
<2-3 sentence summary of what happened and the impact>

## Impact
- Users affected: <Count>
- Duration of user impact: <Duration>
- Error budget consumed:
  - <SLO 1>: <X> minutes of <Y> total (<Z%>)
  - <SLO 2>: ...
- Revenue impact: <If applicable>

## Timeline
| Time (UTC) | Event |
|------------|-------|
| HH:MM | <Event description> |
| HH:MM | <Event description> |

## Root Cause
<Detailed technical explanation of the root cause>

## Contributing Factors
- <Factor 1>
- <Factor 2>

## What Went Well
- <Positive observation 1>
- <Positive observation 2>

## What Could Be Improved
- <Improvement area 1>
- <Improvement area 2>

## Action Items
| ID | Action | Owner | Due Date | Priority |
|----|--------|-------|----------|----------|
| 1 | <Action> | <Name> | <Date> | P1/P2/P3 |
| 2 | <Action> | <Name> | <Date> | P1/P2/P3 |

## Lessons Learned
- <Lesson 1>
- <Lesson 2>
```

### PIR Action Item Categories

| Category | Examples |
|----------|---------|
| **Monitoring** | Add missing alert, improve detection latency, add dashboard panel |
| **Runbook** | Update runbook with new failure mode, add missing diagnosis step |
| **Architecture** | Add circuit breaker, implement fallback, add redundancy |
| **Testing** | Add integration test, update chaos experiment, add canary |
| **Process** | Update escalation matrix, improve communication template |
| **Prevention** | Add pre-deployment check, implement input validation |

---

## Quick Reference

### Emergency Commands

```bash
# Restart the service
systemctl restart temm1e

# Rollback deployment (Kubernetes)
kubectl rollout undo deployment/temm1e

# Check process status
systemctl status temm1e

# View recent logs
journalctl -u temm1e --since "10 minutes ago" -f

# Health check
curl http://localhost:8080/health

# Status check (includes provider, channels, tools)
curl http://localhost:8080/status | jq .

# Metrics endpoint
curl http://localhost:8080/metrics

# Check error budget
curl http://localhost:8080/metrics | grep -E "error|total" | head -20
```

### Key File Locations

| File | Path | Purpose |
|------|------|---------|
| Configuration | `~/.temm1e/temm1e.toml` or `/etc/temm1e/temm1e.toml` | Runtime configuration |
| Vault key | `~/.temm1e/vault.key` | 32-byte encryption key (permissions: 0600) |
| Vault data | `~/.temm1e/vault.enc` | Encrypted secrets (JSON) |
| Memory DB | `~/.temm1e/memory.db` | SQLite memory store |
| Logs | `journalctl -u temm1e` or container logs | Structured JSON logs |

### Key Metrics for Triage

| Metric | Healthy Value | Check Command |
|--------|--------------|---------------|
| `up{job="temm1e"}` | 1 | `curl -s localhost:8080/metrics \| grep ^up` |
| `temm1e_gateway_up` | 1 | Health endpoint returns 200 |
| `temm1e_provider_health_check_success` | 1 | Provider health passing |
| `process_resident_memory_bytes` | < 20 MB (idle) | `ps -o rss= -p $(pgrep temm1e)` |
| `temm1e_active_sessions` | < 50 | Session count within limits |
| `temm1e_vault_decryption_failures_total` | 0 | No decryption failures ever |
