# TEMM1E Release Protocol

**MANDATORY checklist before pushing any release to `main`.** Claude MUST execute every step and verify results before committing.

## Pre-Release Verification

### 1. Compilation Gates (ALL must pass)

```bash
cargo check --workspace
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --all -- --check
cargo test --workspace
```

Record the test count from the output. Every `test result: ok` line's passed count must be summed.

### 2. Collect Metrics

Run these and record the values:

```bash
# Test count
cargo test --workspace 2>&1 | grep 'test result' | awk '{sum += $4} END {print sum}'

# Source files and lines
find . -name '*.rs' -not -path './target/*' | wc -l
find . -name '*.rs' -not -path './target/*' | xargs wc -l | tail -1

# Crate count
ls crates/ | wc -l
```

### 3. Version Bump

Update version in `Cargo.toml` (workspace.package.version). This propagates to all crates.

**File:** `Cargo.toml` line ~22
```toml
[workspace.package]
version = "X.Y.Z"
```

### 4. README.md — Update ALL of These

| Location | What | How to get value |
|----------|------|------------------|
| Line ~13 | Version badge | Match Cargo.toml version |
| Line ~14 | Test count badge | From step 2 |
| Line ~15 | Provider count badge | Count providers in Supported Providers table |
| Line ~23 | Version tagline (`**vX.Y: ...`) | New feature headline |
| Line ~25 | Hero line (`XXK lines \| N tests`) | From step 2 |
| Line ~94 | Lines of Rust metric | From step 2 (exact count + file count) |
| Line ~95 | Tests metric | From step 2 |
| Line ~97 | Workspace crates metric | From step 2 (`crates/ count + 1 binary`) |
| Line ~101 | AI providers metric | Count all providers including variants |
| Line ~103 | Agent tools metric | Count tools in Tools table |
| Line ~354 | Architecture crate count text | Match workspace crates metric |
| Line ~356-372 | Architecture tree | Must list all crates in `crates/` |
| Line ~425 | `temm1e update` example version | Match Cargo.toml version |
| Line ~443 | Dev section test count | From step 2 |
| Release Timeline | New entry at TOP | Date, version, features, test count |
| **Tem's Lab section** | Add subsection for new cognitive systems | If the release adds a new cognitive system (crate in temm1e-*), add a Tem's Lab subsection with: what it does, how it works, key metrics/benchmarks, A/B test results if applicable, and links to research papers/design docs. Follow the existing subsection format (see Lambda Memory, Conscious, Perpetuum as examples). |

### 5. CLAUDE.md — Update Stale References

| Location | What |
|----------|------|
| Line ~7 | Crate count ("X crates plus a root binary") |
| Workspace structure | Must list all crates in `crates/` |

### 6. src/main.rs — Check Version-Sensitive Code

| What | How to verify |
|------|---------------|
| `default_model()` | All providers have entries, new providers added |
| System prompt provider list | All providers listed with models |
| `auth status` output | Recommended model is correct |

### 7. Interactive Interface Parity Gate — MANDATORY

**Every interactive interface must be fully wired before release.** TEMM1E has
independent initialization paths for:

| Interface | Code path | Notes |
|---|---|---|
| **TUI** (`temm1e tui`) | `crates/temm1e-tui/src/agent_bridge.rs :: spawn_agent()` | Primary install method per README — new users hit this first |
| **CLI chat** (`temm1e chat`) | `src/main.rs :: Commands::Chat` | Primary self-test vehicle |
| **Server/messengers** (`temm1e start`) | `src/main.rs :: Commands::Start` | Routes Telegram/Discord/WhatsApp/Slack through the shared agent init |

Each path maintains its own tool-list assembly, Hive init (or lack of),
agent construction, and background-service wiring. **Wiring a feature into
one path does NOT wire it into the others.** This has silently drifted
multiple times — v5.4.0 shipped with JIT `spawn_swarm` registered only in
server; TUI has been missing a dozen subsystems for multiple releases.

#### Parity matrix (update every release)

Before pushing a release, confirm every shipped feature is wired in every
interactive interface. Current snapshot (update at each release):

| Feature | Server | CLI chat | TUI |
|---|:---:|:---:|:---:|
| Hive + JIT `spawn_swarm` | ✓ | ✓ (v5.4.0) | **must wire** |
| Consciousness observer | ✓ | ✓ | **must wire** |
| Social intelligence / user profile | ✓ | ✓ | **must wire** |
| Personality config (`.with_personality`) | ✓ | ✓ | **must wire** |
| Perpetuum (`.with_perpetuum_temporal`) | ✓ | ✓ | **must wire** |
| MCP servers | ✓ | ✓ | **must wire** |
| Custom tools + `SelfCreateTool` | ✓ | ✓ | **must wire** |
| TemDOS cores + `invoke_core` | ✓ | ✓ | **must wire** |
| Eigen-Tune engine | ✓ | ✓ | **must wire** |
| Witness / Cambium trust / auto-oath | ✓ | — | — (opt-in, OK to defer) |
| Shared memory strategy (`/memory lambda`) | ✓ | ✓ | **must wire** |
| Vault + skill_registry wiring | ✓ | ✓ | **must wire** |

#### Per-interface verification steps

For **every** interface above, run a smoke test and confirm the feature's
startup log appears. CLI chat and `start` can be smoke-tested with
pipe/redirect; **TUI cannot** (ratatui needs a real TTY and fails with
`Device not configured` when stdin/stdout are redirected). Use the
dedicated headless harness for TUI:

```bash
# 1) CLI chat parity
./target/release/temm1e chat <<<'hi' 2>&1 | grep -E "JIT spawn_swarm tool registered \(CLI|Many Tems initialized \(CLI|Tem Conscious.*initialized|Social intelligence initialized"

# 2) Server parity
timeout 10 ./target/release/temm1e start > /tmp/parity_start.log 2>&1 &
sleep 12
grep -E "JIT spawn_swarm tool registered|Many Tems initialized|Tem Conscious.*initialized" /tmp/parity_start.log

# 3) TUI parity — MANDATORY use the headless example harness
#    (ratatui crashes with "Device not configured" under pipe/redirect)
cargo build --release --example tui_smoke -p temm1e-tui
./target/release/examples/tui_smoke 2>&1 | grep -E "JIT spawn_swarm tool registered \(TUI|Many Tems initialized \(TUI|JIT spawn_swarm context wired \(TUI|Tem Conscious initialized \(TUI|Social intelligence initialized \(TUI"
```

The **tui_smoke example** (`crates/temm1e-tui/examples/tui_smoke.rs`)
calls the exact `spawn_agent()` function that `launch_tui` calls, but
skips ratatui's terminal init. It sets up a tracing subscriber that
emits logs to stdout, waits 5s for async init to complete, then exits.
**This is the only way to empirically verify TUI wiring without a real
terminal** — never skip it on release.

For every feature listed in the release: include a greppable registration
log message, run ALL THREE smoke tests against the release binary, and
paste the greps into the release report. A missing log = a missing
wiring = blocker for release unless the release notes EXPLICITLY declare
non-parity for that interface.

#### One-shot parity verification script

Save as `scripts/release_parity_smoke.sh` (or inline in the release flow):

```bash
#!/usr/bin/env bash
set -u
BIN=./target/release/temm1e
cargo build --release --bin temm1e 2>&1 | tail -1
cargo build --release --example tui_smoke -p temm1e-tui 2>&1 | tail -1

# CLI
( printf 'hi\n'; sleep 15; printf '/quit\n' ) | "$BIN" chat > /tmp/p_cli.log 2>&1 &
( sleep 30; kill -TERM $! 2>/dev/null ) & wait

# Server
"$BIN" start > /tmp/p_srv.log 2>&1 &
( sleep 12; kill -TERM $! 2>/dev/null ) & wait

# TUI (via dedicated headless harness — DO NOT use `temm1e tui` directly)
./target/release/examples/tui_smoke > /tmp/p_tui.log 2>&1

for anchor in \
  "JIT spawn_swarm tool registered" \
  "Many Tems initialized" \
  "JIT spawn_swarm context wired" \
  "Tem Conscious.*initialized" \
  "Social intelligence initialized" \
  "Perpetuum runtime started" \
  "TemDOS cores loaded" \
  "TemDOS invoke_core tool registered" \
  "Loaded MCP config" \
  "Custom script tools loaded"
do
  cli=$(grep -c "$anchor" /tmp/p_cli.log 2>/dev/null || echo 0)
  srv=$(grep -c "$anchor" /tmp/p_srv.log 2>/dev/null || echo 0)
  tui=$(grep -c "$anchor" /tmp/p_tui.log 2>/dev/null || echo 0)
  printf "%-45s CLI=%s TUI=%s srv=%s\n" "$anchor" "$cli" "$tui" "$srv"
done
```

Paste the output table into the release report's parity section. Any
anchor that shows `0` across any expected interface = blocker.

#### Rules

1. **Never declare a feature "shipped" based on one interface's logs.**
   CLI chat passing ≠ TUI passing ≠ server passing. Each must be checked.
2. **"Feature wasn't triggered" must be distinguished from "feature wasn't
   registered."** Grep for the registration log first; then grep for the
   execution log. Skipping step one confuses a wiring bug with a
   behavioural outcome.
3. **When adding a feature, add its registration log alongside the code.**
   Future wiring checks depend on this anchor.
4. **Opt-in features** (Witness, Cambium, auto_planner_oath) may legitimately
   be absent from interactive interfaces, but the release notes must call
   that out.
5. **Non-interactive paths** (MCP client only, tool servers, background
   cron) are out of scope for the parity gate but still need their own
   smoke tests.

### 8. Final Verification

After all edits, re-run:

```bash
cargo check --workspace
cargo test --workspace 2>&1 | grep 'test result' | awk '{sum += $4} END {print sum}'
```

Confirm test count still matches what you wrote in README.

### 9. Commit and Push — PR-based flow

`main` is branch-protected: direct pushes are rejected, and merging a PR
requires **1 approving review** (per `gh api repos/temm1e-labs/temm1e/branches/main/protection`).
The release workflow is therefore:

```bash
# 1. Commit on a release branch (not main)
git checkout -b release/vX.Y.Z   # or use whatever feature branch is active
git add <only files actually changed by the release>
git commit -m "vX.Y.Z: <one-line summary>"
git push -u origin release/vX.Y.Z

# 2. Open the PR
gh pr create --title "vX.Y.Z: <one-line summary>" --body "<details>"

# 3. Wait for CI checks to pass on the PR
gh pr checks <PR_NUMBER>    # all rows must say "pass"
```

#### Merging the PR

Once CI is green:

- **If a second reviewer is available**: have them `gh pr review <N> --approve`,
  then `gh pr merge <N> --squash --subject "vX.Y.Z: ... (#<N>)" --body "..."`.
- **Solo maintainer (no second reviewer)**: admin override is the only path
  because `enforce_admins: false` on this repo. Authorized usage:

  ```bash
  gh pr merge <PR_NUMBER> --squash --admin \
      --subject "vX.Y.Z: <summary> (#<PR_NUMBER>)" \
      --body "<details>"
  ```

  `--admin` bypasses the required-review gate. **Only use when**:
  1. All CI checks on the PR have passed (verified via `gh pr checks`).
  2. You are the only maintainer with merge rights for this release.
  3. The release commit was self-reviewed end-to-end (compilation gates,
     test count, README/CLAUDE.md updates, interactive parity gate).

  Document the admin merge in the release notes for traceability. Established
  v5.6.1 (2026-05-15) as the first explicit-admin-bypass release on record.

#### After merge

```bash
git checkout main
git pull --ff-only      # main now contains the squashed release commit
```

### 10. Tag and Release

**CRITICAL — this triggers the GitHub release pipeline.**
Without the tag, no binaries are built and no GitHub release is created.

```bash
git tag vX.Y.Z
git push origin vX.Y.Z
```

After pushing the tag:
1. GitHub Actions `release.yml` triggers automatically
2. CI runs checks (cargo check, test, clippy, fmt)
3. Builds 4 binaries (linux-musl, linux-desktop, macos-x86, macos-arm)
4. Creates GitHub Release with binaries + checksums + auto release notes
5. **Verify the release**: `gh run list --limit 1` and check the Actions tab

Do NOT declare the release done until the workflow completes successfully
and the GitHub Release page shows all 4 binaries.

### 10.5 Update-Path Smoke — MANDATORY (added in v5.5.2)

After the release workflow publishes, verify that `temm1e update` from the
previous release actually lands on the new one. This step exists because
v5.5.1 shipped a broken updater across **four platforms** for multiple
releases — an asset-naming drift between `release.yml` and the in-binary
updater went undetected because the protocol never exercised the update
path post-publish. The symptom (`Error: No binary found for <target> in
release v...`) was invisible to anyone not actively upgrading.

Run on each maintainer's machine (covers at least one OS/arch naturally):

```bash
scripts/release_update_smoke.sh <PREV_TAG> <NEW_TAG>
# e.g. scripts/release_update_smoke.sh v5.5.1 v5.5.2
```

The script:
1. Downloads the previous release's binary for the current platform
2. Sanity-checks it reports the previous version
3. Runs its `update` subcommand
4. Asserts the binary now reports the new version

If this fails, the release is not shippable to existing users — roll
forward a hotfix rather than advancing the tag. An online compile-time
gate (`update_assets::every_updater_asset_is_published_by_release_yml`
in `src/update_assets.rs`) also runs on every PR and fails the build if
`release.yml`'s artifact matrix ever drifts from the updater's expected
asset list — so drift should be caught at `cargo test` time, not at
release time. This smoke remains the final empirical check.

## Files That Do NOT Need Updating

- **`docs/benchmarks/BENCHMARK_REPORT.md`** — Version in title reflects when benchmark was taken. Only update if benchmarks are re-run.
- **`crates/temm1e-skills/src/lib.rs`** — Test fixtures use hardcoded version strings. These are test data, not release metadata.
- **`Cargo.lock`** — Auto-generated from Cargo.toml changes.
- **Release Timeline old entries** — Historical entries are frozen. Never modify past versions.

## Common Mistakes

| Mistake | Consequence |
|---------|-------------|
| Bump README but not Cargo.toml | `temm1e -V` shows old version |
| Bump Cargo.toml but not README badges | GitHub page shows old version |
| Forget `temm1e update` example version | Users see wrong version in help output |
| Forget test count in dev section | `cargo test` comment says wrong number |
| Forget architecture tree | New crate invisible in docs |
| Forget CLAUDE.md crate count | Claude starts sessions with wrong context |
| Forget `default_model()` for new provider | Omitting model in config crashes with wrong default |
| Push without running tests | Broken code on main |
| Push without tagging | **No GitHub release created, no binaries built, users stuck on old version** |
| Tag before pushing commit | Tag points to wrong commit |
