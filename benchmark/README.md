# REX Ultra benchmark program

Goal (Agrim's contract): run every legally reproducible open coding benchmark,
raw vs Simple vs Ultra on ONE pinned model (Gemini 3.5 Flash-Lite, exact live
identifier verified against the API before the first scored run), ₹0/free-tier
only, blind executable scoring, resumable daily shards, numbers without spin.

## Layout
- `registry.md` — suite registry: exact versions, licenses, task counts,
  contamination caveats, resource needs, scorer commands, feasibility verdicts.
- `runner/plan.py` — pure shard-plan builder (stable run IDs
  `<suite>/<task_id>/<mode>/seed<N>`, env pin embedded).
- `runner/orchestrate.py` — executor: one rex-bench call per run, append-only
  results JSONL checkpoint, never reruns completed runs, clean quota stop
  (exit 75 + quota-stop.json with resume command).
- `runner/aggregate.py` — completed-triplets-only aggregation; Wilson 95% pass
  intervals; deterministic paired-bootstrap uplift intervals; incomplete cells
  and error/quota records listed, never cherry-picked.
- `runner/adapters/evalplus_adapter.py` — EvalPlus (HumanEval+/MBPP+).
- `runner/adapters/cruxeval_adapter.py` — CRUXEval output/input prediction.
- `runner/adapters/bigcodebench_adapter.py` — BigCodeBench Full/Hard (+libs.json sidecar).
- `runner/adapters/ds1000_adapter.py` — DS-1000 (snippet-insertion scoring).
Every committed adapter is scorer-proven on live dataset rows: canonical/reference
solution passes, broken solution fails (pytest, local). LiveCodeBench adapter is
WIP and deliberately excluded until it passes the same proof.
- `runner/mock_rex_bench.py` — deterministic pipeline stand-in. Never used for
  publishable numbers.
- `upstream-patch.md` — two rex-bench changes required before real runs.

## Daily-window protocol
1. Verify the live model identifier (models.list via the provider key) and pin
   it in the plan env. No run proceeds on a remembered identifier.
2. `plan.py` for the day's shard (suite slice sized to free-tier budget).
3. `orchestrate.py` until done or quota stop (exit 75). On quota stop: do NOT
   rerun anything; next window resumes from the checkpoint.
4. `aggregate.py` only over completed triplets from identical env pins.

## Contract compliance map
- Same model/config across raw/Simple/Ultra and all roles: enforced by plan
  env (`model` is one string consumed by every rex-bench invocation).
- Blind executable scoring: staged tests run post-run only; the scorer never
  sees model prose (rex-bench score_workspace re-executes).
- No cherry-picking: aggregator includes every completed triplet; incomplete
  and failed cells are printed in the report.
- Seeds: plan `--seeds 0,1,...` emits independent runs per seed; aggregation
  treats each (task, seed) cell separately.
- Quota: provider 429/RESOURCE_EXHAUSTED in the terminal string halts the
  shard cleanly with reason recorded.
- Key handling: model key only in the rex-harness FileSecretStore (0600);
  never in suite files, results, logs, or this repo.

## Validated state (2026-09-20, this sandbox)
- Pipeline end-to-end on mock: partial run -> resume -> quota stop (exit 75,
  marker) -> resume retries only the stopped run as attempt 2 -> shard
  completes -> aggregation byte-identical across runs.
- Scorers proven canonical-pass / broken-fail on live rows for: EvalPlus
  HumanEval+ and MBPP+, CRUXEval output and input, BigCodeBench (2 tasks),
  DS-1000 Pandas (2 tasks). LiveCodeBench pending.
- Upstream patches landed in the repo: rex-bench --stage-dir (hidden tests
  staged post-run, overwrite model-planted files, symlinks refused) and
  ToolRuntime::execute_trusted_scoring + verify_contract_with_scoring
  (verifier-only scoring allowlist; model-issued command_policy unchanged).
  75/75 rust tests pass including an end-to-end staged-pytest scoring test.
- NOT yet done (blockers): real model runs (secure key supply undecided),
  LiveCodeBench adapter validation, native evalplus.evaluate cross-check,
  SWE-bench Verified (needs docker).
