#!/usr/bin/env python3
"""Deterministic stand-in for rex-bench: validates the orchestrator,
checkpointing, quota-stop and aggregation pipeline without a model key.
NOT a scorer and never used for publishable numbers."""
import argparse, hashlib, json, os, random, sys

PCT = {"raw": 0.55, "simple": 0.65, "ultra": 0.80}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("command")
    ap.add_argument("--suite"); ap.add_argument("--mode", default="raw")
    ap.add_argument("--provider", default="gemini"); ap.add_argument("--model", default="mock-model")
    ap.add_argument("--out", required=True); ap.add_argument("--task", default="")
    ap.add_argument("--runs-root", default="/tmp/mock-runs"); ap.add_argument("--stage-dir", default="")
    a, _ = ap.parse_known_args()
    if a.command != "run":
        sys.exit(2)
    quota_tasks = set(filter(None, os.environ.get("MOCK_QUOTA_TASKS", "").split(",")))
    tasks = [json.loads(l) for l in open(a.suite) if l.strip() and not l.startswith("#")]
    for t in tasks:
        if a.task and t["id"] != a.task:
            continue
        if a.task in quota_tasks:
            res = {"task_id": t["id"], "mode": a.mode, "passed": False, "checks": [],
                   "tokens_used": 0, "wall_ms": 0, "steps": 0, "approvals": 0,
                   "terminal": "provider_error: 429 RESOURCE_EXHAUSTED: daily free-tier quota exceeded"}
        else:
            h = hashlib.sha256(f"{t['id']}|{a.mode}|{a.model}".encode()).digest()
            passed = (h[0] / 255.0) < PCT[a.mode]
            rng = random.Random(int.from_bytes(h[:8], "big"))
            res = {"task_id": t["id"], "mode": a.mode, "passed": passed,
                   "checks": [{"id": "check-1", "proof": "command_succeeds", "passed": passed}],
                   "tokens_used": rng.randint(800, 4000), "wall_ms": rng.randint(2000, 20000),
                   "steps": rng.randint(1, 12), "approvals": rng.randint(0, 3),
                   "terminal": "Promoted" if passed else "RejectedByVerifier"}
        with open(a.out, "a") as fh:
            fh.write(json.dumps(res) + "\n")

if __name__ == "__main__":
    main()
