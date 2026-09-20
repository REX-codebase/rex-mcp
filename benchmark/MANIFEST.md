# Dataset manifest (pins; no dataset content in git)

Fetch scripts are the adapters in runner/adapters/. Record these pins in
every shard env. Nothing from restricted or bulky datasets is committed;
suites and staged tests are generated locally by the adapters.

| suite | source | pin | license |
|---|---|---|---|
| evalplus-humanevalplus | evalplus==0.3.1 -> github.com/evalplus/humanevalplus_release v0.1.10 | package version pin | Apache-2.0 harness / MIT upstream |
| evalplus-mbppplus | evalplus==0.3.1, MBPP+ dataset v0.2.0 (378 tasks) | package version pin | Apache-2.0 harness / CC-BY-4.0 upstream |
| cruxeval-output / cruxeval-input | raw.githubusercontent.com/facebookresearch/cruxeval/main/data/cruxeval.jsonl | sha256 8368b81047dc5014e4caf5a2f97604eff7644e0ecd7415e3ceeb184bbc2e0c96 | MIT |
| bigcodebench-full / -hard | HF bigcode/bigcodebench config default split v0.1.4 (+ bigcode/bigcodebench-hard) | commit b74c0d0bf70d2c0bc459be537895cca163007f1a; bigcodebench-hard commit 298d2cc7b96612e15e47313c3603ee124cee0c1f | Apache-2.0 |
| ds1000 | HF xlangai/DS-1000 split test | commit 4416080ac5cb80bdf7576aefb8f9a0b4d5426a44 | CC-BY-SA-4.0 |
| livecodebench (WIP, not yet in validated set) | HF livecodebench/code_generation_lite release_v6 | version tag release_v6 (1055 problems) | MIT |

Validation artifacts in validation/ come from the deterministic mock and
hand-written smoke tasks only - they prove pipeline mechanics and are NOT
model results.

## Verified model + quota (2026-09-20 ~08:02 IST)

- Model identifier (first-party docs https://ai.google.dev/gemini-api/docs/models): `gemini-3.5-flash-lite` ("Gemini 3.5 Flash-Lite"). Exact string to be passed to plan.py --model for all real shards.
- Free-tier limits from the account's live AI Studio rate-limit page (https://aistudio.google.com/rate-limit, project "Project Ori" project-ori-ccd9e, Free tier, billing NOT set up): Gemini 3.5 Flash Lite = 15 RPM, 250K TPM, 500 RPD, current usage 0. Agrim's stated 500 req/day confirmed against the live page.
- Rate limits are per project, not per key; RPD resets at midnight Pacific. The AI Studio account has exactly one project (Project Ori); the vault entry's "Zapia project" label has no separate project in AI Studio, so these limits are the operative ones for any AI Studio key from this account.
- Docs page no longer publishes a static per-model free-tier table; the AI Studio page is the authoritative source and was read live.

## Execution status (2026-09-20 ~08:49 IST)

- Owner redirected execution: NO Colab, no laptop (PC broken). Benchmarks run through REX itself; checkpoints/results save directly to private Drive.
- Colab route dismantled: runtime deleted, GEMINI_API_KEY removed from Colab Secrets, notebook + package + unused notebook permanently deleted from Drive. Zero real runs ever executed there; no benchmark content existed on the runtime.
- laptop-setup/ removed: route discarded by owner.
- Exact remaining blocker for real runs: vault fill is browser-input-only; the REX execution environment (sandbox) is not browser-reachable; the vault has no export-to-file path. See improvement-loop.md section 6.
- Non-key work continues: improvement-loop.md design v1, runner/pools.json + plan.py --pool support (dev/heldout/split, deterministic, tested).
