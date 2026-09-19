#!/usr/bin/env python3
"""Build a benchmark shard plan.

A plan is one day's (or one window's) work: an explicit, stable list of
run_ids of the form <suite>/<task_id>/<mode>/seed<N>. Plans are pure
functions of their inputs, so a re-generated plan is byte-identical and
resume logic never depends on wall-clock ordering.
"""
import argparse, hashlib, json, sys

MODES = ("raw", "simple", "ultra")

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
    env = {"model": a.model, "dataset_version": a.dataset_version,
           "harness_version": a.harness_version}
    plan = build_plan(a.suite, a.suite_file, task_ids,
                      tuple(a.modes.split(",")), tuple(int(s) for s in a.seeds.split(",")), env)
    with open(a.out, "w") as fh:
        json.dump(plan, fh, indent=2)
    print(f"plan: {len(plan['runs'])} runs across {len(task_ids)} tasks -> {a.out}", file=sys.stderr)

if __name__ == "__main__":
    main()
