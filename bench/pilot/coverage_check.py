#!/usr/bin/env python3
"""Shaping-coverage pre-check for a benchmark corpus.

The M2 pilot was inconclusive because the corpus almost never produced a tool result the
sidecar could act on: AAES shaped 3 of the 103 shapeable results it saw. A corpus that
cannot exercise the treatment cannot measure it, so `03_BENCHMARK_PLAN.md` requires this
check to pass before the powered study runs.

Coverage counts only results the shaper is allowed to act on (`read`, `search`,
`shell_read`). `other` events are tool calls AAES does not handle (`Bash`, `Edit`, ...) and
are not part of the denominator. A shadow decision counts as covered: it means the shaper
would have acted if shadow mode were off.

    python bench/pilot/coverage_check.py --work C:/Claude_Builds/pilot [--min 0.30] [--by-task]
"""

from __future__ import annotations

import argparse
import collections
import json
import sys
from pathlib import Path

SHAPEABLE = ("read", "search", "shell_read")
COVERED = ("shaped", "shadow")


def events(work: Path):
    """Yield (task, arm, event) for every AAES session event under the work folder."""
    for p in work.glob("runs/*/*/aaes-home/sessions/*/events.jsonl"):
        parts = p.as_posix().split("/")
        task, run = parts[-6], parts[-5]
        arm = run.rsplit("-r", 1)[0]
        with open(p, encoding="utf-8", errors="replace") as f:
            for line in f:
                line = line.strip()
                if not line:
                    continue
                try:
                    yield task, arm, json.loads(line)
                except json.JSONDecodeError:
                    continue


class Tally:
    def __init__(self) -> None:
        self.shapeable = 0
        self.covered = 0
        self.raw = 0
        self.covered_raw = 0
        self.delivered = 0
        self.reasons: collections.Counter = collections.Counter()

    def add(self, e: dict) -> None:
        raw = e.get("raw_bytes", 0) or 0
        self.shapeable += 1
        self.raw += raw
        self.delivered += e.get("delivered_bytes", 0) or 0
        if e.get("action") in COVERED:
            self.covered += 1
            self.covered_raw += raw
        else:
            self.reasons[f'{e.get("kind")}:{e.get("reason")}'] += 1

    @property
    def coverage(self) -> float:
        """Share of shapeable BYTES the shaper acted on.

        Tool-result sizes are heavy-tailed: a few large results hold most of the bytes, and they
        are the only ones worth shaping. Counting results instead would judge a corpus by how
        many small reads it makes, which is not what the study measures.
        """
        return self.covered_raw / self.raw if self.raw else 0.0

    @property
    def count_coverage(self) -> float:
        return self.covered / self.shapeable if self.shapeable else 0.0


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--work", required=True, type=Path, help="pilot work folder")
    ap.add_argument("--min", type=float, default=0.30,
                    help="coverage a corpus must reach to be usable (default 0.30)")
    ap.add_argument("--arm", default=None, help="only this arm (default: every arm with events)")
    ap.add_argument("--task", default=None,
                    help="only tasks whose id contains this text, so one corpus can be checked "
                         "while earlier runs stay in the same work folder")
    ap.add_argument("--by-task", action="store_true", help="also print a per-task table")
    a = ap.parse_args()

    per_arm: dict[str, Tally] = collections.defaultdict(Tally)
    per_task: dict[tuple[str, str], Tally] = collections.defaultdict(Tally)
    other = collections.Counter()

    for task, arm, e in events(a.work):
        if a.arm and arm != a.arm:
            continue
        if a.task and a.task not in task:
            continue
        if e.get("kind") not in SHAPEABLE:
            other[arm] += 1
            continue
        per_arm[arm].add(e)
        per_task[(arm, task)].add(e)

    if not per_arm:
        print(f"no AAES events under {a.work}/runs -- nothing to check", file=sys.stderr)
        return 2

    print(f"# Shaping coverage (threshold {a.min:.0%} of shapeable bytes)\n")
    print("| Arm | Shapeable | Covered | Byte coverage | Result coverage | Raw bytes | "
          "Delivered | Saved |")
    print("|---|---:|---:|---:|---:|---:|---:|---:|")
    for arm in sorted(per_arm):
        t = per_arm[arm]
        print(f"| {arm} | {t.shapeable} | {t.covered} | {t.coverage:.1%} | "
              f"{t.count_coverage:.1%} | {t.raw:,} | {t.delivered:,} | {t.raw - t.delivered:,} |")
    print()

    for arm in sorted(per_arm):
        top = per_arm[arm].reasons.most_common(6)
        if top:
            print(f"{arm} pass reasons: " + ", ".join(f"{k} {v}" for k, v in top))
    print(f"\nunhandled tool results (not in the denominator): "
          + ", ".join(f"{k} {v}" for k, v in sorted(other.items())))

    if a.by_task:
        print("\n| Arm | Task | Shapeable | Covered | Byte coverage |")
        print("|---|---|---:|---:|---:|")
        for (arm, task), t in sorted(per_task.items(), key=lambda kv: kv[1].coverage):
            print(f"| {arm} | {task} | {t.shapeable} | {t.covered} | {t.coverage:.1%} |")

    worst = min((per_arm[arm].coverage for arm in per_arm if arm.startswith("C")
                 and not arm.endswith("shadow")), default=None)
    if worst is None:
        worst = max(per_arm[arm].coverage for arm in per_arm)
    print()
    if worst >= a.min:
        print(f"PASS: coverage {worst:.1%} reaches the {a.min:.0%} threshold; "
              f"this corpus can measure shaping.")
        return 0
    print(f"FAIL: coverage {worst:.1%} is below the {a.min:.0%} threshold. "
          f"This corpus cannot measure shaping -- the study would report noise. "
          f"Use tasks with larger tool output, or turn on the shaping the tasks exercise.")
    return 1


if __name__ == "__main__":
    sys.exit(main())
