#!/bin/bash
# TEMM1E Hive A/B Benchmark — Live Gemini 3.1 Flash Lite
# Budget: $30 max | Model: gemini-3.1-flash-lite-preview
set -euo pipefail

export GEMINI_API_KEY="${GEMINI_API_KEY:?Set GEMINI_API_KEY}"

cd "$(dirname "$0")/../.."
RESULTS_DIR="tems_lab/swarm/results"
mkdir -p "$RESULTS_DIR"

echo "=== TEMM1E Hive A/B Benchmark ==="
echo "Model: gemini-3.1-flash-lite-preview"
echo "Budget: \$30"
echo ""

cargo test -p temm1e-hive --test live_ab_bench -- --nocapture 2>&1 | tee "$RESULTS_DIR/bench_$(date +%Y%m%d_%H%M%S).log"
