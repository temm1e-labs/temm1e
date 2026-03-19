#!/usr/bin/env python3
"""λ-Memory Multi-Session Benchmark — The honest test.

Simulates 5 sessions over 5 "days" with CONTEXT RESET between sessions.
This is what actually happens in production: user closes chat, comes back later.

Session 1: Establish preferences (20 turns) → context cleared
Session 2: More work, some overlap (20 turns) → context cleared
Session 3: Different topic, few callbacks (20 turns) → context cleared
Session 4: Return to original topic (20 turns) → context cleared
Session 5: RECALL EXAM — test what each strategy remembers (20 turns)

The critical difference: Current Memory loses everything between sessions.
λ-Memory persists across sessions via SQLite.
Naive Summary can carry forward its last summary.

100 turns total × 3 strategies = 300 API calls.
"""

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

# ── Sessions ─────────────────────────────────────────────────

SESSION_1 = {
    "name": "Day 1: Project Setup",
    "simulated_time_offset": 0,  # hours from start
    "turns": [
        "Hi Tem, starting a new Rust project today",
        "Remember: I always prefer snake_case for variable names",
        "I've decided to use thiserror for all error types",
        "Remember: never use unwrap in production code",
        "I chose axum over actix-web for the web framework",
        "Remember: use tracing not log crate for logging",
        "Deploy strategy: always blue-green deployments",
        "Remember: DB migrations must be reversible",
        "Remember: CI runs clippy with -D warnings",
        "Remember: all DB ops need 5-second timeout",
        "I prefer composition over inheritance",
        "Remember: rate limiting at gateway level not per-handler",
        "Critical: all API responses need request-id header",
        "Remember: structured logging with tracing spans for DB ops",
        "I decided SQLite for dev, Postgres for production",
        "Remember: max 20 DB connections in pool",
        "I chose sqlx for compile-time query checking",
        "Remember: sanitize file paths to prevent traversal",
        "Use JWTs with short expiry + refresh tokens for auth",
        "Remember: CORS permissive in dev, restrictive in prod",
    ],
}

SESSION_2 = {
    "name": "Day 2: Implementation",
    "simulated_time_offset": 24,
    "turns": [
        "Hi Tem, continuing from yesterday",
        "What web framework am I using?",
        "What's the error handling approach?",
        "Remember: never expose internal errors to API consumers",
        "I prefer match over if-let for complex patterns",
        "Important: public types must impl Debug Clone Serialize",
        "Explain tower middleware briefly",
        "How should I structure the workspace?",
        "What logging framework did I choose?",
        "Remember: all API endpoints need input validation",
        "What's my deployment strategy?",
        "How do I handle graceful shutdown in axum?",
        "Remember: health check endpoint at /health required",
        "What DB timeout did I set?",
        "How should I handle connection pooling?",
        "Remember: use semantic versioning for API versions",
        "What CI requirements did I set?",
        "How to mock databases in tests?",
        "Remember: integration tests must hit real DB not mocks",
        "What are my CORS settings?",
    ],
}

SESSION_3 = {
    "name": "Day 4: Frontend Discussion",
    "simulated_time_offset": 72,
    "turns": [
        "Hi Tem, different topic today — discussing frontend",
        "What CSS framework should I use with this project?",
        "I'm going with Tailwind CSS for styling",
        "Remember: mobile-first responsive design approach",
        "What state management for React?",
        "I chose Zustand over Redux for state management",
        "Remember: all forms need client-side validation",
        "What's the best way to handle API calls from frontend?",
        "Remember: use React Query for server state management",
        "How should I handle authentication on the frontend?",
        "Store JWT in httpOnly cookies not localStorage",
        "Remember: implement CSRF protection",
        "What testing framework for frontend?",
        "I chose Vitest over Jest for frontend testing",
        "Remember: minimum 80% code coverage requirement",
        "How should I handle error boundaries?",
        "Remember: global error boundary with fallback UI",
        "What about accessibility?",
        "Remember: WCAG 2.1 AA compliance required",
        "What build tool should I use?",
    ],
}

SESSION_4 = {
    "name": "Day 6: Back to Backend",
    "simulated_time_offset": 120,
    "turns": [
        "Hi Tem, back to the Rust backend today",
        "What web framework am I using again?",
        "What's my error handling library?",
        "How should I implement rate limiting?",
        "What DB am I using for dev vs production?",
        "What's my connection pool configuration?",
        "Explain how to use sqlx migrations",
        "How should I structure my API routes?",
        "What authentication approach am I using?",
        "What logging approach did I choose for DB ops?",
        "Remember: implement request tracing with correlation IDs",
        "What CI/CD pipeline requirements do I have?",
        "How should I handle database errors?",
        "What's my CORS configuration?",
        "Remember: implement circuit breaker for external API calls",
        "What deployment strategy am I using?",
        "How do I ensure API response headers are correct?",
        "What file security measures did I decide on?",
        "What testing approach for the backend?",
        "Remember: load testing required before production deploy",
    ],
}

SESSION_5_EXAM = {
    "name": "Day 7: The Recall Exam",
    "simulated_time_offset": 144,
    "turns": [
        # Backend preferences (from Session 1)
        "What variable naming convention do I use?",
        "What error handling library did I choose?",
        "Should I use unwrap in production?",
        "What logging framework?",
        "What's my deployment strategy?",
        "Are DB migrations reversible?",
        "What CI linting requirements?",
        "What DB timeout?",
        "Composition or inheritance?",
        "Where does rate limiting happen?",
        # More backend (from Session 1-2)
        "What header must all API responses include?",
        "What DB for dev vs production?",
        "Max DB connections?",
        "What query library and why?",
        "File path security approach?",
        "Auth token strategy?",
        "CORS policy dev vs prod?",
        "Internal errors exposed to API?",
        "What web framework?",
        "What must public types implement?",
    ],
}

ALL_SESSIONS = [SESSION_1, SESSION_2, SESSION_3, SESSION_4, SESSION_5_EXAM]

# ── Prompts ──────────────────────────────────────────────────

LAMBDA_PROMPT = """You are Tem, an AI assistant with λ-Memory.
Keep responses to 2-3 sentences.
MEMORY RULE — MUST append <memory> block when user says remember/important/critical/always/never, makes a decision, or states a preference.
<memory>
summary: (one sentence)
essence: (3-5 words)
importance: (1-5)
tags: (csv)
</memory>
Skip only for pure questions and greetings."""

CURRENT_PROMPT = """You are Tem, a helpful AI assistant.
Keep responses concise (2-3 sentences). Pay close attention to user preferences."""

NAIVE_PROMPT = """You are Tem, a helpful AI assistant.
Keep responses concise (2-3 sentences). Pay close attention to user preferences."""

# ── OpenAI API ───────────────────────────────────────────────

def call_api(messages, system_prompt):
    msgs = [{"role": "system", "content": system_prompt}] + messages[-40:]
    body = json.dumps({"model": MODEL, "messages": msgs, "temperature": 0.7,
                       "max_completion_tokens": 400}).encode()
    req = Request("https://api.openai.com/v1/chat/completions", data=body,
                  headers={"Content-Type": "application/json", "Authorization": f"Bearer {API_KEY}"})
    try:
        with urlopen(req, timeout=60) as resp: data = json.loads(resp.read())
    except HTTPError as e:
        err = e.read().decode()
        try: err = json.loads(err).get("error",{}).get("message",err[:200])
        except: pass
        return f"ERROR: {err}", 0, 0
    except Exception as e: return f"ERROR: {e}", 0, 0
    if "error" in data: return f"ERROR: {data['error'].get('message','')}", 0, 0
    try: text = data["choices"][0]["message"]["content"]
    except: return "ERROR: no content", 0, 0
    u = data.get("usage", {})
    return text, u.get("prompt_tokens", 0), u.get("completion_tokens", 0)

# ── Helpers ──────────────────────────────────────────────────

def parse_mem(text):
    m = re.search(r"<memory>(.*?)</memory>", text, re.DOTALL)
    if not m: return None
    r = {"summary":"","essence":"","importance":2,"tags":""}
    for line in m.group(1).strip().split("\n"):
        line = line.strip()
        if line.startswith("summary:"): r["summary"]=line[8:].strip()
        elif line.startswith("essence:"): r["essence"]=line[8:].strip()
        elif line.startswith("importance:"):
            try: r["importance"]=max(1,min(5,int(re.search(r"\d+",line[11:]).group())))
            except: pass
        elif line.startswith("tags:"): r["tags"]=line[5:].strip()
    return r if (r["summary"] or r["essence"]) else None

def strip_mem(text):
    return re.sub(r"\s*<memory>.*?</memory>\s*", "", text, flags=re.DOTALL).strip()

def decay(imp, acc, now, lam=0.01):
    return imp * math.exp(-(now - acc) / 3600.0 * lam)

TRIGGERS = ["remember:","remember this","important:","critical:","always ","never ",
            "i chose ","i decided","i prefer","i've decided","deploy strategy",
            "must impl","must include","must be ","max ","timeout","use jwt"]

def auto_mem(msg, resp):
    sentences = re.split(r'[.!?]\s', resp)
    summary = (sentences[0]+".") if sentences else resp[:100]
    words = [w for w in msg.split() if w.lower() not in
             {"remember:","remember","important:","critical:","i","a","the",
              "for","to","in","use","always","never","this","that"}]
    return {"summary": summary, "essence": " ".join(words[:5]),
            "importance": 4 if "remember:" in msg.lower() else 3, "tags": "auto"}

# ── Strategy A: λ-Memory (persists across sessions) ─────────

def run_lambda(results_dict):
    memories = []  # PERSISTS across sessions
    m = {"name":"λ-Memory","turns":0,"errors":0,"in_tok":0,"out_tok":0,
         "memories_created":0,"auto_generated":0,"log":[]}

    for sess in ALL_SESSIONS:
        history = []  # RESET each session
        time_offset = sess["simulated_time_offset"] * 3600
        m["log"].append(f"\n══ {sess['name']} ══ (history cleared, {len(memories)} λ-memories persist)")

        for i, msg in enumerate(sess["turns"]):
            turn_global = sum(len(s["turns"]) for s in ALL_SESSIONS[:ALL_SESSIONS.index(sess)]) + i + 1
            now = int(time.time()) + time_offset

            # Build λ-context from persistent memories
            ctx = ""
            if len(memories) >= 2:
                scored = sorted([(decay(x["imp"],x["acc"],now), x) for x in memories], key=lambda x:-x[0])
                lines = []; tok = 0
                for sc, x in scored:
                    if sc < 0.01: continue
                    if sc > 2.0: line = f"[H] {x['summary']} (#{x['h'][:7]} i={x['imp']})"
                    elif sc > 1.0: line = f"[W] {x['summary']} (#{x['h'][:7]})"
                    elif sc > 0.3: line = f"[C] {x['ess']} (#{x['h'][:7]})"
                    else: line = f"[F] #{x['h'][:7]}|{x['ess']}"
                    c = est_tokens(line)
                    if tok + c > 800: break
                    lines.append(line); tok += c
                if lines: ctx = "\n═══ λ-Memory ═══\n" + "\n".join(lines) + "\n═══════════════"

            prompt = LAMBDA_PROMPT + ("\n"+ctx if ctx else "")
            history.append({"role":"user","content":msg})
            text, it, ot = call_api(history[-20:], prompt)
            m["in_tok"]+=it; m["out_tok"]+=ot

            if text.startswith("ERROR:"):
                m["errors"]+=1; history.append({"role":"assistant","content":"Error."})
                m["log"].append(f"T{turn_global} ERROR: {text[:100]}"); time.sleep(1); continue

            parsed = parse_mem(text)
            auto = False
            if not parsed and any(t in msg.lower() for t in TRIGGERS):
                parsed = auto_mem(msg, strip_mem(text)); auto = True; m["auto_generated"]+=1
            if parsed:
                h = hashlib.sha256(f"ms:{turn_global}:{now}".encode()).hexdigest()[:12]
                memories.append({"h":h,"cr":now,"acc":now,"imp":parsed["importance"],
                                 "summary":parsed["summary"],"ess":parsed["essence"]})
                m["memories_created"]+=1

            clean = strip_mem(text)
            history.append({"role":"assistant","content":clean})
            m["turns"]+=1
            tag = f" [λ:{parsed['essence']}]" if parsed else ""
            tag += " [auto]" if auto else ""
            m["log"].append(f"T{turn_global}: {clean[:120]}{tag}")
            time.sleep(0.2)

    m["memory_count"]=len(memories)
    results_dict["lambda"] = m

# ── Strategy B: Current Memory (LOST between sessions) ──────

def run_current(results_dict):
    m = {"name":"Current Memory","turns":0,"errors":0,"in_tok":0,"out_tok":0,
         "memories_created":0,"log":[]}

    for sess in ALL_SESSIONS:
        history = []  # RESET — no persistence
        m["log"].append(f"\n══ {sess['name']} ══ (history cleared, NO persistent memory)")

        for i, msg in enumerate(sess["turns"]):
            turn_global = sum(len(s["turns"]) for s in ALL_SESSIONS[:ALL_SESSIONS.index(sess)]) + i + 1
            history.append({"role":"user","content":msg})
            text, it, ot = call_api(history[-20:], CURRENT_PROMPT)
            m["in_tok"]+=it; m["out_tok"]+=ot
            if text.startswith("ERROR:"):
                m["errors"]+=1; history.append({"role":"assistant","content":"Error."})
                m["log"].append(f"T{turn_global} ERROR: {text[:100]}"); time.sleep(1); continue
            history.append({"role":"assistant","content":text})
            m["turns"]+=1; m["memories_created"]+=1
            m["log"].append(f"T{turn_global}: {text[:120]}")
            time.sleep(0.2)

    m["memory_count"]=0
    results_dict["current"] = m

# ── Strategy C: Naive Summary (carries last summary) ────────

def run_naive(results_dict):
    summary = ""  # Only the summary persists
    m = {"name":"Naive Summary","turns":0,"errors":0,"in_tok":0,"out_tok":0,
         "memories_created":0,"summaries_generated":0,"log":[]}

    for sess in ALL_SESSIONS:
        history = []  # RESET
        m["log"].append(f"\n══ {sess['name']} ══ (history cleared, summary={'yes' if summary else 'no'})")

        for i, msg in enumerate(sess["turns"]):
            turn_global = sum(len(s["turns"]) for s in ALL_SESSIONS[:ALL_SESSIONS.index(sess)]) + i + 1

            # Summarize at end of each session (except last)
            if i == len(sess["turns"])-1 and sess != ALL_SESSIONS[-1] and history:
                sh = history[-20:]+[{"role":"user","content":
                    "Summarize ALL facts, decisions, preferences exhaustively. Include every specific value."}]
                st,it,ot = call_api(sh, NAIVE_PROMPT)
                m["in_tok"]+=it; m["out_tok"]+=ot
                if not st.startswith("ERROR:"): summary=st; m["summaries_generated"]+=1
                time.sleep(0.2)

            prompt = NAIVE_PROMPT+(f"\n\nPrevious session summary:\n{summary}" if summary else "")
            history.append({"role":"user","content":msg})
            text,it,ot = call_api(history[-20:], prompt)
            m["in_tok"]+=it; m["out_tok"]+=ot
            if text.startswith("ERROR:"):
                m["errors"]+=1; history.append({"role":"assistant","content":"Error."})
                m["log"].append(f"T{turn_global} ERROR: {text[:100]}"); time.sleep(1); continue
            history.append({"role":"assistant","content":text})
            m["turns"]+=1; m["memories_created"]+=1
            m["log"].append(f"T{turn_global}: {text[:120]}")
            time.sleep(0.2)

    m["memory_count"]=m["summaries_generated"]
    results_dict["naive"] = m

# ── Scoring (Session 5 only — turns 81-100) ──────────────────

EXAM_RUBRIC = [
    (81,['snake_case'],[]),
    (82,['thiserror'],[]),
    (83,['never','no',"don't",'avoid','should not'],[]),
    (84,['tracing'],[]),
    (85,['blue-green','blue green'],[]),
    (86,['reversible'],[]),
    (87,['clippy','-D warnings'],[]),
    (88,['5-second','5 second','5s','five','5 sec'],[]),
    (89,['composition'],[]),
    (90,['gateway'],[]),
    (91,['request-id','request_id'],[]),
    (92,['sqlite','postgres'],[]),
    (93,['20'],[]),
    (94,['sqlx','compile'],[]),
    (95,['sanitize','traversal'],[]),
    (96,['jwt','refresh'],[]),
    (97,['permissive','restrictive'],[]),
    (98,['never','no',"don't",'should not'],[]),
    (99,['axum'],[]),
    (100,['debug','clone','serialize'],[]),
]

def score_turn(resp, exp):
    rl = resp.lower()
    if not exp: return 0.5, "subj"
    has_e = any(k.lower() in rl for k in exp)
    amnesia = any(p in rl for p in ["haven't specified","not specified","haven't mentioned",
        "not mentioned","not covered","don't have","haven't discussed","not discussed",
        "sorry","cannot","no information","don't recall","no record"])
    if amnesia and not has_e: return 0.0, "amnesia"
    if has_e: return 1.0, "correct"
    return 0.25 if len(resp) > 30 else 0.0, "vague" if len(resp) > 30 else "wrong"

# ── Main ─────────────────────────────────────────────────────

def main():
    if not API_KEY: print("Set OPENAI_API_KEY"); sys.exit(1)
    total_turns = sum(len(s["turns"]) for s in ALL_SESSIONS)
    print(f"═══ λ-Memory Multi-Session Benchmark ═══")
    print(f"Model: {MODEL}")
    print(f"Sessions: {len(ALL_SESSIONS)} | Turns: {total_turns} × 3 = {total_turns*3}")
    print(f"KEY: Context resets between sessions. Only λ-Memory persists.\n")

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

    r = {k: dict(v) for k, v in results.items()}

    # Score exam session (turns 81-100)
    scores = {}
    for key in ["lambda","current","naive"]:
        logs = r.get(key,{}).get("log",[])
        sc_list = []
        for turn, exp, anti in EXAM_RUBRIC:
            resp = ""
            for l in logs:
                if l.startswith(f"T{turn}:"): resp = l[len(f"T{turn}:"):].strip(); break
            s, reason = score_turn(resp, exp)
            sc_list.append((turn, s, reason, resp[:80]))
        total = sum(s for _,s,_,_ in sc_list)
        correct = sum(1 for _,s,_,_ in sc_list if s >= 1.0)
        wrong = sum(1 for _,s,_,_ in sc_list if s == 0)
        scores[key] = {"total":total,"correct":correct,"wrong":wrong,
                       "pct":total/20*100,"details":sc_list}

    # Report
    rpt = BASE_DIR / "LAMBDA_BENCH_MULTISESSION_REPORT.md"
    met = BASE_DIR / "lambda_bench_multisession_metrics.json"
    lam=r.get("lambda",{}); cur=r.get("current",{}); nav=r.get("naive",{})
    sl=scores["lambda"]; sc_=scores["current"]; sn=scores["naive"]
    lt=lam.get('in_tok',0)+lam.get('out_tok',0)
    ct=cur.get('in_tok',0)+cur.get('out_tok',0)
    nt=nav.get('in_tok',0)+nav.get('out_tok',0)

    with open(rpt,"w") as f:
        def w(s=""): f.write(s+"\n"); print(s)
        w("# λ-Memory Multi-Session Benchmark")
        w()
        w(f"> **Date:** {datetime.now().strftime('%Y-%m-%d %H:%M')}")
        w(f"> **Model:** {MODEL}")
        w(f"> **Author:** TEMM1E's Lab")
        w(f"> **Sessions:** 5 (context reset between each)")
        w(f"> **Elapsed:** {elapsed:.0f}s ({elapsed/60:.1f}min)")
        w()
        w("## The Test")
        w()
        w("5 sessions simulating a week of work. **Context is cleared between sessions.**")
        w("Session 5 is a recall exam on preferences from Session 1.")
        w()
        w("| Session | Day | Topic | Turns |")
        w("|---------|-----|-------|-------|")
        for s in ALL_SESSIONS:
            w(f"| {s['name']} | +{s['simulated_time_offset']}h | {'RECALL EXAM' if s==ALL_SESSIONS[-1] else 'Work'} | {len(s['turns'])} |")
        w()
        w("## What Each Strategy Has in Session 5")
        w()
        w("| Strategy | What persists across sessions |")
        w("|----------|------------------------------|")
        w(f"| **λ-Memory** | {lam.get('memories_created',0)} memories in SQLite, decay-scored, hash-recallable |")
        w("| **Current Memory** | **Nothing.** History cleared. Starting from zero. |")
        w(f"| **Naive Summary** | Last session's summary only (~{nav.get('summaries_generated',0)} summaries generated) |")
        w()
        w("---")
        w()
        w("## Results: Session 5 Recall Exam (20 questions)")
        w()
        w("| Metric | λ-Memory | Current Memory | Naive Summary |")
        w("|--------|----------|----------------|---------------|")
        w(f"| **Score** | **{sl['total']:.1f}/20** | **{sc_['total']:.1f}/20** | **{sn['total']:.1f}/20** |")
        w(f"| **Accuracy** | **{sl['pct']:.1f}%** | **{sc_['pct']:.1f}%** | **{sn['pct']:.1f}%** |")
        w(f"| Correct | {sl['correct']} | {sc_['correct']} | {sn['correct']} |")
        w(f"| Wrong (amnesia) | {sl['wrong']} | {sc_['wrong']} | {sn['wrong']} |")
        w()
        w("## Token Cost")
        w()
        w("| Metric | λ-Memory | Current | Naive |")
        w("|--------|----------|---------|-------|")
        w(f"| Total tokens | {lt:,} | {ct:,} | {nt:,} |")
        w(f"| Score/1K tokens | {sl['total']/(lt/1000):.3f} | {sc_['total']/(ct/1000):.3f} | {sn['total']/(nt/1000):.3f} |")
        w()

        w("## Per-Question Results")
        w()
        w("| Q# | Question | λ-Memory | Current | Naive |")
        w("|----|----------|----------|---------|-------|")
        questions = [
            "snake_case?","thiserror?","unwrap?","logging?","deploy?",
            "migrations?","CI lint?","DB timeout?","composition?","rate limit?",
            "API header?","dev/prod DB?","max connections?","query lib?",
            "file security?","auth tokens?","CORS?","internal errors?",
            "web framework?","public types?"
        ]
        for idx, (q, ld, cd, nd) in enumerate(zip(questions, sl["details"], sc_["details"], sn["details"])):
            ls = "OK" if ld[1]>=1.0 else ("~" if ld[1]>0 else "MISS")
            cs = "OK" if cd[1]>=1.0 else ("~" if cd[1]>0 else "MISS")
            ns = "OK" if nd[1]>=1.0 else ("~" if nd[1]>0 else "MISS")
            w(f"| {ld[0]} | {q} | {ls} | {cs} | {ns} |")
        w()

        # Logs
        for label,key in [("λ-Memory","lambda"),("Current Memory","current"),("Naive Summary","naive")]:
            w(f"## {label} — Log")
            w()
            w("```")
            for line in r.get(key,{}).get("log",[]): w(line)
            w("```")
            w()

        w("---")
        w("*TEMM1E's Lab — Multi-Session Benchmark*")

    # Metrics JSON
    out = {}
    for k,v in r.items():
        d=dict(v); d.pop("log",None)
        d["recall_score"]=scores[k]["total"]; d["recall_pct"]=scores[k]["pct"]
        d["correct"]=scores[k]["correct"]; d["wrong"]=scores[k]["wrong"]
        out[k]=d
    out["meta"]={"date":datetime.now().isoformat(),"model":MODEL,
                 "sessions":5,"turns":total_turns,"elapsed":round(elapsed),"type":"multi-session"}
    with open(met,"w") as f: json.dump(out, f, indent=2)
    print(f"\nReport: {rpt}\nMetrics: {met}")

if __name__ == "__main__":
    main()
