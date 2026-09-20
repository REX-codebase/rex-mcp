# REX Ultra improvement loop (design, v1 - 2026-09-20)

Owner instruction (Agrim, 2026-09-20 08:47 IST, via WhatsApp): "keep
iterating ultra until we make a flash lite model reach opus 5 level."
This document defines how that iteration runs. It is a design contract;
every claim here is a rule, not a result.

## 1. Goal and evidence bar

- Objective: raise REX Ultra's measured coding ability with the fixed
  worker model `gemini-3.5-flash-lite` until held-out results reach the
  Opus 5-level reference band.
- "Opus 5-level" is an EVIDENCE TARGET, never a claim. A claim exists
  only when held-out suites (section 3), run under the measurement
  contract (section 5), show it. Reference numbers come from published
  frontier-model results on the same suites, recorded with their source
  URLs at the time of comparison.
- The worker model NEVER changes: exact identifier
  `gemini-3.5-flash-lite`, identical model and config across raw /
  Simple / Ultra and across every internal role. No substitute model,
  ever. What changes between iterations is the Ultra harness only:
  prompts, tool policy, scaffolding, retry/context strategy.
- Mock results, smoke shards, and dev-pool numbers are never reported
  as capability. Every reported number carries its pool label.

## 2. Suite pools (contamination control)

Pool assignments live in runner/pools.json and are pinned by
`split_seed`. Changing a suite's pool is a committed, justified event.

- dev: tuning pool. Iteration decisions are made here.
  cruxeval-output, cruxeval-input, plus the dev halves of
  evalplus-mbppplus, ds1000, bigcodebench-full.
- heldout: final pool. Never used for iteration decisions.
  evalplus-humanevalplus, livecodebench (release_v6), bigcodebench-hard,
  plus the heldout halves of the split suites. SWE-bench Verified joins
  heldout when its Docker/storage requirement is met.
- split suites are partitioned deterministically per task id by
  sha256(split_seed|suite|task_id); the partition is a pure function,
  reproducible by anyone with pools.json.

Rules:
- No iteration decision may use heldout numbers.
- Heldout runs happen only at declared milestones.
- Failure analysis on heldout is class-level only (error categories,
  counts) - never task-level answers, never prompt tuning against
  heldout tasks.
- The improvement loop tunes against dev; the headline raw/Simple/Ultra
  benchmark matrix (the original program) measures on full suites and
  stays separate from tuning decisions.

## 3. Iteration protocol (one change at a time)

1. BASELINE: current Ultra vs raw vs Simple on the dev pool, multiple
   seeds, uncertainty intervals. Recorded as iteration 0.
2. FAILURE CLUSTERING: cluster dev failures by class (parse/format
   failure, wrong algorithm, edge-case miss, tool misuse, context loss,
   truncation/timeout). Pick the largest addressable cluster.
3. HYPOTHESIS: exactly one harness change targeting that cluster.
   Hypothesis and expected effect are written down BEFORE the run.
4. EVAL: dev pool, identical seeds as baseline, blind executable
   scorers. Delta reported with uncertainty intervals.
5. REGRESSION MATRIX: the change is also scored on every other dev
   suite, including ones it does not target. A keep requires no
   regression beyond noise on any dev suite.
6. DECISION: keep or revert, recorded in the iteration ledger
   (runner/ledger.md) with all numbers. Reverted iterations stay in
   the ledger. No cherry-picking: every iteration is logged, kept or
   not.
7. MILESTONE: after a declared number of kept changes (default 3),
   one heldout evaluation. Its numbers are reported as the current
   evidence position vs the Opus 5-level reference band.

## 4. Measurement contract (carried over, unchanged)

- Stable task/run ids, pinned env, seeds, attempt history, checkpoints,
  scorer outputs, quota-stop reason, tokens, latency, tool call counts
  per run.
- Aggregate only completed comparable triplets (raw/Simple/Ultra on the
  same task and seed).
- Blind executable scoring; hidden tests staged by the adapters;
  answers never enter model-visible context.
- Uncertainty intervals on all pass rates.
- Cost/latency recorded per run and per iteration: tokens in/out, wall
  time, RPM/RPD consumption against the 15 RPM / 250K TPM / 500 RPD
  free-tier budget.
- Quota discipline: headroom under 500 RPD, clean stop on 429, resume
  after midnight Pacific without rerunning completed runs.
- All failures preserved. Nothing deleted. No spin. No percentages in
  user-facing pings.

## 5. Anti-contamination controls

- Suite source pins in MANIFEST.md; no dataset content in git.
- Pool assignments pinned in runner/pools.json; the split partition is
  deterministic and auditable.
- Worker is a fixed API model; the contamination risk that matters is
  benchmark leakage into OUR tuning, controlled by sections 2-3.
- Notebook/Colab execution was dismantled 2026-09-20 per owner
  instruction; no benchmark content ever ran there (zero real runs).

## 6. Execution state (current blocker, exact)

Real runs need the Gemini key inside REX's FileSecretStore in the
execution environment. Today that is impossible without disclosure:

- `tools vault fill` can only type into browser inputs; it cannot
  export a secret to a file or environment.
- The REX execution environment (sandbox rex-dev-server / rex-bench)
  has no browser-reachable surface - proven 2026-09-20: cloud browser
  to 127.0.0.1:8787 refused, sandbox has no routable inbound address.
- Colab route: dismantled per owner instruction (would have worked via
  Colab Secrets UI, but the owner directed REX-native execution).
- Laptop route: discarded (owner's PC is broken).

So real-key runs are BLOCKED until a fill path exists that does not
disclose or relay the secret. Candidates (need an owner decision or new
tooling, not invented workarounds):
a. A browser-reachable REX Harness surface the vault can fill directly.
b. Agrim types/pastes the key into a vault-secured field on a device
   that can reach the REX runtime (e.g. his phone, if the Harness can
   be reached there - currently no such surface).
c. New tooling that lets the vault write a 0600 file in the execution
   environment directly.

Non-key work continues regardless: registry upkeep, pool-split runner
support, failure-clustering tooling on mock data, scorer improvements,
and this design's implementation.

## 7. Versioned durability

- Every iteration: commit (code + ledger + numbers) to private main
  (REX-codebase) plus a round-trip-verified Drive artifact.
- results.jsonl per shard, aggregate report per iteration, both
  checksummed; Drive copies verified by download-and-compare.
