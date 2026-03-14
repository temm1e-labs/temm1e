# T4a: Dockerfiles and CI Skeleton -- DevOps Summary

## Artifacts Created

| File | Purpose |
|------|---------|
| `Dockerfile` | Multi-stage build: cargo-chef cached deps, musl static binary, alpine:3.19 runtime |
| `docker-compose.yml` | Local dev stack: temm1e service + optional PostgreSQL |
| `.github/workflows/ci.yml` | Full CI pipeline: check, test, build, docker, security |
| `.dockerignore` | Excludes target/, .git/, docs/, Claude-Production-Grade-Suite/ |

## Dockerfile Design

- **Stage 1 (chef):** `rust:1.83-slim` base with cargo-chef installed, musl-tools, both x86_64 and aarch64 musl targets.
- **Stage 2 (planner):** Copies source, runs `cargo chef prepare` to generate dependency recipe.
- **Stage 3 (builder):** Cooks dependencies from recipe (cached layer), then builds the actual binary with `--release` and musl target. Automatically selects correct target based on `TARGETARCH`.
- **Stage 4 (runtime):** `alpine:3.19` with only curl + ca-certificates. Non-root `temm1e` user. Copies binary + `config/default.toml`. Exposes 8080, curl-based health check.

Build profile from Cargo.toml already applies: `opt-level = "z"`, LTO, `codegen-units = 1`, `strip = true`, `panic = "abort"`.

## docker-compose.yml

- **temm1e** service: port 8080, env vars (TEMM1E_MODE, ANTHROPIC_API_KEY, TELEGRAM_BOT_TOKEN), volume for `~/.temm1e` data, health check.
- **postgres** service: commented out, PostgreSQL 16 Alpine, ready to enable for persistent memory backend.

## CI Pipeline (.github/workflows/ci.yml)

5 jobs triggered on push to main and pull requests:

1. **check** -- `cargo fmt --check` and `cargo clippy -D warnings` across workspace.
2. **test** -- `cargo test --workspace` (depends on check).
3. **build** -- musl release builds for x86_64 and aarch64 matrix. Binary size gate: fails if >10 MB. Uploads artifacts.
4. **docker** -- Builds Docker image with BuildKit + GHA cache. Warns if image >50 MB.
5. **security** -- `cargo audit` for known vulnerability scanning.

All jobs use `dtolnay/rust-toolchain@stable` (pinned to 1.83) and `Swatinem/rust-cache@v2` for caching.
