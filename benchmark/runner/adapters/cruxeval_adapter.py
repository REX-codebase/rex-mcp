#!/usr/bin/env python3
"""CRUXEval (facebookresearch/cruxeval, MIT, 800 samples) -> rex-bench artifacts.

Two modes, both scored by blind execution:
- output prediction (default): prompt shows the function source and the input;
  the model writes solution.py defining OUTPUT. Staged test executes the
  dataset function on the dataset input and compares against OUTPUT.
- input prediction (--task input): prompt shows function and output; model
  writes INPUT_ARGS tuple; staged test asserts f(*INPUT_ARGS) == output.

Dataset: https://raw.githubusercontent.com/facebookresearch/cruxeval/main/data/cruxeval.jsonl
Pin: record the file sha256 in the shard env (registry.md).
"""
import argparse, hashlib, json, os, sys, urllib.request

URL = "https://raw.githubusercontent.com/facebookresearch/cruxeval/main/data/cruxeval.jsonl"

OUT_PROMPT = """Predict the output of this Python function on the given input. Create exactly one file named solution.py defining OUTPUT as the exact value (a Python literal). Do not define or modify the function itself; do not print anything.

Function:
```python
{code}
```

Input: f({input})"""

IN_PROMPT = """Predict an input that makes this Python function produce the given output. Create exactly one file named solution.py defining INPUT_ARGS as a tuple of arguments (a Python literal tuple). Do not define or modify the function; do not print anything.

Function:
```python
{code}
```

Required output: {output}"""

OUT_TEST = '''"""Auto-generated CRUXEval output-prediction check {sid}. Do not edit."""
import importlib.util

_CODE = {code!r}
_INPUT = {input!r}

_ns = {{}}
exec(_CODE, _ns)
_f = _ns["f"]
_expected = eval(f"f({{_INPUT}})", _ns)

def test_output_prediction():
    spec = importlib.util.spec_from_file_location("solution", "solution.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    assert m.OUTPUT == _expected, f"predicted {{m.OUTPUT!r}} != actual {{_expected!r}}"
'''

IN_TEST = '''"""Auto-generated CRUXEval input-prediction check {sid}. Do not edit."""
import importlib.util

_CODE = {code!r}
_EXPECTED = {output}

_ns = {{}}
exec(_CODE, _ns)
_f = _ns["f"]

def test_input_prediction():
    spec = importlib.util.spec_from_file_location("solution", "solution.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    assert _f(*m.INPUT_ARGS) == _EXPECTED, f"f(*{{m.INPUT_ARGS!r}}) != {{_EXPECTED!r}}"
'''

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--task", choices=["output", "input"], default="output")
    ap.add_argument("--out", required=True)
    ap.add_argument("--limit", type=int, default=0)
    a = ap.parse_args()
    data = urllib.request.urlopen(URL, timeout=60).read()
    sha = hashlib.sha256(data).hexdigest()
    rows = [json.loads(l) for l in data.decode().splitlines() if l.strip()]
    os.makedirs(a.out, exist_ok=True)
    suite_path = os.path.join(a.out, "suite.jsonl")
    if os.path.exists(suite_path):
        os.remove(suite_path)
    suite = f"cruxeval-{a.task}"
    n = 0
    for row in rows:
        sid = row["id"]
        stage = os.path.join(a.out, "stage", f"{suite}/{sid}", "tests")
        os.makedirs(stage, exist_ok=True)
        if a.task == "output":
            prompt = OUT_PROMPT.format(code=row["code"], input=row["input"])
            test = OUT_TEST.format(sid=sid, code=row["code"], input=row["input"])
        else:
            prompt = IN_PROMPT.format(code=row["code"], output=row["output"])
            test = IN_TEST.format(sid=sid, code=row["code"], output=row["output"])
        with open(os.path.join(stage, "test_solution.py"), "w") as fh:
            fh.write(test)
        with open(suite_path, "a") as fh:
            fh.write(json.dumps({
                "id": f"{suite}/{sid}",
                "prompt": prompt,
                "checks": [
                    {"kind": "file_exists", "path": "solution.py"},
                    {"kind": "command_succeeds",
                     "argv": ["python3", "-m", "pytest", "-q", "tests/test_solution.py"],
                     "timeout_ms": 60000},
                ],
            }) + "\n")
        n += 1
        if a.limit and n >= a.limit:
            break
    print(f"wrote {suite_path}: {n} tasks; dataset sha256 {sha}")

if __name__ == "__main__":
    main()
