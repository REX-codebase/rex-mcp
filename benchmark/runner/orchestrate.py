#!/usr/bin/env python3
"""Execute a shard plan against rex-bench with checkpointing, resume and
clean quota stops.

Contract:
- A run is COMPLETE when its record status is pass|fail. Completed runs are
  never re-executed. error|quota_stop runs are retried with attempt+1.
- One rex-bench invocation per run (uses --task filter), so a crash or quota
  stop loses at most the in-flight run.
- Quota exhaustion detection: scan the terminal string of each result for
  provider quota signatures; on hit, stop the shard cleanly, write
  quota-stop.json (reason, run_id, ts) and exit 75.
- Every record carries the env pin from the plan plus tokens, wall_ms,
  steps, approvals, terminal reason and per-check outcomes from rex-bench.
"""
import argparse, datetime, json, os, re, subprocess, sys

QUOTA_RE = re.compile(r"429|resource_exhausted|quota|rate.?limit|too many requests|daily limit", re.I)
TERMINAL_OK = re.compile(r".")  # any terminal string is acceptable evidence

def now_iso():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()

def load_records(path):
    recs = {}
    if os.path.exists(path):
        with open(path) as fh:
            for line in fh:
                line = line.strip()
                if line:
                    r = json.loads(line)
                    recs.setdefault(r["run_id"], []).append(r)
    return recs

def completed(recs, run_id):
    return any(r["status"] in ("pass", "fail") for r in recs.get(run_id, []))

def next_attempt(recs, run_id):
    return max([r["attempt"] for r in recs.get(run_id, [])], default=0) + 1

def append_record(path, rec):
    with open(path, "a") as fh:
        fh.write(json.dumps(rec) + "\n")

def run_one(rex_bench, plan, run, args, attempt):
    cmd = [rex_bench, "run", "--suite", plan["suite_path"], "--mode", run["mode"],
           "--provider", args.provider, "--model", plan["env"]["model"],
           "--out", args.raw_out, "--task", run["task_id"],
           "--runs-root", args.runs_root]
    if args.stage_dir:
        cmd += ["--stage-dir", args.stage_dir]
    started = datetime.datetime.now(datetime.timezone.utc)
    proc = subprocess.run(cmd, capture_output=True, text=True)
    # rex-bench appends the TaskResult as the last JSONL line of --out.
    result = None
    if os.path.exists(args.raw_out):
        with open(args.raw_out) as fh:
            lines = [l for l in fh.read().splitlines() if l.strip()]
        if lines:
            result = json.loads(lines[-1])
            os.remove(args.raw_out)
    finished = datetime.datetime.now(datetime.timezone.utc)
    if result is None:
        return {"run_id": run["run_id"], "attempt": attempt, "status": "error",
                "terminal": f"rex-bench produced no result; exit={proc.returncode}; "
                            f"stderr_tail={proc.stderr[-500:]!r}",
                "started_at": started.isoformat(), "finished_at": finished.isoformat()}
    terminal = result.get("terminal", "")
    if QUOTA_RE.search(terminal):
        status = "quota_stop"
    elif proc.returncode != 0 and not result.get("checks"):
        status = "error"
    else:
        status = "pass" if result.get("passed") else "fail"
    return {"run_id": run["run_id"], "attempt": attempt, "status": status,
            "tokens_used": result.get("tokens_used"), "wall_ms": result.get("wall_ms"),
            "steps": result.get("steps"), "approvals": result.get("approvals"),
            "terminal": terminal, "checks": result.get("checks"),
            "stdout_tail": proc.stdout[-1000:],
            "started_at": started.isoformat(), "finished_at": finished.isoformat()}

def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--plan", required=True)
    ap.add_argument("--rex-bench", required=True, help="path to rex-bench binary (or mock)")
    ap.add_argument("--results", required=True, help="append-only results JSONL (checkpoint)")
    ap.add_argument("--provider", default="gemini")
    ap.add_argument("--stage-dir", default="", help="staged hidden test files root")
    ap.add_argument("--runs-root", default="/tmp/rex-bench-runs")
    ap.add_argument("--raw-out", default="/tmp/rex-bench-last.jsonl")
    ap.add_argument("--limit", type=int, default=0, help="max runs this invocation (0=all pending)")
    args = ap.parse_args()
    plan = json.load(open(args.plan))
    os.makedirs(os.path.dirname(os.path.abspath(args.results)), exist_ok=True)
    recs = load_records(args.results)
    done_this_call = 0
    for run in plan["runs"]:
        rid = run["run_id"]
        if completed(recs, rid):
            continue
        if args.limit and done_this_call >= args.limit:
            break
        attempt = next_attempt(recs, rid)
        rec = run_one(args.rex_bench, plan, run, args, attempt)
        rec.update({"suite": plan["suite"], "task_id": run["task_id"],
                    "mode": run["mode"], "seed": run["seed"], "env": plan["env"],
                    "recorded_at": now_iso()})
        append_record(args.results, rec)
        recs.setdefault(rid, []).append(rec)
        done_this_call += 1
        print(f"{rid} attempt{attempt} -> {rec['status']}", flush=True)
        if rec["status"] == "quota_stop":
            marker = {"reason": rec["terminal"], "run_id": rid, "attempt": attempt,
                      "stopped_at": now_iso(),
                      "resume": f"python3 orchestrate.py --plan {args.plan} --rex-bench {args.rex_bench} "
                                f"--results {args.results} --provider {args.provider}"}
            with open(os.path.join(os.path.dirname(os.path.abspath(args.results)), "quota-stop.json"), "w") as fh:
                json.dump(marker, fh, indent=2)
            print("QUOTA STOP: shard halted cleanly; resume after the daily reset with the command in quota-stop.json", file=sys.stderr)
            sys.exit(75)
    pending = sum(1 for r in plan["runs"] if not completed(recs, r["run_id"]))
    print(f"shard state: {pending} runs pending", file=sys.stderr)

if __name__ == "__main__":
    main()
