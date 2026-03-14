# ADR-003: Dual-Mode Runtime (Cloud + Local)

## Status: Proposed

## Context
TEMM1E is cloud-native first, but must also run on developers' local machines. Same binary, different defaults.

## Decision
Support three runtime modes configured via `temm1e.mode` in config or `--mode` CLI flag:

| Mode | Bind | TLS | Memory Default | Vault Default | Browser |
|------|------|-----|---------------|---------------|---------|
| `cloud` | 0.0.0.0 | Required | PostgreSQL | Cloud KMS | Headless |
| `local` | 127.0.0.1 | Optional | SQLite | Local ChaCha20 | Headed or headless |
| `auto` | Auto-detect | Auto | Auto | Auto | Auto |

Auto-detection checks:
1. Container runtime present? (/.dockerenv, cgroup) → cloud
2. Cloud metadata endpoint reachable? (169.254.169.254) → cloud
3. Display server available? ($DISPLAY, $WAYLAND_DISPLAY) → local with GUI
4. Otherwise → local headless

## Consequences
- Single binary for all deployment scenarios
- Cloud deployments get secure defaults (TLS required, bind to all interfaces)
- Local development is frictionless (no TLS, localhost only)
- GUI mode available when display server detected
- Config override always takes precedence over auto-detection
