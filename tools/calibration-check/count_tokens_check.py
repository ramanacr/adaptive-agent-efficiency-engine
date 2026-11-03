#!/usr/bin/env python3
"""Validate `aaes mine`'s calibrated bytes/token against Anthropic's count_tokens API.

Samples real tool results from local Claude Code transcripts, counts their tokens with the
`/v1/messages/count_tokens` endpoint for the model that produced each session, and prints
aggregate ratios only (no content).

Privacy: the sampled tool-output text is sent to the Anthropic API, the same provider that
originally processed it in those sessions. Nothing is stored.

Usage:
    ANTHROPIC_API_KEY=... python count_tokens_check.py [--samples 60] [--expected 2.28]

Stdlib only. Exit code 0 on success, 2 if no usable samples were counted.
"""

from __future__ import annotations

import argparse
import json
import os
import random
import statistics
import sys
import time
import urllib.error
import urllib.request
from pathlib import Path

API = "https://api.anthropic.com/v1/messages/count_tokens"


def default_root() -> Path:
    base = os.environ.get("CLAUDE_CONFIG_DIR")
    return Path(base) / "projects" if base else Path.home() / ".claude" / "projects"


def iter_results(root: Path, min_bytes: int, max_bytes: int):
    """Yield (model, text) for text-only tool results within the size window."""
    for f in sorted(root.rglob("*.jsonl")):
        model = None
        try:
            with open(f, encoding="utf-8", errors="replace") as fh:
                for line in fh:
                    try:
                        o = json.loads(line)
                    except ValueError:
                        continue
                    m = o.get("message")
                    if not isinstance(m, dict):
                        continue
                    if o.get("type") == "assistant":
                        mm = m.get("model")
                        if isinstance(mm, str) and not mm.startswith("<"):
                            model = mm
                        continue
                    if o.get("type") != "user" or not isinstance(m.get("content"), list):
                        continue
                    for b in m["content"]:
                        if not isinstance(b, dict) or b.get("type") != "tool_result":
                            continue
                        c = b.get("content")
                        if isinstance(c, list):
                            if any(isinstance(x, dict) and x.get("type") == "image" for x in c):
                                continue
                            c = "".join(
                                x.get("text", "") for x in c if isinstance(x, dict) and x.get("type") == "text"
                            )
                        if not isinstance(c, str) or model is None:
                            continue
                        n = len(c.encode("utf-8"))
                        if min_bytes <= n <= max_bytes:
                            yield model, c
        except OSError:
            continue


def reservoir(it, k: int, rng: random.Random):
    sample = []
    for i, item in enumerate(it):
        if i < k:
            sample.append(item)
        else:
            j = rng.randint(0, i)
            if j < k:
                sample[j] = item
    return sample


def count_tokens(key: str, model: str, text: str, retries: int = 4) -> int:
    body = json.dumps({"model": model, "messages": [{"role": "user", "content": text}]}).encode()
    for attempt in range(retries):
        req = urllib.request.Request(
            API,
            data=body,
            headers={
                "x-api-key": key,
                "anthropic-version": "2023-06-01",
                "content-type": "application/json",
            },
        )
        try:
            with urllib.request.urlopen(req, timeout=60) as r:
                return int(json.load(r)["input_tokens"])
        except urllib.error.HTTPError as e:
            if e.code in (429, 529) and attempt + 1 < retries:
                time.sleep(float(e.headers.get("retry-after") or 2 * (attempt + 1)))
                continue
            detail = e.read().decode("utf-8", "replace")[:200]
            raise RuntimeError(f"HTTP {e.code}: {detail}") from None
    raise RuntimeError("retries exhausted")


def quartiles(v: list[float]) -> tuple[float, float, float]:
    if len(v) < 4:
        m = statistics.median(v)
        return m, m, m
    q = statistics.quantiles(v, n=4)
    return q[0], q[1], q[2]


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n\n")[0])
    ap.add_argument("--root", type=Path, default=default_root())
    ap.add_argument("--samples", type=int, default=60)
    ap.add_argument("--min-bytes", type=int, default=4000, help="same floor as aaes calibration")
    ap.add_argument("--max-bytes", type=int, default=60000)
    ap.add_argument("--expected", type=float, default=None, help="bytes/token reported by aaes mine")
    ap.add_argument("--seed", type=int, default=20261103)
    a = ap.parse_args()

    key = os.environ.get("ANTHROPIC_API_KEY")
    if not key:
        print("ANTHROPIC_API_KEY is not set", file=sys.stderr)
        return 2

    rng = random.Random(a.seed)
    sample = reservoir(iter_results(a.root, a.min_bytes, a.max_bytes), a.samples, rng)
    print(f"sampled {len(sample)} tool results ({a.min_bytes}-{a.max_bytes} bytes) from {a.root}")

    baseline: dict[str, int] = {}
    errors: dict[str, str] = {}
    per_model: dict[str, list[tuple[int, int]]] = {}
    for model, text in sample:
        if model in errors:
            continue
        try:
            if model not in baseline:
                baseline[model] = count_tokens(key, model, "x") - 1  # message framing
            tokens = count_tokens(key, model, text) - baseline[model]
        except RuntimeError as e:
            errors[model] = str(e)
            continue
        if tokens > 0:
            per_model.setdefault(model, []).append((len(text.encode("utf-8")), tokens))
        time.sleep(0.15)

    all_pairs = [p for v in per_model.values() for p in v]
    if not all_pairs:
        print("no samples counted", file=sys.stderr)
        for m, e in errors.items():
            print(f"  {m}: {e}", file=sys.stderr)
        return 2

    def summarize(label: str, pairs: list[tuple[int, int]]) -> float:
        ratios = sorted(b / t for b, t in pairs)
        q1, med, q3 = quartiles(ratios)
        weighted = sum(b for b, _ in pairs) / sum(t for _, t in pairs)
        print(
            f"{label:<28} n={len(pairs):<4} median={med:.2f} IQR={q1:.2f}-{q3:.2f} "
            f"weighted(sum bytes/sum tokens)={weighted:.2f}"
        )
        return weighted

    print("\nbytes/token measured with count_tokens:")
    for m, pairs in sorted(per_model.items()):
        summarize(m, pairs)
    overall = summarize("ALL", all_pairs)
    med_all = statistics.median(sorted(b / t for b, t in all_pairs))
    if a.expected:
        print(
            f"\naaes calibrated value {a.expected:.2f} vs API median {med_all:.2f} "
            f"({(a.expected / med_all - 1) * 100:+.1f}%), vs API weighted {overall:.2f} "
            f"({(a.expected / overall - 1) * 100:+.1f}%)"
        )
    for m, e in errors.items():
        print(f"skipped model {m}: {e}", file=sys.stderr)
    return 0


if __name__ == "__main__":
    sys.exit(main())
