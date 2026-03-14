# ADR-002: Trait-Based Extensibility (ZeroClaw Pattern)

## Status: Proposed

## Context
ZeroClaw proved that trait-based architecture in Rust works well for agent runtimes. OpenClaw's plugin system has security issues (41.7% of ClawHub skills vulnerable). We need extensibility without supply chain risk.

## Decision
Define 12 core traits in `temm1e-core`. Every subsystem is a trait implementation. Trait objects (`Box<dyn Trait>`) are used for runtime polymorphism where config determines the implementation.

```rust
// Core traits (all async + Send + Sync)
pub trait Provider: Send + Sync { ... }
pub trait Channel: Send + Sync { ... }
pub trait Tool: Send + Sync { ... }
pub trait Memory: Send + Sync { ... }
pub trait Tunnel: Send + Sync { ... }
pub trait Identity: Send + Sync { ... }
pub trait Peripheral: Send + Sync { ... }
pub trait Observable: Send + Sync { ... }
pub trait FileStore: Send + Sync { ... }
pub trait Vault: Send + Sync { ... }
pub trait Orchestrator: Send + Sync { ... }
pub trait Tenant: Send + Sync { ... }
```

Channel trait includes a `FileTransfer` sub-trait for bi-directional file I/O.

## Consequences
- Adding a new provider/channel/tool = implement a trait + add to build
- No runtime plugin loading needed (compiled-in, like ZeroClaw)
- Feature flags gate optional implementations
- Type-safe: compiler catches missing method implementations
- Trade-off: new extensions require Rust + recompilation
