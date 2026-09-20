#!/usr/bin/env python3
"""Materialize frozen Phase-1 task manifests without executing model calls.

This tool deliberately does not download datasets or run scorers. Feed it task
IDs exported from the exact pinned public source after phase1-freeze.json has
all required pins. It deterministically writes disjoint dev/held-out IDs and
hashes for review, before either pool is executed.
"""
import argparse, hashlib, json
from pathlib import Path

FREEZE = Path(__file__).resolve().parents[2] / "phase1-freeze.json"

def digest(lines):
    return hashlib.sha256(("\n".join(lines) + "\n").encode()).hexdigest()

def bucket(suite, task_id):
    raw = f"phase1-v1|{suite}|{task_id}".encode()
    return int(hashlib.sha256(raw).hexdigest(), 16) % 10

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--suite", choices=["swebench-verified", "deepswe-v1.1"], required=True)
    ap.add_argument("--task-ids", type=Path, required=True, help="one public task id per line")
    ap.add_argument("--out", type=Path, required=True)
    a = ap.parse_args()
    freeze = json.loads(FREEZE.read_text())
    cfg = freeze["suites"][a.suite]
    required = ("dataset_revision", "harness_commit", "agent_harness_commit_or_release") if a.suite == "swebench-verified" else ("dataset_commit",)
    missing = [k for k in required if not cfg.get(k)]
    if missing:
        raise SystemExit("freeze incomplete; fill exact public pins first: " + ", ".join(missing))
    ids = sorted(set(line.strip() for line in a.task_ids.read_text().splitlines() if line.strip()))
    if not ids:
        raise SystemExit("task id list is empty")
    dev = [x for x in ids if bucket(a.suite, x) in (0, 1)]
    heldout = [x for x in ids if bucket(a.suite, x) not in (0, 1)]
    if set(dev) & set(heldout) or len(dev) + len(heldout) != len(ids):
        raise AssertionError("split is not disjoint and exhaustive")
    a.out.mkdir(parents=True, exist_ok=True)
    for name, rows in (("all", ids), ("dev", dev), ("heldout", heldout)):
        (a.out / f"{name}.txt").write_text("\n".join(rows) + "\n")
    manifest = {"suite": a.suite, "freeze_version": freeze["freeze_version"], "counts": {"all":len(ids),"dev":len(dev),"heldout":len(heldout)}, "sha256": {"all":digest(ids),"dev":digest(dev),"heldout":digest(heldout)}}
    (a.out / "manifest.json").write_text(json.dumps(manifest, indent=2) + "\n")
    print(json.dumps(manifest, sort_keys=True))
if __name__ == "__main__": main()
