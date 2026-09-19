# Dataset manifest (pins; no dataset content in git)

Fetch scripts are the adapters in runner/adapters/. Record these pins in
every shard env. Nothing from restricted or bulky datasets is committed;
suites and staged tests are generated locally by the adapters.

| suite | source | pin | license |
|---|---|---|---|
| evalplus-humanevalplus | evalplus==0.3.1 -> github.com/evalplus/humanevalplus_release v0.1.10 | package version pin | Apache-2.0 harness / MIT upstream |
| evalplus-mbppplus | evalplus==0.3.1, MBPP+ dataset v0.2.0 (378 tasks) | package version pin | Apache-2.0 harness / CC-BY-4.0 upstream |
| cruxeval-output / cruxeval-input | raw.githubusercontent.com/facebookresearch/cruxeval/main/data/cruxeval.jsonl | sha256 8368b81047dc5014e4caf5a2f97604eff7644e0ecd7415e3ceeb184bbc2e0c96 | MIT |
| bigcodebench-full / -hard | HF bigcode/bigcodebench config default split v0.1.4 (+ bigcode/bigcodebench-hard) | commit b74c0d0bf70d2c0bc459be537895cca163007f1a | Apache-2.0 |
| ds1000 | HF xlangai/DS-1000 split test | commit 4416080ac5cb80bdf7576aefb8f9a0b4d5426a44 | CC-BY-SA-4.0 |
| livecodebench (WIP, not yet in validated set) | HF livecodebench/code_generation_lite release_v6 | version tag release_v6 (1055 problems) | MIT |

Validation artifacts in validation/ come from the deterministic mock and
hand-written smoke tasks only - they prove pipeline mechanics and are NOT
model results.
