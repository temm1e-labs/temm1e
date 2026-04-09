# TEMM1E v4.8.0 TUI Enhancement — Research & Zero-Risk Documentation

This directory contains the full research and zero-risk analysis for the
TEMM1E v4.8.0 TUI polish & observability release. **No code is modified
until every document here is reviewed and approved.**

## Document index

| # | File | Purpose |
|---|------|---------|
| 00 | [`00-findings.md`](./00-findings.md) | All discovered problems, root causes, and load-bearing discoveries from parallel research agents. |
| 01 | [`01-tier-a-zero-risk-report.md`](./01-tier-a-zero-risk-report.md) | Tier A (pure TUI polish, ZERO RISK) — 6 items including the newly discovered empty-commands bug fix. |
| 02 | [`02-tier-b-zero-risk-report.md`](./02-tier-b-zero-risk-report.md) | Tier B (observability enhancement, ZERO RISK) — including the complete `AgentTaskPhase` pattern-match audit, serialization verification, latent semantic bug discovery + fix, and atomic commit strategy. |
| 03 | [`03-tier-c-zero-risk-report.md`](./03-tier-c-zero-risk-report.md) | Tier C (Escape → cancel, ZERO RISK after pivot) — reuses existing `Arc<AtomicBool>` interrupt path in `runtime.rs:919-944` that gateway worker already uses for higher-priority message preemption. Runtime changes: none. |
| 04 | [`04-tier-d-zero-risk-report.md`](./04-tier-d-zero-risk-report.md) | Tier D (polish picks, ZERO RISK) — 5 optional enhancements. |
| 05 | [`05-implementation-spec.md`](./05-implementation-spec.md) | File-by-file exact change plan with line references and code snippets. |
| 06 | [`06-testing-strategy.md`](./06-testing-strategy.md) | Test strategy — unit, integration, manual, multi-turn CLI self-test. |

## Scope summary

**7 original problems + 1 newly discovered bug**, resolved across 4 risk tiers:

| Tier | Theme | Items | Risk |
|------|-------|-------|------|
| A | Pure TUI polish | 6 | ZERO |
| B | Observability enhancement | 5 | ZERO (after full grep + atomic commit) |
| C | Escape → cancel | 5 | ZERO (reuses existing `Arc<AtomicBool>` interrupt) |
| D | Polish picks (optional) | 5 | ZERO |

**All four tiers are ZERO risk.**

Total: **21 deliverables** across ~15 modified files + 4 new files.

## The newly discovered bug

During research, user reported that most slash commands (`/config`, `/keys`,
`/usage`, `/status`, `/model`) open an empty panel. Root cause found in
[`00-findings.md` §2](./00-findings.md#2-empty-command-overlays-critical-bug):
`crates/temm1e-tui/src/views/config_panel.rs:12-48` is a single stub
function that renders the same placeholder for all five overlay kinds and
physically cannot access `AppState` (no data parameter). Fix is mechanical
and is folded into Tier A as item **A1**.

## Approval gate

After review, the user approves scope (full plan, or a subset). On approval,
the implementation proceeds tier by tier against
[`05-implementation-spec.md`](./05-implementation-spec.md) with the testing
protocol in [`06-testing-strategy.md`](./06-testing-strategy.md) enforced at
each stage.

**No code changes until the gate opens.**

## Related artifacts

- Original pre-research plan: `Claude-Production-Grade-Suite/tui-enhancement/plan.md`
- Release protocol: `docs/RELEASE_PROTOCOL.md`
- Agentic developer protocol: `AGENTIC_DEVELOPER_PROTOCOL.md`
- Project memory (workflow rules): `~/.claude/projects/-Users-quanduong-Documents-Github-skyclaw/memory/MEMORY.md`
