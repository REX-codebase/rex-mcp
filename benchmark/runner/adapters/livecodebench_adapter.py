#!/usr/bin/env python3
"""LiveCodeBench codegeneration (MIT) -> rex-bench artifacts.

Dataset: HF livecodebench/code_generation_lite. release_v6 = test.jsonl +
test2..test6 (1055 problems, May 2023 - Apr 2025). Files are streamed to
disk and parsed line-by-line (test.jsonl is 1.2GB - never load whole).
--release selects the cumulative window; --start-date/--end-date (YYYY-MM-DD,
contest_date) implement the paper's contamination fencing.

Two task shapes, both scored blind after the run:
- stdin/stdout (atcoder/codeforces): staged pytest runs `python3 solution.py`
  per test case via subprocess and compares exact stdout (whitespace-stripped).
- functional (leetcode, starter_code present): model writes solution.py with
  class Solution; staged pytest calls the metadata func_name on decoded JSON
  inputs and compares to expected outputs.

Simplifications vs the official runner (recorded, not hidden): output compare
is exact-after-strip (no float tolerance), private+public cases capped by
--max-cases (default 25 per task) to bound free-tier runtime; the cap is
recorded in the shard env. No canonical solutions ship with this dataset, so
scorer proof uses a synthetic row (see repo validation notes).
"""
import argparse, hashlib, json, os, sys, urllib.request

BASE = "https://huggingface.co/datasets/livecodebench/code_generation_lite/resolve/main/"
FILES = ["test.jsonl", "test2.jsonl", "test3.jsonl", "test4.jsonl", "test5.jsonl", "test6.jsonl"]
RELEASES = {f"release_v{i}": FILES[:i] for i in range(1, 7)}

STDIN_PROMPT = """Solve this competitive programming task. Create exactly one file named solution.py that reads the input from standard input and writes the answer to standard output exactly as specified. No extra prints.

{title}

{content}"""

FUNC_PROMPT = """Solve this programming task. Create exactly one file named solution.py defining `class Solution` with the required method (the starter code shows the signature). No prints.

{title}

{content}

Starter code:
```python
{starter}
```"""

STDIN_TEST = '''"""Auto-generated LiveCodeBench stdin check {qid}. Do not edit."""
import subprocess, sys

CASES = {cases!r}

def test_stdin_all_cases():
    for i, case in enumerate(CASES):
        p = subprocess.run([sys.executable, "solution.py"], input=case["input"],
                           capture_output=True, text=True, timeout=30)
        got = p.stdout.strip()
        want = case["output"].strip()
        assert p.returncode == 0, f"case {{i}} exited {{p.returncode}}: {{p.stderr[-300:]}}"
        assert got == want, f"case {{i}}: got {{got[:200]!r}} want {{want[:200]!r}}"
'''

FUNC_TEST = '''"""Auto-generated LiveCodeBench functional check {qid}. Do not edit."""
import importlib.util, inspect, json

CASES = {cases!r}
FUNC = {func!r}

def _load():
    spec = importlib.util.spec_from_file_location("solution", "solution.py")
    m = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(m)
    return m

def test_functional_all_cases():
    sol = _load().Solution()
    fn = getattr(sol, FUNC)
    for i, case in enumerate(CASES):
        args = json.loads(case["input"])
        arity = len(inspect.signature(fn).parameters)
        if arity == 1:
            args = [args]
        want = json.loads(case["output"])
        got = fn(*args)
        assert got == want, f"case {{i}}: got {{got!r}} want {{want!r}}"
'''

def stream_rows(path, out_dir):
    local = os.path.join(out_dir, "_src", path)
    os.makedirs(os.path.dirname(local), exist_ok=True)
    if not os.path.exists(local):
        print(f"fetching {path} ...", file=sys.stderr)
        with urllib.request.urlopen(BASE + path, timeout=120) as r, open(local, "wb") as w:
            while True:
                chunk = r.read(1 << 20)
                if not chunk:
                    break
                w.write(chunk)
    with open(local, "rb") as fh:
        for line in fh:
            line = line.strip()
            if line:
                yield json.loads(line)

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", required=True)
    ap.add_argument("--release", default="release_v6", choices=list(RELEASES))
    ap.add_argument("--start-date", default="")
    ap.add_argument("--end-date", default="")
    ap.add_argument("--max-cases", type=int, default=25)
    ap.add_argument("--limit", type=int, default=0)
    ap.add_argument("--files", nargs="*", default=None, help="override release file list (validation/debug only)")
    a = ap.parse_args()
    os.makedirs(a.out, exist_ok=True)
    suite_path = os.path.join(a.out, "suite.jsonl")
    if os.path.exists(suite_path):
        os.remove(suite_path)
    suite = f"livecodebench-{a.release}"
    n = 0
    files = a.files if a.files else RELEASES[a.release]
    for path in files:
        for row in stream_rows(path, a.out):
            date = (row.get("contest_date") or "")[:10]
            if a.start_date and date < a.start_date:
                continue
            if a.end_date and date > a.end_date:
                continue
            qid = f"{row['platform']}-{row['question_id']}".replace(" ", "_")
            pub = row.get("public_test_cases")
            priv = row.get("private_test_cases")
            cases = []
            for blob in (pub, priv):
                if not blob:
                    continue
                try:
                    cases += json.loads(blob)
                except Exception:
                    pass
            if not cases:
                continue
            cases = cases[: a.max_cases]
            functional = bool((row.get("starter_code") or "").strip())
            meta = row.get("metadata")
            try:
                meta = json.loads(meta) if isinstance(meta, str) else (meta or {})
            except Exception:
                meta = {}
            if functional and not meta.get("func_name"):
                continue  # cannot bind entry point; skip (counted in report)
            stage = os.path.join(a.out, "stage", f"{suite}/{qid}", "tests")
            os.makedirs(stage, exist_ok=True)
            if functional:
                prompt = FUNC_PROMPT.format(title=row.get("question_title", ""),
                                            content=row["question_content"],
                                            starter=row["starter_code"])
                test = FUNC_TEST.format(qid=qid, cases=cases, func=meta["func_name"])
            else:
                prompt = STDIN_PROMPT.format(title=row.get("question_title", ""),
                                             content=row["question_content"])
                test = STDIN_TEST.format(qid=qid, cases=cases)
            with open(os.path.join(stage, "test_solution.py"), "w") as fh:
                fh.write(test)
            with open(suite_path, "a") as fh:
                fh.write(json.dumps({
                    "id": f"{suite}/{qid}",
                    "prompt": prompt,
                    "checks": [
                        {"kind": "file_exists", "path": "solution.py"},
                        {"kind": "command_succeeds",
                         "argv": ["pytest", "-q", "tests/test_solution.py"],
                         "timeout_ms": 120000},
                    ],
                }) + "\n")
            n += 1
            if a.limit and n >= a.limit:
                break
        if a.limit and n >= a.limit:
            break
    sha = hashlib.sha256(open(suite_path, "rb").read()).hexdigest()
    print(f"wrote {suite_path}: {n} tasks ({a.release}); suite sha256 {sha}")

if __name__ == "__main__":
    main()
