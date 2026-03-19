#!/usr/bin/env python3
"""λ-Memory 3-Way Benchmark — GPT-5.2 edition.
Same 100 turns × 3 strategies, parallel, with scoring built in."""

import json, os, re, hashlib, time, sys, math
from datetime import datetime
from urllib.request import Request, urlopen
from urllib.error import HTTPError
from multiprocessing import Process, Manager
from pathlib import Path

API_KEY = os.environ.get("OPENAI_API_KEY", "")
MODEL = "gpt-5.2"
BASE_DIR = Path(__file__).parent

def est_tokens(s): return len(s) // 4

# ── Turns ────────────────────────────────────────────────────

ESTABLISH = [
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

RECALL = [
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

ALL_TURNS = ESTABLISH + RECALL

# ── Prompts ──────────────────────────────────────────────────

LAMBDA_PROMPT = """You are Tem, an AI assistant with λ-Memory.
Keep responses to 2-3 sentences.

MEMORY RULE — You MUST append a <memory> block when ANY of these apply:
- User says "remember", "important", "critical", "always", "never"
- User makes a decision or states a preference (e.g. "I chose", "I prefer", "I decided")
- User sets a requirement or configuration value
Format:
<memory>
summary: (one sentence capturing the key fact)
essence: (3-5 words)
importance: (1-5)
tags: (csv)
</memory>
Only SKIP the block for pure questions with no preference and greetings/farewells."""

CURRENT_PROMPT = """You are Tem, a helpful AI assistant.
Keep responses concise (2-3 sentences). Pay close attention to user preferences and decisions.
When asked to recall, be specific — cite exact values and choices the user stated."""

NAIVE_PROMPT = """You are Tem, a helpful AI assistant.
Keep responses concise (2-3 sentences). Pay close attention to user preferences and decisions.
When provided with a conversation summary, reference it precisely for recall questions."""

# ── OpenAI API ───────────────────────────────────────────────

def call_openai(messages, system_prompt):
    msgs = [{"role": "system", "content": system_prompt}] + messages[-40:]
    body = json.dumps({
        "model": MODEL,
        "messages": msgs,
        "temperature": 0.7,
        "max_completion_tokens": 400,
    }).encode()
    req = Request("https://api.openai.com/v1/chat/completions", data=body,
                  headers={"Content-Type": "application/json", "Authorization": f"Bearer {API_KEY}"})
    try:
        with urlopen(req, timeout=60) as resp:
            data = json.loads(resp.read())
    except HTTPError as e:
        err = e.read().decode()
        try: err = json.loads(err).get("error", {}).get("message", err[:200])
        except: err = err[:200]
        return f"ERROR: {err}", 0, 0
    except Exception as e:
        return f"ERROR: {e}", 0, 0
    if "error" in data:
        return f"ERROR: {data['error'].get('message','')}", 0, 0
    try:
        text = data["choices"][0]["message"]["content"]
    except: return "ERROR: no content", 0, 0
    u = data.get("usage", {})
    return text, u.get("prompt_tokens", 0), u.get("completion_tokens", 0)

# ── Helpers ──────────────────────────────────────────────────

def parse_mem(text):
    m = re.search(r"<memory>(.*?)</memory>", text, re.DOTALL)
    if not m: return None
    r = {"summary": "", "essence": "", "importance": 2, "tags": ""}
    for line in m.group(1).strip().split("\n"):
        line = line.strip()
        if line.startswith("summary:"): r["summary"] = line[8:].strip()
        elif line.startswith("essence:"): r["essence"] = line[8:].strip()
        elif line.startswith("importance:"):
            try: r["importance"] = max(1, min(5, int(re.search(r"\d+", line[11:]).group())))
            except: pass
        elif line.startswith("tags:"): r["tags"] = line[5:].strip()
    return r if (r["summary"] or r["essence"]) else None

def strip_mem(text):
    return re.sub(r"\s*<memory>.*?</memory>\s*", "", text, flags=re.DOTALL).strip()

def decay(imp, accessed, now, lam=0.01):
    return imp * math.exp(-(now - accessed) / 3600.0 * lam)

TRIGGERS = ["remember:", "remember this", "important:", "critical:",
            "always ", "never ", "i chose ", "i decided", "i prefer",
            "i've decided", "deploy strategy", "use jwt",
            "must impl", "must include", "must be ", "max ", "timeout"]

def auto_mem(user_msg, resp):
    sentences = re.split(r'[.!?]\s', resp)
    summary = (sentences[0] + ".") if sentences else resp[:100]
    words = [w for w in user_msg.split() if w.lower() not in
             {"remember:", "remember", "important:", "critical:", "i", "a", "the",
              "for", "to", "in", "use", "always", "never", "this", "that"}]
    essence = " ".join(words[:5])
    imp = 4 if any(t in user_msg.lower() for t in ["remember:", "critical:"]) else 3
    return {"summary": summary, "essence": essence, "importance": imp, "tags": "auto"}

MAX_CTX_TOKENS = 800

# ── Strategy A: λ-Memory ────────────────────────────────────

def run_lambda(results_dict):
    memories = []; history = []
    m = {"name": "λ-Memory (GPT-5.2)", "turns": 0, "errors": 0, "in_tok": 0, "out_tok": 0,
         "memories_created": 0, "explicit_saves": 0, "auto_generated": 0, "log": []}
    for i, msg in enumerate(ALL_TURNS):
        turn = i + 1; now = int(time.time())
        ctx = ""
        if len(memories) >= 3:
            scored = sorted([(decay(x["imp"], x["acc"], now), x) for x in memories], key=lambda x: -x[0])
            lines = []; tok = 0
            for sc, x in scored:
                if sc < 0.01: continue
                if sc > 2.0: line = f"[H] {x['summary']} (#{x['h'][:7]} i={x['imp']})"
                elif sc > 1.0: line = f"[W] {x['summary']} (#{x['h'][:7]})"
                elif sc > 0.3: line = f"[C] {x['ess']} (#{x['h'][:7]})"
                else: line = f"[F] #{x['h'][:7]}|{x['ess']}"
                c = est_tokens(line)
                if tok + c > MAX_CTX_TOKENS: break
                lines.append(line); tok += c
            if lines: ctx = "\n═══ λ-Memory ═══\n" + "\n".join(lines) + "\n═══════════════"
        prompt = LAMBDA_PROMPT + ("\n" + ctx if ctx else "")
        history.append({"role": "user", "content": msg})
        text, it, ot = call_openai(history[-30:], prompt)
        m["in_tok"] += it; m["out_tok"] += ot
        if text.startswith("ERROR:"):
            m["errors"] += 1; history.append({"role": "assistant", "content": "Error."})
            m["log"].append(f"T{turn} ERROR: {text[:100]}"); time.sleep(1); continue
        parsed = parse_mem(text)
        auto = False
        if not parsed and any(t in msg.lower() for t in TRIGGERS):
            parsed = auto_mem(msg, strip_mem(text)); auto = True; m["auto_generated"] += 1
        if parsed:
            h = hashlib.sha256(f"gpt52:{turn}:{now}".encode()).hexdigest()[:12]
            exp = 1 if "remember" in msg.lower() else 0
            memories.append({"h": h, "cr": now, "acc": now, "imp": parsed["importance"],
                             "exp": exp, "summary": parsed["summary"], "ess": parsed["essence"]})
            m["memories_created"] += 1
            if exp: m["explicit_saves"] += 1
        clean = strip_mem(text)
        history.append({"role": "assistant", "content": clean})
        m["turns"] += 1
        tag = f" [λ:{parsed['essence']}]" if parsed else ""
        tag += " [auto]" if auto else ""
        m["log"].append(f"T{turn}: {clean[:120]}{'...' if len(clean)>120 else ''}{tag}")
        time.sleep(0.2)
    m["memory_count"] = len(memories)
    results_dict["lambda"] = m

# ── Strategy B: Current ─────────────────────────────────────

def run_current(results_dict):
    stored = []; history = []
    m = {"name": "Current Memory (GPT-5.2)", "turns": 0, "errors": 0, "in_tok": 0, "out_tok": 0,
         "memories_created": 0, "log": []}
    for i, msg in enumerate(ALL_TURNS):
        turn = i + 1
        kw = set(msg.lower().split()) - {"i","a","the","is","my","what","did","do","how","for","in",
            "to","of","and","or","you","me","we","it","that","this"}
        top = sorted([(sum(1 for k in kw if k in e["t"].lower()), e) for e in stored], key=lambda x: -x[0])[:5]
        ctx = ""
        if top and top[0][0] > 0:
            lines = [f"[{e['ts']}] {e['t'][:150]}" for s, e in top if s > 0]
            if lines: ctx = "\nRelevant from memory:\n" + "\n".join(lines)
        history.append({"role": "user", "content": msg})
        text, it, ot = call_openai(history[-30:], CURRENT_PROMPT + ctx)
        m["in_tok"] += it; m["out_tok"] += ot
        if text.startswith("ERROR:"):
            m["errors"] += 1; history.append({"role": "assistant", "content": "Error."})
            m["log"].append(f"T{turn} ERROR: {text[:100]}"); time.sleep(1); continue
        stored.append({"t": f"User: {msg} | Tem: {text[:200]}", "ts": datetime.now().strftime("%H:%M")})
        m["memories_created"] += 1
        history.append({"role": "assistant", "content": text})
        m["turns"] += 1
        m["log"].append(f"T{turn}: {text[:120]}{'...' if len(text)>120 else ''}")
        time.sleep(0.2)
    m["memory_count"] = len(stored)
    results_dict["current"] = m

# ── Strategy C: Naive ────────────────────────────────────────

def run_naive(results_dict):
    history = []; summary = ""
    m = {"name": "Naive Summary (GPT-5.2)", "turns": 0, "errors": 0, "in_tok": 0, "out_tok": 0,
         "memories_created": 0, "summaries_generated": 0, "log": []}
    for i, msg in enumerate(ALL_TURNS):
        turn = i + 1
        if turn > 1 and (turn - 1) % 10 == 0 and history:
            sh = history[-20:] + [{"role": "user", "content":
                "Summarize ALL key facts, decisions, preferences, and requirements. Be exhaustive."}]
            st, it, ot = call_openai(sh, NAIVE_PROMPT)
            m["in_tok"] += it; m["out_tok"] += ot
            if not st.startswith("ERROR:"): summary = st; m["summaries_generated"] += 1
            time.sleep(0.2)
        prompt = NAIVE_PROMPT + (f"\n\nConversation summary:\n{summary}" if summary else "")
        history.append({"role": "user", "content": msg})
        text, it, ot = call_openai(history[-30:], prompt)
        m["in_tok"] += it; m["out_tok"] += ot
        if text.startswith("ERROR:"):
            m["errors"] += 1; history.append({"role": "assistant", "content": "Error."})
            m["log"].append(f"T{turn} ERROR: {text[:100]}"); time.sleep(1); continue
        history.append({"role": "assistant", "content": text})
        m["turns"] += 1; m["memories_created"] += 1
        m["log"].append(f"T{turn}: {text[:120]}{'...' if len(text)>120 else ''}")
        time.sleep(0.2)
    m["memory_count"] = m["summaries_generated"]
    results_dict["naive"] = m

# ── Scoring ──────────────────────────────────────────────────

RUBRIC = [
    (51,['snake_case'],[]), (52,['thiserror'],['anyhow']),
    (53,['never','no',"don't",'avoid','should not'],['yes','acceptable']),
    (54,['tracing'],[]), (55,['blue-green','blue green'],[]),
    (56,['reversible','yes','always'],[]), (57,['clippy','-D warnings','-d warnings'],[]),
    (58,['5-second','5 second','5s','five','5 sec','5-sec'],[]),
    (59,['composition'],[]), (60,['gateway'],[]),
    (61,['request-id','request_id','requestid'],[]),
    (62,['tracing','structured','spans'],[]),
    (63,['sqlite','postgres'],[]), (64,['20'],[]),
    (65,['sqlx','compile'],[]), (66,['sanitize','traversal'],[]),
    (67,['jwt','refresh'],[]), (68,['permissive','restrictive'],[]),
    (69,['never','no',"don't",'should not'],['yes']),
    (70,['snake_case','thiserror','tracing','unwrap'],[]),
    (71,['axum'],['actix']), (72,['match'],[]),
    (73,['debug','clone','serialize'],[]),
    (74,['frustrated','verbose','concise','reduce'],[]),
    (75,[],[]), (76,[],[]),
    (77,['snake_case','thiserror','axum','tracing'],[]),
    (78,['axum','sqlx','thiserror','tracing'],[]),
    (79,[],[]), (80,[],[]),
    (81,['hi','testing','memory'],['error']),
    (82,['sqlite','postgres','5','20'],[]),
    (83,['sanitize','traversal','internal error'],[]),
    (84,['request-id','error','version'],[]),
    (85,['builder','ownership','match'],[]),
    (86,['no',"haven't",'consistent'],[]),
    (87,['axum','sqlx','thiserror'],[]),
    (88,['test','mock','async'],[]),
    (89,[],[]), (90,[],[]), (91,[],[]),
    (92,['sqlite','postgres','blue-green','pool'],[]),
    (93,['thiserror','result','unwrap'],[]),
    (94,['tower','axum'],[]), (95,['async'],[]),
    (96,['arc','weak','cycle'],[]),
    (97,[],[]), (98,[],[]), (99,[],[]), (100,[],[]),
]

def score_turn(resp, turn, exp, anti):
    rl = resp.lower()
    if turn >= 99: return 1.0, 'farewell'
    if not exp: return (0.5, 'subj') if len(resp) > 20 else (0.0, 'empty')
    has_e = any(k.lower() in rl for k in exp)
    has_a = any(k.lower() in rl for k in anti) if anti else False
    amnesia_phrases = ["haven't specified","not specified","haven't mentioned","not mentioned",
        "not covered","haven't stated","not stated","haven't chosen","haven't set",
        "not discussed","sorry","cannot fulfill","don't have"]
    if any(p in rl for p in amnesia_phrases) and not has_e: return 0.0, 'amnesia'
    if has_e and not has_a: return 1.0, 'correct'
    if has_e: return 0.5, 'partial'
    if has_a: return -0.5, 'halluc'
    return (0.25, 'vague') if len(resp) > 30 else (0.0, 'wrong')

def score_strategy(logs):
    results = []
    for turn, exp, anti in RUBRIC:
        resp = ""
        prefix = f"T{turn}:"
        for l in logs:
            if l.startswith(prefix): resp = l[len(prefix):].strip(); break
        s, reason = score_turn(resp, turn, exp, anti)
        results.append((turn, s, reason, resp[:80]))
    return results

# ── Main ─────────────────────────────────────────────────────

def main():
    if not API_KEY: print("Set OPENAI_API_KEY"); sys.exit(1)
    print(f"═══ λ-Memory 3-Way Benchmark — GPT-5.2 ═══")
    print(f"Turns: {len(ALL_TURNS)} × 3 = {len(ALL_TURNS)*3}")
    print(f"Running 3 strategies in PARALLEL...\n")

    manager = Manager(); results = manager.dict()
    start = time.time()
    procs = [
        Process(target=run_lambda, args=(results,), name="λ-Memory"),
        Process(target=run_current, args=(results,), name="Current"),
        Process(target=run_naive, args=(results,), name="Naive"),
    ]
    for p in procs: p.start(); print(f"  Started: {p.name}")
    for p in procs: p.join(); print(f"  Done: {p.name}")
    elapsed = time.time() - start
    print(f"\nAll complete in {elapsed:.0f}s ({elapsed/60:.1f}min)\n")

    r = {k: dict(v) for k, v in results.items()}
    lam = r.get("lambda",{}); cur = r.get("current",{}); nav = r.get("naive",{})

    # Score
    scores = {}
    for key in ["lambda","current","naive"]:
        sc = score_strategy(r.get(key,{}).get("log",[]))
        total = sum(s for _,s,_,_ in sc)
        correct = sum(1 for _,s,_,_ in sc if s >= 1.0)
        partial = sum(1 for _,s,_,_ in sc if 0 < s < 1.0)
        wrong = sum(1 for _,s,_,_ in sc if s == 0.0)
        halluc = sum(1 for _,s,_,_ in sc if s < 0)
        scores[key] = {"total": total, "correct": correct, "partial": partial,
                       "wrong": wrong, "halluc": halluc, "pct": total/50*100, "details": sc}

    # Write report
    rpt = BASE_DIR / "LAMBDA_BENCH_GPT52_REPORT.md"
    met = BASE_DIR / "lambda_bench_gpt52_metrics.json"

    with open(rpt, "w") as f:
        def w(s=""): f.write(s+"\n"); print(s)
        w("# λ-Memory 3-Way Benchmark — GPT-5.2")
        w()
        w(f"> **Date:** {datetime.now().strftime('%Y-%m-%d %H:%M')}")
        w(f"> **Model:** {MODEL}")
        w(f"> **Author:** TEMM1E's Lab")
        w(f"> **Elapsed:** {elapsed:.0f}s ({elapsed/60:.1f}min)")
        w()
        w("---")
        w()
        w("## Results")
        w()
        w("| Metric | λ-Memory | Current | Naive |")
        w("|--------|----------|---------|-------|")
        w(f"| Turns | {lam.get('turns',0)} | {cur.get('turns',0)} | {nav.get('turns',0)} |")
        w(f"| Errors | {lam.get('errors',0)} | {cur.get('errors',0)} | {nav.get('errors',0)} |")
        w(f"| Memories | {lam.get('memories_created',0)} ({lam.get('auto_generated',0)} auto) | {cur.get('memories_created',0)} | {nav.get('summaries_generated',0)} summaries |")
        lt=lam.get('in_tok',0)+lam.get('out_tok',0)
        ct=cur.get('in_tok',0)+cur.get('out_tok',0)
        nt=nav.get('in_tok',0)+nav.get('out_tok',0)
        w(f"| Total tokens | {lt:,} | {ct:,} | {nt:,} |")
        if ct>0: w(f"| vs Current | {((lt-ct)/ct*100):+.1f}% | baseline | {((nt-ct)/ct*100):+.1f}% |")
        w()
        w("## Effectiveness Scores (Recall Phase: Turns 51-100)")
        w()
        w("| Metric | λ-Memory | Current | Naive |")
        w("|--------|----------|---------|-------|")
        sl=scores["lambda"];sc_=scores["current"];sn=scores["naive"]
        w(f"| **Score** | **{sl['total']:.1f}/50** | **{sc_['total']:.1f}/50** | **{sn['total']:.1f}/50** |")
        w(f"| **Accuracy** | **{sl['pct']:.1f}%** | **{sc_['pct']:.1f}%** | **{sn['pct']:.1f}%** |")
        w(f"| Correct | {sl['correct']} | {sc_['correct']} | {sn['correct']} |")
        w(f"| Partial | {sl['partial']} | {sc_['partial']} | {sn['partial']} |")
        w(f"| Wrong | {sl['wrong']} | {sc_['wrong']} | {sn['wrong']} |")
        w(f"| Hallucinated | {sl['halluc']} | {sc_['halluc']} | {sn['halluc']} |")
        w()
        w("## Efficiency")
        w()
        w("| Metric | λ-Memory | Current | Naive |")
        w("|--------|----------|---------|-------|")
        for key,label in [("lambda","λ-Memory"),("current","Current"),("naive","Naive")]:
            tok = r.get(key,{}).get('in_tok',0)+r.get(key,{}).get('out_tok',0)
            sc2 = scores[key]['total']
            eff = sc2/(tok/1000) if tok>0 else 0
        w(f"| Score/1K tokens | {scores['lambda']['total']/(lt/1000):.3f} | {scores['current']['total']/(ct/1000):.3f} | {scores['naive']['total']/(nt/1000):.3f} |")
        w()

        # Gemini comparison
        w("## Cross-Model Comparison (Gemini v1 vs GPT-5.2)")
        w()
        w("| Metric | Gemini λ-Mem | GPT-5.2 λ-Mem | Gemini Current | GPT-5.2 Current |")
        w("|--------|-------------|---------------|----------------|-----------------|")
        w(f"| Recall accuracy | 67.0% | {sl['pct']:.1f}% | 76.0% | {sc_['pct']:.1f}% |")
        w(f"| Correct answers | 26/50 | {sl['correct']}/50 | 30/50 | {sc_['correct']}/50 |")
        w(f"| Wrong (amnesia) | 4 | {sl['wrong']} | 0 | {sc_['wrong']} |")
        w(f"| Total tokens | 172,984 | {lt:,} | 76,821 | {ct:,} |")
        w()

        # Logs
        for label, key in [("λ-Memory","lambda"),("Current","current"),("Naive","naive")]:
            w(f"## {label} — Log")
            w()
            w("```")
            for line in r.get(key,{}).get("log",[]): w(line)
            w("```")
            w()

            # Wrong answers
            wrongs = [(t,reason,resp) for t,s,reason,resp in scores[key]["details"] if s <= 0]
            if wrongs:
                w(f"### {label} — Wrong Answers")
                w()
                for t,reason,resp in wrongs:
                    w(f"- **T{t}** [{reason}]: {resp}")
                w()

        w("---")
        w("*TEMM1E's Lab*")

    # Save metrics
    out = {}
    for k,v in r.items():
        d = dict(v); d.pop("log",None)
        d["recall_score"] = scores[k]["total"]
        d["recall_pct"] = scores[k]["pct"]
        d["correct"] = scores[k]["correct"]
        d["wrong"] = scores[k]["wrong"]
        out[k] = d
    out["meta"] = {"date": datetime.now().isoformat(), "model": MODEL,
                   "turns": len(ALL_TURNS), "elapsed": round(elapsed)}
    with open(met, "w") as f: json.dump(out, f, indent=2)

    print(f"\nReport: {rpt}")
    print(f"Metrics: {met}")

if __name__ == "__main__":
    main()
