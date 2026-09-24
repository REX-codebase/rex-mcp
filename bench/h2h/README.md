# REX vs opencode vs Hermes: live head-to-head (not yet run)

Five small, synthetic Python repos with a task each (`tasks/*/prompt.md`) and
a hidden unittest (`tasks/*/hidden_test.py`) the agent never sees. Each
task's starting repo fails its hidden test and the reference fix in
`tasks/*/reference/` passes it (checked with `score.sh`).

| task | kind |
|------|------|
| paginate | two off-by-one bugs plus a new error case |
| durations | implement a parser from a spec, with strict rejection |
| rename | rename a function across 3 files, no alias left |
| csvsum | fix a crash: quoted commas, header row, blank rows |
| lru | turn a dict cache into an LRU cache, add `len()` |

Protocol:
- Same model for all three tools (`gemini-3.5-flash-lite`, free tier).
- One attempt per tool per task, fresh copy of the repo, 20-minute limit.
- Score is only the hidden test (pass/fail), plus wall time. A run that
  hits the rate limit counts as a failure and is reported as such.
- The prompts are synthetic, because free-tier prompts may be reviewed by
  Google (https://ai.google.dev/gemini-api/terms, "Unpaid Services").
- opencode runs with sharing disabled.
- The key is never written to a file or printed by these scripts.

Checked in the clones: opencode's `run` takes `--model` and reads
`OPENCODE_CONFIG_CONTENT`, whose `share` accepts `disabled`
(packages/core/src/config.ts:47); Hermes `chat -q ... --oneshot` answers
and exits, and `--yolo` skips its approval prompts
(hermes_cli/_parser.py:181,214-229). How opencode's `run` handles
permission prompts is to be confirmed at run time.

Status: the run is waiting on the owner's approval of where and how the key
is used. `run.sh` uses each tool's headless entry point: `opencode run`,
`hermes chat -q --oneshot`, and `rex exec --json --yes` (crates/rex-cli).
