#!/usr/bin/env bash
# Witness E2E exhaustive test harness.
#
# Runs every Witness test surface in sequence and aggregates results:
#   1. Unit + integration tests for temm1e-witness
#   2. Unit tests for temm1e-cambium (record_verdict)
#   3. Unit tests for temm1e-watchdog (root anchor)
#   4. Integration tests for temm1e-agent/tests/witness_integration.rs
#   5. cargo clippy -D warnings on all four crates
#   6. cargo fmt --check on all four crates
#   7. E2E demo (examples/e2e_demo.rs)
#   8. Basic A/B bench (examples/ab_bench.rs) — 500 trajectories
#   9. Full sweep bench (examples/full_sweep.rs) — 1800 trajectories
#
# Exits 0 on complete success, non-zero with a summary on first failure.
#
# Usage:
#   bash tems_lab/witness/e2e_test.sh           # full suite (~90s)
#   WITNESS_SWEEP_TASKS=50 bash tems_lab/...    # larger sweep
#
# Output is captured to tems_lab/witness/e2e_test_log.txt for diffing
# between runs.

set -uo pipefail
cd "$(dirname "$0")/../.."

LOG=tems_lab/witness/e2e_test_log.txt
mkdir -p tems_lab/witness
: > "$LOG"

run_step() {
  local name="$1"
  shift
  echo ""
  echo "══════════════════════════════════════════════════════════════════"
  echo "  $name"
  echo "══════════════════════════════════════════════════════════════════"
  {
    echo ""
    echo "── $name ──"
  } >> "$LOG"
  "$@" 2>&1 | tee -a "$LOG"
  return ${PIPESTATUS[0]}
}

SECONDS=0
FAILED=0

run_step "1. temm1e-witness unit + integration tests" \
  cargo test -p temm1e-witness || FAILED=1

run_step "2. temm1e-cambium trust tests" \
  cargo test -p temm1e-cambium --lib trust || FAILED=1

run_step "3. temm1e-watchdog tests" \
  cargo test -p temm1e-watchdog || FAILED=1

run_step "4. temm1e-agent witness_integration" \
  cargo test -p temm1e-agent --test witness_integration || FAILED=1

run_step "5. Clippy across all four crates" \
  cargo clippy -p temm1e-witness -p temm1e-agent -p temm1e-cambium -p temm1e-watchdog \
    --all-targets -- -D warnings || FAILED=1

run_step "6. Fmt check across all four crates" \
  cargo fmt -p temm1e-witness -p temm1e-agent -p temm1e-cambium -p temm1e-watchdog -- --check || FAILED=1

run_step "7. E2E demo" \
  cargo run --release -p temm1e-witness --example e2e_demo || FAILED=1

run_step "8. A/B bench (500 trajectories)" \
  cargo run --release -p temm1e-witness --example ab_bench || FAILED=1

run_step "9. Full sweep (1800 trajectories)" \
  cargo run --release -p temm1e-witness --example full_sweep || FAILED=1

TOTAL_SECONDS=$SECONDS
echo ""
echo "══════════════════════════════════════════════════════════════════"
echo "  E2E TEST SUITE COMPLETE"
echo "══════════════════════════════════════════════════════════════════"
echo "  Wall-clock: ${TOTAL_SECONDS}s"
echo "  Log:        $LOG"
echo ""

if [ $FAILED -ne 0 ]; then
  echo "❌ At least one step reported failures. See $LOG for details."
  exit 1
fi

echo "✓ All steps green."
