# Required rex-bench upstream changes (before first real shard)

Two gaps in rex-harness @ 25bec7a block hidden-test benchmark scoring. Both
are small and scoped; neither weakens the agent sandbox.

## 1. Stage hidden files at scoring time (`--stage-dir`)
Gap: rex-bench wipes and recreates an empty workspace per task
(`run_task` in crates/rex-ultra/src/bin/rex-bench.rs), and BenchTask carries
only {id, prompt, checks}. Benchmark suites need hidden tests present at
SCORING time but absent during the model run.

Change: add `--stage-dir <root>` to rex-bench. In `run_task`, AFTER the mode
match completes and BEFORE `bench::score_workspace`, copy
`<root>/<task-id>/` contents into the workspace (fail closed if the copy
fails). The model never sees the stage dir; scoring stays blind. The mock
(mock_rex_bench.py) already accepts the flag so the contract is testable.

## 2. Let the VERIFIER run the scorer interpreter (scoped policy)
Gap: Proof commands flow through rex-tools `command_policy`
(crates/rex-tools/src/lib.rs), whose denied list includes python/python3/node
and all shells. That deny list is correct for MODEL-issued tool calls, but it
also blocks the trusted verifier from executing test harnesses. Checks today
can only use non-denied executables (pytest works because the file_name is
"pytest", but that is an accident of naming, not a designed path).

Change: split the policy at the call site in rex-ultra verify.rs: verifier
Proof execution uses a scoring policy that permits the scorer entry points
named by the suite (python3/pytest/cargo test/node), confined to the task
workspace, with the existing timeout and output hashing. Model-issued
RunCommand keeps the current deny list unchanged. Suite-authored commands are
trusted task infrastructure, not model output; the suite file itself is part
of the pinned env.

## 3. (Recording, no code) Env pinning
Shard env records: suite name, dataset version + download URL + sha256,
harness commit, model identifier, date window, seeds, scorer command. The
adapter already prints suite sha256; record stage dir sha256 alongside.
