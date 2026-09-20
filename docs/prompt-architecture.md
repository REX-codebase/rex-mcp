# REX prompt architecture (rex-prompt v1.0.0)

Every REX model call assembles its system prompt from versioned modules in
`crates/rex-prompt` instead of ad-hoc strings. This document is the map.

## What every call gets

| Module | Source | Notes |
|---|---|---|
| Constitution | `constitution.rs` | Immutable within a version; pinned-hash test is the tripwire. Always first. |
| Role card | `roles.rs` | Worker, Builder, ContractDrafter, Adversary, Shadow, Recovery, CleanRoomJudge. |
| Repo/runtime twin | `twin.rs` | Facts carry provenance (evidence id / file+sha256 / probe command) and a freshness window; expired facts are omitted with a visible stale count. Model claims render as unverified, never as facts. |
| Tool contract | `tools.rs` | Derived from the tools actually enabled at that call, never a global capability list. Role policies: `All`, `ReadOnly`, `None`. |
| Task acceptance contract | rendered by callers | Ultra phases render the compiled contract; clean-room roles get it without builder narrative. |
| Epistemic state | `epistemic.rs` | OBSERVED (needs evidence id) / INFERRED (needs source fact ids) / GUESS. `from_model_prose` can only produce a guess: prose cannot mint facts. |
| Failure/replay history | `history.rs` | Bounded (8 entries x 400 chars, oldest drops first). Not shown to clean-room roles. |
| Completion gate | `gate.rs` | Fixed text + `validate_completion_claim`, which rejects claims citing no or unknown evidence. |

Assembly (`Assembler`) is deterministic (canonical order regardless of
insertion order), budgeted per module with a visible truncation marker, and
hashed: sha256 per module, one `prompt_hash` over version + module hashes,
identity string `rex-prompt/<version>#<hash12>`.

Untrusted text (task content, tool output) goes through `untrusted_block`,
which brackets it in delimiters the constitution defines as data and
neutralizes spoofed delimiters inside it.

## Where it is wired

- **Simple (autonomous loop)**: `assemble_run_prompt` in
  `rex-providers/src/autonomous.rs` = constitution + role card + tool
  contract (actual offering) + completion gate, sent as the real system
  instruction on every protocol - Gemini via `systemInstruction`, Anthropic
  via `system`, OpenAI via the system message. Before v1.0.0, Gemini
  received no system instruction at all and the others a single hardcoded
  line.
- **Ultra**: spec compiler, clean-room judge and reconstruction judge get
  `ultra_oneshot_system(role)` (constitution + role card; these calls have
  no tools). Builder and adversary run through
  `begin_in_workspace_with_role`; the adversary's `ReadOnly` policy is
  enforced by the harness (out-of-scope tool calls are refused and logged),
  not just requested in prose. The "must have mutated something" completion
  gate applies only to roles that may mutate.
- **Raw benchmark mode**: unchanged. Its identity `raw-fixed/1` hashes the
  exact system string + output contract (`bench::RAW_FORMAT_SUFFIX`,
  pinned verbatim by test).

## Version pinning and comparability

- Every autonomous run records `prompt_version`/`prompt_hash` in its brief,
  checkpoint and snapshot; every bench `TaskResult` carries them, and
  `orchestrate.py` copies them into result records.
- `rex-bench report` and `aggregate.py` fail closed when a mode's records
  mix prompt identities. `legacy-unknown` (pre-versioning files) conflicts
  loudly with versioned records instead of pooling silently.
- Resume fails closed when a checkpoint's prompt identity differs from the
  current build; a legacy checkpoint (no metadata) migrates onto the
  current identity and the run log says so.

## Held-out answers

Suite checks never enter worker prompts: raw mode builds from
`BenchTask.prompt` only (test: `held_out_checks_never_enter_the_raw_prompt`),
Simple/Ultra receive the task text, and hidden suite files land in the
workspace only after the run, for blind scoring.

## Changing a prompt

Any edit to constitution, role cards, module rendering or assembly order is
a prompt-architecture change: bump `PROMPT_VERSION`, expect the pinned-hash
tests to fail, re-pin deliberately, and treat pre-change benchmark records
as a different identity (the tooling enforces this).
