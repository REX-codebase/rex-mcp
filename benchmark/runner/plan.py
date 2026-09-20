#!/usr/bin/env python3
"""Build a benchmark shard plan.

A plan is one day's (or one window's) work: an explicit, stable list of
run_ids of the form <suite>/<task_id>/<mode>/seed<N>. Plans are pure
functions of their inputs, so a re-generated plan is byte-identical and
resume logic never depends on wall-clock ordering.
"""
import argparse, hashlib, json, sys

MODES = ("raw", "simple", "ultra")
POOLS = ("all", "dev", "heldout")


def load_pools(path):
    with open(path) as fh:
        cfg = json.load(fh)
    return cfg["split_seed"], cfg["suites"]


def pool_filter(suite, task_ids, pool, split_seed, assignments):
    """Return (kept_ids, role) for a pool. role: dev|heldout|split.

    dev/heldout suites belong wholly to their pool; split suites are
    partitioned deterministically per task id.
    """
    role = assignments.get(suite)
    if role is None:
        raise SystemExit(f"suite {suite} has no pool assignment in pools.json")
    if pool == "all":
        return list(task_ids), role
    if role == "split":
        kept = [t for t in task_ids
                if (int(hashlib.sha256(f"{split_seed}|{suite}|{t}".encode()).hexdigest(), 16) % 2 == 0)
                == (pool == "dev")]
        return kept, role
    # whole-suite pools
    if role == pool:
        return list(task_ids), role
    return [], role


def load_suite_task_ids(suite_path):
    ids = []
    with open(suite_path) as fh:
        for line in fh:
            line = line.strip()
            if not line or line.startswith("#"):
                continue
            ids.append(json.loads(line)["id"])
    if not ids:
        raise SystemExit(f"suite {suite_path} has no tasks")
    return ids

def build_plan(suite, suite_path, task_ids, modes, seeds, env_pin):
    runs = []
    for task_id in task_ids:
        for mode in modes:
            for seed in seeds:
                run_id = f"{suite}/{task_id}/{mode}/seed{seed}"
                runs.append({
                    "run_id": run_id,
                    "suite": suite,
                    "task_id": task_id,
                    "mode": mode,
                    "seed": seed,
                    "run_sha": hashlib.sha256(run_id.encode()).hexdigest()[:16],
                })
    return {"plan_version": 1, "suite": suite, "suite_path": suite_path,
            "env": env_pin, "runs": runs}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--suite", required=True, help="suite name, e.g. evalplus-humanevalplus")
    ap.add_argument("--suite-file", required=True, help="rex-bench suite JSONL")
    ap.add_argument("--tasks", default="", help="comma task ids; default: all in suite file")
    ap.add_argument("--task-range", default="", help="START:END slice into sorted suite ids (0-based, END exclusive)")
    ap.add_argument("--modes", default=",".join(MODES))
    ap.add_argument("--seeds", default="0", help="comma seed list, e.g. 0,1")
    ap.add_argument("--model", required=True, help="exact live model identifier")
    ap.add_argument("--pool", default="all", choices=POOLS,
                    help="improvement-loop pool filter (see pools.json); default all keeps historical behavior")
    ap.add_argument("--pools-file", default="", help="path to pools.json (required when --pool != all)")
    ap.add_argument("--dataset-version", required=True)
    ap.add_argument("--harness-version", required=True, help="rex-harness commit sha")
    ap.add_argument("--out", required=True)
    a = ap.parse_args()
    all_ids = load_suite_task_ids(a.suite_file)
    if a.tasks:
        wanted = set(a.tasks.split(","))
        task_ids = [t for t in all_ids if t in wanted]
        missing = wanted - set(task_ids)
        if missing:
            raise SystemExit(f"unknown task ids: {sorted(missing)}")
    elif a.task_range:
        start, _, end = a.task_range.partition(":")
        task_ids = all_ids[int(start): int(end) if end else None]
    else:
        task_ids = all_ids
    pool_role = None
    if a.pool != "all":
        if not a.pools_file:
            raise SystemExit("--pools-file is required when --pool != all")
        split_seed, assignments = load_pools(a.pools_file)
        task_ids, pool_role = pool_filter(a.suite, task_ids, a.pool, split_seed, assignments)
        if not task_ids:
            raise SystemExit(f"pool {a.pool} selects no tasks in suite {a.suite} (role {pool_role})")
    env = {"model": a.model, "dataset_version": a.dataset_version,
           "harness_version": a.harness_version, "pool": a.pool}
    plan = build_plan(a.suite, a.suite_file, task_ids,
                      tuple(a.modes.split(",")), tuple(int(s) for s in a.seeds.split(",")), env)
    plan["pool"] = a.pool
    if pool_role:
        plan["pool_role"] = pool_role
    with open(a.out, "w") as fh:
        json.dump(plan, fh, indent=2)
    print(f"plan: {len(plan['runs'])} runs across {len(task_ids)} tasks -> {a.out}", file=sys.stderr)

if __name__ == "__main__":
    main()
