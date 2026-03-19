#!/usr/bin/env python3
"""λ-Memory 3-Way Benchmark — 300 turns total (100 per strategy), parallel.

Strategies:
  A) λ-Memory: decay-scored fidelity layers + <memory> block extraction
  B) Current: keyword search over raw conversation (TEMM1E's existing Cat 5/5b/6)
  C) Naive: periodic LLM summarization injected as system context

All use Gemini 3 Flash Preview for speed. Same 100 turns per strategy.
Runs all 3 in parallel via multiprocessing.

Usage: GEMINI_API_KEY=... python3 lambda_bench_3way.py
"""

import json, os, re, hashlib, sqlite3, time, sys, math
from datetime import datetime
from urllib.request import Request, urlopen
from urllib.error import HTTPError
from multiprocessing import Process, Manager
from pathlib import Path

API_KEY = os.environ.get("GEMINI_API_KEY", "")
MODEL = "gemini-2.0-flash"  # Gemini 3 Flash
BASE_DIR = Path(__file__).parent
DB_PATH = Path.home() / ".temm1e" / "memory.db"

# ── Shared turns (same for all 3 strategies) ────────────────────

ESTABLISH_TURNS = [
    # Phase 1: Establishing memories (1-25)
    "Hi, I'm testing your memory today",
    "Remember: I always prefer snake_case for variable names",
    "Best error handling approach in Rust?",
    "I've decided to use thiserror for all error types",
    "Explain async traits in one sentence",
    "Remember: never use unwrap in production, always proper error handling",
    "Box<dyn Error> vs custom error types?",
    "I prefer match over if-let for complex patterns",
    "Design patterns for Rust ownership?",
    "I'm frustrated with verbose error handling",
    "Remember: use tracing not log crate",
    "Builder pattern in Rust briefly",
    "I chose axum over actix-web",
    "HashMap vs BTreeMap perf?",
    "Deploy strategy: always blue-green deployments",
    "Remember: DB migrations must be reversible",
    "Orphan rule in Rust?",
    "Important: public types must impl Debug Clone Serialize",
    "Remember: CI runs clippy -D warnings",
    "Graceful shutdown in async Rust?",
    "Remember: all DB ops need 5-second timeout",
    "I prefer composition over inheritance",
    "Remember: rate limiting at gateway level",
    "Critical: all API responses need request-id header",
    "Remember: structured logging with tracing spans for DB",
    # Phase 2: More context (26-50)
    "I decided SQLite for dev, Postgres for production",
    "Remember: max 20 DB connections in pool",
    "I chose sqlx for compile-time query checking",
    "Remember: sanitize file paths to prevent traversal",
    "Use JWTs with short expiry + refresh tokens for auth",
    "Remember: CORS permissive in dev, restrictive in prod",
    "Remember: never expose internal errors to API consumers",
    "Async vs sync tradeoffs?",
    "How to break Arc cycles?",
    "Tower middleware briefly",
    "Layer vs Service in tower?",
    "API versioning approach?",
    "Backpressure in async systems?",
    "Best way to test async code?",
    "How to mock databases in Rust?",
    "Schema migrations with sqlx?",
    "Connection pooling best practices?",
    "Actor model and async Rust?",
    "sqlx vs diesel comparison?",
    "CORS in axum?",
    "Send and Sync traits?",
    "Dependency injection in Rust?",
    "Remember: benchmark started at " + datetime.now().strftime("%H:%M"),
    "What's my overall dev philosophy?",
    "What CI requirements did I set?",
]

RECALL_TURNS = [
    # Phase 3: Recall questions (51-100) — these test memory
    "What variable naming convention do I prefer?",
    "What error handling library did I choose?",
    "Should I use unwrap in production?",
    "What logging framework did I pick?",
    "What's my deployment strategy?",
    "Are DB migrations reversible or not?",
    "What CI linting requirements did I set?",
    "What DB timeout did I specify?",
    "Do I prefer composition or inheritance?",
    "Where should rate limiting happen?",
    "What header must all API responses include?",
    "What logging approach for DB operations?",
    "What DB for dev vs production?",
    "Max DB connections in pool?",
    "What query library did I choose and why?",
    "Security: what about file paths?",
    "Auth approach: what tokens?",
    "CORS policy for dev vs prod?",
    "Should internal errors be exposed to API consumers?",
    "List ALL my explicit remember requests",
    "What web framework did I choose?",
    "What pattern matching style do I prefer?",
    "What must public types implement?",
    "What's my view on error handling verbosity?",
    "What's the most critical architectural decision?",
    "How many remember requests did I make?",
    "Summarize ALL my preferences in a numbered list",
    "What frameworks and libraries did I pick?",
    "Rate your confidence recalling my preferences: 1-10",
    "If you could only keep ONE preference, which?",
    "What was the first thing I said?",
    "Database configuration summary?",
    "Security requirements summary?",
    "API design decisions summary?",
    "What patterns did we discuss?",
    "Did I change my mind on anything?",
    "Complete summary: everything about my project",
    "What's my testing philosophy?",
    "What should I document first?",
    "What's the single most important thing to remember?",
    "How many distinct topics did we cover?",
    "What infrastructure decisions did I make?",
    "Error handling: full summary",
    "What middleware approach?",
    "Async or sync preference?",
    "What about memory leaks?",
    "Final: grade yourself A-F on memory recall",
    "What time did the benchmark start?",
    "Thank you, end of test",
    "Goodbye",
]

ALL_TURNS = ESTABLISH_TURNS + RECALL_TURNS

# ── System prompts per strategy ─────────────────────────────────

LAMBDA_SYSTEM = """You are Tem, an AI assistant with λ-Memory (decay-scored memory).

For memorable turns (decisions, preferences, critical info), append a <memory> block:
<memory>
summary: (one sentence)
essence: (5 words max)
importance: (1-5: 1=casual, 3=decision, 5=critical)
tags: (up to 5, comma-separated)
</memory>
Skip the block for trivial turns. Keep responses to 2-3 sentences."""

CURRENT_SYSTEM = """You are Tem, a helpful AI assistant.
Keep responses concise (2-3 sentences). Pay attention to user preferences and decisions."""

NAIVE_SYSTEM = """You are Tem, a helpful AI assistant.
Keep responses concise (2-3 sentences). Pay attention to user preferences and decisions.

When provided with a conversation summary, use it to recall earlier context."""

# ── Gemini API ──────────────────────────────────────────────────

def call_gemini(history, system_prompt):
    """Call Gemini API. Returns (text, input_tokens, output_tokens) or raises."""
    contents = []
    for msg in history:
        role = "user" if msg["role"] == "user" else "model"
        contents.append({"role": role, "parts": [{"text": msg["content"]}]})

    body = json.dumps({
        "contents": contents,
        "systemInstruction": {"parts": [{"text": system_prompt}]},
        "generationConfig": {"maxOutputTokens": 400, "temperature": 0.7},
    }).encode()

    url = f"https://generativelanguage.googleapis.com/v1beta/models/{MODEL}:generateContent?key={API_KEY}"
    req = Request(url, data=body, headers={"Content-Type": "application/json"})

    try:
        with urlopen(req, timeout=30) as resp:
            data = json.loads(resp.read())
    except HTTPError as e:
        err = e.read().decode()
        return f"ERROR: {err[:200]}", 0, 0
    except Exception as e:
        return f"ERROR: {e}", 0, 0

    if "error" in data:
        return f"ERROR: {data['error'].get('message', str(data['error']))}", 0, 0

    try:
        text = data["candidates"][0]["content"]["parts"][0]["text"]
    except (KeyError, IndexError):
        return "ERROR: no content in response", 0, 0

    usage = data.get("usageMetadata", {})
    in_tok = usage.get("promptTokenCount", 0)
    out_tok = usage.get("candidatesTokenCount", 0)
    return text, in_tok, out_tok

# ── Memory helpers ──────────────────────────────────────────────

def parse_memory_block(text):
    m = re.search(r"<memory>(.*?)</memory>", text, re.DOTALL)
    if not m: return None
    block = m.group(1)
    r = {"summary": "", "essence": "", "importance": 2, "tags": ""}
    for line in block.strip().split("\n"):
        line = line.strip()
        if line.startswith("summary:"): r["summary"] = line[8:].strip()
        elif line.startswith("essence:"): r["essence"] = line[8:].strip()
        elif line.startswith("importance:"):
            try: r["importance"] = max(1, min(5, int(re.search(r"\d+", line[11:]).group())))
            except: pass
        elif line.startswith("tags:"): r["tags"] = line[5:].strip()
    return r if (r["summary"] or r["essence"]) else None

def strip_memory(text):
    return re.sub(r"\s*<memory>.*?</memory>\s*", "", text, flags=re.DOTALL).strip()

def decay_score(importance, created, accessed, now, lam=0.01):
    age_h = (now - accessed) / 3600.0
    return importance * math.exp(-age_h * lam)

# ── Strategy A: λ-Memory ───────────────────────────────────────

def run_lambda(results_dict):
    """λ-Memory strategy: extract <memory> blocks, build decayed context."""
    sid = "bench-lambda"
    memories = []  # list of dicts
    history = []
    metrics = {"name": "λ-Memory", "turns": 0, "errors": 0, "in_tok": 0, "out_tok": 0,
               "memories_created": 0, "explicit_saves": 0, "log": []}
    log = metrics["log"]

    for i, msg in enumerate(ALL_TURNS):
        turn = i + 1
        now = int(time.time())

        # Build λ-Memory context block
        lambda_ctx = ""
        if memories:
            scored = [(decay_score(m["imp"], m["created"], m["accessed"], now), m) for m in memories]
            scored.sort(key=lambda x: -x[0])
            lines = []
            for score, m in scored[:30]:  # top 30
                if score > 2.0:
                    lines.append(f"[hot] {m['full']} (#{m['hash'][:7]} imp={m['imp']})")
                elif score > 1.0:
                    lines.append(f"[warm] {m['summary']} (#{m['hash'][:7]})")
                elif score > 0.3:
                    lines.append(f"[cool] {m['essence']} (#{m['hash'][:7]})")
                elif score > 0.01:
                    lines.append(f"[faded] #{m['hash'][:7]} | {m['essence']}")
            if lines:
                lambda_ctx = "\n═══ λ-Memory ═══\n" + "\n".join(lines) + "\n═══════════════\n"

        sys_prompt = LAMBDA_SYSTEM
        if lambda_ctx:
            sys_prompt = LAMBDA_SYSTEM + "\n\n" + lambda_ctx

        history.append({"role": "user", "content": msg})
        text, in_t, out_t = call_gemini(history[-30:], sys_prompt)
        metrics["in_tok"] += in_t
        metrics["out_tok"] += out_t

        if text.startswith("ERROR:"):
            metrics["errors"] += 1
            history.append({"role": "assistant", "content": "Error."})
            log.append(f"T{turn} ERROR: {text[:100]}")
            time.sleep(0.5)
            continue

        # Parse memory block
        parsed = parse_memory_block(text)
        if parsed:
            h = hashlib.sha256(f"{sid}:{turn}:{now}".encode()).hexdigest()[:12]
            explicit = 1 if "remember" in msg.lower() else 0
            memories.append({
                "hash": h, "created": now, "accessed": now, "imp": parsed["importance"],
                "explicit": explicit, "full": f"User: {msg[:200]} → {parsed['summary']}",
                "summary": parsed["summary"], "essence": parsed["essence"],
                "tags": parsed["tags"],
            })
            metrics["memories_created"] += 1
            if explicit: metrics["explicit_saves"] += 1

        clean = strip_memory(text)
        history.append({"role": "assistant", "content": clean})
        metrics["turns"] += 1
        log.append(f"T{turn}: {clean[:120]}{'...' if len(clean)>120 else ''}"
                   + (f" [λ:{parsed['essence']}]" if parsed else ""))
        time.sleep(0.15)  # rate limit

    metrics["memory_count"] = len(memories)
    results_dict["lambda"] = metrics

# ── Strategy B: Current Memory (keyword search, no decay) ──────

def run_current(results_dict):
    """Current TEMM1E memory: store all turns, keyword-match last query."""
    stored = []  # all conversation entries
    history = []
    metrics = {"name": "Current Memory", "turns": 0, "errors": 0, "in_tok": 0, "out_tok": 0,
               "memories_created": 0, "explicit_saves": 0, "log": []}
    log = metrics["log"]

    for i, msg in enumerate(ALL_TURNS):
        turn = i + 1

        # Keyword search: find stored entries matching words in current message
        keywords = set(msg.lower().split()) - {"i", "a", "the", "is", "my", "what", "did", "do",
            "how", "for", "in", "to", "of", "and", "or", "you", "me", "we", "it", "that", "this"}
        matches = []
        for entry in stored:
            score = sum(1 for k in keywords if k in entry["text"].lower())
            if score > 0:
                matches.append((score, entry))
        matches.sort(key=lambda x: -x[0])
        top = matches[:5]

        memory_ctx = ""
        if top:
            lines = [f"[{e['ts']}] {e['text'][:150]}" for _, e in top]
            memory_ctx = "\nRelevant context from memory:\n" + "\n".join(lines)

        sys_prompt = CURRENT_SYSTEM
        if memory_ctx:
            sys_prompt = CURRENT_SYSTEM + "\n" + memory_ctx

        history.append({"role": "user", "content": msg})
        text, in_t, out_t = call_gemini(history[-30:], sys_prompt)
        metrics["in_tok"] += in_t
        metrics["out_tok"] += out_t

        if text.startswith("ERROR:"):
            metrics["errors"] += 1
            history.append({"role": "assistant", "content": "Error."})
            log.append(f"T{turn} ERROR: {text[:100]}")
            time.sleep(0.5)
            continue

        # Store conversation entry
        stored.append({"text": f"User: {msg} | Assistant: {text[:200]}",
                       "ts": datetime.now().strftime("%H:%M")})
        metrics["memories_created"] += 1

        history.append({"role": "assistant", "content": text})
        metrics["turns"] += 1
        log.append(f"T{turn}: {text[:120]}{'...' if len(text)>120 else ''}")
        time.sleep(0.15)

    metrics["memory_count"] = len(stored)
    results_dict["current"] = metrics

# ── Strategy C: Naive Summarization ────────────────────────────

def run_naive(results_dict):
    """Naive: every 10 turns, ask the LLM to summarize, inject as context."""
    history = []
    summary = ""
    metrics = {"name": "Naive Summary", "turns": 0, "errors": 0, "in_tok": 0, "out_tok": 0,
               "memories_created": 0, "explicit_saves": 0, "log": [], "summaries_generated": 0}
    log = metrics["log"]

    for i, msg in enumerate(ALL_TURNS):
        turn = i + 1

        # Every 10 turns, generate a summary
        if turn > 1 and (turn - 1) % 10 == 0 and history:
            sum_prompt = "Summarize the key facts, decisions, and preferences from this conversation in bullet points. Be comprehensive."
            sum_history = history[-20:] + [{"role": "user", "content": sum_prompt}]
            sum_text, in_t, out_t = call_gemini(sum_history, NAIVE_SYSTEM)
            metrics["in_tok"] += in_t
            metrics["out_tok"] += out_t
            if not sum_text.startswith("ERROR:"):
                summary = sum_text
                metrics["summaries_generated"] = metrics.get("summaries_generated", 0) + 1
            time.sleep(0.15)

        sys_prompt = NAIVE_SYSTEM
        if summary:
            sys_prompt = NAIVE_SYSTEM + f"\n\nConversation summary so far:\n{summary}"

        history.append({"role": "user", "content": msg})
        text, in_t, out_t = call_gemini(history[-30:], sys_prompt)
        metrics["in_tok"] += in_t
        metrics["out_tok"] += out_t

        if text.startswith("ERROR:"):
            metrics["errors"] += 1
            history.append({"role": "assistant", "content": "Error."})
            log.append(f"T{turn} ERROR: {text[:100]}")
            time.sleep(0.5)
            continue

        history.append({"role": "assistant", "content": text})
        metrics["turns"] += 1
        metrics["memories_created"] += 1  # count turns as "memories" for comparison
        log.append(f"T{turn}: {text[:120]}{'...' if len(text)>120 else ''}")
        time.sleep(0.15)

    metrics["memory_count"] = metrics.get("summaries_generated", 0)
    results_dict["naive"] = metrics

# ── Main: run all 3 in parallel ─────────────────────────────────

def main():
    if not API_KEY:
        print("Set GEMINI_API_KEY"); sys.exit(1)

    print(f"═══ λ-Memory 3-Way Benchmark ═══")
    print(f"Date: {datetime.now()}")
    print(f"Model: {MODEL}")
    print(f"Turns per strategy: {len(ALL_TURNS)}")
    print(f"Total API calls: ~{len(ALL_TURNS) * 3}")
    print(f"Running 3 strategies in PARALLEL...")
    print()

    manager = Manager()
    results = manager.dict()
    start = time.time()

    procs = [
        Process(target=run_lambda, args=(results,), name="λ-Memory"),
        Process(target=run_current, args=(results,), name="Current"),
        Process(target=run_naive, args=(results,), name="Naive"),
    ]

    for p in procs:
        p.start()
        print(f"  Started: {p.name} (pid={p.pid})")

    for p in procs:
        p.join()
        print(f"  Finished: {p.name}")

    elapsed = time.time() - start
    print(f"\nAll 3 complete in {elapsed:.0f}s ({elapsed/60:.1f}min)\n")

    # ── Write report ────────────────────────────────────────────
    report_path = BASE_DIR / "LAMBDA_BENCH_REPORT.md"
    with open(report_path, "w") as f:
        def w(s=""): f.write(s + "\n"); print(s)

        w("# λ-Memory 3-Way Benchmark Report")
        w()
        w(f"> **Date:** {datetime.now().strftime('%Y-%m-%d %H:%M')}")
        w(f"> **Model:** {MODEL}")
        w(f"> **Author:** TEMM1E's Lab")
        w(f"> **Turns per strategy:** {len(ALL_TURNS)}")
        w(f"> **Total elapsed:** {elapsed:.0f}s ({elapsed/60:.1f}min)")
        w()
        w("---")
        w()
        w("## Executive Summary")
        w()
        w("| Metric | λ-Memory | Current Memory | Naive Summary |")
        w("|--------|----------|----------------|---------------|")

        r = {k: dict(v) for k, v in results.items()}
        lam = r.get("lambda", {})
        cur = r.get("current", {})
        nav = r.get("naive", {})

        w(f"| Turns completed | {lam.get('turns',0)} | {cur.get('turns',0)} | {nav.get('turns',0)} |")
        w(f"| Errors | {lam.get('errors',0)} | {cur.get('errors',0)} | {nav.get('errors',0)} |")
        w(f"| Memories created | {lam.get('memories_created',0)} | {cur.get('memories_created',0)} | {nav.get('summaries_generated', nav.get('memory_count',0))} summaries |")
        w(f"| Explicit saves | {lam.get('explicit_saves',0)} | N/A | N/A |")
        w(f"| Input tokens | {lam.get('in_tok',0):,} | {cur.get('in_tok',0):,} | {nav.get('in_tok',0):,} |")
        w(f"| Output tokens | {lam.get('out_tok',0):,} | {cur.get('out_tok',0):,} | {nav.get('out_tok',0):,} |")
        total_lam = lam.get('in_tok',0) + lam.get('out_tok',0)
        total_cur = cur.get('in_tok',0) + cur.get('out_tok',0)
        total_nav = nav.get('in_tok',0) + nav.get('out_tok',0)
        w(f"| Total tokens | {total_lam:,} | {total_cur:,} | {total_nav:,} |")
        w()

        # Token efficiency
        w("## Token Efficiency")
        w()
        baseline = max(total_cur, 1)
        w(f"- **λ-Memory vs Current:** {((total_lam - total_cur) / baseline * 100):+.1f}% tokens")
        w(f"- **Naive vs Current:** {((total_nav - total_cur) / baseline * 100):+.1f}% tokens")
        w(f"- **λ-Memory memories stored:** {lam.get('memories_created',0)} (selective)")
        w(f"- **Current entries stored:** {cur.get('memories_created',0)} (every turn)")
        w(f"- **Naive summaries:** {nav.get('summaries_generated', 0)} (periodic)")
        w()

        # Memory analysis for λ-Memory
        w("## λ-Memory Analysis")
        w()
        mem_count = lam.get("memories_created", 0)
        total_turns = lam.get("turns", 0)
        w(f"- **Memory creation rate:** {mem_count}/{total_turns} turns = {100*mem_count/max(1,total_turns):.0f}%")
        w(f"- **Explicit saves:** {lam.get('explicit_saves', 0)}")
        w(f"- **Implicit saves:** {mem_count - lam.get('explicit_saves', 0)}")
        w()

        # Full conversation logs
        for strategy, key in [("λ-Memory", "lambda"), ("Current Memory", "current"), ("Naive Summary", "naive")]:
            w(f"## {strategy} — Full Log")
            w()
            w("```")
            for line in r.get(key, {}).get("log", []):
                w(line)
            w("```")
            w()

        w("---")
        w(f"*Generated by TEMM1E's Lab benchmark suite*")

    print(f"\nReport saved: {report_path}")

    # Also save raw metrics as JSON
    metrics_path = BASE_DIR / "lambda_bench_metrics.json"
    with open(metrics_path, "w") as f:
        # Convert to serializable
        out = {}
        for k, v in results.items():
            d = dict(v)
            d.pop("log", None)  # logs are in the markdown
            out[k] = d
        out["meta"] = {
            "date": datetime.now().isoformat(),
            "model": MODEL,
            "turns": len(ALL_TURNS),
            "elapsed_seconds": round(elapsed),
        }
        json.dump(out, f, indent=2)
    print(f"Metrics saved: {metrics_path}")


if __name__ == "__main__":
    main()
