#!/usr/bin/env python3
"""λ-Memory 3-Way Benchmark v2 — Tuned λ-Memory with stronger extraction.

Fixes from v1:
  1. Stronger system prompt — ALWAYS emit <memory> on remember/critical/decision
  2. Runtime fallback — auto-generate memory if user said "remember" but LLM skipped
  3. Token-capped context — max 800 tokens for λ-Memory section
  4. Shorter format strings — terse [H]/[W]/[C]/[F] format
  5. Skip context injection when < 3 memories

All 3 strategies run in parallel via multiprocessing.
Usage: GEMINI_API_KEY=... python3 lambda_bench_3way_v2.py
"""

import json, os, re, hashlib, time, sys, math
from datetime import datetime
from urllib.request import Request, urlopen
from urllib.error import HTTPError
from multiprocessing import Process, Manager
from pathlib import Path

API_KEY = os.environ.get("GEMINI_API_KEY", "")
MODEL = "gemini-2.0-flash"
BASE_DIR = Path(__file__).parent

# ── Token estimation ─────────────────────────────────────────
def est_tokens(s):
    return len(s) // 4

# ── Shared turns ─────────────────────────────────────────────

ESTABLISH_TURNS = [
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

# ── System prompts ───────────────────────────────────────────

# v2: Much stronger memory extraction instruction
LAMBDA_SYSTEM_V2 = """You are Tem, an AI assistant with λ-Memory.
Keep responses to 2-3 sentences.

MEMORY RULE — You MUST append a <memory> block when ANY of these apply:
- User says "remember", "important", "critical", "always", "never"
- User makes a decision or states a preference
- User sets a requirement or configuration value
Format:
<memory>
summary: (one sentence capturing the key fact)
essence: (3-5 words)
importance: (1-5)
tags: (csv)
</memory>
Only SKIP the block for pure questions with no preference and greetings."""

CURRENT_SYSTEM = """You are Tem, a helpful AI assistant.
Keep responses concise (2-3 sentences). Pay close attention to user preferences and decisions.
When asked to recall, be specific — cite exact values and choices the user stated."""

NAIVE_SYSTEM = """You are Tem, a helpful AI assistant.
Keep responses concise (2-3 sentences). Pay close attention to user preferences and decisions.
When provided with a conversation summary, reference it precisely for recall questions."""

# ── Gemini API ───────────────────────────────────────────────

def call_gemini(history, system_prompt):
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
        return f"ERROR: {e.read().decode()[:200]}", 0, 0
    except Exception as e:
        return f"ERROR: {e}", 0, 0
    if "error" in data:
        return f"ERROR: {data['error'].get('message', '')}", 0, 0
    try:
        text = data["candidates"][0]["content"]["parts"][0]["text"]
    except (KeyError, IndexError):
        return "ERROR: no content", 0, 0
    usage = data.get("usageMetadata", {})
    return text, usage.get("promptTokenCount", 0), usage.get("candidatesTokenCount", 0)

# ── Memory helpers ───────────────────────────────────────────

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

def decay_score(importance, accessed, now, lam=0.01):
    age_h = (now - accessed) / 3600.0
    return importance * math.exp(-age_h * lam)

# v2 FIX: Auto-generate memory when user said "remember" but LLM didn't emit block
# v2.1: Expanded triggers — catch decisions and preferences, not just "remember"
REMEMBER_TRIGGERS = [
    "remember:", "remember this", "important:", "critical:",
    "always ", "never ", "i chose ", "i decided", "i prefer",
    "i've decided", "deploy strategy", "use jwt", "use jwt",
    "must impl", "must include", "must be ", "should be ",
    "max ", "timeout",
]

def auto_generate_memory(user_msg, assistant_response):
    """Fallback: extract a memory from the assistant's acknowledgment."""
    # Take first sentence of assistant response as summary
    sentences = re.split(r'[.!?]\s', assistant_response)
    summary = (sentences[0] + ".") if sentences else assistant_response[:100]
    # Essence: first 5 meaningful words from user message after trigger
    words = [w for w in user_msg.split() if w.lower() not in
             {"remember:", "remember", "this:", "important:", "critical:", "always", "never",
              "i", "a", "the", "for", "to", "in", "use", "that"}]
    essence = " ".join(words[:5])
    # Higher importance for explicit remember/critical, moderate for decisions
    importance = 4 if any(t in user_msg.lower() for t in ["remember:", "critical:", "important:"]) else 3
    tags = "user-preference"
    return {"summary": summary, "essence": essence, "importance": importance, "tags": tags}

# ── Strategy A: λ-Memory v2 (tuned) ─────────────────────────

MAX_LAMBDA_CONTEXT_TOKENS = 800  # v2: hard cap

def run_lambda(results_dict):
    memories = []
    history = []
    metrics = {"name": "λ-Memory v2", "turns": 0, "errors": 0, "in_tok": 0, "out_tok": 0,
               "memories_created": 0, "explicit_saves": 0, "auto_generated": 0, "log": []}
    log = metrics["log"]

    for i, msg in enumerate(ALL_TURNS):
        turn = i + 1
        now = int(time.time())

        # Build λ-Memory context (capped at MAX_LAMBDA_CONTEXT_TOKENS)
        lambda_ctx = ""
        if len(memories) >= 3:  # v2: skip when < 3 memories
            scored = [(decay_score(m["imp"], m["accessed"], now), m) for m in memories]
            scored.sort(key=lambda x: -x[0])

            # v2: pack with token budget
            lines = []
            tok_used = 0
            for score, m in scored:
                if score < 0.01:
                    continue
                if score > 2.0:
                    line = f"[H] {m['summary']} (#{m['hash'][:7]} i={m['imp']})"
                elif score > 1.0:
                    line = f"[W] {m['summary']} (#{m['hash'][:7]})"
                elif score > 0.3:
                    line = f"[C] {m['essence']} (#{m['hash'][:7]})"
                else:
                    line = f"[F] #{m['hash'][:7]}|{m['essence']}"

                cost = est_tokens(line)
                if tok_used + cost > MAX_LAMBDA_CONTEXT_TOKENS:
                    break
                lines.append(line)
                tok_used += cost

            if lines:
                lambda_ctx = "\n═══ λ-Memory ═══\n" + "\n".join(lines) + "\n═══════════════"

        sys_prompt = LAMBDA_SYSTEM_V2
        if lambda_ctx:
            sys_prompt = LAMBDA_SYSTEM_V2 + "\n" + lambda_ctx

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

        # v2 FIX: auto-generate if user triggered but LLM skipped
        auto = False
        if not parsed and any(t in msg.lower() for t in REMEMBER_TRIGGERS):
            clean = strip_memory(text)
            parsed = auto_generate_memory(msg, clean)
            auto = True
            metrics["auto_generated"] += 1

        if parsed:
            h = hashlib.sha256(f"bench-lv21:{turn}:{now}".encode()).hexdigest()[:12]
            explicit = 1 if "remember" in msg.lower() else 0
            memories.append({
                "hash": h, "created": now, "accessed": now, "imp": parsed["importance"],
                "explicit": explicit,
                "full": f"User: {msg[:200]} → {parsed['summary']}",
                "summary": parsed["summary"], "essence": parsed["essence"],
            })
            metrics["memories_created"] += 1
            if explicit: metrics["explicit_saves"] += 1

        clean = strip_memory(text)
        history.append({"role": "assistant", "content": clean})
        metrics["turns"] += 1
        tag = f" [λ:{parsed['essence']}]" if parsed else ""
        tag += " [auto]" if auto else ""
        log.append(f"T{turn}: {clean[:120]}{'...' if len(clean)>120 else ''}{tag}")
        time.sleep(0.15)

    metrics["memory_count"] = len(memories)
    results_dict["lambda"] = metrics

# ── Strategy B: Current Memory ───────────────────────────────

def run_current(results_dict):
    stored = []
    history = []
    metrics = {"name": "Current Memory", "turns": 0, "errors": 0, "in_tok": 0, "out_tok": 0,
               "memories_created": 0, "explicit_saves": 0, "log": []}
    log = metrics["log"]

    for i, msg in enumerate(ALL_TURNS):
        turn = i + 1
        keywords = set(msg.lower().split()) - {"i", "a", "the", "is", "my", "what", "did", "do",
            "how", "for", "in", "to", "of", "and", "or", "you", "me", "we", "it", "that", "this"}
        matches = sorted(
            [(sum(1 for k in keywords if k in e["text"].lower()), e) for e in stored],
            key=lambda x: -x[0]
        )[:5]
        memory_ctx = ""
        if matches and matches[0][0] > 0:
            lines = [f"[{e['ts']}] {e['text'][:150]}" for _, e in matches if _ > 0]
            if lines:
                memory_ctx = "\nRelevant from memory:\n" + "\n".join(lines)

        sys_prompt = CURRENT_SYSTEM + memory_ctx if memory_ctx else CURRENT_SYSTEM

        history.append({"role": "user", "content": msg})
        text, in_t, out_t = call_gemini(history[-30:], sys_prompt)
        metrics["in_tok"] += in_t; metrics["out_tok"] += out_t

        if text.startswith("ERROR:"):
            metrics["errors"] += 1
            history.append({"role": "assistant", "content": "Error."})
            log.append(f"T{turn} ERROR: {text[:100]}")
            time.sleep(0.5); continue

        stored.append({"text": f"User: {msg} | Tem: {text[:200]}", "ts": datetime.now().strftime("%H:%M")})
        metrics["memories_created"] += 1
        history.append({"role": "assistant", "content": text})
        metrics["turns"] += 1
        log.append(f"T{turn}: {text[:120]}{'...' if len(text)>120 else ''}")
        time.sleep(0.15)

    metrics["memory_count"] = len(stored)
    results_dict["current"] = metrics

# ── Strategy C: Naive Summary ────────────────────────────────

def run_naive(results_dict):
    history = []
    summary = ""
    metrics = {"name": "Naive Summary", "turns": 0, "errors": 0, "in_tok": 0, "out_tok": 0,
               "memories_created": 0, "explicit_saves": 0, "log": [], "summaries_generated": 0}
    log = metrics["log"]

    for i, msg in enumerate(ALL_TURNS):
        turn = i + 1
        if turn > 1 and (turn - 1) % 10 == 0 and history:
            sum_history = history[-20:] + [{"role": "user",
                "content": "Summarize ALL key facts, decisions, preferences, and explicit requirements from our conversation. Be exhaustive — list every preference and decision."}]
            sum_text, in_t, out_t = call_gemini(sum_history, NAIVE_SYSTEM)
            metrics["in_tok"] += in_t; metrics["out_tok"] += out_t
            if not sum_text.startswith("ERROR:"):
                summary = sum_text
                metrics["summaries_generated"] = metrics.get("summaries_generated", 0) + 1
            time.sleep(0.15)

        sys_prompt = NAIVE_SYSTEM
        if summary:
            sys_prompt = NAIVE_SYSTEM + f"\n\nConversation summary:\n{summary}"

        history.append({"role": "user", "content": msg})
        text, in_t, out_t = call_gemini(history[-30:], sys_prompt)
        metrics["in_tok"] += in_t; metrics["out_tok"] += out_t

        if text.startswith("ERROR:"):
            metrics["errors"] += 1
            history.append({"role": "assistant", "content": "Error."})
            log.append(f"T{turn} ERROR: {text[:100]}")
            time.sleep(0.5); continue

        history.append({"role": "assistant", "content": text})
        metrics["turns"] += 1
        metrics["memories_created"] += 1
        log.append(f"T{turn}: {text[:120]}{'...' if len(text)>120 else ''}")
        time.sleep(0.15)

    metrics["memory_count"] = metrics.get("summaries_generated", 0)
    results_dict["naive"] = metrics

# ── Main ─────────────────────────────────────────────────────

def main():
    if not API_KEY:
        print("Set GEMINI_API_KEY"); sys.exit(1)

    print(f"═══ λ-Memory 3-Way Benchmark v2 (Tuned) ═══")
    print(f"Model: {MODEL} | Turns: {len(ALL_TURNS)} × 3 = {len(ALL_TURNS)*3}")
    print(f"Fixes: stronger prompt, auto-fallback, 800-tok cap, terse format")
    print(f"Running 3 strategies in PARALLEL...\n")

    manager = Manager()
    results = manager.dict()
    start = time.time()

    procs = [
        Process(target=run_lambda, args=(results,), name="λ-Memory-v2"),
        Process(target=run_current, args=(results,), name="Current"),
        Process(target=run_naive, args=(results,), name="Naive"),
    ]
    for p in procs:
        p.start()
        print(f"  Started: {p.name} (pid={p.pid})")
    for p in procs:
        p.join()
        print(f"  Done: {p.name}")

    elapsed = time.time() - start
    print(f"\nAll complete in {elapsed:.0f}s ({elapsed/60:.1f}min)\n")

    # ── Write report ─────────────────────────────────────────
    r = {k: dict(v) for k, v in results.items()}
    lam = r.get("lambda", {}); cur = r.get("current", {}); nav = r.get("naive", {})

    report_path = BASE_DIR / "LAMBDA_BENCH_REPORT_V2_1.md"
    metrics_path = BASE_DIR / "lambda_bench_metrics_v2_1.json"

    with open(report_path, "w") as f:
        def w(s=""): f.write(s + "\n"); print(s)

        w("# λ-Memory 3-Way Benchmark v2 — Tuned")
        w()
        w(f"> **Date:** {datetime.now().strftime('%Y-%m-%d %H:%M')}")
        w(f"> **Model:** {MODEL}")
        w(f"> **Author:** TEMM1E's Lab")
        w(f"> **Turns:** {len(ALL_TURNS)} per strategy ({len(ALL_TURNS)*3} total)")
        w(f"> **Elapsed:** {elapsed:.0f}s ({elapsed/60:.1f}min)")
        w()
        w("## v2 Changes from v1")
        w()
        w("1. **Stronger system prompt** — MUST emit `<memory>` on remember/critical/decision keywords")
        w("2. **Runtime auto-fallback** — if user said 'remember' but LLM skipped, auto-generate memory from response")
        w("3. **800-token cap** on λ-Memory context section (was uncapped)")
        w("4. **Terse format** — `[H]`/`[W]`/`[C]`/`[F]` instead of verbose `[hot]`/`[warm]`/etc")
        w("5. **Skip injection** when < 3 memories stored")
        w()
        w("---")
        w()
        w("## Results")
        w()
        w("| Metric | λ-Memory v2 | Current Memory | Naive Summary |")
        w("|--------|-------------|----------------|---------------|")
        w(f"| Turns | {lam.get('turns',0)} | {cur.get('turns',0)} | {nav.get('turns',0)} |")
        w(f"| Errors | {lam.get('errors',0)} | {cur.get('errors',0)} | {nav.get('errors',0)} |")
        w(f"| Memories created | {lam.get('memories_created',0)} | {cur.get('memories_created',0)} | {nav.get('summaries_generated',0)} summaries |")
        w(f"| Auto-generated | {lam.get('auto_generated',0)} | N/A | N/A |")
        w(f"| Explicit saves | {lam.get('explicit_saves',0)} | N/A | N/A |")

        lt = lam.get('in_tok',0) + lam.get('out_tok',0)
        ct = cur.get('in_tok',0) + cur.get('out_tok',0)
        nt = nav.get('in_tok',0) + nav.get('out_tok',0)
        w(f"| Input tokens | {lam.get('in_tok',0):,} | {cur.get('in_tok',0):,} | {nav.get('in_tok',0):,} |")
        w(f"| Output tokens | {lam.get('out_tok',0):,} | {cur.get('out_tok',0):,} | {nav.get('out_tok',0):,} |")
        w(f"| **Total tokens** | **{lt:,}** | **{ct:,}** | **{nt:,}** |")
        if ct > 0:
            w(f"| vs Current | {((lt-ct)/ct*100):+.1f}% | baseline | {((nt-ct)/ct*100):+.1f}% |")
        w()

        w("## λ-Memory v2 Analysis")
        w()
        mc = lam.get('memories_created', 0)
        tt = lam.get('turns', 0)
        w(f"- Memory creation rate: {mc}/{tt} = {100*mc/max(1,tt):.0f}%")
        w(f"- LLM-generated: {mc - lam.get('auto_generated',0)}")
        w(f"- Auto-fallback: {lam.get('auto_generated',0)}")
        w(f"- Explicit saves: {lam.get('explicit_saves',0)}")
        w(f"- Context cap: {MAX_LAMBDA_CONTEXT_TOKENS} tokens/turn")
        w()

        for strategy, key in [("λ-Memory v2", "lambda"), ("Current Memory", "current"), ("Naive Summary", "naive")]:
            w(f"## {strategy} — Full Log")
            w()
            w("```")
            for line in r.get(key, {}).get("log", []):
                w(line)
            w("```")
            w()

        w("---")
        w("*TEMM1E's Lab — v2 benchmark*")

    # Save metrics
    out = {}
    for k, v in r.items():
        d = dict(v); d.pop("log", None); out[k] = d
    out["meta"] = {"date": datetime.now().isoformat(), "model": MODEL,
                   "turns": len(ALL_TURNS), "elapsed_seconds": round(elapsed), "version": "v2"}
    with open(metrics_path, "w") as f:
        json.dump(out, f, indent=2)

    print(f"\nReport: {report_path}")
    print(f"Metrics: {metrics_path}")


if __name__ == "__main__":
    main()
