#!/usr/bin/env python3
"""BigCodeBench (Apache-2.0) -> rex-bench artifacts.

Dataset: HF bigcode/bigcodebench, config v0.1.4, pinned commit
b74c0d0bf70d2c0bc459be537895cca163007f1a (no HF tags exist; the sha is the pin).
Full = 1140 tasks; --hard filters to the official BigCodeBench-Hard 148
(task ids read from bigcode/bigcodebench-hard pinned commit
298d2cc7b96612e15e47313c3603ee124cee0c1f, last modified 2025-02-23).

Tasks need the PyPI libs each task names (field `libs`); the scoring sandbox
must have them installed (see runner README - env pin records the lib set).
Staged test: `from solution import *` plus the dataset's unittest code,
executed blind by pytest after the run.
"""
import argparse, hashlib, json, os, sys

PROMPT_TMPL = """Solve this programming task. Create exactly one file named solution.py in the workspace root implementing the required entry point exactly as specified. Do not print anything at import time.

{prompt}"""

TEST_TMPL = '''"""Auto-generated BigCodeBench check {task_id}. Do not edit."""
import os, sys
sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
from solution import *

{test}
'''

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--hard", action="store_true", help="BigCodeBench-Hard 148 subset")
    a = ap.parse_args()
    from datasets import load_dataset
    pin = "b74c0d0bf70d2c0bc459be537895cca163007f1a"
    ds = load_dataset("bigcode/bigcodebench", revision=pin)
    split = ds["v0.1.4"]
    hard_ids = None
    if a.hard:
        hard = load_dataset("bigcode/bigcodebench-hard", revision="298d2cc7b96612e15e47313c3603ee124cee0c1f")
        hsplit = hard[list(hard.keys())[0]]
        hard_ids = set(hsplit["task_id"])
    os.makedirs(a.out, exist_ok=True)
    suite_path = os.path.join(a.out, "suite.jsonl")
    if os.path.exists(suite_path):
        os.remove(suite_path)
    suite = "bigcodebench-hard" if a.hard else "bigcodebench-full"
    libs_map = {}
    n = 0
    for row in sorted(split, key=lambda r: int(r["task_id"].split("/")[-1])):
        tid = row["task_id"]
        if hard_ids is not None and tid not in hard_ids:
            continue
        stage = os.path.join(a.out, "stage", f"{suite}/{tid}", "tests")
        os.makedirs(stage, exist_ok=True)
        with open(os.path.join(stage, "test_solution.py"), "w") as fh:
            fh.write(TEST_TMPL.format(task_id=tid, test=row["test"]))
        with open(suite_path, "a") as fh:
            fh.write(json.dumps({
                "id": f"{suite}/{tid}",
                "prompt": PROMPT_TMPL.format(prompt=row["complete_prompt"]),
                "checks": [
                    {"kind": "file_exists", "path": "solution.py"},
                    {"kind": "command_succeeds",
                     "argv": ["python3", "-m", "pytest", "-q", "tests/test_solution.py"],
                     "timeout_ms": 120000},
                ],
            }) + "\n")
        libs_map[f"{suite}/{tid}"] = row["libs"]
        n += 1
        if a.limit and n >= a.limit:
            break
    with open(os.path.join(a.out, "libs.json"), "w") as fh:
        json.dump(libs_map, fh, indent=1)
    sha = hashlib.sha256(open(suite_path, "rb").read()).hexdigest()
    print(f"wrote {suite_path}: {n} tasks (dataset pin {pin}); suite sha256 {sha}")

if __name__ == "__main__":
    main()
