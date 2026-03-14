# ADR-001: Rust Workspace with Multi-Crate Structure

## Status: Proposed

## Context
TEMM1E is a complex system with 12+ subsystems (channels, providers, memory, tools, etc.). We need a code organization strategy that enables:
- Independent compilation of subsystems
- Clear dependency boundaries
- Feature-flag control over optional components
- Fast incremental builds during development

## Decision
Use a Cargo workspace with 13 crates:
- `temm1e-core`: Trait definitions + shared types (zero external deps beyond serde/async-trait)
- `temm1e-gateway`: axum-based gateway server
- `temm1e-agent`: Agent runtime loop
- `temm1e-providers`: AI provider implementations
- `temm1e-channels`: Messaging channel implementations
- `temm1e-memory`: Memory backend implementations
- `temm1e-vault`: Secrets management
- `temm1e-tools`: Built-in tool implementations
- `temm1e-skills`: Skill loading & management
- `temm1e-automation`: Heartbeat & cron
- `temm1e-observable`: Logging, metrics, tracing
- `temm1e-filestore`: File storage backends
- `temm1e` (binary): CLI entry point

## Consequences
- Clear separation of concerns — each crate has a focused responsibility
- Feature flags can exclude entire crates (e.g., `--no-default-features` to skip browser)
- Parallel compilation of independent crates speeds up builds
- More complex Cargo.toml management
- All crates depend on `temm1e-core` for trait definitions
