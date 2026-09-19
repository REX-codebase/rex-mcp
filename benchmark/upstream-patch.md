# rex-bench benchmark changes (landed)

Two gaps in rex-harness @ 25bec7a blocked hidden-test benchmark scoring.
Both are fixed on main; model-issued command authority is unchanged.

## 1. Hidden staged tests (`--stage-dir`) - landed
`bench::stage_hidden_files` + `--stage-dir <root>` on rex-bench. After a run
and before scoring, `<root>/<task-id>/` is copied into the workspace,
overwriting files the model wrote at the same paths (blind scoring
guarantee); symlinks are refused; copy failure fails the task closed
(`stage_error`). Unit test: overwrites model-planted checks, refuses
symlinks, missing stage dir is a no-op.

## 2. Trusted verifier scoring policy - landed
`ToolRuntime::execute_trusted_scoring(argv, cwd, timeout, allowed)` in
rex-tools: same sandbox as model-approved commands (env_clear, rlimits,
kill-tree timeout, capped output, redaction, audit receipt) but gated by a
suite scoring allowlist (bare executable names only; paths and shells
refused). `verify_contract_with_scoring` in rex-ultra routes ONLY verifier
Proof commands through it when the caller pins an allowlist;
`verify_contract` keeps the old behavior (denial stands) for every other
caller. Model-facing `command_policy` is untouched. rex-bench passes
SCORING_EXECUTABLES = python3/python/pytest/node/cargo/go/java/javac/ruby.
Tests: allowlisted runs, nonzero exit fails-but-executes, shells and paths
stay blocked, no-allowlist keeps the old denial; plus an end-to-end staged
pytest check that passes a correct solution.py and fails a broken one.

## 3. Env pinning
Shard env records: suite name, dataset pin (see MANIFEST.md), harness
commit, model identifier, date window, seeds, scorer command.
