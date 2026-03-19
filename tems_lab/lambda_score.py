#!/usr/bin/env python3
"""λ-Memory Effectiveness Scorer — grades recall accuracy from benchmark logs.

Turns 51-100 are recall questions with known correct answers.
Each response is scored: CORRECT (1), PARTIAL (0.5), WRONG (0), or HALLUCINATED (-0.5).
"""

import re, json
from pathlib import Path

REPORT_PATH = Path(__file__).parent / "LAMBDA_BENCH_REPORT.md"
METRICS_PATH = Path(__file__).parent / "lambda_bench_metrics.json"
SCORE_REPORT_PATH = Path(__file__).parent / "LAMBDA_EFFECTIVENESS_REPORT.md"

# Known correct answers for recall turns (51-100)
# Format: (turn_number, question, expected_keywords, anti_keywords)
# expected_keywords: ANY of these must appear for CORRECT
# anti_keywords: if these appear AND expected don't, it's HALLUCINATED
RECALL_RUBRIC = [
    (51, "Variable naming convention?", ["snake_case"], []),
    (52, "Error handling library?", ["thiserror"], ["anyhow"]),
    (53, "unwrap in production?", ["never", "no", "don't", "avoid", "should not"], ["yes", "acceptable"]),
    (54, "Logging framework?", ["tracing"], ["log crate"]),
    (55, "Deployment strategy?", ["blue-green", "blue green"], []),
    (56, "DB migrations reversible?", ["reversible", "yes", "always"], ["no", "not"]),
    (57, "CI linting?", ["clippy", "-D warnings", "-d warnings"], []),
    (58, "DB timeout?", ["5-second", "5 second", "5s", "five"], []),
    (59, "Composition or inheritance?", ["composition"], ["inheritance preferred"]),
    (60, "Rate limiting location?", ["gateway"], ["per-handler", "per handler", "endpoint"]),
    (61, "Required API header?", ["request-id", "request_id", "requestid"], []),
    (62, "Logging for DB ops?", ["tracing", "structured", "spans"], []),
    (63, "DB for dev vs prod?", ["sqlite", "postgres"], []),
    (64, "Max DB connections?", ["20"], []),
    (65, "Query library + why?", ["sqlx", "compile"], []),
    (66, "File path security?", ["sanitize", "traversal", "prevent"], []),
    (67, "Auth tokens?", ["jwt", "refresh"], []),
    (68, "CORS dev vs prod?", ["permissive", "restrictive"], []),
    (69, "Internal errors exposed?", ["never", "no", "don't", "should not"], ["yes"]),
    (70, "List all remember requests", ["snake_case", "thiserror", "tracing", "unwrap"], []),
    (71, "Web framework?", ["axum"], ["actix"]),
    (72, "Pattern matching style?", ["match"], ["if-let preferred"]),
    (73, "Public types must impl?", ["debug", "clone", "serialize"], []),
    (74, "Error handling verbosity?", ["frustrated", "verbose", "concise", "reduce"], []),
    (75, "Most critical decision?", [], []),  # Subjective — score as partial if reasonable
    (76, "How many remember requests?", [], []),  # Approximate — check if close
    (77, "ALL preferences list?", ["snake_case", "thiserror", "axum", "tracing"], []),
    (78, "Frameworks + libraries?", ["axum", "sqlx", "thiserror", "tracing"], []),
    (79, "Confidence 1-10?", [], []),  # Any number is fine
    (80, "Keep ONE preference?", [], []),  # Subjective
    (81, "First thing I said?", ["hi", "testing", "memory"], ["error", "library"]),
    (82, "Database config?", ["sqlite", "postgres", "5", "20"], []),
    (83, "Security summary?", ["sanitize", "traversal", "internal error"], []),
    (84, "API design summary?", ["request-id", "error", "version"], []),
    (85, "Patterns discussed?", ["builder", "ownership", "match"], []),
    (86, "Change mind?", ["no", "haven't", "consistent"], ["yes changed"]),
    (87, "Everything about project?", ["axum", "sqlx", "thiserror"], []),
    (88, "Testing philosophy?", ["test", "mock", "async"], []),
    (89, "Document first?", [], []),  # Subjective
    (90, "Most important thing?", [], []),  # Subjective
    (91, "Topics covered?", [], []),  # Check if number > 8
    (92, "Infrastructure decisions?", ["sqlite", "postgres", "blue-green", "pool"], []),
    (93, "Error handling full summary?", ["thiserror", "result", "unwrap"], []),
    (94, "Middleware approach?", ["tower", "axum"], []),
    (95, "Async or sync?", ["async"], ["sync preferred"]),
    (96, "Memory leaks?", ["arc", "weak", "cycle"], []),
    (97, "Grade A-F?", [], []),  # Any grade
    (98, "Benchmark start time?", ["13:45", "13:4"], []),  # Stored in turn 48
    (99, "Thank you response", [], []),  # Always correct
    (100, "Goodbye", [], []),  # Always correct
]


def extract_logs(report_text):
    """Extract per-strategy logs from the markdown report."""
    strategies = {}
    current = None
    in_code = False
    lines = []

    for line in report_text.split("\n"):
        if "## λ-Memory — Full Log" in line:
            current = "lambda"
            lines = []
        elif "## Current Memory — Full Log" in line:
            if current and lines:
                strategies[current] = lines
            current = "current"
            lines = []
        elif "## Naive Summary — Full Log" in line:
            if current and lines:
                strategies[current] = lines
            current = "naive"
            lines = []
        elif line.strip() == "```" and current:
            in_code = not in_code
            if not in_code and lines:
                strategies[current] = lines
                current = None
        elif in_code and current:
            lines.append(line)

    if current and lines:
        strategies[current] = lines

    return strategies


def get_turn_response(logs, turn_num):
    """Extract response text for a specific turn number."""
    prefix = f"T{turn_num}:"
    for line in logs:
        if line.startswith(prefix):
            return line[len(prefix):].strip()
    return ""


def score_response(response, rubric_entry):
    """Score a single response. Returns (score, reason)."""
    turn, question, expected, anti = rubric_entry
    resp_lower = response.lower()

    # Turns 99-100 are greetings — always correct
    if turn >= 99:
        return 1.0, "greeting/farewell"

    # Subjective questions (empty expected) — partial credit if reasonable
    if not expected:
        if len(response) > 20:
            return 0.5, "subjective (answered)"
        return 0.0, "subjective (no answer)"

    # Check for hallucination
    has_expected = any(kw.lower() in resp_lower for kw in expected)
    has_anti = any(kw.lower() in resp_lower for kw in anti) if anti else False

    # "haven't specified" / "not mentioned" / "not covered" = WRONG
    amnesia_phrases = ["haven't specified", "not specified", "haven't mentioned",
                       "not mentioned", "not covered", "haven't stated",
                       "not stated", "haven't chosen", "haven't set",
                       "haven't discussed", "not discussed", "sorry"]
    has_amnesia = any(p in resp_lower for p in amnesia_phrases)

    if has_amnesia and not has_expected:
        return 0.0, "amnesia (claimed not discussed)"

    if has_expected and not has_anti:
        return 1.0, "correct"

    if has_expected and has_anti:
        return 0.5, "partial (mixed signals)"

    if has_anti and not has_expected:
        return -0.5, "hallucinated"

    # No expected keywords found but also no anti-keywords
    if len(response) > 30:
        return 0.25, "vague (related but missing key terms)"
    return 0.0, "wrong"


def main():
    report = REPORT_PATH.read_text()
    strategies = extract_logs(report)

    if not strategies:
        print("ERROR: Could not parse benchmark report")
        return

    results = {}

    for name, logs in strategies.items():
        scores = []
        details = []
        for rubric in RECALL_RUBRIC:
            turn = rubric[0]
            response = get_turn_response(logs, turn)
            score, reason = score_response(response, rubric)
            scores.append(score)
            details.append({
                "turn": turn,
                "question": rubric[1],
                "score": score,
                "reason": reason,
                "response_preview": response[:80],
            })

        total = sum(s for s in scores)
        max_possible = len(scores)  # 1.0 per question
        correct = sum(1 for s in scores if s >= 1.0)
        partial = sum(1 for s in scores if 0 < s < 1.0)
        wrong = sum(1 for s in scores if s == 0.0)
        hallucinated = sum(1 for s in scores if s < 0)
        pct = (total / max_possible) * 100 if max_possible > 0 else 0

        results[name] = {
            "total_score": round(total, 1),
            "max_possible": max_possible,
            "percentage": round(pct, 1),
            "correct": correct,
            "partial": partial,
            "wrong": wrong,
            "hallucinated": hallucinated,
            "details": details,
        }

    # ── Write report ─────────────────────────────────────────
    with open(SCORE_REPORT_PATH, "w") as f:
        def w(s=""): f.write(s + "\n"); print(s)

        w("# λ-Memory Effectiveness Report")
        w()
        w("> Scoring recall accuracy across 50 recall questions (turns 51-100).")
        w(f"> **Author:** TEMM1E's Lab")
        w(f"> **Scoring:** CORRECT=1.0 | PARTIAL=0.5 | VAGUE=0.25 | WRONG=0.0 | HALLUCINATED=-0.5")
        w()
        w("---")
        w()
        w("## Results")
        w()
        w("| Metric | λ-Memory | Current Memory | Naive Summary |")
        w("|--------|----------|----------------|---------------|")

        lam = results.get("lambda", {})
        cur = results.get("current", {})
        nav = results.get("naive", {})

        w(f"| **Score** | **{lam.get('total_score',0)}/{lam.get('max_possible',0)}** | **{cur.get('total_score',0)}/{cur.get('max_possible',0)}** | **{nav.get('total_score',0)}/{nav.get('max_possible',0)}** |")
        w(f"| **Accuracy %** | **{lam.get('percentage',0)}%** | **{cur.get('percentage',0)}%** | **{nav.get('percentage',0)}%** |")
        w(f"| Correct (1.0) | {lam.get('correct',0)} | {cur.get('correct',0)} | {nav.get('correct',0)} |")
        w(f"| Partial (0.5) | {lam.get('partial',0)} | {cur.get('partial',0)} | {nav.get('partial',0)} |")
        w(f"| Wrong (0.0) | {lam.get('wrong',0)} | {cur.get('wrong',0)} | {nav.get('wrong',0)} |")
        w(f"| Hallucinated (-0.5) | {lam.get('hallucinated',0)} | {cur.get('hallucinated',0)} | {nav.get('hallucinated',0)} |")
        w()

        # Load token data
        try:
            metrics = json.loads(METRICS_PATH.read_text())
            lam_tok = metrics.get("lambda", {}).get("in_tok", 0) + metrics.get("lambda", {}).get("out_tok", 0)
            cur_tok = metrics.get("current", {}).get("in_tok", 0) + metrics.get("current", {}).get("out_tok", 0)
            nav_tok = metrics.get("naive", {}).get("in_tok", 0) + metrics.get("naive", {}).get("out_tok", 0)

            w("## Efficiency: Score per Token")
            w()
            w("| Metric | λ-Memory | Current Memory | Naive Summary |")
            w("|--------|----------|----------------|---------------|")
            w(f"| Total tokens | {lam_tok:,} | {cur_tok:,} | {nav_tok:,} |")
            w(f"| Recall score | {lam.get('total_score',0)} | {cur.get('total_score',0)} | {nav.get('total_score',0)} |")
            lam_eff = lam.get('total_score', 0) / (lam_tok / 1000) if lam_tok > 0 else 0
            cur_eff = cur.get('total_score', 0) / (cur_tok / 1000) if cur_tok > 0 else 0
            nav_eff = nav.get('total_score', 0) / (nav_tok / 1000) if nav_tok > 0 else 0
            w(f"| **Score per 1K tokens** | **{lam_eff:.3f}** | **{cur_eff:.3f}** | **{nav_eff:.3f}** |")
            w()
        except:
            pass

        w("## Per-Question Breakdown")
        w()
        for name, label in [("lambda", "λ-Memory"), ("current", "Current"), ("naive", "Naive")]:
            r = results.get(name, {})
            w(f"### {label}")
            w()
            w(f"| Turn | Score | Reason | Response |")
            w(f"|------|-------|--------|----------|")
            for d in r.get("details", []):
                score_str = {1.0: "1.0", 0.5: "0.5", 0.25: "0.25", 0.0: "0.0", -0.5: "-0.5"}.get(d["score"], str(d["score"]))
                preview = d["response_preview"].replace("|", "\\|")[:60]
                w(f"| T{d['turn']} | {score_str} | {d['reason']} | {preview} |")
            w()

        w("---")
        w("*Scored by TEMM1E's Lab automated rubric*")

    print(f"\nReport saved: {SCORE_REPORT_PATH}")

    # Update metrics JSON
    try:
        metrics = json.loads(METRICS_PATH.read_text())
        for key in ["lambda", "current", "naive"]:
            if key in results:
                r = results[key]
                metrics[key]["recall_score"] = r["total_score"]
                metrics[key]["recall_max"] = r["max_possible"]
                metrics[key]["recall_pct"] = r["percentage"]
                metrics[key]["correct"] = r["correct"]
                metrics[key]["wrong"] = r["wrong"]
                metrics[key]["hallucinated"] = r["hallucinated"]
        METRICS_PATH.write_text(json.dumps(metrics, indent=2))
        print(f"Metrics updated: {METRICS_PATH}")
    except Exception as e:
        print(f"Warning: couldn't update metrics: {e}")


if __name__ == "__main__":
    main()
