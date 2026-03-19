#!/usr/bin/env rust-script
//! λ-Memory benchmark — 100-turn conversation via direct API calls.
//! This script bypasses the CLI and directly calls OpenAI's API,
//! simulating what TEMM1E's runtime does.
//!
//! Usage: OPENAI_API_KEY=sk-... cargo test -p temm1e-agent --test lambda_bench -- --nocapture

// This is a reference — the actual test is in the shell script below.
