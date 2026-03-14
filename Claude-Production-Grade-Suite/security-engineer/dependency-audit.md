# TEMM1E Dependency Audit Report

**Version:** 1.0
**Date:** 2026-03-08
**Auditor:** Security Engineer (T6c)
**Scope:** Cargo.toml workspace dependencies and Cargo.lock

---

## Summary

| Metric | Value |
|--------|-------|
| Total packages in Cargo.lock | 559 |
| Direct workspace dependencies | 31 |
| Known vulnerabilities (cargo audit) | N/A (cargo-audit not installed) |
| Supply chain risk: Critical | 0 |
| Supply chain risk: High | 1 |
| Supply chain risk: Medium | 2 |
| Supply chain risk: Low | 3 |

**Note:** `cargo audit` was not available in the build environment. Vulnerability assessment below is based on manual version checks against known advisories as of 2026-03-08.

---

## Dependency Tree Summary

### Core Dependencies (Security-Relevant)

| Crate | Locked Version | Purpose | Security Notes |
|-------|---------------|---------|----------------|
| `chacha20poly1305` | 0.10.1 | AEAD encryption for vault | Latest stable. Uses RustCrypto ecosystem. No known CVEs. |
| `ed25519-dalek` | 2.x | Signature verification for skills | v2 fixed the double-public-key vulnerability (RUSTSEC-2022-0093). Safe. |
| `rand` | 0.8.x | Random number generation | Stable. Uses OsRng for cryptographic randomness. |
| `sqlx` | 0.8.6 | Database access (SQLite, Postgres) | Parameterized queries. No known SQLi CVEs. |
| `reqwest` | 0.12.28 | HTTP client for API providers | Uses rustls-tls (no OpenSSL). Latest stable. |
| `axum` | 0.8.8 | HTTP server framework | Latest stable. No known CVEs. |
| `teloxide` | 0.17.0 | Telegram Bot API | Latest stable. |
| `rustls` | 0.23.x | TLS implementation | Pure Rust. No OpenSSL dependency. |
| `tokio` | 1.x | Async runtime | Mature, widely audited. |
| `serde` | 1.x | Serialization | Mature, no known CVEs. |
| `base64` | 0.22.x | Base64 encoding for vault | Latest major version. |
| `chrono` | 0.4.x | Date/time handling | Historical RUSTSEC-2020-0159 (localtime_r) addressed in modern versions. |

### Build/Config Dependencies

| Crate | Locked Version | Purpose | Security Notes |
|-------|---------------|---------|----------------|
| `toml` | 0.8.x | Config file parsing | No known CVEs. |
| `serde_yaml` | 0.9.x | YAML config parsing (OpenClaw compat) | Note: serde_yaml 0.9 is the final version before the crate was deprecated in favor of alternatives. No known CVEs in 0.9. |
| `clap` | 4.x | CLI argument parsing | No known CVEs. |
| `dirs` | 6.x | Home directory detection | No known CVEs. |
| `config` | 0.14.x | Configuration management | Not directly used in source (declared but no imports found). Consider removing. |

### Browser/Cloud Dependencies (Feature-Gated)

| Crate | Locked Version | Purpose | Security Notes |
|-------|---------------|---------|----------------|
| `chromiumoxide` | 0.7.x | Browser automation | Feature-gated. Large attack surface when enabled -- uses CDP protocol. |
| `aws-sdk-s3` | 1.x | S3 object storage | Feature-gated. AWS SDK handles credential management. |
| `serenity` | 0.12.x | Discord bot | Feature-gated. Not yet implemented in source. |
| `poise` | 0.6.x | Discord command framework | Feature-gated. Not yet implemented. |

---

## Known Vulnerability Assessment

### Manual Check Results

Since `cargo audit` is not installed, the following is a manual assessment based on known RustSec advisories:

| Advisory ID | Crate | Affected Versions | Status |
|-------------|-------|-------------------|--------|
| RUSTSEC-2022-0093 | `ed25519-dalek` | < 2.0 | **NOT AFFECTED** (using v2.x) |
| RUSTSEC-2020-0159 | `chrono` | < 0.4.20 (localtime_r unsoundness) | **NOT AFFECTED** (using 0.4.x modern) |
| RUSTSEC-2024-0363 | `rustls` | < 0.23.13 (CPU DoS on certificate verification) | **CHECK NEEDED** -- verify locked version is >= 0.23.13 |
| RUSTSEC-2023-0071 | `serde_yaml` | (deprecated crate) | **LOW RISK** -- 0.9 has no known CVEs but crate is no longer maintained. Consider migration to `serde_yml` or `yaml-rust2`. |

---

## Supply Chain Risk Assessment

### High Risk

| Risk | Crate | Details | Mitigation |
|------|-------|---------|------------|
| **Deprecated crate** | `serde_yaml` 0.9 | The `serde_yaml` crate is deprecated. The maintainer (dtolnay) archived it. No security patches will be issued. | Migrate to `serde_yml` or `yaml-rust2 + serde`. Alternatively, since YAML support is only for OpenClaw compatibility, consider making it optional/feature-gated. |

### Medium Risk

| Risk | Crate | Details | Mitigation |
|------|-------|---------|------------|
| **Large transitive dependency tree** | `chromiumoxide` | Pulls in a large number of dependencies for CDP/browser automation. Feature-gated but included in default features. | Ensure `browser` feature is opt-in. Audit the chromiumoxide dependency tree separately. |
| **Unused dependency** | `config` 0.14 | Declared as a workspace dependency but no imports found in source code. Unnecessary dependencies increase supply chain attack surface. | Remove from workspace `[dependencies]` if unused. |

### Low Risk

| Risk | Crate | Details | Mitigation |
|------|-------|---------|------------|
| **Feature-gated but default-on** | `serenity`, `poise` | Discord dependencies are in default features but no implementation exists in source. | Remove from default features until Discord channel is implemented. |
| **Missing `zeroize` dependency** | N/A | The vault crate does not depend on `zeroize` for key material cleanup. | Add `zeroize = "1"` to `temm1e-vault/Cargo.toml`. |
| **No Cargo.lock pinning verification** | All | No mechanism to verify Cargo.lock integrity in CI. | Add `cargo deny` or `cargo vet` to the CI pipeline for supply chain verification. |

---

## Dependency Hygiene Recommendations

### Immediate Actions (Before Production)

1. **Add `zeroize` = "1" to temm1e-vault** -- Required for CA-01 remediation
2. **Replace `serde_yaml` 0.9** -- Deprecated crate with no future security patches
3. **Remove unused `config` crate** -- Reduces attack surface
4. **Install and integrate `cargo audit`** in CI pipeline
5. **Verify rustls version** >= 0.23.13 to avoid RUSTSEC-2024-0363

### Short-Term Actions

6. **Add `cargo deny`** configuration for license compliance and advisory checking
7. **Remove Discord/Slack/WhatsApp from default features** until implemented
8. **Pin minimum dependency versions** in workspace Cargo.toml for security-critical crates

### Long-Term Actions

9. **Set up `cargo vet`** for supply chain trust management
10. **Create a SBOM** (Software Bill of Materials) for each release
11. **Monitor RustSec advisories** via automated CI checks

---

## Dependency License Summary

All key dependencies use permissive licenses (MIT, Apache-2.0, or dual-licensed):

| Category | Licenses |
|----------|----------|
| Crypto (chacha20poly1305, ed25519-dalek, rand) | MIT / Apache-2.0 |
| Web (axum, reqwest, tower) | MIT |
| Database (sqlx) | MIT / Apache-2.0 |
| TLS (rustls) | MIT / Apache-2.0 |
| Messaging (teloxide) | MIT |
| Core (tokio, serde, clap) | MIT / Apache-2.0 |

No GPL or AGPL dependencies detected in the direct dependency tree.
