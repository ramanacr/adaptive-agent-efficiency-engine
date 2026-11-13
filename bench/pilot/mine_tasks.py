#!/usr/bin/env python3
"""Mine fix-commit-replay tasks for the M2 pilot (03_BENCHMARK_PLAN.md, "M2 pilot protocol").

A task is a recent fix commit of a public OSS repository that changes 1-5 source files and adds
or changes a test. The task snapshot is the parent commit plus the commit's test changes. The
oracle is the commit's own test command:

- it must FAIL on the snapshot (tests applied, fix not applied), and
- it must PASS once the commit's source changes are applied.

Usage:
    python bench/pilot/mine_tasks.py --work C:/Claude_Builds/pilot --repo marked --want 8
    python bench/pilot/mine_tasks.py --work C:/Claude_Builds/pilot --all --want 8

Writes bench/pilot/tasks/<repo>.json (public data only: repo URL, commit ids, file lists, the
oracle command and the commit subject used as the bug report). Verification runs in
<work>/verify/<repo>, a local clone; nothing runs in this repository.
"""
from __future__ import annotations

import argparse
import hashlib
import json
import re
import shlex
import shutil
import subprocess
import sys
import time
from pathlib import Path

HERE = Path(__file__).resolve().parent


def npm_test_script(root: Path) -> str:
    try:
        return json.loads((root / "package.json").read_text(encoding="utf-8")).get("scripts", {}).get("test", "")
    except (OSError, ValueError):
        return ""


def commander_oracle(tests: list[str], root: Path) -> str:
    # Older commits test with Jest, newer ones with node:test.
    if "jest" in npm_test_script(root):
        return "npx jest " + " ".join(tests)
    return "node --test " + " ".join(tests)


def marked_oracle(tests: list[str], root: Path) -> str:
    script = npm_test_script(root)
    if "jest" in script or "jasmine" in script:
        return "npm run build --if-present && npm test --silent"
    return "npm run build && node --test test/run-spec-tests.js && node --test test/unit/*.test.js"


def csv_oracle(tests: list[str], root: Path) -> str:
    """Newest .NET 8+ target of the test project, run on the installed runtime (roll forward)."""
    proj = root / "tests" / "CsvHelper.Tests" / "CsvHelper.Tests.csproj"
    text = proj.read_text(encoding="utf-8", errors="replace") if proj.exists() else ""
    tfms = sorted({int(m) for m in re.findall(r"net(\d+)\.0", text) if int(m) >= 8})
    if not tfms:
        return "echo no .NET 8+ target && exit 1"
    return (f"set DOTNET_ROLL_FORWARD=Major&& dotnet test tests/CsvHelper.Tests -f net{tfms[-1]}.0 "
            f'-v q --nologo --filter "{csv_filter(tests)}"')


def csv_filter(tests: list[str]) -> str:
    classes = sorted({Path(t).stem for t in tests})
    return "|".join(f"FullyQualifiedName~{c}" for c in classes)


REPOS: dict[str, dict] = {
    "marked": {
        "url": "https://github.com/markedjs/marked",
        "strata": [1, 13],
        "src": r"^src/[^/]+\.ts$",
        "test": r"^test/(specs/|unit/).+",
        "lock": "package-lock.json",
        "setup": "npm ci --no-audit --no-fund",
        "oracle": marked_oracle,
    },
    "commander.js": {
        "url": "https://github.com/tj/commander.js",
        "strata": [1, 13],
        "src": r"^(lib/[^/]+\.js|index\.js)$",
        "test": r"^tests/.+\.test\.js$",
        "lock": "package-lock.json",
        "setup": "npm ci --no-audit --no-fund",
        "oracle": commander_oracle,
    },
    "CsvHelper": {
        "url": "https://github.com/JoshClose/CsvHelper",
        "strata": [2, 13],
        "src": r"^src/CsvHelper/.+\.cs$",
        "test": r"^tests/CsvHelper\.Tests/.+\.cs$",
        "lock": None,
        # Older commits pin an SDK that is not installed; let the installed SDK build them.
        "setup": "if exist global.json del global.json",
        "oracle": csv_oracle,
    },
}

MAX_SRC_FILES = 5
MAX_SRC_LINES = 80
SAFE_PATH = re.compile(r"^[A-Za-z0-9._/-]+$")
SKIP_SUBJECT = re.compile(r"^(revert|merge|release|chore\(release\)|bump|build\(deps)", re.I)


def sh(cmd: str, cwd: Path, timeout: int = 900) -> tuple[int, str]:
    try:
        p = subprocess.run(cmd, cwd=cwd, shell=True, capture_output=True, text=True,
                           encoding="utf-8", errors="replace", timeout=timeout)
        return p.returncode, (p.stdout + p.stderr)[-4000:]
    except subprocess.TimeoutExpired:
        return 124, "timeout"


def git(args: str, cwd: Path) -> str:
    """Run git without a shell (cmd.exe would expand `%H` in format strings)."""
    p = subprocess.run(["git", *shlex.split(args)], cwd=cwd, capture_output=True, text=True,
                       encoding="utf-8", errors="replace")
    if p.returncode:
        raise RuntimeError(f"git {args}: {(p.stdout + p.stderr)[-1000:]}")
    return p.stdout


def candidates(repo: Path, cfg: dict, limit: int):
    src_re, test_re = re.compile(cfg["src"]), re.compile(cfg["test"])
    log = git(f"log --no-merges -n {limit} --format=%H%x09%s", repo).splitlines()
    for line in log:
        sha, _, subject = line.partition("\t")
        if SKIP_SUBJECT.match(subject):
            continue
        numstat = git(f"show --numstat --format= {sha}", repo).splitlines()
        srcs, tests, src_lines = [], [], 0
        for row in numstat:
            parts = row.split("\t")
            if len(parts) != 3:
                continue
            add, rem, path = parts
            # Paths go into shell commands (oracle, git checkout): allow plain names only.
            if not SAFE_PATH.match(path):
                srcs, tests = [], []
                break
            if test_re.match(path):
                tests.append(path)
            elif src_re.match(path):
                srcs.append(path)
                src_lines += int(add) + int(rem) if add.isdigit() and rem.isdigit() else 999
        if tests and 1 <= len(srcs) <= MAX_SRC_FILES and src_lines <= MAX_SRC_LINES:
            yield sha, subject, srcs, tests, src_lines


def report_text(subject: str) -> str:
    s = re.sub(r"^(fix|bugfix|bug)(\([^)]*\))?!?:\s*", "", subject, flags=re.I)
    s = re.sub(r"\s*\(#\d+\)\s*$", "", s)
    return s[:1].upper() + s[1:]


def ensure_clone(work: Path, name: str, cfg: dict) -> tuple[Path, Path]:
    src = work / "repos" / name
    if not src.exists():
        src.parent.mkdir(parents=True, exist_ok=True)
        git(f"clone -q {cfg['url']}.git {src.as_posix()}", work)
    ver = work / "verify" / name
    if not ver.exists():
        ver.parent.mkdir(parents=True, exist_ok=True)
        # A local clone of the full clone: checkouts need no network.
        git(f"clone -q --no-checkout {src.as_posix()} {ver.as_posix()}", work)
    return src, ver


def verify(ver: Path, cfg: dict, sha: str, srcs: list[str], tests: list[str], state: dict) -> tuple[bool, str]:
    parent = git(f"rev-parse {sha}^", ver).strip()
    git(f"checkout -q -f {parent}", ver)
    git("clean -q -fdx -e node_modules", ver)
    if cfg["lock"]:
        lock = (ver / cfg["lock"]).read_bytes() if (ver / cfg["lock"]).exists() else b""
        h = hashlib.sha256(lock).hexdigest()
        if state.get("lock") != h:
            shutil.rmtree(ver / "node_modules", ignore_errors=True)
            code, out = sh(cfg["setup"], ver, 1200)
            if code:
                return False, f"setup failed: {out[-300:]}"
            state["lock"] = h
    elif cfg["setup"]:
        sh(cfg["setup"], ver, 1200)
    # Test files the fix commit deleted cannot be checked out from it.
    kept = set(git(f"ls-tree -r --name-only {sha}", ver).splitlines())
    present = [t for t in tests if t in kept]
    if present:
        git(f"checkout -q {sha} -- " + " ".join(present), ver)
    oracle = cfg["oracle"](tests, ver)
    state["oracle"] = oracle
    t0 = time.time()
    code, out = sh(oracle, ver)
    if code == 0:
        return False, "oracle passes without the fix"
    git(f"checkout -q {sha} -- " + " ".join(srcs), ver)
    code, out2 = sh(oracle, ver)
    if code != 0:
        return False, f"oracle fails with the fix: {out2[-300:]}"
    return True, f"{time.time() - t0:.0f}s"


def mine(work: Path, name: str, want: int, scan: int) -> list[dict]:
    cfg = REPOS[name]
    src, ver = ensure_clone(work, name, cfg)
    try:
        git("fetch -q origin", src)
    except RuntimeError as e:  # offline: mine from what is there
        print(f"[{name}] fetch failed, using the local clone: {str(e)[:120]}")
    out_path = HERE / "tasks" / f"{name}.json"
    tasks = json.loads(out_path.read_text()) if out_path.exists() else []
    done = {t["fix"] for t in tasks}
    rejected_path = work / "verify" / f"{name}.rejected.json"
    rejected = json.loads(rejected_path.read_text()) if rejected_path.exists() else {}
    state: dict = {}
    for sha, subject, srcs, tests, lines in candidates(src, cfg, scan):
        if len(tasks) >= want:
            break
        if sha in done or sha in rejected:
            continue
        print(f"[{name}] {sha[:9]} {subject[:70]} ({len(srcs)} src, {lines} lines)", flush=True)
        try:
            ok, why = verify(ver, cfg, sha, srcs, tests, state)
        except RuntimeError as e:
            ok, why = False, str(e)[:300]
        print(f"    -> {'TASK' if ok else 'skip'}: {why}", flush=True)
        if not ok:
            rejected[sha] = why
            rejected_path.write_text(json.dumps(rejected, indent=1))
            continue
        tasks.append({
            "id": f"{name}-{sha[:9]}",
            "repo": name,
            "url": cfg["url"],
            "strata": cfg["strata"],
            "base": git(f"rev-parse {sha}^", src).strip(),
            "fix": sha,
            "test_files": tests,
            "src_files": srcs,
            "src_lines": lines,
            "setup": cfg["setup"],
            "oracle": state["oracle"],
            "report": report_text(subject),
        })
        out_path.parent.mkdir(parents=True, exist_ok=True)
        out_path.write_text(json.dumps(tasks, indent=2) + "\n")
    return tasks


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", type=Path, required=True)
    ap.add_argument("--repo", choices=sorted(REPOS))
    ap.add_argument("--all", action="store_true")
    ap.add_argument("--want", type=int, default=8)
    ap.add_argument("--scan", type=int, default=600, help="commits to scan per repo")
    a = ap.parse_args()
    names = sorted(REPOS) if a.all else [a.repo]
    if names == [None]:
        ap.error("--repo or --all")
    for n in names:
        t = mine(a.work, n, a.want, a.scan)
        print(f"[{n}] {len(t)} tasks")
    return 0


if __name__ == "__main__":
    sys.exit(main())
