#!/bin/bash
# λ-Memory 100-Turn Self-Test with GPT-5.2
# This script pipes 100 turns of conversation to TEMM1E CLI
# and captures full output for analysis.

set -euo pipefail

export OPENAI_API_KEY="$1"
BINARY="$(dirname "$0")/../target/release/temm1e"
LOG="$(dirname "$0")/lambda_test_100turns_log.txt"
DB="$HOME/.temm1e/memory.db"

echo "=== λ-Memory 100-Turn Test ===" | tee "$LOG"
echo "Date: $(date)" | tee -a "$LOG"
echo "Model: gpt-5.2" | tee -a "$LOG"
echo "Binary: $BINARY" | tee -a "$LOG"
echo "" | tee -a "$LOG"

# Clear any previous λ-memory test data
if [ -f "$DB" ]; then
    sqlite3 "$DB" "DELETE FROM lambda_memories WHERE session_id LIKE 'cli%';" 2>/dev/null || true
    echo "Cleared previous λ-memory test data" | tee -a "$LOG"
fi

# Define 100 turns of conversation - varied topics, decisions, preferences, recalls
# Designed to exercise: creation, decay awareness, explicit saves, tool use, recall
TURNS=(
    # Phase 1: Establishing memories (turns 1-20)
    "Hi Tem, I'm testing your new memory system today"
    "Remember this: I always prefer snake_case for variable names"
    "What's the best way to handle errors in Rust?"
    "I've decided to use thiserror for all error types in this project"
    "Can you explain how async traits work?"
    "Remember: never use unwrap in production code, always use proper error handling"
    "What's the difference between Box<dyn Error> and custom error types?"
    "I prefer explicit match statements over if-let chains for complex pattern matching"
    "Create a file called /tmp/lambda_test.txt with the text 'hello from lambda test'"
    "Read that file back to verify it was created correctly"
    "What design patterns work well with Rust's ownership model?"
    "I'm frustrated with how verbose error handling is in Rust sometimes"
    "Remember: for this project, use tracing instead of log crate"
    "Explain the builder pattern in Rust"
    "I chose to use axum over actix-web for the web framework"
    "What's the performance difference between HashMap and BTreeMap?"
    "Deploy strategy: always use blue-green deployments for production"
    "Write a simple Rust function that calculates fibonacci numbers to /tmp/fib.rs"
    "Read /tmp/fib.rs back and verify it compiles conceptually"
    "Remember: database migrations should always be reversible"

    # Phase 2: Testing continuity (turns 21-40)
    "What have we been discussing so far?"
    "What are my coding preferences you remember?"
    "I want to switch from thiserror to a custom error enum approach"
    "Actually no, let's stick with thiserror - it was the right choice"
    "How should I structure a Rust workspace with multiple crates?"
    "Important: all public API types must implement Debug, Clone, and Serialize"
    "What frameworks did I choose earlier?"
    "Explain the difference between Send and Sync traits"
    "Create /tmp/lambda_workspace_plan.txt with a simple workspace layout"
    "What's the recommended way to do dependency injection in Rust?"
    "Remember: CI pipeline must run clippy with -D warnings"
    "How do you handle graceful shutdown in async Rust?"
    "What error handling approach did I decide on?"
    "I'm excited about how the architecture is coming together"
    "What file did we create earlier? Can you check it?"
    "Explain the orphan rule in Rust and how to work around it"
    "Remember: all database operations need a 5-second timeout"
    "What are the tradeoffs of using async vs sync Rust?"
    "I'm worried about memory leaks with Arc cycles"
    "How do you break Arc cycles in Rust?"

    # Phase 3: Deeper work and recall testing (turns 41-60)
    "List all the preferences and decisions I've told you about"
    "What deployment strategy did I choose?"
    "Explain how tower middleware works with axum"
    "Write a simple tower middleware that adds a request ID header to /tmp/middleware.rs"
    "Read that middleware file"
    "Remember: rate limiting should be at the gateway level, not per-handler"
    "What's the difference between tower::Layer and tower::Service?"
    "I prefer composition over inheritance - Rust makes this natural"
    "How should we handle API versioning?"
    "Critical: all API responses must include a request-id header for traceability"
    "What have we built so far in terms of files?"
    "Explain backpressure in async systems"
    "Remember: use structured logging with tracing spans for all database operations"
    "What's the best way to test async code?"
    "I decided to use SQLite for development and Postgres for production"
    "How do you mock a database in Rust tests?"
    "What CI requirements did I mention?"
    "Create a summary of all architectural decisions we've made to /tmp/arch_decisions.txt"
    "Read /tmp/arch_decisions.txt"
    "Remember: never expose internal error details to API consumers"

    # Phase 4: Complex interactions and memory stress (turns 61-80)
    "We need to revisit the error handling decision - what did I originally choose?"
    "Actually thiserror is still the right call, confirmed"
    "What logging approach did I choose and why?"
    "How should we handle database connection pooling?"
    "Remember: max 20 database connections in the pool"
    "What's the difference between sqlx and diesel?"
    "I chose sqlx because it supports compile-time query checking"
    "How do you handle database schema migrations with sqlx?"
    "What timeout did I set for database operations?"
    "Explain how to use sqlx with connection pooling"
    "Remember: all file operations must sanitize paths to prevent traversal"
    "What security practices have I mentioned?"
    "How should we handle authentication tokens?"
    "Use JWTs with short expiry and refresh tokens for auth"
    "What framework did I pick for the web layer?"
    "How do we handle CORS in axum?"
    "Remember: CORS should be permissive in dev, restrictive in production"
    "What databases am I using for dev vs production?"
    "Explain the actor model and how it relates to async Rust"
    "What rate limiting approach did I choose?"

    # Phase 5: Final recall and verification (turns 81-100)
    "Give me a complete summary of everything you remember about my preferences"
    "What were the 3 most important decisions we made?"
    "How many files did we create during this conversation?"
    "What's the architectural philosophy we've been building toward?"
    "Remember: this entire conversation is a test of the lambda memory system"
    "What error handling libraries did I consider?"
    "What deployment strategy did I choose?"
    "What are my views on error handling verbosity?"
    "How should database timeouts be configured?"
    "What CI/CD requirements did I establish?"
    "Verify the files we created still exist - check /tmp/lambda_test.txt"
    "What logging framework did I choose?"
    "What are my coding style preferences?"
    "How should API errors be exposed?"
    "What connection pool settings did I specify?"
    "What's the overall testing strategy we discussed?"
    "Remember: this test was run on $(date +%Y-%m-%d) as the first lambda memory benchmark"
    "Give me a confidence score 1-10: how well do you remember our earlier conversations?"
    "What was the very first thing I said to you?"
    "/quit"
)

echo "Starting 100-turn conversation..." | tee -a "$LOG"
echo "==========================================" | tee -a "$LOG"
echo "" | tee -a "$LOG"

# Create a FIFO for piping
PIPE="/tmp/temm1e_test_pipe_$$"
mkfifo "$PIPE"

# Start TEMM1E in background, reading from pipe
"$BINARY" chat < "$PIPE" >> "$LOG" 2>&1 &
TEMM1E_PID=$!

# Small delay for startup
sleep 3

# Feed messages with delay between them
turn=0
for msg in "${TURNS[@]}"; do
    turn=$((turn + 1))
    echo "" >> "$LOG"
    echo "────── TURN $turn ──────" >> "$LOG"
    echo "USER: $msg" >> "$LOG"
    echo "$msg" > "$PIPE"

    # Wait for response (longer for tool calls, shorter for chat)
    if echo "$msg" | grep -qiE "create|write|read|check|verify"; then
        sleep 15
    elif echo "$msg" | grep -qiE "summary|list all|everything|complete"; then
        sleep 20
    else
        sleep 8
    fi

    echo "  [Turn $turn/$((${#TURNS[@]})) complete]" >&2
done

# Clean up
rm -f "$PIPE"
wait "$TEMM1E_PID" 2>/dev/null || true

echo "" | tee -a "$LOG"
echo "==========================================" | tee -a "$LOG"
echo "Test complete at $(date)" | tee -a "$LOG"

# Dump λ-memory table stats
echo "" >> "$LOG"
echo "=== λ-Memory Database Stats ===" >> "$LOG"
if [ -f "$DB" ]; then
    echo "Total λ-memories:" >> "$LOG"
    sqlite3 "$DB" "SELECT COUNT(*) FROM lambda_memories;" >> "$LOG" 2>/dev/null || echo "table not found" >> "$LOG"

    echo "" >> "$LOG"
    echo "By type:" >> "$LOG"
    sqlite3 "$DB" "SELECT memory_type, COUNT(*) FROM lambda_memories GROUP BY memory_type;" >> "$LOG" 2>/dev/null || true

    echo "" >> "$LOG"
    echo "Explicit saves:" >> "$LOG"
    sqlite3 "$DB" "SELECT COUNT(*) FROM lambda_memories WHERE explicit_save = 1;" >> "$LOG" 2>/dev/null || true

    echo "" >> "$LOG"
    echo "Importance distribution:" >> "$LOG"
    sqlite3 "$DB" "SELECT ROUND(importance, 0) as imp, COUNT(*) FROM lambda_memories GROUP BY imp ORDER BY imp;" >> "$LOG" 2>/dev/null || true

    echo "" >> "$LOG"
    echo "Top 10 by importance:" >> "$LOG"
    sqlite3 "$DB" -header -column "SELECT hash, importance, explicit_save, essence_text, memory_type FROM lambda_memories ORDER BY importance DESC LIMIT 10;" >> "$LOG" 2>/dev/null || true

    echo "" >> "$LOG"
    echo "All λ-memories (essence view):" >> "$LOG"
    sqlite3 "$DB" -header -column "SELECT hash, ROUND(importance,1) as imp, explicit_save as exp, essence_text, memory_type as type FROM lambda_memories ORDER BY created_at;" >> "$LOG" 2>/dev/null || true
fi

echo "" >> "$LOG"
echo "Full log saved to: $LOG"
echo "Done. Check $LOG for results."
