# Agentic Developer Protocol

**Status: MANDATORY for ALL releases, features, and implementations.**

This protocol defines how Claude Code (the agentic developer) must self-test and validate all TEMM1E work. The user does not have time or capacity to manually test — Claude must do it.

---

## Core Principle

**Every change must be proven working before it is presented as done.** No exceptions.

- Build it
- Run it
- Test it
- Debug it yourself
- Only then call it done

---

## 1. Compilation Gate

Every change MUST pass all four checks before proceeding:

```bash
cargo check --workspace                                    # Must compile
cargo clippy --workspace --all-targets --all-features -- -D warnings  # Zero warnings
cargo fmt --all -- --check                                 # Formatting clean
cargo test --workspace                                     # All tests pass
```

If any check fails, fix the issue and re-run. Do NOT present work as done if any check fails.

---

## 2. Unit Test Requirement

Every new module, function, or feature MUST have unit tests in a `#[cfg(test)] mod tests` block. Minimum coverage:

- Happy path for every public function
- Error cases and edge cases
- At least one integration-style test if the module interacts with other crates

Run the specific crate's tests to verify:

```bash
cargo test -p temm1e-<crate> -- --nocapture
```

---

## 3. Self-Testing via CLI

After compilation and unit tests pass, test the feature end-to-end by running TEMM1E directly.

### Current approach (until `chat` command is wired):

```bash
# Build release binary
cargo build --release

# Start TEMM1E with CLI-accessible configuration
# The service starts and listens for messages via configured channels
./target/release/temm1e start 2>&1 | tee /tmp/temm1e.log &

# Monitor logs in real-time for debugging
tail -f /tmp/temm1e.log
```

### Target approach (once `chat` command is implemented):

```bash
# Build and run interactive CLI chat directly
cargo build --release
./target/release/temm1e chat
```

The `chat` command should wire the CLI channel (`CliChannel`) into the agent runtime, enabling direct conversation with the agent without needing Telegram or any external service.

### What to test:

- Send messages and verify the agent responds correctly
- Test new tools by asking the agent to use them
- Verify error handling by sending edge-case inputs
- Check log output for warnings or errors
- Verify memory persistence by restarting and checking state

---

## 4. Debugging Protocol

When something fails:

1. **Read the error output carefully** — Rust compiler errors are precise
2. **Tail the logs** — `tail -f /tmp/temm1e.log | grep --line-buffered -E "ERROR|WARN|panic"`
3. **Add tracing** — Use `tracing::debug!` with structured fields to trace data flow
4. **Isolate the failure** — Run specific crate tests: `cargo test -p temm1e-<crate> <test_name>`
5. **Fix and re-verify** — After fixing, re-run the full compilation gate (step 1)

Never skip to the next step without understanding and fixing the current failure.

---

## 5. Release Checklist

Before any version bump or release:

- [ ] All four compilation gate checks pass
- [ ] New unit tests written and passing
- [ ] Self-testing completed (feature works end-to-end)
- [ ] No regressions in existing tests
- [ ] README updated if user-facing changes
- [ ] Version bumped in root `Cargo.toml`
- [ ] Release build succeeds: `cargo build --release`

---

## 6. Log Monitoring

Always start TEMM1E with log output captured:

```bash
# Start with logging to file
./target/release/temm1e start 2>&1 | tee /tmp/temm1e.log &

# Watch for errors
tail -f /tmp/temm1e.log | grep --line-buffered -E "ERROR|WARN|panic"

# Watch specific subsystem
tail -f /tmp/temm1e.log | grep --line-buffered "memory_manage"
```

Read log output BEFORE reporting success. Silent failures are the worst kind.

---

## 7. Priority: CLI Chat Command

The `Commands::Chat` handler in `src/main.rs` is currently a stub. Implementing it is a prerequisite for proper self-testing. It should:

1. Initialize memory backend, vault, tools, and agent runtime (same as `start`)
2. Create a `CliChannel` and wire it into the message processing loop
3. Run an interactive REPL where the developer can talk directly to the agent
4. Support all tools including `memory_manage`, `shell`, `file_ops`, etc.

Until this is implemented, self-testing uses `start` with Telegram or log inspection.

---

## Non-Negotiable Rules

1. **Never skip tests.** "It should work" is not acceptable — prove it.
2. **Never present stub code as done.** If a function has `todo!()` or `unimplemented!()`, it is not done.
3. **Always run clippy.** Zero warnings is the CI gate — match it locally.
4. **Always check logs.** Start the service, read the output, verify behavior.
5. **Fix failures immediately.** Do not accumulate technical debt across steps.
6. **The user should never find a bug that tests could have caught.**
