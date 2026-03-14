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

### 5. CLAUDE.md — Update Stale References

| Location | What |
|----------|------|
| Line ~7 | Crate count ("X crates plus a root binary") |

### 6. src/main.rs — Check Version-Sensitive Code

| What | How to verify |
|------|---------------|
| `default_model()` | All providers have entries, new providers added |
| System prompt provider list | All providers listed with models |
| `auth status` output | Recommended model is correct |

### 7. Final Verification

After all edits, re-run:

```bash
cargo check --workspace
cargo test --workspace 2>&1 | grep 'test result' | awk '{sum += $4} END {print sum}'
```

Confirm test count still matches what you wrote in README.

### 8. Commit and Push

```bash
git add -A
git commit -m "vX.Y.Z: <one-line summary>"
git push origin main
```

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
