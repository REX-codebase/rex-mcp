#!/usr/bin/env python3
"""DS-1000 (CC-BY-SA-4.0, 1000 tasks) -> rex-bench artifacts.

Dataset: HF xlangai/DS-1000 pinned commit 4416080ac5cb80bdf7576aefb8f9a0b4d5426a44
(no HF tags; the sha is the pin), split "test".

The model writes solution.py containing the answer CODE SNIPPET (it is
inserted at [insert] in the dataset exec_context, not imported). The staged
test embeds the dataset code_context verbatim and calls its own
test_execution(snippet) - blind execution with the dataset's own checks.
Tasks need their library (metadata.library) installed in the scoring env.
"""
import argparse, hashlib, json, os, sys

PROMPT_TMPL = """Solve this data-science problem. Create exactly one file named solution.py containing ONLY the solution code snippet. The snippet runs inside a prepared context that already imports the needed libraries and defines the input variables; it must assign its answer to the variable named `result` unless the problem states otherwise. Do not include imports, test code, or prints.

{prompt}"""

TEST_TMPL = '''"""Auto-generated DS-1000 check problem {pid} ({lib}). Do not edit."""
import pathlib

{code_context}

test_execution.__test__ = False  # keep pytest from collecting the dataset helper

def test_ds1000():
    snippet = pathlib.Path("solution.py").read_text()
    test_execution(snippet)
'''

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--library", default="", help="only tasks for this library (e.g. Pandas, Numpy)")
    a = ap.parse_args()
    from datasets import load_dataset
    pin = "4416080ac5cb80bdf7576aefb8f9a0b4d5426a44"
    ds = load_dataset("xlangai/DS-1000", revision=pin)["test"]
    os.makedirs(a.out, exist_ok=True)
    suite_path = os.path.join(a.out, "suite.jsonl")
    if os.path.exists(suite_path):
        os.remove(suite_path)
    libs_map = {}
    n = 0
    for pid, row in enumerate(ds):
        lib = row["metadata"]["library"]
        if a.library and lib.lower() != a.library.lower():
            continue
        tid = f"DS-1000/{pid}"
        suite = "ds1000"
        stage = os.path.join(a.out, "stage", f"{suite}/{tid}", "tests")
        os.makedirs(stage, exist_ok=True)
        with open(os.path.join(stage, "test_solution.py"), "w") as fh:
            fh.write(TEST_TMPL.format(pid=pid, lib=lib, code_context=row["code_context"]))
        with open(suite_path, "a") as fh:
            fh.write(json.dumps({
                "id": f"{suite}/{tid}",
                "prompt": PROMPT_TMPL.format(prompt=row["prompt"]),
                "checks": [
                    {"kind": "file_exists", "path": "solution.py"},
                    {"kind": "command_succeeds",
                     "argv": ["pytest", "-q", "tests/test_solution.py"],
                     "timeout_ms": 120000},
                ],
            }) + "\n")
        libs_map[f"{suite}/{tid}"] = [lib]
        n += 1
        if a.limit and n >= a.limit:
            break
    with open(os.path.join(a.out, "libs.json"), "w") as fh:
        json.dump(libs_map, fh, indent=1)
    sha = hashlib.sha256(open(suite_path, "rb").read()).hexdigest()
    print(f"wrote {suite_path}: {n} tasks (pin {pin}); suite sha256 {sha}")

if __name__ == "__main__":
    main()
