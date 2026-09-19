#!/usr/bin/env python3
"""Aggregate results into comparable raw/Simple/Ultra numbers.

Rules (hard contract):
- Only COMPLETED triplets count: a (suite, task_id, seed) cell enters the
  aggregate only when raw, simple AND ultra all have status pass|fail for the
  same env pin (model, dataset version, harness version).
- Incomplete cells are listed, never silently dropped or partially counted.
- error/quota_stop records stay in the file and in the report as excluded.
- Intervals: Wilson 95% per-mode pass rate; paired bootstrap (10k resamples,
  fixed seed -> deterministic) for uplift deltas.
"""
import argparse, json, math, random, statistics, sys
from collections import defaultdict

MODES = ("raw", "simple", "ultra")

def wilson(k, n, z=1.959964):
    if n == 0:
        return (0.0, 0.0, 0.0)
    p = k / n
    d = 1 + z * z / n
    c = p + z * z / (2 * n)
    m = z * math.sqrt(p * (1 - p) / n + z * z / (4 * n * n))
    return (p, (c - m) / d, (c + m) / d)

def load(path):
    recs = defaultdict(list)
    with open(path) as fh:
        for line in fh:
            line = line.strip()
            if line:
                r = json.loads(line)
                recs[(r["suite"], r["task_id"], r["seed"], r["mode"])].append(r)
    return recs

def env_key(r):
    e = r.get("env", {})
    return (e.get("model"), e.get("dataset_version"), e.get("harness_version"))

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--results", required=True, nargs="+")
    ap.add_argument("--json-out", default="")
    a = ap.parse_args()
    recs = {}
    for path in a.results:
        for k, v in load(path).items():
            recs.setdefault(k, []).extend(v)
    cells = defaultdict(dict)  # (suite, task, seed) -> mode -> best record
    for (suite, task, seed, mode), rs in recs.items():
        good = [r for r in rs if r["status"] in ("pass", "fail")]
        if good:
            cells[(suite, task, seed)][mode] = good[-1]
    complete, incomplete = [], []
    for cell, modes in sorted(cells.items()):
        (complete if all(m in modes for m in MODES) else incomplete).append(cell)
    envs = {env_key(r) for cell in complete for r in cells[cell].values()}
    if len(envs) > 1:
        print(f"FATAL: mixed env pins across completed cells: {sorted(envs)}", file=sys.stderr)
        sys.exit(2)
    per_mode = {m: [] for m in MODES}
    tokens = {m: [] for m in MODES}
    wall = {m: [] for m in MODES}
    steps = {m: [] for m in MODES}
    for cell in complete:
        for m in MODES:
            r = cells[cell][m]
            per_mode[m].append(1 if r["status"] == "pass" else 0)
            if r.get("tokens_used") is not None: tokens[m].append(r["tokens_used"])
            if r.get("wall_ms") is not None: wall[m].append(r["wall_ms"])
            if r.get("steps") is not None: steps[m].append(r["steps"])
    n = len(complete)
    rng = random.Random(20260920)
    def boot_delta(m1, m2):
        if n == 0: return (0.0, 0.0, 0.0)
        idx = list(range(n))
        deltas = []
        for _ in range(10000):
            s = [idx[rng.randrange(n)] for _ in idx]
            d = sum(per_mode[m1][i] - per_mode[m2][i] for i in s) / n
            deltas.append(d)
        deltas.sort()
        return (sum(per_mode[m1]) / n - sum(per_mode[m2]) / n,
                deltas[250], deltas[9750])
    report = {"n_complete_triplets": n, "n_incomplete_cells": len(incomplete),
              "incomplete_cells": [{"suite": c[0], "task_id": c[1], "seed": c[2],
                                    "modes_present": sorted(cells[c])} for c in incomplete],
              "env": sorted(envs), "modes": {}}
    print(f"completed triplets: {n}   incomplete cells excluded: {len(incomplete)}")
    for m in MODES:
        k = sum(per_mode[m])
        p, lo, hi = wilson(k, n)
        report["modes"][m] = {"passed": k, "total": n, "pass_rate": p,
                              "wilson95": [lo, hi],
                              "tokens_median": statistics.median(tokens[m]) if tokens[m] else None,
                              "wall_ms_median": statistics.median(wall[m]) if wall[m] else None,
                              "steps_median": statistics.median(steps[m]) if steps[m] else None}
        print(f"  {m:6s} {k}/{n} = {p:.1%}  (Wilson95 {lo:.1%}..{hi:.1%})  "
              f"tokens_med={report['modes'][m]['tokens_median']}  wall_med_ms={report['modes'][m]['wall_ms_median']}")
    report["uplift"] = {}
    for m1, m2 in (("ultra", "raw"), ("ultra", "simple"), ("simple", "raw")):
        d, lo, hi = boot_delta(m1, m2)
        report["uplift"][f"{m1}_minus_{m2}"] = {"delta": d, "boot95": [lo, hi]}
        if n:
            print(f"  {m1} - {m2}: {d:+.1%} (boot95 {lo:+.1%}..{hi:+.1%})")
    if a.json_out:
        with open(a.json_out, "w") as fh:
            json.dump(report, fh, indent=2)

if __name__ == "__main__":
    main()
