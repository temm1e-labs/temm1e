#!/bin/bash
# λ-Memory 100-Turn Benchmark — Direct OpenAI API
# Tests the full λ-Memory lifecycle: creation, decay, recall, budget
set -euo pipefail

API_KEY="${OPENAI_API_KEY:?Set OPENAI_API_KEY}"
MODEL="gpt-5.2"
DB="$HOME/.temm1e/memory.db"
LOG="$(cd "$(dirname "$0")" && pwd)/lambda_bench_100turns_log.txt"
SYSTEM_PROMPT='You are Tem, a helpful AI assistant with λ-Memory.

For memorable turns (decisions, preferences, important actions), include a <memory> block at the end of your response:
<memory>
summary: (one sentence)
essence: (5 words max)
importance: (1-5: 1=casual, 2=routine, 3=decision, 4=preference, 5=critical)
tags: (up to 5, comma-separated)
</memory>
Do NOT include this block for trivial turns (greetings, simple acknowledgments).
Keep responses concise — 1-3 sentences max unless asked for detail.'

# Initialize
echo "═══ λ-Memory 100-Turn Benchmark ═══" | tee "$LOG"
echo "Date: $(date)" | tee -a "$LOG"
echo "Model: $MODEL" | tee -a "$LOG"
echo "" | tee -a "$LOG"

# Ensure λ-memory table exists
sqlite3 "$DB" "
CREATE TABLE IF NOT EXISTS lambda_memories (
    hash TEXT PRIMARY KEY, created_at INTEGER NOT NULL, last_accessed INTEGER NOT NULL,
    access_count INTEGER NOT NULL DEFAULT 0, importance REAL NOT NULL DEFAULT 1.0,
    explicit_save INTEGER NOT NULL DEFAULT 0, full_text TEXT NOT NULL,
    summary_text TEXT NOT NULL, essence_text TEXT NOT NULL,
    tags TEXT NOT NULL DEFAULT '[]', memory_type TEXT NOT NULL DEFAULT 'conversation',
    session_id TEXT NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS lambda_memories_fts USING fts5(
    summary_text, essence_text, tags, content=''
);
DELETE FROM lambda_memories WHERE session_id = 'bench-100';
" 2>/dev/null

# Build conversation history as JSON array
HISTORY="[]"
TOTAL_INPUT=0
TOTAL_OUTPUT=0
TOTAL_COST=0
MEMORY_COUNT=0
MEMORY_BLOCKS_PARSED=0
ERRORS=0

# 100 conversation turns
TURNS=(
    "Hi Tem, I'm testing your memory today"
    "Remember this: I always prefer snake_case for variables"
    "What's the best error handling approach in Rust?"
    "I've decided to use thiserror for all error types"
    "Explain async traits briefly"
    "Remember: never use unwrap in production code"
    "What's Box<dyn Error> vs custom error types?"
    "I prefer match statements over if-let for complex patterns"
    "What design patterns work with Rust ownership?"
    "I'm frustrated with verbose error handling sometimes"
    "Remember: use tracing instead of log crate"
    "Explain the builder pattern in Rust briefly"
    "I chose axum over actix-web for the web framework"
    "HashMap vs BTreeMap performance?"
    "Deploy strategy: always use blue-green deployments"
    "Remember: database migrations must be reversible"
    "What's the orphan rule in Rust?"
    "Important: all public types must impl Debug, Clone, Serialize"
    "Remember: CI must run clippy with -D warnings"
    "How to handle graceful shutdown in async Rust?"
    "What coding preferences do you remember about me?"
    "What error handling approach did I decide on?"
    "I'm excited about the architecture coming together"
    "Explain Send and Sync traits briefly"
    "What's dependency injection in Rust?"
    "Remember: all DB operations need 5-second timeout"
    "Async vs sync Rust tradeoffs?"
    "I'm worried about Arc cycle memory leaks"
    "How to break Arc cycles?"
    "List all preferences I've told you"
    "What deployment strategy did I choose?"
    "Explain tower middleware briefly"
    "Remember: rate limiting at gateway level, not per-handler"
    "Layer vs Service in tower?"
    "I prefer composition over inheritance"
    "How should we handle API versioning?"
    "Critical: all API responses must include request-id header"
    "Explain backpressure in async systems"
    "Remember: structured logging with tracing spans for DB ops"
    "Best way to test async code?"
    "I decided SQLite for dev, Postgres for production"
    "How to mock databases in Rust tests?"
    "What CI requirements did I mention?"
    "Remember: never expose internal errors to API consumers"
    "Revisit: what error handling did I originally choose?"
    "Confirmed: thiserror is still the right call"
    "What logging approach did I choose?"
    "How to handle connection pooling?"
    "Remember: max 20 database connections in pool"
    "sqlx vs diesel differences?"
    "I chose sqlx for compile-time query checking"
    "Schema migrations with sqlx?"
    "What timeout did I set for DB operations?"
    "Remember: sanitize file paths to prevent traversal"
    "What security practices have I mentioned?"
    "How to handle auth tokens?"
    "Use JWTs with short expiry and refresh tokens"
    "What web framework did I pick?"
    "CORS in axum?"
    "Remember: CORS permissive in dev, restrictive in production"
    "What databases for dev vs production?"
    "Actor model and async Rust?"
    "What rate limiting approach did I choose?"
    "Give me a summary of my preferences"
    "What were the 3 most important decisions?"
    "What's our architectural philosophy?"
    "Remember: this conversation tests lambda memory"
    "What error libraries did I consider?"
    "What deployment strategy again?"
    "My views on error handling verbosity?"
    "DB timeout configuration?"
    "CI/CD requirements I established?"
    "What logging framework?"
    "My coding style preferences?"
    "How should API errors be exposed?"
    "Connection pool settings?"
    "Testing strategy we discussed?"
    "How well do you remember our earlier conversation?"
    "What was the first thing I said to you?"
    "What's the most important preference I set?"
    "How many explicit 'remember' requests did I make?"
    "What frameworks and libraries did I choose?"
    "Summarize all my architectural decisions"
    "What patterns did we discuss?"
    "Security requirements summary?"
    "Database configuration summary?"
    "API design decisions summary?"
    "What's my overall development philosophy?"
    "Rate my consistency - did I change my mind on anything?"
    "What would you recommend I document first?"
    "Final summary: everything you know about my project"
    "Remember: this benchmark completed successfully on $(date +%Y-%m-%d)"
    "How many things do you remember from this conversation?"
    "What's your confidence level on recalling my preferences?"
    "Thank you Tem, great test session"
    "One more: what's the most critical decision I made?"
    "Last question: if you had to pick ONE thing to never forget from this session, what would it be?"
    "Perfect. End of test."
    "goodbye"
)

call_api() {
    local messages="$1"
    curl -s "https://api.openai.com/v1/chat/completions" \
        -H "Content-Type: application/json" \
        -H "Authorization: Bearer $API_KEY" \
        -d "{
            \"model\": \"$MODEL\",
            \"messages\": $messages,
            \"temperature\": 0.7,
            \"max_completion_tokens\": 500
        }" 2>/dev/null
}

parse_memory_block() {
    local text="$1"
    # Extract between <memory> and </memory>
    echo "$text" | sed -n '/<memory>/,/<\/memory>/p' | grep -v '<\/?memory>'
}

store_lambda_memory() {
    local hash="$1" summary="$2" essence="$3" importance="$4" tags="$5" full="$6" now="$7"
    local explicit=0
    echo "$full" | grep -qi "remember" && explicit=1

    # Escape single quotes for SQL
    summary="${summary//\'/\'\'}"
    essence="${essence//\'/\'\'}"
    full="${full//\'/\'\'}"
    tags="${tags//\'/\'\'}"

    sqlite3 "$DB" "
        INSERT OR REPLACE INTO lambda_memories
        (hash, created_at, last_accessed, access_count, importance, explicit_save,
         full_text, summary_text, essence_text, tags, memory_type, session_id)
        VALUES ('$hash', $now, $now, 0, $importance, $explicit,
                '$full', '$summary', '$essence', '$tags', 'conversation', 'bench-100');
    " 2>/dev/null

    # FTS sync
    local rowid
    rowid=$(sqlite3 "$DB" "SELECT rowid FROM lambda_memories WHERE hash='$hash';" 2>/dev/null)
    if [ -n "$rowid" ]; then
        sqlite3 "$DB" "
            INSERT INTO lambda_memories_fts(rowid, summary_text, essence_text, tags)
            VALUES ($rowid, '$summary', '$essence', '$tags');
        " 2>/dev/null || true
    fi
}

echo "Starting ${#TURNS[@]}-turn conversation..." | tee -a "$LOG"
echo "" | tee -a "$LOG"

for i in "${!TURNS[@]}"; do
    turn=$((i + 1))
    msg="${TURNS[$i]}"
    now=$(date +%s)

    echo "────── Turn $turn/${#TURNS[@]} ──────" | tee -a "$LOG"
    echo "USER: $msg" | tee -a "$LOG"

    # Build message with system prompt
    # Escape message for JSON
    escaped_msg=$(echo "$msg" | python3 -c "import sys,json; print(json.dumps(sys.stdin.read().strip()))")

    # Add user message to history
    HISTORY=$(echo "$HISTORY" | python3 -c "
import sys, json
h = json.load(sys.stdin)
h.append({'role': 'user', 'content': $escaped_msg})
print(json.dumps(h))
")

    # Build full messages array with system prompt
    full_messages=$(echo "$HISTORY" | python3 -c "
import sys, json
h = json.load(sys.stdin)
system = {'role': 'system', 'content': '''$SYSTEM_PROMPT'''}
print(json.dumps([system] + h[-30:]))  # Keep last 30 messages
")

    # Call API
    response=$(call_api "$full_messages")

    # Parse response
    assistant_text=$(echo "$response" | python3 -c "
import sys, json
try:
    r = json.load(sys.stdin)
    if 'error' in r:
        print(f'ERROR: {r[\"error\"][\"message\"]}')
    else:
        print(r['choices'][0]['message']['content'])
except Exception as e:
    print(f'PARSE_ERROR: {e}')
" 2>/dev/null)

    # Extract usage
    input_tokens=$(echo "$response" | python3 -c "
import sys, json
try:
    r = json.load(sys.stdin)
    print(r.get('usage', {}).get('prompt_tokens', 0))
except: print(0)
" 2>/dev/null)
    output_tokens=$(echo "$response" | python3 -c "
import sys, json
try:
    r = json.load(sys.stdin)
    print(r.get('usage', {}).get('completion_tokens', 0))
except: print(0)
" 2>/dev/null)

    TOTAL_INPUT=$((TOTAL_INPUT + input_tokens))
    TOTAL_OUTPUT=$((TOTAL_OUTPUT + output_tokens))

    if echo "$assistant_text" | grep -q "ERROR:"; then
        ERRORS=$((ERRORS + 1))
        echo "  ERROR: $assistant_text" | tee -a "$LOG"
        # Still add to history to maintain flow
        HISTORY=$(echo "$HISTORY" | python3 -c "
import sys, json
h = json.load(sys.stdin)
h.append({'role': 'assistant', 'content': 'I encountered an error processing that.'})
print(json.dumps(h))
")
        echo "" | tee -a "$LOG"
        continue
    fi

    # Strip <memory> block for display
    display_text=$(echo "$assistant_text" | sed '/<memory>/,/<\/memory>/d' | head -10)
    echo "TEM: $display_text" | tee -a "$LOG"
    echo "  [tokens: in=$input_tokens out=$output_tokens]" | tee -a "$LOG"

    # Parse and store <memory> block if present
    if echo "$assistant_text" | grep -q "<memory>"; then
        MEMORY_BLOCKS_PARSED=$((MEMORY_BLOCKS_PARSED + 1))

        summary=$(echo "$assistant_text" | sed -n '/<memory>/,/<\/memory>/p' | grep "summary:" | sed 's/summary: *//' | head -1)
        essence=$(echo "$assistant_text" | sed -n '/<memory>/,/<\/memory>/p' | grep "essence:" | sed 's/essence: *//' | head -1)
        importance=$(echo "$assistant_text" | sed -n '/<memory>/,/<\/memory>/p' | grep "importance:" | sed 's/importance: *//' | head -1)
        tags=$(echo "$assistant_text" | sed -n '/<memory>/,/<\/memory>/p' | grep "tags:" | sed 's/tags: *//' | head -1)

        # Default importance if not parsed
        [ -z "$importance" ] && importance="2"
        # Validate importance is numeric
        importance=$(echo "$importance" | grep -o '[0-9]*' | head -1)
        [ -z "$importance" ] && importance="2"

        # Generate hash
        hash=$(echo -n "bench-100:$turn:$now" | shasum -a 256 | cut -c1-12)

        # Build full text
        full_text="User: ${msg:0:300} | Assistant: ${display_text:0:300}"

        store_lambda_memory "$hash" "$summary" "$essence" "$importance" "[$tags]" "$full_text" "$now"
        MEMORY_COUNT=$((MEMORY_COUNT + 1))

        echo "  [λ-memory: #$hash imp=$importance essence=\"$essence\"]" | tee -a "$LOG"
    fi

    # Add assistant response to history (stripped of memory block)
    clean_text=$(echo "$assistant_text" | sed '/<memory>/,/<\/memory>/d')
    escaped_clean=$(echo "$clean_text" | python3 -c "import sys,json; print(json.dumps(sys.stdin.read().strip()))")
    HISTORY=$(echo "$HISTORY" | python3 -c "
import sys, json
h = json.load(sys.stdin)
h.append({'role': 'assistant', 'content': $escaped_clean})
print(json.dumps(h))
")

    echo "" | tee -a "$LOG"

    # Rate limit: 0.5s between turns
    sleep 0.5
done

# ── Final Stats ───────────────────────────────────────────
echo "═══════════════════════════════════════" | tee -a "$LOG"
echo "═══ BENCHMARK RESULTS ═══" | tee -a "$LOG"
echo "" | tee -a "$LOG"
echo "Turns: ${#TURNS[@]}" | tee -a "$LOG"
echo "Errors: $ERRORS" | tee -a "$LOG"
echo "Memory blocks parsed: $MEMORY_BLOCKS_PARSED" | tee -a "$LOG"
echo "Memories stored: $MEMORY_COUNT" | tee -a "$LOG"
echo "Total input tokens: $TOTAL_INPUT" | tee -a "$LOG"
echo "Total output tokens: $TOTAL_OUTPUT" | tee -a "$LOG"

# Cost estimate (GPT-5.2 pricing: ~$2/M input, ~$8/M output)
cost=$(python3 -c "print(f'{($TOTAL_INPUT * 2 + $TOTAL_OUTPUT * 8) / 1000000:.4f}')")
echo "Estimated cost: \$$cost" | tee -a "$LOG"
echo "" | tee -a "$LOG"

# Database stats
echo "═══ λ-Memory Database ═══" | tee -a "$LOG"
echo "Total memories:" | tee -a "$LOG"
sqlite3 "$DB" "SELECT COUNT(*) FROM lambda_memories WHERE session_id='bench-100';" 2>/dev/null | tee -a "$LOG"

echo "" | tee -a "$LOG"
echo "By importance:" | tee -a "$LOG"
sqlite3 "$DB" "SELECT CAST(importance AS INTEGER) as imp, COUNT(*) as cnt FROM lambda_memories WHERE session_id='bench-100' GROUP BY imp ORDER BY imp;" 2>/dev/null | tee -a "$LOG"

echo "" | tee -a "$LOG"
echo "Explicit saves:" | tee -a "$LOG"
sqlite3 "$DB" "SELECT COUNT(*) FROM lambda_memories WHERE session_id='bench-100' AND explicit_save=1;" 2>/dev/null | tee -a "$LOG"

echo "" | tee -a "$LOG"
echo "All memories (chronological):" | tee -a "$LOG"
sqlite3 "$DB" -header -column "
SELECT substr(hash,1,7) as hash, importance as imp, explicit_save as exp,
       substr(essence_text,1,40) as essence, substr(tags,1,30) as tags
FROM lambda_memories
WHERE session_id='bench-100'
ORDER BY created_at;
" 2>/dev/null | tee -a "$LOG"

echo "" | tee -a "$LOG"
echo "Decay simulation (if scored now):" | tee -a "$LOG"
sqlite3 "$DB" "
SELECT substr(hash,1,7) as hash,
       ROUND(importance * exp(-(($(date +%s) - last_accessed) / 3600.0) * 0.01), 2) as score,
       importance as raw_imp,
       CASE
           WHEN importance * exp(-(($(date +%s) - last_accessed) / 3600.0) * 0.01) > 2.0 THEN 'HOT'
           WHEN importance * exp(-(($(date +%s) - last_accessed) / 3600.0) * 0.01) > 1.0 THEN 'WARM'
           WHEN importance * exp(-(($(date +%s) - last_accessed) / 3600.0) * 0.01) > 0.3 THEN 'COOL'
           ELSE 'FADED'
       END as tier,
       substr(essence_text,1,30) as essence
FROM lambda_memories
WHERE session_id='bench-100'
ORDER BY score DESC;
" 2>/dev/null | tee -a "$LOG"

echo "" | tee -a "$LOG"
echo "Done. Full log: $LOG" | tee -a "$LOG"
