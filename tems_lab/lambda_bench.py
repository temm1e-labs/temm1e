#!/usr/bin/env python3
"""λ-Memory 100-Turn Benchmark — Direct OpenAI API via Python.
Robust JSON handling, proper error recovery, full SQLite integration."""

import json, os, re, hashlib, sqlite3, time, sys
from datetime import datetime
from urllib.request import Request, urlopen
from urllib.error import HTTPError

API_KEY = os.environ.get("OPENAI_API_KEY", "")
MODEL = "gpt-5.2"
DB_PATH = os.path.expanduser("~/.temm1e/memory.db")
LOG_PATH = os.path.join(os.path.dirname(os.path.abspath(__file__)), "lambda_bench_100turns_log.txt")

SYSTEM_PROMPT = """You are Tem, a helpful AI assistant with λ-Memory.

For memorable turns (decisions, preferences, important actions), include a <memory> block at the end of your response:
<memory>
summary: (one sentence)
essence: (5 words max)
importance: (1-5: 1=casual, 2=routine, 3=decision, 4=preference, 5=critical)
tags: (up to 5, comma-separated)
</memory>
Do NOT include this block for trivial turns (greetings, simple acknowledgments, goodbyes).
Keep responses concise — 2-4 sentences max unless asked for detail."""

TURNS = [
    # Phase 1: Establishing memories (1-25)
    "Hi Tem, I'm testing your new memory system today",
    "Remember this: I always prefer snake_case for variable names",
    "What's the best way to handle errors in Rust?",
    "I've decided to use thiserror for all error types in this project",
    "Can you explain how async traits work in one paragraph?",
    "Remember: never use unwrap in production code, always proper error handling",
    "What's the difference between Box<dyn Error> and custom error types?",
    "I prefer explicit match statements over if-let chains for complex patterns",
    "What design patterns work well with Rust's ownership model?",
    "I'm frustrated with how verbose error handling is in Rust sometimes",
    "Remember: for this project, use tracing instead of log crate",
    "Explain the builder pattern in Rust briefly",
    "I chose axum over actix-web for the web framework",
    "HashMap vs BTreeMap performance differences?",
    "Deploy strategy: always use blue-green deployments for production",
    "Remember: database migrations should always be reversible",
    "What's the orphan rule in Rust?",
    "Important: all public API types must implement Debug, Clone, and Serialize",
    "Remember: CI pipeline must run clippy with -D warnings",
    "How to handle graceful shutdown in async Rust?",
    "What coding preferences do you remember about me so far?",
    "What error handling approach did I decide on?",
    "I'm excited about how the architecture is coming together",
    "Explain Send and Sync traits briefly",
    "What's dependency injection in Rust?",
    # Phase 2: Preferences + recall (26-50)
    "Remember: all DB operations need a 5-second timeout",
    "Async vs sync Rust tradeoffs?",
    "I'm worried about Arc cycle memory leaks",
    "How to break Arc cycles?",
    "List all preferences and decisions I've told you about",
    "What deployment strategy did I choose?",
    "Explain tower middleware briefly",
    "Remember: rate limiting at gateway level, not per-handler",
    "Layer vs Service in tower?",
    "I prefer composition over inheritance - Rust makes this natural",
    "How should we handle API versioning?",
    "Critical: all API responses must include request-id header for traceability",
    "Explain backpressure in async systems briefly",
    "Remember: structured logging with tracing spans for all DB operations",
    "Best way to test async code?",
    "I decided SQLite for dev, Postgres for production",
    "How to mock databases in Rust tests?",
    "What CI requirements did I mention?",
    "Remember: never expose internal error details to API consumers",
    "Revisit: what error handling approach did I originally choose?",
    "Confirmed: thiserror is still the right call",
    "What logging approach did I choose and why?",
    "How to handle connection pooling?",
    "Remember: max 20 database connections in the pool",
    "sqlx vs diesel - which did I pick and why?",
    # Phase 3: Deeper work (51-75)
    "I chose sqlx for compile-time query checking",
    "How do schema migrations work with sqlx?",
    "What timeout did I set for database operations?",
    "Remember: sanitize all file paths to prevent path traversal attacks",
    "What security practices have I mentioned so far?",
    "How should we handle auth tokens?",
    "Use JWTs with short expiry and refresh tokens for auth",
    "What web framework did I pick?",
    "How to configure CORS in axum?",
    "Remember: CORS should be permissive in dev, restrictive in production",
    "What databases am I using for dev vs production?",
    "How does the actor model relate to async Rust?",
    "What rate limiting approach did I choose?",
    "Give me a complete summary of all my preferences",
    "What were the 3 most important decisions we made?",
    "What's our architectural philosophy?",
    "Remember: this entire conversation is a test of lambda memory",
    "What error handling libraries did I consider?",
    "What deployment strategy did I choose?",
    "My views on error handling verbosity?",
    "How should database timeouts be configured?",
    "What CI/CD requirements did I establish?",
    "What logging framework did I choose?",
    "My coding style preferences summary?",
    "How should API errors be exposed to consumers?",
    # Phase 4: Final recall (76-100)
    "What connection pool settings did I specify?",
    "What's the testing strategy we discussed?",
    "How well do you remember everything from this conversation?",
    "What was the first thing I said to you?",
    "What's the single most important preference I set?",
    "How many explicit remember requests did I make?",
    "What frameworks and libraries did I choose?",
    "Summarize all my architectural decisions in a list",
    "What patterns did we discuss?",
    "Security requirements summary?",
    "Database configuration summary?",
    "API design decisions summary?",
    "What's my overall development philosophy?",
    "Did I change my mind on any decisions? Rate my consistency",
    "What would you recommend I document first from our discussion?",
    "Final: everything you know about my project in one paragraph",
    "Remember: benchmark completed successfully on " + datetime.now().strftime("%Y-%m-%d"),
    "How many distinct things do you remember from this conversation?",
    "What's your confidence level on recalling my preferences? 1-10",
    "Thank you Tem, great test session",
    "One more: what's the most critical decision I made today?",
    "If you could only keep ONE memory from this session, which?",
    "Perfect. End of test. Goodbye",
]


def call_api(messages):
    body = json.dumps({
        "model": MODEL,
        "messages": messages,
        "temperature": 0.7,
        "max_completion_tokens": 500,
    }).encode()
    req = Request(
        "https://api.openai.com/v1/chat/completions",
        data=body,
        headers={
            "Content-Type": "application/json",
            "Authorization": f"Bearer {API_KEY}",
        },
    )
    try:
        with urlopen(req, timeout=60) as resp:
            return json.loads(resp.read())
    except HTTPError as e:
        error_body = e.read().decode()
        try:
            return json.loads(error_body)
        except:
            return {"error": {"message": error_body}}
    except Exception as e:
        return {"error": {"message": str(e)}}


def parse_memory_block(text):
    m = re.search(r"<memory>(.*?)</memory>", text, re.DOTALL)
    if not m:
        return None
    block = m.group(1)
    result = {"summary": "", "essence": "", "importance": 2, "tags": ""}
    for line in block.strip().split("\n"):
        line = line.strip()
        if line.startswith("summary:"):
            result["summary"] = line[8:].strip()
        elif line.startswith("essence:"):
            result["essence"] = line[8:].strip()
        elif line.startswith("importance:"):
            try:
                result["importance"] = max(1, min(5, int(re.search(r"\d+", line[11:]).group())))
            except:
                pass
        elif line.startswith("tags:"):
            result["tags"] = line[5:].strip()
    if not result["summary"] and not result["essence"]:
        return None
    return result


def strip_memory_block(text):
    return re.sub(r"\s*<memory>.*?</memory>\s*", "", text, flags=re.DOTALL).strip()


def init_db():
    conn = sqlite3.connect(DB_PATH)
    conn.execute("""
        CREATE TABLE IF NOT EXISTS lambda_memories (
            hash TEXT PRIMARY KEY, created_at INTEGER NOT NULL, last_accessed INTEGER NOT NULL,
            access_count INTEGER NOT NULL DEFAULT 0, importance REAL NOT NULL DEFAULT 1.0,
            explicit_save INTEGER NOT NULL DEFAULT 0, full_text TEXT NOT NULL,
            summary_text TEXT NOT NULL, essence_text TEXT NOT NULL,
            tags TEXT NOT NULL DEFAULT '[]', memory_type TEXT NOT NULL DEFAULT 'conversation',
            session_id TEXT NOT NULL
        )
    """)
    conn.execute("DELETE FROM lambda_memories WHERE session_id = 'bench-100'")
    conn.commit()
    return conn


def store_memory(conn, turn, msg, parsed, now):
    h = hashlib.sha256(f"bench-100:{turn}:{now}".encode()).hexdigest()[:12]
    explicit = 1 if "remember" in msg.lower() else 0
    full = f"User: {msg[:300]} | Summary: {parsed['summary']}"
    conn.execute(
        "INSERT OR REPLACE INTO lambda_memories VALUES (?,?,?,?,?,?,?,?,?,?,?,?)",
        (h, now, now, 0, parsed["importance"], explicit, full,
         parsed["summary"], parsed["essence"], f"[{parsed['tags']}]",
         "conversation", "bench-100"),
    )
    conn.commit()
    return h


def main():
    if not API_KEY:
        print("Set OPENAI_API_KEY"); sys.exit(1)

    conn = init_db()
    log = open(LOG_PATH, "w")

    def out(s):
        print(s)
        log.write(s + "\n")
        log.flush()

    out(f"═══ λ-Memory 100-Turn Benchmark ═══")
    out(f"Date: {datetime.now()}")
    out(f"Model: {MODEL}")
    out(f"Turns: {len(TURNS)}")
    out("")

    history = []
    total_in = 0
    total_out = 0
    memories_stored = 0
    memory_blocks_found = 0
    errors = 0
    start_time = time.time()

    for i, msg in enumerate(TURNS):
        turn = i + 1
        now = int(time.time())
        out(f"────── Turn {turn}/{len(TURNS)} ──────")
        out(f"USER: {msg}")

        history.append({"role": "user", "content": msg})

        # Build messages with system prompt, keep last 40
        messages = [{"role": "system", "content": SYSTEM_PROMPT}] + history[-40:]

        resp = call_api(messages)

        if "error" in resp:
            errors += 1
            err = resp["error"].get("message", str(resp["error"]))
            out(f"  ERROR: {err}")
            history.append({"role": "assistant", "content": "Error processing."})
            out("")
            time.sleep(1)
            continue

        text = resp["choices"][0]["message"]["content"]
        usage = resp.get("usage", {})
        in_tok = usage.get("prompt_tokens", 0)
        out_tok = usage.get("completion_tokens", 0)
        total_in += in_tok
        total_out += out_tok

        display = strip_memory_block(text)[:500]
        out(f"TEM: {display}")
        out(f"  [tokens: in={in_tok} out={out_tok}]")

        # Parse memory block
        parsed = parse_memory_block(text)
        if parsed:
            memory_blocks_found += 1
            h = store_memory(conn, turn, msg, parsed, now)
            memories_stored += 1
            out(f"  [λ-memory: #{h} imp={parsed['importance']} essence=\"{parsed['essence']}\"]")

        # Add clean response to history
        history.append({"role": "assistant", "content": strip_memory_block(text)})
        out("")
        time.sleep(0.3)

    elapsed = time.time() - start_time

    # ── Results ──
    out("═══════════════════════════════════════")
    out("═══ BENCHMARK RESULTS ═══")
    out("")
    out(f"Turns completed: {len(TURNS)}")
    out(f"Errors: {errors}")
    out(f"Successful turns: {len(TURNS) - errors}")
    out(f"Memory blocks found: {memory_blocks_found}")
    out(f"Memories stored: {memories_stored}")
    out(f"Memory rate: {memory_blocks_found}/{len(TURNS)} = {100*memory_blocks_found/max(1,len(TURNS)):.1f}%")
    out(f"Total input tokens: {total_in}")
    out(f"Total output tokens: {total_out}")
    cost = (total_in * 2 + total_out * 8) / 1_000_000
    out(f"Estimated cost: ${cost:.4f}")
    out(f"Elapsed time: {elapsed:.0f}s ({elapsed/60:.1f}min)")
    out(f"Avg turn time: {elapsed/len(TURNS):.1f}s")
    out("")

    # ── Database analysis ──
    out("═══ λ-Memory Database Analysis ═══")
    out("")

    cur = conn.cursor()
    cur.execute("SELECT COUNT(*) FROM lambda_memories WHERE session_id='bench-100'")
    out(f"Total memories: {cur.fetchone()[0]}")

    cur.execute("SELECT COUNT(*) FROM lambda_memories WHERE session_id='bench-100' AND explicit_save=1")
    out(f"Explicit saves: {cur.fetchone()[0]}")

    out("")
    out("By importance:")
    for row in cur.execute(
        "SELECT CAST(importance AS INTEGER) as imp, COUNT(*) FROM lambda_memories WHERE session_id='bench-100' GROUP BY imp ORDER BY imp"
    ):
        out(f"  Importance {row[0]}: {row[1]} memories")

    out("")
    out("All memories (chronological):")
    out(f"{'Hash':>12} {'Imp':>4} {'Exp':>4} {'Essence':<40} {'Tags':<30}")
    out("-" * 95)
    for row in cur.execute(
        "SELECT substr(hash,1,12), importance, explicit_save, substr(essence_text,1,40), substr(tags,1,30) FROM lambda_memories WHERE session_id='bench-100' ORDER BY created_at"
    ):
        out(f"{row[0]:>12} {row[1]:>4.0f} {row[2]:>4} {row[3]:<40} {row[4]:<30}")

    out("")
    out("Decay simulation (scored NOW):")
    out(f"{'Hash':>12} {'Score':>6} {'RawImp':>7} {'Tier':<6} {'Essence':<35}")
    out("-" * 80)
    now = int(time.time())
    for row in cur.execute(
        f"""SELECT substr(hash,1,12),
            ROUND(importance * exp(-(({now} - last_accessed) / 3600.0) * 0.01), 3) as score,
            importance,
            CASE
                WHEN importance * exp(-(({now} - last_accessed) / 3600.0) * 0.01) > 2.0 THEN 'HOT'
                WHEN importance * exp(-(({now} - last_accessed) / 3600.0) * 0.01) > 1.0 THEN 'WARM'
                WHEN importance * exp(-(({now} - last_accessed) / 3600.0) * 0.01) > 0.3 THEN 'COOL'
                ELSE 'FADED'
            END,
            substr(essence_text,1,35)
        FROM lambda_memories WHERE session_id='bench-100' ORDER BY score DESC"""
    ):
        out(f"{row[0]:>12} {row[1]:>6} {row[2]:>7.0f} {row[3]:<6} {row[4]:<35}")

    out("")
    out(f"Full log: {LOG_PATH}")
    conn.close()
    log.close()


if __name__ == "__main__":
    main()
