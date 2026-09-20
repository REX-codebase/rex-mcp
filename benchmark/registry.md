# REX Ultra benchmark registry - Opus 5 / exactly-reproducible scope

Re-scoped 2026-09-20. A suite can receive real model calls only if it has an
official Claude Opus 5 result and an openly reproducible task set plus scorer.
All earlier registry entries remain git history only; no further model calls
may be made on them.

## Phase 1 - adapter preparation

### SWE-bench Verified
- Official comparator: Claude Opus 5 96.0 (Anthropic System Card section 8.2).
- Open benchmark: 500 tasks, `SWE-bench/SWE-bench_Verified`.
- Open scorer: `swe-bench/swe-bench` Docker F2P/P2P evaluation.
- Public comparison harness: mini-SWE-agent bash-only. Anthropic's own scaffold
  is undisclosed, so 96.0 is not an exact scaffold reproduction.
- Freeze exact dataset/harness/image pins and dev/held-out task ids before any
  held-out execution.

### DeepSWE v1.1
- Preserve two comparator records, never merge them:
  1. Anthropic System Card section 8.3: 68.8, average of 5 trials.
  2. Datacurve v1.1 leaderboard (updated 2026-09-03): 74% +/- 4%, max effort.
- Open tasks/verifiers: `datacurve-ai/deep-swe`, Apache-2.0.
- Public harness: Pier + mini-SWE-agent pinned at `adfe2023`, 9000 s.
- Compare only against the record matching the reproduced version/config.
- Freeze exact repo/task/harness pins and dev/held-out task ids before held-out
  execution.

## Phase 2

### SWE-bench Multilingual
Open 300-task benchmark and scorer. Prepare only after Phase 1.

### SWE-bench Pro
Public 731-task split and open scorer are reproducible. Opus 5's 79.2 result
is NON-EXACT unless first-party evidence proves it used that public split.
Never infer the split from a leaderboard convention.

## Gated / excluded

### SWE-bench Multimodal v2 - gated
Open v2 (480 tasks) is a new benchmark. Anthropic's 59.4 is directional only,
not apples-to-apples (different split and internal modifications). Google's exact-model page confirms `gemini-3.5-flash-lite` accepts Image input.
This clears the model-capability gate only; v2 still needs exact dataset/tooling
pins before adapter work.

### FrontierBench v0.1 - registry-only
Pinned public tasks/scorer exist. Retain only after an oracle dry-run proves the
public task and scorer execute at ₹0 on available infrastructure.

### FrontierCode 1.1 - excluded
No public task set or scorer; no reproducible comparison.

## Permanently stopped suites

No further model calls on HumanEval+, MBPP+, CRUXEval, LiveCodeBench,
BigCodeBench, DS-1000, ClassEval, MultiPL-E, or aider. Their prior invalid
pre-modular smoke records remain preserved as history and are never overwritten.

## Run contract

- Fixed model/config across raw/Simple/Ultra: `gemini-3.5-flash-lite`.
- ₹0/free tier only; stop cleanly on quota and never enable billing.
- Stable task/run IDs, pinned environments, seeds, attempt history, checkpoint,
  scorer evidence, tokens, latency, quota-stop reason and prompt version/hash.
- Aggregate only completed comparable triplets. No tuning on held-out tasks.
- Mock and real evidence are always labeled separately.
