#!/usr/bin/env python3
"""Analyze M2 pilot runs (03_BENCHMARK_PLAN.md, "Pre-registered metrics" and "Analysis").

    python bench/pilot/analyze_pilot.py --work C:/Claude_Builds/pilot [--md out.md]

Per run, from the copied transcripts (main thread and sub-agents, usage de-duplicated by
message id): cost-weighted input (uncached x1, cache write x1.25 (5 min) / x2 (1 h), cache read
x0.1), tool calls, `aaes show` calls and compactions. Per arm: mean over tasks of the per-task
mean over replications. Differences against arm A are paired by task, with a bootstrap 95%
interval over tasks. psi is the share of discordant (A, C) pairs, paired by replication.
"""
from __future__ import annotations

import argparse
import json
import random
import statistics as st
import sys
from collections import defaultdict
from pathlib import Path

W = {"in": 1.0, "w5": 1.25, "w1": 2.0, "read": 0.1}


def transcript_metrics(run: Path) -> dict:
    seen, units, calls, shows, compactions, reqs = set(), 0.0, 0, 0, 0, 0
    for f in sorted((run / "transcripts").rglob("*.jsonl")):
        for line in f.read_text(encoding="utf-8", errors="replace").splitlines():
            try:
                v = json.loads(line)
            except json.JSONDecodeError:
                continue
            if v.get("type") == "system" and v.get("subtype") == "compact_boundary":
                compactions += 1
            if v.get("type") != "assistant":
                continue
            m = v.get("message") or {}
            for c in m.get("content") or []:
                if isinstance(c, dict) and c.get("type") == "tool_use":
                    calls += 1
                    cmd = str((c.get("input") or {}).get("command", ""))
                    if "aaes show" in cmd:
                        shows += 1
            u = m.get("usage")
            mid = m.get("id")
            if not u or mid in seen:
                continue
            seen.add(mid)
            reqs += 1
            cc = u.get("cache_creation_input_tokens") or 0
            w1 = min((u.get("cache_creation") or {}).get("ephemeral_1h_input_tokens") or 0, cc)
            units += ((u.get("input_tokens") or 0) * W["in"] + (u.get("cache_read_input_tokens") or 0) * W["read"]
                      + (cc - w1) * W["w5"] + w1 * W["w1"])
    return {"units": units, "tool_calls": calls, "shows": shows, "compactions": compactions, "requests": reqs}


def boot(diffs: list[float], n: int = 4000, seed: int = 7) -> tuple[float, float]:
    if len(diffs) < 2:
        return (float("nan"), float("nan"))
    rng = random.Random(seed)
    means = sorted(st.fmean(rng.choices(diffs, k=len(diffs))) for _ in range(n))
    return means[int(0.025 * n)], means[int(0.975 * n) - 1]


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", type=Path, required=True)
    ap.add_argument("--md", type=Path)
    a = ap.parse_args()
    runs = [json.loads(l) for l in (a.work / "results" / "runs.jsonl").read_text().splitlines() if l.strip()]
    for r in runs:
        r.update(transcript_metrics(a.work / "runs" / r["task"] / f"{r['arm']}-r{r['rep']}"))
    arms = [x for x in ["A", "Aprime", "C", "Cshadow"] if any(r["arm"] == x for r in runs)]
    # task -> arm -> list of runs
    by = defaultdict(lambda: defaultdict(list))
    for r in runs:
        by[r["task"]][r["arm"]].append(r)
    complete = [t for t in by if all(by[t][x] for x in arms)]

    def task_mean(t, arm, key):
        return st.fmean(float(r[key] or 0) for r in by[t][arm])

    out = []
    p = out.append
    spent = sum(r.get("cost_usd") or 0 for r in runs)
    p("# M2 pilot results\n")
    p(f"{len(runs)} runs, {len(by)} tasks ({len(complete)} with every arm), arms {', '.join(arms)}. "
      f"Model {runs[0].get('model')}, {runs[0].get('claude')}. Cost reported by Claude Code: ${spent:.2f}.\n")
    p("| Arm | Runs | Success | Cost-weighted input / run | Cost / run (USD) | Cost / success (USD) | Tool calls | `aaes show` | Compactions | Wall (s) |")
    p("|---|---:|---:|---:|---:|---:|---:|---:|---:|---:|")
    for x in arms:
        rs = [r for r in runs if r["arm"] == x]
        succ = sum(r["success"] for r in rs)
        cost = sum(r.get("cost_usd") or 0 for r in rs)
        p(f"| {x} | {len(rs)} | {succ / len(rs):.0%} | {st.fmean(r['units'] for r in rs):,.0f} | "
          f"{cost / len(rs):.3f} | {cost / succ if succ else float('nan'):.3f} | "
          f"{st.fmean(r['tool_calls'] for r in rs):.1f} | {sum(r['shows'] for r in rs)} | "
          f"{sum(r['compactions'] for r in rs)} | {st.fmean(r['wall_s'] for r in rs):.0f} |")
    p("\n## Paired differences against A (per task, mean over replications; 95% bootstrap interval over tasks)\n")
    p("| Arm | Tasks | Success (pp) | Cost-weighted input | Cost (USD) | Wall (s) |")
    p("|---|---:|---|---|---|---|")
    for x in arms:
        if x == "A":
            continue
        ts = [t for t in complete]
        cells = []
        for key, scale, fmt in (("success", 100, "{:+.1f}"), ("units", 1, "{:+,.0f}"),
                                ("cost_usd", 1, "{:+.3f}"), ("wall_s", 1, "{:+.0f}")):
            d = [scale * (task_mean(t, x, key) - task_mean(t, "A", key)) for t in ts]
            lo, hi = boot(d)
            rel = ""
            if key == "units":
                base = st.fmean(task_mean(t, "A", key) for t in ts) if ts else 0
                rel = f" ({st.fmean(d) / base:+.1%})" if base else ""
            cells.append(f"{fmt.format(st.fmean(d)) if d else 'n/a'}{rel} [{fmt.format(lo)}, {fmt.format(hi)}]")
        p(f"| {x} | {len(ts)} | " + " | ".join(cells) + " |")
    if "C" in arms:
        pairs = disc = 0
        for t in by:
            a_runs = {r["rep"]: r for r in by[t]["A"]}
            for r in by[t]["C"]:
                if r["rep"] in a_runs:
                    pairs += 1
                    disc += a_runs[r["rep"]]["success"] != r["success"]
        psi = disc / pairs if pairs else float("nan")
        p(f"\n**psi (A vs C discordant pairs):** {disc}/{pairs} = {psi:.2f}.")
        for delta in (0.03, 0.05):
            n = (1.645 + 0.842) ** 2 * psi / delta ** 2 if pairs else float("nan")
            p(f"Paired tasks needed for a {delta:.0%} margin (one-sided alpha 0.05, power 0.8): about {n:.0f}.")
    errs = [r for r in runs if (r.get("hook_errors") or 0) > 0]
    p(f"\nHook errors in C arms: {len(errs)} runs." + (" " + ", ".join(f"{r['task']}/{r['arm']}-r{r['rep']}" for r in errs) if errs else ""))
    stop = [t for t in by if "C" in by[t] and by[t]["C"] and not any(r["success"] for r in by[t]["C"])
            and all(r["success"] for r in by[t].get("A", [])) and all(r["success"] for r in by[t].get("Cshadow", []))
            and by[t].get("A") and by[t].get("Cshadow")]
    p(f"Stop-rule tasks (C fails every replication, A and C-shadow pass every one): {', '.join(stop) or 'none'}.")
    errors = [r for r in runs if r.get("is_error") or r.get("claude_exit")]
    if errors:
        p(f"\nAgent runs that ended with an error: {len(errors)} "
          f"({', '.join(sorted({str(r.get('terminal_reason')) for r in errors}))}).")
    text = "\n".join(out) + "\n"
    if a.md:
        a.md.write_text(text, encoding="utf-8")
    print(text)
    return 0


if __name__ == "__main__":
    sys.exit(main())
