#!/usr/bin/env python3
"""Convert EvalPlus datasets (HumanEval+ / MBPP+, evalplus==0.3.1 schema) into
rex-bench artifacts.

Outputs:
  <out>/suite.jsonl      BenchTask lines: {id, prompt, checks}
  <out>/stage/<task_id>/tests/test_solution.py   hidden tests, staged by
                         rex-bench --stage-dir at scoring time only

Scoring semantics: blind executable comparison of the candidate entry point
against the dataset canonical solution over base_input + plus_input (the "+"
tests), with the dataset atol for float outputs. This mirrors evalplus's own
harness semantics; for publishable numbers, the produced solution files are
additionally re-scored with the native `evalplus.evaluate` as a cross-check
(recorded in the shard report). The dataset `contract` field (input-validation
hints upstream injects into solutions) is not enforced - noted as a slight
leniency.

Provenance (registry.md): HumanEval+ fetched from evalplus/humanevalplus_release
v0.1.10 (164 tasks, MIT upstream); MBPP+ dataset v0.2.0 lineage (378 tasks,
CC-BY-4.0 upstream). Record the resolved download URL + sha256 in shard env.
"""
import argparse, hashlib, json, os, sys

PROMPT_TMPL = """Solve this programming task. Create exactly one file named solution.py in the workspace root. It must define the required entry point exactly as specified. Do not print anything at import time.

{prompt}"""

TEST_TMPL = '''"""Auto-generated from EvalPlus task {task_id}. Do not edit."""
import importlib.util

_CANONICAL_SRC = {canonical_src!r}
_ENTRY = {entry_point!r}
_INPUTS = {inputs!r}
_ATOL = {atol!r}

def _load(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m

_ns = {{}}
exec(_CANONICAL_SRC, _ns)
_expected = _ns[_ENTRY]

def test_against_canonical_outputs():
    mod = _load("solution.py", "solution")
    candidate = getattr(mod, _ENTRY)
    for inp in _INPUTS:
        got = candidate(*inp)
        want = _expected(*inp)
        if _ATOL is not None and isinstance(want, float):
            assert abs(got - want) <= _ATOL, f"inputs={{inp}}: {{got}} != {{want}} (atol={{_ATOL}})"
        else:
            assert got == want, f"inputs={{inp}}: {{got}} != {{want}}"
'''

def emit(out_root, suite_name, task_id, prompt, checks, test_src):
    stage_dir = os.path.join(out_root, "stage", suite_name, task_id, "tests")
    os.makedirs(stage_dir, exist_ok=True)
    with open(os.path.join(stage_dir, "test_solution.py"), "w") as fh:
        fh.write(test_src)
    with open(os.path.join(out_root, "suite.jsonl"), "a") as fh:
        fh.write(json.dumps({"id": f"{suite_name}/{task_id}", "prompt": prompt,
                             "checks": checks}) + "\n")

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--dataset", choices=["humanevalplus", "mbppplus"], required=True)
    ap.add_argument("--out", required=True)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--max-plus-inputs", type=int, default=0,
                    help="cap plus_input per task (0 = all; record in shard env if capped)")
    a = ap.parse_args()
    try:
        from evalplus.data import get_human_eval_plus, get_mbpp_plus
    except ImportError:
        sys.exit("evalplus package missing: pip install evalplus==0.3.1")
    os.makedirs(a.out, exist_ok=True)
    suite_path = os.path.join(a.out, "suite.jsonl")
    if os.path.exists(suite_path):
        os.remove(suite_path)
    data = get_human_eval_plus() if a.dataset == "humanevalplus" else get_mbpp_plus()
    suite_name = f"evalplus-{a.dataset}"
    checks = [{"kind": "file_exists", "path": "solution.py"},
              {"kind": "command_succeeds",
               "argv": ["python3", "-m", "pytest", "-q", "tests/test_solution.py"],
               "timeout_ms": 120000}]
    n = 0
    for task_id in sorted(data, key=lambda x: int(x.split("/")[-1])):
        t = data[task_id]
        inputs = list(t.get("base_input", [])) + list(t.get("plus_input", []))
        if a.max_plus_inputs and t.get("plus_input"):
            inputs = list(t.get("base_input", [])) + list(t.get("plus_input", []))[: a.max_plus_inputs]
        canonical = (t["prompt"] + t["canonical_solution"]) if a.dataset == "humanevalplus" else t["canonical_solution"]
        test_src = TEST_TMPL.format(task_id=task_id, canonical_src=canonical,
                                    entry_point=t["entry_point"], inputs=inputs,
                                    atol=t.get("atol"))
        emit(a.out, suite_name, task_id, PROMPT_TMPL.format(prompt=t["prompt"]), checks, test_src)
        n += 1
        if a.limit and n >= a.limit:
            break
    sha = hashlib.sha256(open(suite_path, "rb").read()).hexdigest()
    print(f"wrote {suite_path}: {n} tasks, suite sha256 {sha}")

if __name__ == "__main__":
    main()
