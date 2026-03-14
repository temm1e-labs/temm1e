# ADR-005: Deny-by-Default Security Model

## Status: Proposed

## Context
OpenClaw's security is opt-in (sandbox is optional, WebSocket has cross-origin issues). ZeroClaw is better (deny-by-default allowlists) but TEMM1E handles user credentials via chat, making security even more critical.

## Decision
All security policies are mandatory and enforced by default:

1. **Channel allowlists**: Empty allowlist = deny all. No exceptions.
2. **Tool sandboxing**: Mandatory workspace-scoped execution. No opt-out.
3. **File system**: 14 system dirs + sensitive dotfiles blocked. Symlink escape detection.
4. **Network egress**: Only configured allowlist domains.
5. **Vault encryption**: ChaCha20-Poly1305 AEAD. Plaintext secrets never touch disk.
6. **Credential detection**: Auto-detect and encrypt API keys in chat messages.
7. **Audit logging**: All tool executions, vault access, and file transfers logged.
8. **Skill capabilities**: Skills declare required permissions; runtime enforces them.

Encryption: ChaCha20-Poly1305 for vault, Ed25519 for skill signing.

## Consequences
- Secure by default — users can't accidentally expose themselves
- Slightly more setup friction (must configure allowlists)
- Performance cost of sandboxing is negligible for I/O-bound operations
- Audit log may grow large — rotation policy needed
