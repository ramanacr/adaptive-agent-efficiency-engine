#!/usr/bin/env python3
"""Run the M2 pilot (03_BENCHMARK_PLAN.md, "M2 pilot protocol").

For each replication, task and arm (arm order randomized per task and replication):
  1. copy the task snapshot (parent commit + the fix commit's tests, a fresh one-commit git repo
     with no upstream history, dependencies installed) to a new run folder;
  2. give the run its own CLAUDE_CONFIG_DIR and AAES_HOME, and set up the arm:
       A         no hooks
       Aprime    A + CLAUDE.md with the prompt-only instructions below
       C         AAES hooks, command mode, default config
       Cshadow   AAES hooks, command mode, "shadow": ["read", "search"]
  3. run `claude -p` headless (pinned binary and model, same tools in every arm, turn and time
     limits), with the API key from AAES_ANTHROPIC_API_KEY set for the child process only;
  4. restore the test files and run the oracle; record pass/fail and the source diff size;
  5. keep the transcript, the result JSON and, for C arms, `aaes report --json` and the AAES log
     errors.

Stops before the next run would pass the budget (spent + 1.5 x mean run cost > --budget).
Resumable: runs already in results/runs.jsonl are skipped.

    python bench/pilot/run_pilot.py --work C:/Claude_Builds/pilot --claude <claude.exe> \
        --aaes <aaes.exe> --budget 250 --reps 3
"""
from __future__ import annotations

import argparse
import json
import os
import random
import re
import shlex
import shutil
import stat
import subprocess
import sys
import threading
import time
from concurrent.futures import ThreadPoolExecutor
from pathlib import Path

HERE = Path(__file__).resolve().parent
ARMS = ["A", "Aprime", "C", "Cshadow"]

APRIME_MD = """# Working efficiently in this repository

- Keep tool output small. Prefer quiet flags: `node --test --test-reporter=dot`,
  `dotnet test -v q --nologo`, `npm run <script> --silent`.
- Search with a narrow pattern and path; list matching files first (files only), then look at
  the matches you need.
- Read large files in ranges (offset and limit) around what you need instead of whole files.
- Use `git --no-pager diff --stat` before a full diff.
"""

PROMPT = """You are working in a checkout of {repo} ({url}).

Bug report: {report}

The test changes that reproduce the bug are already in the working tree ({tests}).
Run them with:
    {oracle}

Fix the library source code so that these tests pass. Do not change the tests.
When you are done, reply with one line: DONE."""

TOOLS = "Read,Edit,Write,Glob,Grep,Bash,Task"


def api_key() -> str:
    k = os.environ.get("AAES_ANTHROPIC_API_KEY")
    if not k and sys.platform == "win32":
        import winreg
        try:
            with winreg.OpenKey(winreg.HKEY_CURRENT_USER, "Environment") as h:
                k = winreg.QueryValueEx(h, "AAES_ANTHROPIC_API_KEY")[0]
        except OSError:
            k = None
    if not k:
        sys.exit("AAES_ANTHROPIC_API_KEY is not set (process or user scope)")
    return k


def drop_links(root: Path) -> None:
    """Remove junctions and directory symlinks under `root`, link only.

    Python 3.11's shutil.rmtree walks into a Windows junction, which would delete the shared
    `node_modules` it points at. Removing the links first makes the delete safe.
    """
    for dirpath, dirnames, _ in os.walk(root, topdown=True):
        for name in list(dirnames):
            p = Path(dirpath) / name
            try:
                attrs = os.lstat(p).st_file_attributes  # Windows only
                link = bool(attrs & stat.FILE_ATTRIBUTE_REPARSE_POINT)
            except AttributeError:
                link = p.is_symlink()
            except OSError:
                continue
            if link:
                dirnames.remove(name)
                try:
                    os.rmdir(p) if not p.is_symlink() else p.unlink()
                except OSError:
                    pass


def rmtree(p: Path) -> None:
    """Remove a folder, including read-only files (git objects on Windows)."""
    def onerror(func, path, _exc):
        os.chmod(path, stat.S_IWRITE)
        func(path)
    if p.exists():
        drop_links(p)
        shutil.rmtree(p, onerror=onerror)


def sh(cmd: str, cwd: Path, env: dict | None = None, timeout: int = 900) -> tuple[int, str]:
    try:
        p = subprocess.run(cmd, cwd=cwd, shell=True, capture_output=True, text=True,
                           encoding="utf-8", errors="replace", env=env, timeout=timeout)
        return p.returncode, (p.stdout + p.stderr)[-6000:]
    except subprocess.TimeoutExpired:
        return 124, "timeout"


def git(args: str, cwd: Path) -> str:
    # --no-ext-diff: the user's global diff tool must not run here. An empty GIT_EXTERNAL_DIFF
    # would make git spawn "", so the variable is removed instead of being set empty.
    env = {k: v for k, v in os.environ.items() if k != "GIT_EXTERNAL_DIFF"}
    parts = shlex.split(args)
    if "diff" in parts:  # it is a subcommand option, not a top-level one
        parts.insert(parts.index("diff") + 1, "--no-ext-diff")
    p = subprocess.run(["git", "-c", "core.pager=cat", *parts],
                       cwd=cwd, capture_output=True, text=True, encoding="utf-8", errors="replace",
                       env=dict(env, GIT_PAGER="cat"))
    if p.returncode:
        raise RuntimeError(f"git {args}: {(p.stdout + p.stderr)[-800:]}")
    return p.stdout


def load_tasks(only: list[str] | None) -> list[dict]:
    tasks = []
    for f in sorted((HERE / "tasks").glob("*.json")):
        tasks += json.loads(f.read_text())
    if only:
        tasks = [t for t in tasks if t["id"] in only]
    return tasks


def snapshot(work: Path, t: dict) -> Path:
    """Parent commit + the fix commit's test files, as a fresh one-commit repo, deps installed."""
    snap = work / "snap" / t["id"]
    tree = snap / "tree"
    if (snap / "ready").exists():
        return tree
    rmtree(snap)
    tree.mkdir(parents=True)
    ver = work / "verify" / t["repo"]
    archive = snap / "base.tar"
    git(f"archive --format=tar -o {archive.as_posix()} {t['base']}", ver)
    subprocess.run(["tar", "-xf", str(archive), "-C", str(tree)], check=True)
    archive.unlink()
    kept = set(git(f"ls-tree -r --name-only {t['fix']}", ver).splitlines())
    for path in t["test_files"]:
        dst = tree / path
        if path in kept:
            dst.parent.mkdir(parents=True, exist_ok=True)
            data = subprocess.run(["git", "show", f"{t['fix']}:{path}"], cwd=ver,
                                  capture_output=True, check=True).stdout
            dst.write_bytes(data)
        elif dst.exists():
            dst.unlink()
    git("init -q", tree)
    git("add -A", tree)
    git("-c user.name=pilot -c user.email=pilot@example.invalid -c commit.gpgsign=false commit -q -m snapshot", tree)
    if t.get("setup"):
        code, out = sh(t["setup"], tree, timeout=1800)
        if code:
            raise RuntimeError(f"setup failed for {t['id']}: {out[-500:]}")
    drop_speed_assertion(tree)
    # Dependencies and build output stay out of `git diff`.
    (tree / ".git" / "info" / "exclude").write_text("node_modules/\nbin/\nobj/\nlib/*.js.map\nCLAUDE.md\n", encoding="utf-8", errors="replace")
    (snap / "ready").write_text("ok", encoding="utf-8", errors="replace")
    return tree


def drop_speed_assertion(tree: Path) -> bool:
    """Turn off marked's per-spec speed assertion in the snapshot's test helper.

    `@markedjs/testutils` fails any spec whose wall time reaches 1 second
    (`assert.fail("took too long: ...")`). That measures how busy the machine is, not whether the
    fix is correct, and it fails at random while several pilot runs share the machine. It is
    switched off for every arm alike, so it cannot favour one. Every other assertion stays.
    """
    f = tree / "node_modules" / "@markedjs" / "testutils" / "lib" / "run-tests.js"
    if not f.is_file():
        return False
    src = f.read_text(encoding="utf-8", errors="replace")
    pat = re.compile(r"assert\.fail\(`took too long[^`]*`\);")
    if not pat.search(src):
        return False
    f.write_text(pat.sub("void 0; // speed assertion disabled for the pilot", src),
                 encoding="utf-8", errors="replace")
    return True


def setup_arm(arm: str, run: Path, repo: Path, aaes: str, env: dict) -> None:
    cfg = run / "cfg"
    home = run / "aaes-home"
    cfg.mkdir(parents=True)
    home.mkdir(parents=True)
    settings = cfg / "settings.json"
    settings.write_text("{}\n", encoding="utf-8", errors="replace")
    if arm == "Aprime":
        (repo / "CLAUDE.md").write_text(APRIME_MD, encoding="utf-8", errors="replace")
    if arm in ("C", "Cshadow"):
        if arm == "Cshadow":
            (home / "config.json").write_text('{"shadow": ["read", "search"]}\n', encoding="utf-8", errors="replace")
        code, out = sh(f'"{aaes}" init --mode command --settings "{settings}"', run, env)
        if code:
            raise RuntimeError(f"aaes init failed: {out}")


def copy_snapshot(src: Path, dst: Path) -> None:
    """Copy a snapshot without `node_modules`, which is linked instead.

    Copying it would be most of the work (marked: 235 MB in 16.5k files per run, against about
    5 MB of sources). Runs only read it; they install nothing.
    """
    shutil.copytree(src, dst, symlinks=True, ignore=shutil.ignore_patterns("node_modules"))
    shared = src / "node_modules"
    if not shared.is_dir():
        return
    link = dst / "node_modules"
    if sys.platform == "win32":
        # A directory junction needs no privileges, and tools follow it like a real folder.
        p = subprocess.run(["cmd", "/c", "mklink", "/J", str(link), str(shared)],
                           capture_output=True, text=True)
        if p.returncode:
            raise RuntimeError(f"mklink failed: {p.stdout} {p.stderr}")
    else:
        link.symlink_to(shared, target_is_directory=True)


def transcripts(projects: Path, session_id: str | None) -> list[tuple[Path, Path]]:
    """(file, path relative to `projects`) of the run's transcripts. With a shared projects folder
    (subscription auth), only the session's own file and its sub-agent folder."""
    if not projects.exists():
        return []
    if session_id is None:
        files = projects.rglob("*.jsonl")
    else:
        files = [f for f in projects.rglob("*.jsonl")
                 if f.stem == session_id or session_id in f.parts]
    return sorted((f, f.relative_to(projects)) for f in files)


def one_run(work: Path, t: dict, arm: str, rep: int, a, key: str | None) -> dict:
    run = work / "runs" / t["id"] / f"{arm}-r{rep}"
    rmtree(run)
    run.mkdir(parents=True)
    repo = run / "repo"
    copy_snapshot(snapshot(work, t), repo)
    env = dict(os.environ)
    for k in ("ANTHROPIC_API_KEY", "ANTHROPIC_AUTH_TOKEN", "AAES_HOOK_TOKEN", "AAES_RAW"):
        env.pop(k, None)
    if a.auth == "api":
        env["CLAUDE_CONFIG_DIR"] = str(run / "cfg")
    else:
        # Subscription: the user's own login and projects folder; user settings, hooks, plugins
        # and MCP servers are left out below.
        env.pop("CLAUDE_CONFIG_DIR", None)
    env.update({
        "AAES_HOME": str(run / "aaes-home"),
        "DISABLE_AUTOUPDATER": "1",
        "DISABLE_TELEMETRY": "1",
    })
    setup_arm(arm, run, repo, a.aaes, env)
    prompt = PROMPT.format(repo=t["repo"], url=t["url"], report=t["report"],
                           tests=", ".join(t["test_files"]), oracle=t["oracle"])
    cmd = [a.claude, "-p", prompt, "--model", a.model, "--output-format", "json",
           "--max-turns", str(a.max_turns), "--allowedTools", TOOLS,
           "--settings", str(run / "cfg" / "settings.json"), "--strict-mcp-config"]
    if a.auth == "subscription":
        cmd += ["--setting-sources", "project,local"]
    child = dict(env, ANTHROPIC_API_KEY=key) if key else env
    t0 = time.time()
    try:
        p = subprocess.run(cmd, cwd=repo, env=child, capture_output=True, text=True,
                           encoding="utf-8", errors="replace", timeout=a.timeout)
        out, err, code = p.stdout, p.stderr, p.returncode
    except subprocess.TimeoutExpired as e:
        out, err, code = (e.stdout or ""), "timeout", 124
        out = out.decode("utf-8", "replace") if isinstance(out, bytes) else out
    wall = time.time() - t0
    (run / "claude.json").write_text(out, encoding="utf-8", errors="replace")
    (run / "claude.err").write_text(err[-20000:] if isinstance(err, str) else "", encoding="utf-8", errors="replace")
    try:
        res = json.loads(out)
    except json.JSONDecodeError:
        res = {}

    # Oracle on the original tests.
    tracked = set(git("ls-files", repo).splitlines())
    for path in t["test_files"]:
        if path in tracked:
            git(f"checkout -q HEAD -- {shlex.quote(path)}", repo)
        elif (repo / path).exists():
            (repo / path).unlink()
    ocode, oout = sh(t["oracle"], repo, env=env, timeout=1200)
    (run / "oracle.txt").write_text(oout, encoding="utf-8", errors="replace")
    diff = git("--no-pager diff --numstat HEAD", repo)
    changed = [l.split("\t") for l in diff.splitlines() if l.count("\t") == 2]
    diff_lines = sum(int(x[0]) + int(x[1]) for x in changed if x[0].isdigit() and x[1].isdigit())

    tdir = run / "transcripts"
    if a.auth == "api":
        found = transcripts(run / "cfg" / "projects", None)
    else:
        home = Path(os.environ.get("CLAUDE_CONFIG_DIR") or Path.home() / ".claude")
        found = transcripts(home / "projects", res.get("session_id")) if res.get("session_id") else []
    for f, rel in found:
        dst = tdir / rel
        dst.parent.mkdir(parents=True, exist_ok=True)
        shutil.copy2(f, dst)
    report, hook_errors = None, None
    if arm in ("C", "Cshadow"):
        c, o = sh(f'"{a.aaes}" report --json', run, env)
        (run / "aaes-report.json").write_text(o, encoding="utf-8", errors="replace")
        try:
            report = json.loads(o)
        except json.JSONDecodeError:
            report = None
        c, o = sh(f'"{a.aaes}" logs --errors --all', run, env)
        lines = [l for l in o.splitlines() if l.strip() and "client closed early" not in l]
        hook_errors = len(lines)
        (run / "aaes-errors.txt").write_text(o, encoding="utf-8", errors="replace")
    # The copied repo (with node_modules) is large; keep only the diff.
    (run / "diff.patch").write_text(git("--no-pager diff HEAD", repo), encoding="utf-8", errors="replace")
    if not a.keep_repo:
        rmtree(repo)
    return {
        "task": t["id"], "repo": t["repo"], "arm": arm, "rep": rep,
        "success": ocode == 0, "claude_exit": code, "wall_s": round(wall, 1),
        "cost_usd": res.get("total_cost_usd", 0.0), "num_turns": res.get("num_turns"),
        "is_error": res.get("is_error"), "terminal_reason": res.get("terminal_reason"),
        "result": (res.get("result") or "")[:200], "session_id": res.get("session_id"),
        "duration_api_ms": res.get("duration_api_ms"), "diff_lines": diff_lines,
        "files_changed": len(changed), "hook_errors": hook_errors,
        "aaes_report": bool(report), "model": a.model, "claude": a.claude_version,
        "ts": time.strftime("%Y-%m-%dT%H:%M:%S"),
    }


def main() -> int:
    sys.stdout.reconfigure(encoding="utf-8", errors="replace")
    ap = argparse.ArgumentParser()
    ap.add_argument("--work", type=Path, required=True)
    ap.add_argument("--claude", required=True)
    ap.add_argument("--aaes", required=True)
    ap.add_argument("--model", default="sonnet")
    ap.add_argument("--budget", type=float, required=True, help="USD cap (Claude Code reported cost)")
    ap.add_argument("--reps", type=int, default=3)
    ap.add_argument("--arms", default=",".join(ARMS))
    ap.add_argument("--tasks", help="comma-separated task ids (default: all)")
    ap.add_argument("--max-turns", type=int, default=60)
    ap.add_argument("--timeout", type=int, default=1200, help="seconds per agent run")
    ap.add_argument("--seed", type=int, default=20261113)
    ap.add_argument("--jobs", type=int, default=3, help="runs at the same time")
    ap.add_argument("--auth", choices=["api", "subscription"], default="api",
                    help="api: isolated config + AAES_ANTHROPIC_API_KEY; subscription: your login, "
                         "user settings/hooks/MCP left out, transcripts under ~/.claude/projects")
    ap.add_argument("--keep-repo", action="store_true")
    ap.add_argument("--prepare-only", action="store_true", help="build snapshots, run nothing")
    a = ap.parse_args()
    a.claude_version = subprocess.run([a.claude, "--version"], capture_output=True, text=True).stdout.strip()
    tasks = load_tasks(a.tasks.split(",") if a.tasks else None)
    arms = a.arms.split(",")
    print(f"{len(tasks)} tasks, arms {arms}, {a.reps} reps, {a.claude_version}, model {a.model}")
    for t in tasks:
        snapshot(a.work, t)
    if a.prepare_only:
        print("snapshots ready")
        return 0
    key = api_key() if a.auth == "api" else None
    res_path = a.work / "results" / "runs.jsonl"
    res_path.parent.mkdir(parents=True, exist_ok=True)
    done = []
    if res_path.exists():
        done = [json.loads(l) for l in res_path.read_text().splitlines() if l.strip()]
    seen = {(r["task"], r["arm"], r["rep"]) for r in done}
    spent = sum(r.get("cost_usd") or 0 for r in done)
    costs = [r["cost_usd"] for r in done if r.get("cost_usd")]
    rng = random.Random(a.seed)
    # Replications outermost: if the budget stops the pilot, the runs done are balanced.
    pending = []
    for rep in range(1, a.reps + 1):
        order = tasks[:]
        rng.shuffle(order)
        for t in order:
            arm_order = arms[:]
            rng.shuffle(arm_order)
            pending += [(rep, t, arm) for arm in arm_order if (t["id"], arm, rep) not in seen]

    lock = threading.Lock()
    state = {"spent": spent, "inflight": 0, "stop": None}

    def admit() -> bool:
        """Start another run only if spent + in-flight + this run stay under the budget."""
        with lock:
            if state["stop"]:
                return False
            mean = sum(costs) / len(costs) if costs else 1.0
            if state["spent"] + 1.5 * mean * (state["inflight"] + 1) > a.budget:
                state["stop"] = f"budget: spent ${state['spent']:.2f}, mean run ${mean:.2f}, cap ${a.budget:.2f}"
                return False
            state["inflight"] += 1
            return True

    def work(item):
        rep, t, arm = item
        print(f"[{time.strftime('%H:%M:%S')}] r{rep} {t['id']} {arm} ...", flush=True)
        try:
            r = one_run(a.work, t, arm, rep, a, key)
        except Exception as e:  # keep the pilot going; the run is retried on resume
            with lock:
                state["inflight"] -= 1
            print(f"    run failed: {e}", flush=True)
            return
        with lock:
            state["inflight"] -= 1
            state["spent"] += r["cost_usd"] or 0
            if r["cost_usd"]:
                costs.append(r["cost_usd"])
            msg = (r["result"] or "").lower()
            blocked = r["is_error"] and any(
                x in msg for x in ("credit balance", "authenticate", "usage limit", "rate limit"))
            if blocked:
                # Not a task outcome: stop, and leave the run out so a resume retries it.
                state["stop"] = f"agent could not run: {r['result'][:120]}"
            else:
                with res_path.open("a", encoding="utf-8") as f:
                    f.write(json.dumps(r) + "\n")
        print(f"    r{rep} {t['id']} {arm}: success={r['success']} cost=${r['cost_usd'] or 0:.3f} "
              f"turns={r['num_turns']} wall={r['wall_s']}s spent=${state['spent']:.2f} "
              f"{r['terminal_reason'] or ''}", flush=True)

    with ThreadPoolExecutor(max_workers=a.jobs) as ex:
        futures = []
        for item in pending:
            # Wait for a free worker before deciding, so the budget check sees finished runs.
            while sum(not f.done() for f in futures) >= a.jobs:
                time.sleep(2)
            if not admit():
                break
            futures.append(ex.submit(work, item))
        for f in futures:
            f.result()
    print(f"{'stopped: ' + state['stop'] if state['stop'] else 'done'}; spent ${state['spent']:.2f}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
