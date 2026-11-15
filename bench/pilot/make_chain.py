#!/usr/bin/env python3
"""Build a chain task: many bugs from one base commit, fixed in a single session.

A fix-replay task is about seven turns long. The saving AAES produces on a shaped result is
multiplied by the number of later requests that re-read that result before compaction, so a
seven-turn run measures the saving near its floor: real sessions re-read a tool result about two
hundred times. A chain task makes the session long by construction. One base commit carries the
failing tests of every fix in the set, the agent receives all the bug reports at once, and the
context accumulates across the whole run.

The base is the parent of the earliest fix in the set, so no fix in the set is present in the
tree. `03_BENCHMARK_PLAN.md` (Track B) states the validity conditions; this script checks the
two static ones and, with `--verify`, the dynamic one.

    python bench/pilot/make_chain.py --work C:/Claude_Builds/pilot --repo eslint \
        --out bench/pilot/tasks/eslint-chain.json [--ids a,b,c] [--verify]
"""

from __future__ import annotations

import argparse
import json
import shlex
import subprocess
import sys
from pathlib import Path

HERE = Path(__file__).resolve().parent


def sh(cmd: str, cwd: Path, timeout: int = 1800) -> tuple[int, str]:
    p = subprocess.run(cmd, cwd=cwd, shell=True, capture_output=True, timeout=timeout)
    out = (p.stdout + p.stderr).decode("utf-8", "replace")
    return p.returncode, out


def git(args: str, cwd: Path) -> str:
    """Run git without a shell, as `mine_tasks.py` does.

    cmd.exe treats `^` as its escape character and expands `%H`, so `rev-parse <sha>^` through a
    shell silently returns `<sha>` itself. Only `sh()` below needs a shell, for its pipes.
    """
    p = subprocess.run(["git", *shlex.split(args)], cwd=cwd, capture_output=True)
    if p.returncode:
        raise RuntimeError(f"git {args}: {p.stderr.decode('utf-8', 'replace')[-400:]}")
    return p.stdout.decode("utf-8", "replace").strip()


def load(repo: str, ids: list[str] | None) -> list[dict]:
    tasks = []
    for f in sorted((HERE / "tasks").glob("*.json")):
        for t in json.loads(f.read_text(encoding="utf-8")):
            if t["repo"] == repo and not t.get("chain"):
                tasks.append(t)
    if ids:
        tasks = [t for t in tasks if t["id"] in ids]
    return tasks


def order(ver: Path, tasks: list[dict]) -> list[dict]:
    """Oldest fix first. The earliest fix's parent becomes the base for the whole chain."""
    dated = [(git(f"show -s --format=%ct {t['fix']}", ver), t) for t in tasks]
    return [t for _, t in sorted(dated, key=lambda kv: int(kv[0]))]


def at_base(ver: Path, tree: Path, base: str) -> None:
    """Put a scratch clone of `ver` at `base`, and prove it got there.

    A clone does not inherit a detached HEAD, so the fresh tree starts on the default branch. An
    unchecked checkout would leave the tree on the wrong commit and every later check would be
    measured against it, which is how a valid chain was first rejected.
    """
    if not tree.exists():
        tree.parent.mkdir(parents=True, exist_ok=True)
        subprocess.run(["git", "clone", "-q", "--shared", str(ver), str(tree)], check=True)
    code, out = sh(f"git -c advice.detachedHead=false checkout -q --detach {base}", tree)
    if code:
        raise RuntimeError(f"checkout of {base[:9]} failed: {out[-300:]}")
    head = git("rev-parse HEAD", tree)
    if head != base:
        raise RuntimeError(f"tree is at {head[:9]}, not at the base {base[:9]}")
    sh("git clean -qfd -e node_modules", tree)


def check_static(work: Path, ver: Path, base: str, tasks: list[dict]) -> list[str]:
    """Each fix's source diff must apply to the base, and no two fixes may share a test file."""
    problems = []
    seen: dict[str, str] = {}
    for t in tasks:
        for path in t["test_files"]:
            if path in seen:
                problems.append(f"{t['id']} and {seen[path]} both change {path}")
            seen[path] = t["id"]

    tree = work / "chain-check" / tasks[0]["repo"]
    at_base(ver, tree, base)
    for t in tasks:
        src = " ".join(f'"{p}"' for p in t["src_files"])
        code, _ = sh(f'git show {t["fix"]} -- {src} | git apply --check -', tree)
        if code:
            problems.append(f"{t['id']} source diff does not apply to the base")
    return problems


def build(work: Path, repo: str, ids: list[str] | None) -> dict:
    ver = work / "verify" / repo
    if not ver.exists():
        raise SystemExit(f"no verify clone at {ver}; run mine_tasks.py for {repo} first")
    tasks = load(repo, ids)
    if len(tasks) < 2:
        raise SystemExit(f"need at least two tasks for a chain, found {len(tasks)}")
    tasks = order(ver, tasks)
    # `~1`, never `^`: these commands run through cmd.exe on Windows, where `^` is the escape
    # character, so `<sha>^` silently resolves to `<sha>` and the chain is built on its own fix.
    base = git(f"rev-parse {tasks[0]['fix']}~1", ver)

    problems = check_static(work, ver, base, tasks)
    if problems:
        for p in problems:
            print(f"  reject: {p}", file=sys.stderr)
        raise SystemExit("chain is not valid; drop the offending tasks and try again")

    strata = sorted({s for t in tasks for s in t.get("strata", [])})
    return {
        "id": f"{repo}-chain-{len(tasks)}",
        "repo": repo,
        "url": tasks[0]["url"],
        "strata": strata,
        "base": base,
        "setup": tasks[0].get("setup"),
        "discover_cmd": tasks[0].get("discover_cmd"),
        "chain": [
            {
                "id": t["id"],
                "fix": t["fix"],
                "report": t["report"],
                "test_files": t["test_files"],
                "src_files": t["src_files"],
                "oracle": t["oracle"],
            }
            for t in tasks
        ],
    }


def verify(work: Path, chain: dict) -> bool:
    """Every test must fail on the base and pass once every source fix is applied."""
    ver = work / "verify" / chain["repo"]
    tree = work / "chain-verify" / chain["id"]
    at_base(ver, tree, chain["base"])

    for bug in chain["chain"]:
        for path in bug["test_files"]:
            data = subprocess.run(["git", "show", f'{bug["fix"]}:{path}'],
                                  cwd=tree, capture_output=True, check=True).stdout
            dst = tree / path
            dst.parent.mkdir(parents=True, exist_ok=True)
            dst.write_bytes(data)

    if chain.get("setup") and not (tree / "node_modules").exists():
        code, out = sh(chain["setup"], tree)
        if code:
            print(f"setup failed: {out[-400:]}", file=sys.stderr)
            return False

    ok = True
    for bug in chain["chain"]:
        code, _ = sh(bug["oracle"], tree, timeout=600)
        if code == 0:
            print(f"  reject: {bug['id']} already passes on the base", file=sys.stderr)
            ok = False
        else:
            print(f"  {bug['id']}: fails on the base, as required")
    if not ok:
        return False

    for bug in chain["chain"]:
        src = " ".join(f'"{p}"' for p in bug["src_files"])
        code, out = sh(f'git show {bug["fix"]} -- {src} | git apply -', tree)
        if code:
            print(f"  reject: {bug['id']} source diff did not apply: {out[-200:]}", file=sys.stderr)
            return False
    for bug in chain["chain"]:
        code, out = sh(bug["oracle"], tree, timeout=600)
        if code:
            print(f"  reject: {bug['id']} still fails with its fix applied", file=sys.stderr)
            ok = False
        else:
            print(f"  {bug['id']}: passes with the fixes applied")
    return ok


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__,
                                 formatter_class=argparse.RawDescriptionHelpFormatter)
    ap.add_argument("--work", required=True, type=Path, help="pilot work folder")
    ap.add_argument("--repo", required=True, help="repo key, as used in the task files")
    ap.add_argument("--ids", help="comma-separated task ids (default: every task for the repo)")
    ap.add_argument("--out", type=Path, help="write the chain task here")
    ap.add_argument("--verify", action="store_true",
                    help="also run every oracle on the base and with the fixes applied")
    a = ap.parse_args()

    chain = build(a.work, a.repo, a.ids.split(",") if a.ids else None)
    print(f"{chain['id']}: {len(chain['chain'])} bugs on base {chain['base'][:9]}")
    for bug in chain["chain"]:
        print(f"  {bug['id']}: {bug['report']}")

    if a.verify and not verify(a.work, chain):
        return 1

    if a.out:
        a.out.write_text(json.dumps([chain], indent=1) + "\n", encoding="utf-8")
        print(f"wrote {a.out}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
