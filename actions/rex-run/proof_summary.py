#!/usr/bin/env python3
"""Build the REX proof-replay summary for GitHub Checks / PR comments.

Reads the run receipt and the `rex replay --json` report, then writes:
  - summary.md : markdown body posted to the check run and the PR comment
  - verdict.json : {"conclusion": "success"|"failure", "title": ...}

Never fails: a missing receipt or replay report is itself reported as a
failure verdict. Stdlib only.
"""

import json
import os
import sys

MARKER = "<!-- rex-proof-summary -->"


def load(path):
    try:
        with open(path) as f:
            return json.load(f)
    except (OSError, ValueError):
        return None


def main():
    receipt_path, replay_path, out_dir = sys.argv[1], sys.argv[2], sys.argv[3]
    os.makedirs(out_dir, exist_ok=True)

    receipt = load(receipt_path)
    replay = load(replay_path)
    problems = []
    if receipt is None:
        problems.append("run receipt is missing or unreadable")
    if replay is None:
        problems.append("proof-replay report is missing or unreadable")

    status = (receipt or {}).get("status", "unknown")
    if status != "completed":
        problems.append(f"run status is `{status}`, not `completed`")
    checks = (replay or {}).get("checks", [])
    failed = [c for c in checks if c.get("verdict") == "fail"]
    for c in failed:
        problems.append(f"replay check `{c.get('name')}` failed: {c.get('detail')}")

    ok = not problems
    conclusion = "success" if ok else "failure"
    title = "verified by REX" if ok else "REX verification failed"

    r = receipt or {}
    run_id = r.get("run_id", "?")
    bundle = os.environ.get("REX_BUNDLE_URL", "")
    lines = [
        MARKER,
        f"## {'✅' if ok else '❌'} {title}",
        "",
        f"Run `{run_id}` — `{r.get('provider', '?')}` / `{r.get('model', '?')}`",
        "",
        "| claim | value |",
        "|---|---|",
        f"| status | `{status}` |",
        f"| steps | `{r.get('steps', '?')}` / `{r.get('max_steps', '?')}` |",
        f"| tool calls | `{r.get('tool_calls', '?')}` / `{r.get('max_tool_calls', '?')}` |",
        f"| tokens | `{r.get('tokens_used', '?')}` / `{r.get('max_tokens', '?')}` |",
        f"| cost ceiling | `{r.get('cost_usd_ceiling', '?')}` |",
        f"| cost estimate | `{r.get('cost_usd_estimate', '?')}` |",
        f"| bid met | `{r.get('bid_met', '?')}` |",
        "",
        "### proof replay",
        "",
    ]
    if replay:
        lines.append(
            f"`{replay.get('passed', 0)}` passed, "
            f"`{replay.get('failed', 0)}` failed, "
            f"`{replay.get('skipped', 0)}` skipped"
        )
        lines.append("")
        for c in checks:
            mark = {"pass": "✅", "fail": "❌", "skip": "⏭️"}.get(c.get("verdict"), "?")
            lines.append(f"- {mark} `{c.get('name')}` — {c.get('detail')}")
    else:
        lines.append("replay report unavailable")
    if bundle:
        lines += ["", f"[run bundle (receipt artifact)]({bundle})"]
    lines.append("")

    with open(os.path.join(out_dir, "summary.md"), "w") as f:
        f.write("\n".join(lines))
    with open(os.path.join(out_dir, "verdict.json"), "w") as f:
        json.dump({"conclusion": conclusion, "title": title}, f)
    print(conclusion)


if __name__ == "__main__":
    main()
