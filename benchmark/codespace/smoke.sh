#!/usr/bin/env bash
# Tiny real smoke shard in the codespace (real model, smoke-only, NOT publishable).
# Requires: key already in FileSecretStore via rex-key-fill (has_key=true).
# 2 tasks x 3 modes x 1 seed on evalplus-humanevalplus + cruxeval-output.
set -euo pipefail
WS=/workspaces/rex-harness
BENCH=$HOME/bench
SUITES=$BENCH/suites
PLANS=$BENCH/plans
RESULTS=$BENCH/results
mkdir -p "$SUITES" "$PLANS" "$RESULTS"

HARN=$(git -C "$WS" rev-parse --short HEAD)
echo "harness-version: $HARN"

# Suites (hidden staged scorers)
cd "$WS/benchmark/runner/adapters"
python3 evalplus_adapter.py --dataset humanevalplus --out "$SUITES/humanevalplus" --limit 2
python3 cruxeval_adapter.py --task output --out "$SUITES/cruxeval-output" --limit 2

# Plans (identical model/config across raw/Simple/Ultra)
cd "$WS/benchmark/runner"
python3 plan.py --suite evalplus-humanevalplus --suite-file "$SUITES/humanevalplus/suite.jsonl" \
  --model gemini-3.5-flash-lite --dataset-version evalplus==0.3.1/humanevalplus-v0.1.10 \
  --harness-version "$HARN" --out "$PLANS/hep.json"
python3 plan.py --suite cruxeval-output --suite-file "$SUITES/cruxeval-output/suite.jsonl" \
  --model gemini-3.5-flash-lite --dataset-version raw-jsonl-sha256-8368b810 \
  --harness-version "$HARN" --out "$PLANS/crx.json"

# Orchestrate (checkpointed; clean stop on 429)
python3 orchestrate.py --plan "$PLANS/hep.json" --rex-bench "$HOME/bin/rex-bench" \
  --results "$RESULTS/hep.jsonl" --stage-dir "$SUITES/humanevalplus/stage"
python3 orchestrate.py --plan "$PLANS/crx.json" --rex-bench "$HOME/bin/rex-bench" \
  --results "$RESULTS/crx.jsonl" --stage-dir "$SUITES/cruxeval-output/stage"

# Aggregate (completed comparable triplets only)
python3 aggregate.py --results "$RESULTS/hep.jsonl" "$RESULTS/crx.jsonl" | tee "$RESULTS/smoke-aggregate.txt"

cd "$BENCH"
tar -czf smoke-results.tar.gz results plans
sha256sum smoke-results.tar.gz
echo "SMOKE DONE -> $BENCH/smoke-results.tar.gz"
