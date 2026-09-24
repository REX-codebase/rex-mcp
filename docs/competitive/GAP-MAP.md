# REX vs Hermes Agent vs opencode: verified gap map

Behaviour-based comparison of the model-facing agent core. No benchmark
scores and no invented metrics: each row cites the file where the behaviour
lives. Competitor revisions read on 2026-09-24:

- Hermes Agent: `NousResearch/hermes-agent` @ `687ca6ed` (MIT)
- opencode: `sst/opencode` @ `0f549842` (MIT)

REX's code is written for REX. No competitor code is copied; where an idea
is shared it is noted in the module docs.

## Round 1: tool layer (`crates/rex-tools`) and loop (`crates/rex-providers`)

| Capability | Before (REX) | Hermes | opencode | After (REX) |
|---|---|---|---|---|
| Edit tolerance | exact text only (`edit_file`) | ordered strategy chain, `tools/fuzzy_match.py` | 9 replacers, `src/tool/edit.ts` | 8-strategy precision chain, unique-match only, `fuzzy.rs` |
| Indentation after a loose match | n/a | uniform base-indent swap, `_reindent_replacement` in `tools/fuzzy_match.py` | not re-based (error text asks for exact whitespace, `edit.ts`) | per-indent map, then line alignment, so fully flattened blocks recover their nesting too |
| Miss diagnostics | "expected text was not found" | post-miss hint path, `tools/fuzzy_match.py:595` | "must match exactly" error | closest region, line number and % similarity |
| Approved diff == written bytes | exact only | n/a | n/a | same planner for diff and write |
| Stale-write guard | none | read tracking, `tools/file_tools_read_tracking.py` | not compared yet | content fingerprint; refuses writes after an external change |
| Read paging | whole file, 2 MiB cap | offset/limit with `next_offset` char budget, `tools/file_tools.py` | offset/limit, 2000 lines, `src/tool/read.ts` | offset/limit, numbered, continue footer, up to 64 MiB paged; small files stay raw |
| Binary / long lines / dirs | UTF-8 error only | not compared yet | long-line cut and dir handling in `read.ts` | NUL-sniffed binary refusal, 2000-char line cut, directory listing |
| Content search | case-insensitive substring; walks `.git`/`target`/`node_modules` under a 10k-file cap | ripgrep-backed, `tools/file_operations_search.py` | `grep` (regex + include), `glob`, ripgrep | literal or regex, include glob, skips build dirs and `.gitignore`, says when cut |
| File finding | none | not compared yet | `src/tool/glob.ts` | `glob_files`, newest first, read-only class |
| Command output capture | first 512 KiB per stream | spill to file, `tools/hook_output_spill.py` | 2000 lines / 50 KB + file, `src/tool/truncate.ts` | head + tail per stream, so the final error survives |
| Output to model | byte slice at 4000: panics on multi-byte text; drops the tail; failed commands passed their full output | not compared yet | truncation service, `truncate.ts` | char-safe head+tail clip for output and error detail; truncation flag set |
| Web excerpt clip | byte slice at 300: panics on multi-byte text | n/a | n/a | char-boundary clip |
| Multi-file patch | none | not compared yet | `src/tool/apply_patch.ts` | `apply_patch`: same envelope format, tolerant unique hunk matching, plan-all-then-write, temp+rename, rollback on failure, every path (incl. move target) checked by custody scope |
| Identical-call loop | repeated-failure stop only | loop guardrail controller, `agent/tool_guardrails.py` | `DOOM_LOOP_THRESHOLD = 3`, `session/processor.ts` | nudge on the 3rd identical call, whether it succeeds or fails; no-progress budget still terminates |

## Round 2 candidates (verified present in both, absent in REX)

- Project instruction files: opencode `session/instruction.ts` loads
  `AGENTS.md` / `CLAUDE.md`; Hermes `agent/coding_context.py` loads
  `AGENTS.md`, `CLAUDE.md`, `.cursorrules`. REX (round 2) loads the first of these at the workspace root into per-turn state, capped at 8k chars, symlinks refused. Round 5 (48566bc): the nearest file from the workspace up to its git root wins (no walk without a git root or when the root is $HOME), plus a user-level `~/.config/rex/AGENTS.md` sent as `user_instructions` with its own precedence note. opencode (`session/instruction.ts:122-126`) walks to the worktree the same way.
- Sub-agent delegation: opencode `src/tool/task.ts`; Hermes
  `tools/delegate_tool.py`. REX (round 2): `explore` tool starts a read-only
  explorer sub-agent (read/search/glob only, no recursion, inside the parent's
  remaining budgets, honours cancel) and returns one report. opencode picks
  agent types (`task.ts:46,131`); Hermes gives children the parent toolsets
  and a batch/parallel mode (`delegate_tool.py:6-8`). Parallel explorers
  shipped in round 4, research children in round 6, writing (edit)
  children in round 8 and parallel edit children in round 9 (see below).
- Context compaction of old turns: opencode `session/compaction.ts`; Hermes
  `agent/context_compressor.py`. Behaviour compared (round 2): both keep a
  transcript and, near overflow, clear old tool output (opencode protects
  the newest PRUNE_PROTECT=40k tokens of tool output, compaction.ts:28-29,
  271-308; Hermes protects a token-budget tail and replaces old output with
  a placeholder, context_compressor.py:2,798) and then LLM-summarise. REX
  never overflows (fresh state + last exchange each turn) but used to lose
  older tool results entirely. REX now keeps bounded working memory
  (crates/rex-providers/src/memory.rs): newest tool-result excerpts under a
  12k-char budget, oldest dropped first, and reads of files the run later
  changed are dropped so the model never sees stale content. Gap left: no LLM summary of
  very old history (REX relies on plan + digest + ledger instead).
  Round 12 adds one as an opt-in; see below.

## Round 3

- Post-edit diagnostics. opencode returns LSP diagnostics with each edit
  (`src/tool/edit.ts:198-200`); Hermes runs per-extension linters
  (`python -m py_compile`, `node --check`, `rustfmt --check`, ...) plus
  in-process JSON/YAML/TOML parses and reports only new errors
  (`tools/file_operations_lint.py:17-22,111-116,172-193`). REX before: no
  feedback. REX now (`crates/rex-tools/src/check.rs`): every create/edit/
  patch result carries an in-process check with no subprocess and no
  approval: JSON parse (JSON-with-comments tolerated), and a
  lexer-aware delimiter/string/comment check for Rust, JS/TS/JSX/TSX, Go,
  C-family, Python and CSS. Problems are labelled new vs pre-existing.
  The check is advisory and never blocks a write. False-positive sweep
  (ignored test `sweep_tree_for_false_positives`): 0 of 269 files in this repo,
  2 of 3,824 in opencode and 1 of 11,046 in Hermes flagged; all 3 are JSX
  prose like "1) item", and JSX notes are worded as possible. Gap left:
  no semantic (type/LSP) diagnostics. (YAML/TOML checks were added in
  round 5; see below.)
- URL fetch. opencode `src/tool/webfetch.ts` (5 MB cap, format text/
  markdown/html, permission `webfetch`); Hermes `tools/web_tools_extract.py`.
  REX before: `web_search` only. REX now: `web_fetch`
  (`crates/rex-providers/src/fetch.rs`) goes through REX-search's own
  fetcher, which handles robots.txt, refuses private addresses on every hop,
  and applies the redirect limit, body cap and HTML-to-text. REX also
  refuses URLs that look like data channels before any request is made:
  query over 256 chars, encoded-looking runs over 64 chars, embedded
  credentials. Long pages are paged by char offset. Live smoke test
  (ignored test `live_fetch_example_dot_com`) fetched example.com.
  Round 7: HTML comes back as Markdown by default (REX's own converter in
  `crates/rex-search/src/markdown.rs`: headings, paragraphs, nested lists,
  absolute links, code blocks, inline code, table rows; scripts, styles,
  head, SVG dropped; `javascript:`/`data:` links keep only their text);
  `format: "text"` gives flat text. opencode defaults to markdown too
  (`tool/webfetch.ts:15-17`). Live smoke test fetched the Rust Book
  closures chapter with headings, code blocks and absolute links. A
  generated hostile-input test caught a UTF-8 slicing panic in entity
  decoding before commit. Fixed alongside: flat-text extraction pushed each
  UTF-8 byte as its own character ("Ã©" for "é") in every search result.
  Gap left: no raw `html` format (not offered, to keep pages small).
- Found while wiring this: `web_search` in the autonomous loop sent
  `seeds: []`, so the keyless REX provider had nothing to crawl. Fixed in
  round 4 (seeds from `sites` and real domains in the query; see below).
- Checkpoint/undo. Hermes snapshots into a shadow git store before
  mutating calls (`tools/checkpoint_manager.py:1-9`); opencode keeps git
  snapshots with `restore`/`revert` (`src/snapshot/index.ts:36-42`). REX
  before: `rex serve` had manual checkpoint/rewind of a live run's staged
  workspace (whole-directory copies, `crates/rex-cli/src/serve.rs:328-390`);
  the desktop autonomous agent had nothing. REX now (`crates/rex-tools/src/journal.rs`): every
  successful create/edit/patch is journaled on disk with the prior bytes (or
  "absent") and a fingerprint of what the agent wrote. `undo_last_write`
  works on the runtime and on the service for finished or crashed runs. It
  goes newest-first, restores deleted files, removes created ones, and
  refuses (changing nothing) when a file changed after the agent wrote it,
  so user edits are never clobbered. No git needed. Exposed as Tauri command
  `agent_undo`, sidecar route `POST /api/agent/runs/:id/undo` and
  `agentUndo()` in `src/data/agentRun.ts`. The app shows "Undo last file
  change" on finished runs that wrote files (round 5, c75d092). Round 9:
  `Journal::undo_to(seq)` rewinds every write after entry `seq` (0 = the
  whole run) in one call, all or nothing: each step is checked against
  the file state the earlier undos would leave before any file is
  touched, so one file changed by the user blocks the whole rewind and
  nothing moves. Exposed as `undo_to_write` on the service, `{"to": N}`
  on the sidecar undo route, an optional `to` on Tauri `agent_undo`
  (review only; the Tauri crate does not build here) and
  `agentUndo(runId, to)`. Compare opencode's per-step `revert`
  (`src/snapshot/index.ts:36-42`) and Hermes' shadow-git checkpoints
  (`tools/checkpoint_manager.py:1-9`). Tests: journal rewind to a step,
  to zero, through a file two writes touched, and the blocked all-or-
  nothing case; service rewind; sidecar response shape. 3 mutations
  (no simulated state, off-by-one on `seq`, no pre-check) each fail.
- Round 10: the run view lists a finished run's successful writes
  (newest first, failed ones left out) under "Rewind to an earlier step",
  with "Rewind to here" on each older write and "Undo every change". It
  calls `agentUndo(runId, {afterCall})`, which the service resolves to
  the journal entry of that tool call (`undo_after_call`); rows the
  backend reports as undone are struck through. Covered by a vitest test
  (3 mutations each fail it) and a render with the compiled dark theme.

## Round 4: ask the user (`ask_user`)

- opencode: `question` tool asks one or more questions and returns answers
  (`src/tool/question.ts`). Hermes: `clarify` with up to 4 choices plus
  free text; on timeout it tells the model to use its best judgement
  (`tools/clarify_tool.py:9-15`).
- REX now: `ask_user` {question, choices<=4}. The run parks as
  `awaiting_answer` with `pending_question` in the snapshot; only the
  trusted UI path `AutonomousRunService::answer` (sidecar
  `POST /api/agent/runs/:id/answer`, Tauri `agent_answer`, `agentAnswer()`)
  can reply. Decline, blank or a 30-min timeout tell the model to decide
  and state the assumption; the run never dies from a missed question.
  Cap of 3 questions per run (durable in the checkpoint). Cancel while
  parked ends the run. Explorer children cannot ask. Headless `rex exec`
  declines automatically and prints the question.
- Round 5 (c75d092): the app renders the question panel (choices, free
  text, "Let REX decide"). Round 6: `ask_user` takes `questions` (up to 3
  in one call) and returns every answer in one response with a status
  each (answered / declined / timeout / not_asked past the 3-per-run cap).
- Round 7: batched questions show on one form (choices fill each answer,
  blanks mean "let REX decide", one "Send answers"). The snapshot carries
  the whole `batch`; `answer_many` answers the open question and queues
  the rest, which resolve without parking again; the queue is cleared when
  the call ends so answers never leak into a later ask_user. Exposed on the
  `agent_answer` Tauri command (`answers`) and the sidecar `/answer` route.
  Caveat: the Tauri crate cannot be compiled on this build box (no GTK),
  so that 4-line change is checked by review only; the sidecar route and
  service are compiled and tested.

## Round 4: web_search with the keyless engine

- Bug found in REX: autonomous runs sent `seeds: []`, and the keyless
  REX-search engine only crawls outward from seeds, so web_search on the
  default provider could not find any page.
- REX now: `web_search` takes optional `sites` (URLs or bare domains).
  Seeds also come from URLs and well-formed domains in the query (free-text
  domains need a common ending so `main.rs`/`config.json` are not taken as
  sites). Each seed passes web_fetch's URL vetting; IP literals and local
  names (`localhost`, `.local`, `.internal`, ...) are refused; max 5. With no
  seed, the model gets guidance (pass sites or use web_fetch) instead of a
  silent empty result. Keyed providers (Exa, Tinyfish) are unchanged.
- Gap left: no web-wide index at ₹0. opencode reaches one through Exa's and
  Parallel's hosted endpoints with no key (`tool/mcp-websearch.ts:5-7`);
  adopting that would send queries to a third party, so it is an owner
  decision.
- Owner decision (5:25 IST): Exa's keyless endpoint was approved only if it
  costs ₹0. Exa's MCP docs limit the free plan to "casual use" and ask for an
  API key for production; their ToS bars sublicensing the Services to third
  parties. REX therefore stays self-hosted; bring-your-own Exa key remains.

## Round 4: parallel explorers

- Hermes: batch mode runs several children in parallel
  (`tools/delegate_tool.py:6-8`, concurrency cap via
  `delegation.max_concurrent_children`, line 500).
- REX now: `explore` takes `tasks` (up to 3 independent questions) as well as
  `task`. Children run concurrently on scoped threads; the parent's
  remaining tool-call and token budgets are split evenly, so a batch never
  spends more than one explorer could. Each child stays read-only, cannot
  recurse or ask the user, and stops on cancel (any cancelled child ends the
  run). The parent gets one `explorers` list of reports; each child gets its
  own ledger entry. A concurrency probe in the test proves both child
  requests were in flight at once.
- Round 6: `explore` takes `kind`: `explore` (default, workspace only) or
  `research` (workspace plus `web_fetch`, with the parent's vetting:
  robots, private addresses and data-like URLs refused). A research child
  needs the parent run's own web_fetch permission. Reports list
  `urls_fetched`. Compare opencode's `subagent_type` (`tool/task.ts:46`)
  with built-in `general` and `explore` agents (`agent/agent.ts:183-216`).
- Round 8: `kind: "edit"` starts a writing child for one self-contained
  change (read, search, glob, create/edit file, apply_patch, run_command;
  no web, ask_user or recursion). Each write or command that needs
  approval parks on the parent's trusted UI decision (`wait_for_decision`),
  the same gate the parent's own calls use; a denial is counted toward the
  run's denial limit, and no decision stops the child. It runs one at a
  time (an edit batch is rejected), is refused for roles with no write
  tools (Adversary/Shadow can still explore), and each child tool is also
  checked against the run's role list. The parent gets `files_written`
  and `denials` with the report and in the ledger. Tests: approve-then-deny
  (only the approved file exists), batch rejection, read-only role
  refusal; 5 of 6 mutations fail a test directly, and the per-tool role
  check (defence in depth, no current role reaches it) fails only together
  with the role gate.
- Round 9: edit children run in parallel (up to 3 per call, budgets
  split). They take turns at the single approval slot (a lock around
  `wait_for_decision_on`), and each file a child writes is claimed for
  it: another child of the same batch that tries to create or edit that
  path (`./x` and `x` are the same) is refused and told to report the
  overlap. Compare: Hermes runs batch children with the parent's toolsets
  minus blocked tools (`tools/delegate_tool.py:6,202,441-528`), and no
  file-overlap check turned up in `tools/delegate_tool*.py`. opencode
  serialises concurrent edits of one file with a per-path semaphore
  (`src/tool/edit.ts:35-43`) but lets a second agent edit it afterwards.
  Test: one batch where two children hit the same file (one approval, one
  write, one refusal), then one batch on separate files (two approvals,
  both writes, 2 requests in flight). 3 mutations fail it: no claim, no
  `./` normalisation, no approval lock (the run hangs until timeout).
- Claims also cover every path an `apply_patch` touches (added, updated,
  deleted and move targets), all or none: a patch that hits one claimed
  path claims nothing. Unit test plus 4 mutations (partial claim, no patch
  paths, no owner check, no insert), each failing it.
- Gap left: `run_command` from parallel edit children is not checked for
  file overlap (a command's writes are not known before it runs). Round 12f adds an after-the-fact check; see below.

## Round 5: YAML/TOML checks and app panels

- Syntax check now covers TOML (full parse via the `toml` crate already in
  Cargo.lock; messages carry no position text so shifted problems still
  count as pre-existing) and YAML (REX's own conservative check: tab
  indentation, unclosed or mismatched flow collections; block scalars and
  brackets inside plain scalars such as `${{ x }}` are ignored). False-alarm
  sweep: 0 YAML/TOML files flagged across this repo, opencode, Hermes and
  the local cargo registry (630 YAML + 248 TOML files).
- Round 7: YAML duplicate keys in block mappings are flagged ("duplicate
  key 'x' (first on line N)"), per mapping and per `---` document; merge
  keys, aliases and `?` keys are skipped, and Helm/Jinja files (lines
  starting `{{` or `{%`) are not checked. Sweep: 0 YAML files flagged in
  this repo, opencode, Hermes and the cargo registry (748 YAML files; the
  5 flags in the sweep are pre-existing JSX/Kotlin/C notes).
- Round 8: a key indented under a key that already holds a plain value
  ("'image' is indented under 'web'") is flagged. Multi-line quoted
  strings (with `\"` and `''` escapes) are skipped until they close, and
  wrapped prose whose "key" has spaces or punctuation is left alone.
  Sweep: 0 YAML files flagged across this repo, opencode, Hermes and the
  cargo registry (same 5 pre-existing non-YAML notes). 4 mutations of the
  new logic each fail the test.
- Round 9: text after a closed flow collection (`a: [1, 2] x`,
  `a: {b: 1}}`, also when the flow spans lines) is flagged; a comment or
  a complex key (`[x, y]: v`, `[p, q]:` then a block) is allowed.
  Mismatched closers and unclosed flows were already caught. Sweep: 0
  YAML files flagged across this repo, opencode, Hermes and the cargo
  registry. 4 mutations of the rule each fail the test.
- Round 10: duplicate keys inside flow mappings (`{x: 1, x: 2}`, nested,
  quoted keys, across lines) are flagged; value-only entries (`{x, x}`),
  aliases, colons inside plain keys and URLs are left alone. Sweep: 0
  YAML files flagged over about 1,190 flow-mapping lines (mostly in
  Hermes). Mutations: 6 of 7 fail the test; the survivor (not clearing
  the key stack after a mismatch) cannot change output, because the
  stack is only read through the flow stack, which is cleared.
- Gap left: YAML check is still not a full parser (for example, no
  anchors-before-use or complex-key checks); opencode's LSP route covers
  more. Round 12c adds a full parse; see below.
- App: AgentRunView shows the ask_user question (choices, free text, "Let
  REX decide") and an "Undo last file change" button on finished runs that
  wrote files. Covered by 2 vitest tests; rendered with the compiled theme
  and checked visually.

## Round 10: repair slightly-off tool calls

- Hermes coerces string arguments against each tool's schema before
  dispatch (`tools/arg_coercion.py`, called from `model_tools.py:888`):
  `"42"` to 42, `"true"` to true, JSON-encoded arrays and objects parsed.
  opencode fixes a tool name that differs only by case and sends anything
  else to an `invalid` tool that returns the error
  (`session/llm.ts:296-311`, `tool/invalid.ts`). REX already returned bad
  calls to the model as errors, but made no repairs, so `"max_results":"5"`
  or `Read_File` cost a turn.
- REX now repairs before decoding (`rex_tools::repair_tool_args`,
  `canonical_call_name` in `autonomous.rs`), for Gemini, Anthropic and
  OpenAI-style calls: counts given as `"5"` or `5.0`, flags given as
  `"true"`/`"FALSE"`, `"null"` for optional counts and flags, `argv` sent
  as a JSON-encoded string array, MCP `arguments` sent as a JSON-encoded
  object, and tool names that differ by case, spaces or `-` for `_`.
  Unclear values (`"yes"`, `"3.5"`, `"+5"`, `-1.0`, a bare `"git status"`
  argv, `"null"` for a required flag) are left alone, so decoding still
  rejects them with its usual error. Unlike Hermes, a bare string is not
  wrapped into a one-item `argv`: `["git status"]` would name the wrong
  program.
- Tests: 3 new (2 unit, 1 decode across both call formats). Mutations: 12
  tried, all fail the tests except one removed filter that could never
  change a result, which I deleted.
- Gap left: repairs are silent (not logged to the run journal). Closed
  in round 11.

## Round 11

- Tool-call repairs are now shown as run events (e5aa840): each repaired
  call emits an info event such as "repaired call: tool name
  'Create_File' read as 'create_file'" or "repaired call: create_file:
  fixed argument types for overwrite", so the user can see what was
  changed before it ran. Hermes coerces silently (`tools/arg_coercion.py`);
  opencode's case fix is silent too (`session/llm.ts:296-311`). Test:
  `repaired_calls_run_and_show_in_events`.
- YAML: lists and keys mixed at one indent are flagged: a `- `
  item at the column of a key that already has a value (`a: 1` then
  `- x`), and a key at the column of a list that is not owned by a key
  there (`- x` then `b: 1`, or `list:` / `  - a` / `  key: 1`). A list
  right under its key (`a:` then `- x`) with sibling keys after it, lists
  inside list items, block scalars, comments and document markers are
  left alone; templated files are skipped. Sweep: 0 new flags across this
  repo, opencode, Hermes and the cargo registry (26,894 files checked;
  the same 5 pre-existing non-YAML notes before and after). Mutations:
  7 of 7 fail the test after one dead check was removed.
- Head-to-head scaffold (9f0b12c, 9e2c6ba): `bench/h2h` holds synthetic
  tasks with hidden checks plus `run.sh` and `score.sh`, for running
  REX, opencode and Hermes on the same tasks. It has not been run (halted
  before any run), so this map still has no outcome numbers.

## Round 12: opt-in summary of old history

- opencode compacts automatically unless `compaction.auto` is false
  (`session/overflow.ts:28`, `session/processor.ts:622`) and carries the
  previous summary into the next one (`session/compaction.ts:366`).
  Hermes compresses by default (`compression.enabled` defaults to true,
  `hermes_cli/config.py:2936`) with one summary LLM call
  (`agent/context_compressor.py`). Both summarise the transcript.
- REX now has the same idea as a switch that is off by default, since
  each summary is an extra model call on the user's key
  (`crates/rex-providers/src/history.rs`). With `summarize_history` on,
  the tool-result excerpts that working memory drops are kept aside.
  Once they reach 6,000 characters, one tool-free call folds them and the
  previous summary into a new summary (at most 2,000 characters). The
  model sees it each turn under `history_summary`, marked as
  model-written and possibly out of date. Reads of files the run later
  changed are removed before summarising, the same rule working memory
  uses. The call counts against the token budget and is skipped (with a
  run event) when it would pass it. A provider error or empty answer
  leaves the run going without a new summary. Each summary shows as a
  run event and a ledger entry, and its request and response are saved
  as evidence.
- Switch: `RunOptions::summarize_history` on the run service, a
  `summarize_history` field on the custody request, the
  `summarize_history` flag on the sidecar custody route and the Tauri
  `custody_begin` command (review only; this change has not been through
  CI yet), and
  a third `custodyBegin` argument in `src/data/custodyRun.ts`. Legacy
  briefs and checkpoints load with it off.
- Tests: 5 unit (trigger, cap, prompt, requests, clipping), 1 memory
  spill test, 5 end-to-end runs (off makes no extra call and keeps
  nothing; on summarises right after the overflowing turn and shows the
  summary next turn; stale reads left out after an edit and after a
  patch; token budget respected), 1 vitest. Mutations: 17 of 17 fail the
  tests.
- App: a "Summarize" checkbox next to Plan mode, off by default and
  reset after each run like Plan mode; its tooltip says each summary is
  an extra model call on the user's key. The "Ctrl + Enter" hint now
  shows from the xl breakpoint so the controls stay on one line at
  1040px. Rendered with the compiled CSS in the dark theme at 1040px and
  1440px. 2 vitest tests.
- Gap left: REX summarises dropped tool results, not the whole conversation, because
  its loop sends fresh state each turn instead of a growing transcript.

### Round 12b: full Rust syntax check after writes

- Before: `.rs` writes got only the delimiter/string/comment check, so
  `let x = 1 +;` or `struct S { a: u8 b: u8 }` passed silently.
- Hermes: `rustfmt --check {file}` per write
  (`tools/file_operations_lint.py:22`). Its own comment calls this
  style-only and skips it when an LSP server claims the file
  (`tools/file_operations_lint.py:26-29`), and it needs a Rust toolchain.
  opencode: LSP diagnostics with each edit (`src/tool/edit.ts:198-200`),
  which need rust-analyzer running.
- REX now (`crates/rex-tools/src/check.rs`, `check_rust_parse`): when the
  delimiter check is clean, the file is parsed with `syn` (already in the
  lockfile through serde; `span-locations` added to `proc-macro2`). The
  first syntax error is reported with line and column, and goes through
  the same new vs already-there labelling. No subprocess, no toolchain,
  and formatting is never flagged. Files nested more than 128 brackets deep
  are skipped and the parse runs on a thread with a 64 MB stack; without
  this, a deep file in the cargo registry overflowed the stack and
  aborted the process during the sweep.
- Tests: 2 new unit tests. Hand mutation: 11/11 killed.
- False-alarm sweep: this repo 0 of 317; opencode unchanged (2 of 3,926,
  both older JSX notes); Hermes 1 new of 11,586: `contributors/emails/d@rko.rs`,
  a one-word text file with a `.rs` name (not Rust). Cargo registry: 4 new
  of 11,067: syn's own negative test input (`syn-1.0.109/tests/test_item.rs`),
  erased-serde's deliberately broken `features_check/error.rs`, dbus
  `methoddisp.rs` (inner attribute after a doc comment, which rustc also
  rejects; the file is marked unused), and one false alarm:
  `serial-core-0.4.0/src/lib.rs:495`, 2015-edition syntax that `syn`
  does not accept. Pre-existing errors in edited files are still labelled
  as already there.
- Gap left: no type or borrow errors (needs rustc or an LSP).

### Round 12c: full YAML parse after writes

- Before: YAML got line checks only (tabs, unclosed flow brackets,
  duplicate keys, mixed lists and keys). Bad indentation inside a mapping
  (`a:\n  b: 1\n c: 2`), aliases to anchors never defined, and plain
  values containing `: ` on a wrapped line went through.
- Hermes: in-process PyYAML event parse, `yaml.parse` not a load
  (`tools/file_operations_lint.py:70-83,114`). opencode: only when a YAML
  language server is running (`src/tool/edit.ts:198-200`).
- REX now (`check_yaml_parse` in `crates/rex-tools/src/check.rs`): when
  the line checks are clean and the file is not a template, it is parsed
  with `saphyr-parser` 0.1.0 (YAML 1.2, MIT OR Apache-2.0; new dependency,
  plus `arraydeque`), and the first error comes back with line and column.
  Templates are skipped: any `{%`, or `{{` not written as GitHub Actions
  `${{`.
- Tuned to PyYAML: saphyr's indentation errors are dropped, because it is
  stricter than PyYAML on flow and quoted content. Before this, the sweep
  flagged 15 Hermes locale files (multi-line quoted strings) and
  3 base64 CircleCI configs (a `]` less indented than its items); PyYAML
  accepts all of them. Cost: an unclosed quote followed by a
  less-indented line is not reported.
- Tests: 1 new unit test, plus fixture updates in 2 existing tests. One
  of them had marked a wrapped plain value containing `: ` as fine,
  which PyYAML rejects; it is now a flagged case. Hand mutation: 10/10
  killed.
- False-alarm sweep: 0 YAML flags in this repo (317 files), opencode
  (3,926), Hermes (11,586) and the cargo registry (18,666). Totals
  unchanged: 0 / 2 / 2 / 7.
- Gap left: no schema checks (e.g. GitHub Actions keys); indentation
  errors that PyYAML would also reject can be missed.

### Round 12d: full Python parse after writes

- Before: `.py` writes got only the bracket/string/comment check, so
  `x = 1 +`, a missing `:` or Python 2 `print 'x'` went through.
- Hermes: `python -m py_compile` per written `.py` file
  (`tools/file_operations_lint.py:17-22`), which needs Python installed.
  opencode: only with a Python language server running
  (`src/tool/edit.ts:198-200`).
- REX now (`python_parse_findings` in `crates/rex-tools/src/check.rs`):
  when the bracket check is clean, the file is parsed with
  `ruff_python_parser` 0.0.14 (MIT, Astral; 46 new crates in Cargo.lock,
  all MIT and/or Apache-2.0, one with an LLVM exception, one also under Unicode-DFS-2016). The first error comes
  back with line and column (in characters). Errors that depend on the
  target Python version are not reported, because REX does not know it.
  It shares the depth guard and big-stack thread with the Rust parse
  (`guarded_parse`).
- Parser choice: `rustpython-parser` 0.4 was tried first and dropped. It
  failed 22 of the 175 valid files in ruff's own test corpus (3.12
  f-strings, 3.13 type parameter defaults, 3.14 t-strings, 3.14 except
  without brackets). ruff's parser passes all 175.
- Checked with a standalone build of the same parser: 7,094 of 7,094 `.py`
  files in this repo, Hermes and opencode parse, and 312 of the 382 files
  in ruff's invalid corpus are flagged. The other 70 are version-dependent
  or semantic errors.
- Tests: 1 new unit test (positions, multi-byte column, current syntax,
  Python 2 print, depth skip). Hand mutation: 6/6 killed.
- False-alarm sweep: this repo 0 of 317, opencode 2 of 3,926 and Hermes
  2 of 11,586 (all older notes, none from Python). In the cargo registry
  (20,602 files, now including ruff's test resources) the Python flags
  seen are ruff's deliberately invalid inputs and
  `unicode_names2-1.3.0/src/ngrams.py`, which is Python 2 code (a real
  error for Python 3). The sweep prints only the first 40 flags.
- Gap left: no name, import or type errors (needs a type checker or an LSP).

### Round 12e: full JavaScript/TypeScript parse after writes

- Before: `.js/.jsx/.ts/.tsx/.mjs/.cjs/.mts/.cts` writes got only the
  lexical bracket check. Balanced-but-wrong code (`let x = 1 +;`, a missing
  comma in an object, TS types in a `.js` file) went through, and JSX prose
  like "1) item" could raise a false alarm.
- Hermes: `node --check` for `.js` and single-file `npx tsc --noEmit` for
  `.ts`. It skips the tsc run when a language server claims the file,
  because single-file tsc ignores tsconfig (`tools/file_operations_lint.py:17-29`).
  Both need Node installed. opencode: LSP diagnostics with each edit when
  a TS server is running (`src/tool/edit.ts:198-200`).
- REX now (`js_parse_findings` in `crates/rex-tools/src/check.rs`): the
  file is parsed with `oxc_parser` 0.151 (MIT; 32 new crates in Cargo.lock,
  MIT and/or Apache-2.0, plus 4 ICU data crates under the permissive
  Unicode-3.0 licence), with the dialect taken from
  the file name (TS, JSX, `.d.ts`, module or script). A top-level `return`
  is allowed (CommonJS runs inside a function wrapper). The parser decides
  the result:
  - A clean parse clears any lexical flag. This removes the JSX prose false
    alarms.
  - A failed parse reports the lexical finding if there is one (it names
    the unmatched bracket), otherwise the parser's first error with line
    and column.
  - Files nested more than 128 deep are not parsed, so the lexical result
    stands. The parse uses the same big-stack thread as the Rust and
    Python parses.
- Checked with a standalone build of the same parser: 7,257 of 7,257 JS/TS
  files in this repo, opencode and Hermes parse.
- Tests: 1 new unit test. Hand mutation: 8/8 killed.
- False-alarm sweep: this repo 0 of 317, and opencode down from 2 to 0 of
  3,926 (both old JSX prose flags cleared). Hermes 1 of 11,586 (the
  non-Rust `d@rko.rs` text file from Round 12b; the old JSX flag cleared).
  Cargo registry: 322 flags in total, the same count as before this change.
  No JS/TS flags among the 40 printed.
- Gap left: no type errors (needs tsc or an LSP).

### Round 12f: commands in parallel edit children checked for overlap

- Before: parallel edit children claim each file they create, edit or
  patch, and another child of the batch is refused that file. A
  `run_command` could still overwrite a claimed file unnoticed, because a
  command's writes are not known before it runs.
- Hermes: no file-overlap check in `tools/delegate_tool*.py`. opencode: a
  per-path lock for its edit tool only (`src/tool/edit.ts:35-43`); bash
  commands are not covered.
- REX now (`claimed_by_others` / `changed_since` in
  `crates/rex-providers/src/explore.rs`): just before an edit child's
  approved command runs, REX takes a content hash of every file the other
  children of the batch have claimed, and compares after. Changed files
  are:
  - put at the top of that command's result for the child, which is told
    not to touch them again and to report the overlap;
  - listed as `overlaps` ("path (edit sub-agent N)") in the batch result
    the parent sees and in the ledger.
  Hashes are taken after the approval wait, so a long wait does not widen
  the window. Caveat: if the owning child writes its own file while the
  command runs, the change is still reported against the command. The
  message says the files were "modified while this command ran" rather
  than claiming the command did it.
- Tests: an end-to-end batch where both children copy over the same file
  (exactly one overlap, on the non-owner, naming the owner, reported to
  the parent; stable
  over 3 runs), and a fingerprint unit test (missing files, same-length
  change, only other children's files watched). Hand mutation: 8/8
  killed.
- Gap left: files a command creates that no child has claimed yet are not
  claimed for it. Closed in round 12g.

### Round 12g: files an edit child's command writes are claimed for it

- REX now (`tree_snapshot` / `tree_changes` in
  `crates/rex-providers/src/explore.rs`): around each approved command of
  an edit child, REX records the size and modified time of every file in
  the workspace. It skips `.git`, `target`, `node_modules`, `.venv`,
  `venv` and Python caches. Files the command created or changed that no
  other child owns are claimed for this child and listed in its
  `files_written`. A later create, edit or patch of them by another child
  of the batch is then refused, as for files written with the edit tools.
  Workspaces over 20,000 files are not scanned. Files already owned by
  another child stay with it (the round 12f overlap note covers them).
- Neither Hermes nor opencode tracks what a sub-agent's shell command
  wrote (see round 12f).
- Tests: a single-child batch whose `cp` makes a file (`files_written` is
  the command marker plus `made.txt`; the unchanged source is not
  listed), and a snapshot unit test (skipped directories, new and changed
  files, removals ignored, sorted output, a size change seen with the
  modified time put back, the file cap). Hand mutation: 8/8 killed.
- Gap left: a change that keeps both size and modified time is not seen.

## Round 13: run-scoped standing approval

- Competitors: opencode's permission prompt offers Allow once / Allow
  always / Reject (`packages/tui/src/routes/session/permission.tsx`).
  "Always" adds a session rule keyed on a command prefix
  (`packages/opencode/src/permission/arity.ts`). Hermes keeps
  per-session and permanent approval sets (`tools/approval.py`,
  `_session_approved`, `_permanent_approved`).
- REX before: only "Approve once", so a run that re-runs the same test
  command after each fix asked for the same approval each time.
- REX now: an "Allow for this run" button on command approvals
  (`standing_key` in `crates/rex-tools/src/lib.rs`,
  `AutonomousRunService::decide_always` and the standing check in the
  approval wait in `crates/rex-providers/src/autonomous.rs`, Tauri
  command `agent_decide_always`). It is stricter than both:
  - it covers the exact argv plus working directory only, with no prefix
    matching;
  - only commands that pass the command policy qualify, and only when the
    program is a bare name (never `./build.sh` or `bin/tool`, which the
    model could rewrite);
  - never offered for file writes, patches or MCP calls;
  - kept in memory for one run (a resumed run starts empty), at most 8
    per run;
  - every command it lets through is logged (`ApprovedByStanding`) and
    shown in the activity timeline. Deny keeps the default focus.
- Tests: rex-tools key rules and a prepare test; a loop test (a second
  identical `cp` runs without asking, a different target still asks, a
  file write refuses a standing approval); a cap test; 2 vitest cases for
  the button and the timeline rows. Hand mutation: 7/7 killed, plus one
  equivalent mutant (the `approval_required` gate in `prepare`: every
  command that passes the policy already needs approval).
- Gap left: the dev sidecar (`rex-mcp` dev route) has no route for it, so
  the button is desktop-only. MCP work is parked.

## Round 14: the model can look at images in the workspace

- Competitors: opencode's `read` tool returns PNG/JPEG/GIF/WebP files as
  image attachments (`packages/opencode/src/tool/read.ts`,
  `SUPPORTED_IMAGE_MIMES`). Hermes has `vision_analyze`
  (`tools/vision_tools.py`), which attaches images natively to a
  vision-capable model or sends them to a helper model.
- REX before: `read_file` refused every image as a binary file, so the
  model could not look at a screenshot, mockup or chart in the workspace.
- REX now (`with_image_reads` / `take_images` / `sniff_image` in
  `crates/rex-tools/src/lib.rs`, `WireImage` / `user_content` in
  `crates/rex-providers/src/autonomous.rs`):
  - In autonomous runs, `read_file` on a PNG, JPEG, GIF or WebP returns a
    short note ("image/png image, N bytes ...") and queues the image.
  - The next model turn carries it after the state message, with a label
    naming the file. Formats: Gemini `inlineData`, Anthropic base64
    `image` blocks, OpenAI `image_url` data URLs (the same shapes Hermes'
    adapters emit, `agent/gemini_native_adapter.py`,
    `agent/anthropic_message_convert.py`). It is sent on that one turn
    only, so image tokens don't pile up.
  - The kind comes from magic bytes, never from the file name, so a text
    file named `x.png` is still read as text. Limits: 3.75 MB per image
    (keeps base64 under Anthropic's 5 MB per-image limit), 3 per turn. A
    read over either limit is refused with a clear message, not dropped
    silently.
  - Evidence files record the request with the image bytes replaced by a
    placeholder, and the token estimate uses that copy.
  - Other callers (explorer children, CLI tools) keep the binary-file
    refusal, and the tool description says so.
- Tests: 2 rex-tools tests (queueing, off by default, name vs magic bytes,
  size cap, queue cap, draining; magic-byte sniffing) and 2 loop tests
  (all 3 wire shapes plus unchanged shapes with no image; a run where the
  image reaches the next request only, and never reaches evidence). Hand
  mutation: 12/12 killed.
- Gap left: a model without image input will reject the request. REX
  does not yet know which models take images, so it cannot fall back to
  text. SVG is not rasterised (Hermes does rasterise it). The text
  fallback was added in round 14b.

### Round 14b: text-only models get the turn again without images

- Hermes (`agent/turn_recovery.py`, `recover_before_classification`;
  `agent/message_sanitization.py`, `_looks_like_image_content_rejection`)
  matches provider error phrases, records the model as rejecting images and
  strips images from its later requests. opencode decides from its models.dev
  modality data (`packages/opencode/src/provider/transform.ts`), which REX
  cannot fetch offline.
- REX now: when a turn carrying images fails after the normal retries, REX
  sends it once more with each image replaced by a note ("Image from
  read_file X was not shown: this model rejected image input earlier in the
  run"). Later image reads in the run go out as that note, without another
  failed attempt. One Info event records the rejection. REX does not rely on
  error wording, so any final failure of an image turn gets the text-only
  retry. The cost is at most one extra request per run.
- Tests: a scripted text-only model (400 on any `inlineData`). The run
  finishes without a provider error, only one request ever carries the
  image, the retry and the next turn both carry the note, and the Info
  event appears once. Hand mutation: 5/5 killed.
- Gap left: SVG is not rasterised. REX still has no per-model capability
  data, so the first image turn on a text-only model costs one failed
  request.

## Round 15: the full text of clipped command output (`read_output`)

- Competitors: opencode writes any tool output past 2,000 lines or 50 KB to
  a file and returns a preview plus a hint to read or grep that file
  (`packages/opencode/src/tool/truncate.ts`). Its read tool may open that
  directory, and old files are removed after 7 days.
- REX before: a `run_command` result reached the model as head plus tail
  in 4,000 characters (`OUTCOME_CHARS`). The middle of a long test or build
  log, often where the failure is, was gone for good.
- REX now (`crates/rex-providers/src/output_store.rs`, the `read_output`
  tool in `crates/rex-providers/src/autonomous.rs`):
  - When a command's output or error text is clipped, the result carries
    `full_output` (call_id, line count and a hint), and REX keeps the full
    text.
  - `read_output` pages it (200 lines, numbered, with `next_offset`) or
    searches it (literal, case-insensitive, up to 100 numbered matching
    lines).
  - Unlike opencode, nothing is written to disk or into the workspace. The
    text is kept in memory for this run only: the last 8 outputs, 1.1 MB
    each. A reply is capped at 16,000 characters and 400 per line.
  - It is read-only and needs no approval. Role allowlists apply, and
    sub-agents get a clear refusal.
- Tests: 2 store tests (paging, end of text, search, match cap, oldest
  dropped, same id replaced, line and reply caps, byte cap on a char
  boundary) and a scripted run. In that run `cat` of a 3,000-line log
  hides line 1,500, a search finds it, paging shows it in context, and an
  unknown id gets a readable error. Hand mutation: 13/13 killed.
- Gap left: edit sub-agents' clipped command output is not kept (closed
  in round 18b). The saved text is lost on resume.

## Round 16: recall earlier runs in the same workspace (`past_runs`)

- Competitors: Hermes' `session_search` tool
  (`tools/session_search_tool.py`) lets the model search past sessions
  (full-text search over its session database, no LLM calls). opencode
  has no model-facing tool for earlier sessions: its tool directory
  (`packages/opencode/src/tool/`) has none.
- REX before: a new run knew nothing about earlier runs in the same
  workspace unless the user resumed a named session. The completion
  summary lived only in memory and was lost when the app closed.
- REX now:
  - `terminal.json` also stores the outcome label, the completion summary
    and the files the run's recent turns wrote (`files_changed`).
  - The `past_runs` tool (`crates/rex-providers/src/run_history.rs`)
    returns up to 5 earlier finished runs of this same workspace, newest
    first: run id, task, outcome, summary and up to 10 files. Tasks and
    summaries are cut at 600 characters.
  - An optional query keeps runs whose task, summary or files contain
    every word, case-insensitively.
  - It reads only REX's own run records: never file contents, never other
    workspaces, never the current run. The reply tells the model to check
    the files before relying on it. Runs that are still going are left
    out. Older `terminal.json` files still count, with outcome "finished"
    and no summary.
  - Sub-agents get a clear refusal. Role allowlists apply.
- Tests: 2 run_history tests (workspace filter, newest first, unfinished
  and own runs left out, legacy records, all-words case-insensitive query,
  limit, clipping, file cap, default workspace), a `files_changed` test,
  and a scripted two-run test: run 2 recalls run 1's id, summary, outcome
  and index.html, a miss says so, and run 2 never lists itself. Hand
  mutation: 13/13 killed, plus 1 equivalent in the loop test (the current
  run has no terminal.json while it runs; the unit test covers `exclude`).
- Gap left: files came from the turn digest (last 12 turns), so a long
  run listed only its later changes (closed in round 16b). There is no
  ranking beyond newest first.

## Round 16b: whole-run file list for `past_runs`

- Competitors: Hermes' session search returns matching messages from a
  session, not a list of changed files (`tools/session_search_tool.py`).
  opencode has no recall tool (round 16). This round closes REX's own
  round 16 gap; it adds nothing either competitor has.
- REX before: `terminal.json`'s `files` came from the 12-turn digest, so
  a long run lost its early changes. It also skipped `apply_patch`
  entirely, and files an edit sub-agent wrote were never listed.
- REX now:
  - The checkpoint keeps `files_written` for the whole run:
    create_file, edit_file, every path of an apply_patch (added, changed
    or deleted) and the files edit sub-agents wrote. The list keeps
    first-seen order, skips repeats, blanks and markers such as
    "(run_command)", and stops at 200 paths.
  - It lives in the checkpoint, so it survives resume. A checkpoint saved
    before this round still works: the digest's writes are merged in
    after the list.
  - Only successful calls count. The prompt and tools are unchanged, so
    the prompt identity (and checkpoint resume) is not affected.
- Tests: a merge unit test (whole-run list first, digest extras after,
  apply_patch split, markers, blanks, repeats, the cap); a scripted
  16-turn run (one apply_patch adding two files, 13 create_file turns and
  a failed edit) whose `terminal.json` lists all 15 files in order; and
  the round 12f sub-agent test now checks the run lists `seed.txt` and
  `made.txt` but not the marker. Hand mutation: 12/12 killed. One more
  mutant (dropping the success check) is equivalent today: failed tool
  results never carry a target.
- Gap left: a command the main agent runs (not a sub-agent) that writes
  files is still not listed (closed in round 18).

## Round 17: agent skills (`load_skill`)

- Competitors: opencode finds `SKILL.md` folders (front matter `name`,
  `description`) under `.claude/skills` and `.agents/skills` in the
  project and home directory plus its own config dirs, lists them to the
  model, and loads one on demand with its `skill` tool
  (`packages/opencode/src/skill/index.ts`, `src/tool/skill.ts`; the reply
  adds the skill's folder and a sample of up to 10 of its files). Hermes
  keeps skills in `~/.hermes/skills` and exposes `skills_list` and
  `skill_view` (`tools/skills_tool.py`).
- REX before: no skills. Only one `AGENTS.md`-style file reached the model.
- REX now (`crates/rex-prompt/src/skills.rs`, REX's own code):
  - Lookup, nearest first, first name wins: `.rex/skills`,
    `.agents/skills`, `.claude/skills` in the workspace and its parents up
    to the git root (the same rule as `AGENTS.md`), then
    `~/.config/rex/skills`, `~/.agents/skills`, `~/.claude/skills`. One
    level only (`<dir>/<folder>/SKILL.md`), at most 64 folders per
    directory and 40 skills.
  - A skill needs front matter with a valid name (lowercase letters,
    digits, `-`, `_`, `.`; up to 64 characters) and a description.
    Symlinked files, non-UTF-8 files and files without front matter are
    skipped. Descriptions are cut at 300 characters.
  - The per-turn state lists name, description and source with a
    precedence note: skills are guidance, rank below the constitution,
    approvals, tool limits and the task, and never grant permissions.
    The list is not in the system prompt.
  - `load_skill {name}` returns the body (up to 16,000 characters,
    marked when cut), the source and up to 10 other files in the skill
    folder (two levels, hidden files skipped). An unknown name gets an
    error listing the skills that exist. It is read-only and needs no
    approval; role allowlists apply, sub-agents get a clear refusal, and
    each load is written to the run ledger.
  - Adding the tool changes the prompt identity, so older checkpoints
    fail closed on resume (as in rounds 15 and 16).
- Tests: 5 skills tests (front matter incl. BOM, CRLF and a `----` line,
  quotes, names; lookup order across workspace, repo root, config and
  home; duplicates, skips and description cap; the 40-skill cap; load
  with body, files, truncation, unknown name and a skill removed after
  lookup; symlinked `SKILL.md` skipped) and a scripted run (the first
  request lists the skill without its body, `load_skill` returns the body
  and `ship.sh`, an unknown name lists `deploy`, a missing name is a bad
  call, plus event and ledger entry). Hand mutation: 28/28 killed after
  one test was strengthened (a front-matter closer check survived the
  first pass).
- Gap left: no nested skill folders (opencode globs `skills/**`), no
  remote skill sources, and `read_file` cannot open files of a
  user-level skill (they are outside the workspace; closed in 17b).

## Round 17b: read a skill's own files; UTF-8 cut at the byte cap

- Competitors: opencode's skill reply names the skill folder and says
  relative paths in the skill are relative to it; its `read` tool then
  opens them, asking first when the folder is outside the project
  (`src/tool/skill.ts`, `src/tool/external-directory.ts`).
- REX before: `load_skill` listed a skill's files but nothing could open
  them when the skill was user-level (`read_file` stays in the
  workspace).
- REX now:
  - `load_skill {name, file}` reads one text file inside that skill's
    folder. The path must be relative, at most two levels deep, with no
    `..`, hidden or empty parts, and not SKILL.md. Every part is checked
    without following links, so a symlinked file or folder is refused.
    Non-UTF-8 files are refused. Up to 16,000 characters, marked when
    cut. The event, ledger entry and model part name the file.
  - Bug fixed while testing: a file over the 64 KiB read cap whose cap
    fell inside a multi-byte character was treated as non-UTF-8 and
    dropped. That hit SKILL.md and also `AGENTS.md`-style project
    instructions (`project.rs`, rounds 2 and 5). Now only the cut last character
    is dropped.
- Tests: `load_file` (plain read, `./` prefix, a file over the cap cut
  inside a character, 12 refused paths with their reasons, unknown skill)
  and symlinked file and folder refused; a `decode_prefix` unit test; a
  large SKILL.md and a large `AGENTS.md` cut inside a character still
  load; the scripted run reads `ship.sh` and refuses `../../../x`. Hand
  mutation: 16/16 distinct mutants killed
  (17 runs; one mutant was run twice). The first pass left 4 survivors: two overlapping
  checks (I removed the redundant symlink check) and two tests whose
  byte cap happened to land on a character boundary (fixed).
- Gap left: no nested skill folders (closed in 17c) and no remote skill
  sources.

## Round 17c: nested skill folders

- Competitors: opencode globs `skills/**/SKILL.md` under `.claude` and
  `.agents` and `{skill,skills}/**/SKILL.md` in its config dirs
  (`packages/opencode/src/skill/index.ts`), so a skill can sit in a
  category folder. Hermes groups skills by category folder too
  (`~/.hermes/skills/<category>/<skill>/SKILL.md`, `tools/skills_tool.py`).
- REX before: only `<skills dir>/<folder>/SKILL.md` (one level).
- REX now: a SKILL.md up to three folders below the skills directory is
  found (`tools/web/css/SKILL.md`). The walk is depth-first and sorted,
  skips hidden folders and symlinks, reads at most 64 subfolders per
  folder and 256 folders per skills directory, and still stops at 40
  skills. First name wins as before.
- Tests: nested lookup (one, two and three deep found, four deep and a
  hidden folder not, a non-skill `ref` folder ignored, load of a nested
  skill) and a bounded-walk test (400+ nested empty folders use up the
  walk; 64 folders fill the per-folder cap). Hand mutation: 10/10 killed
  after the bounded-walk test was rebuilt: in the first pass the
  per-folder cap hid the walk cap, so 3 mutants survived.
- Gap left: no remote skill sources (opencode can pull skills from URLs;
  that needs network and trust rules).

## Round 18: files the main agent's commands write

- Competitors: neither tracks which files a shell command wrote for a
  per-session file list (see round 12f). This closes REX's own round 16b
  gap.
- REX before: the whole-run file list (`past_runs`) had create, edit,
  patch and edit sub-agent writes, but not files made by the main
  agent's own `run_command` (a `cp`, a code generator, a formatter).
- REX now: right before an approved command runs (after the approval
  wait, so the user's own edits meanwhile are not counted) REX stamps
  the workspace (size and modified time, skipping `.git`, `target`,
  `node_modules` and other build and cache folders, off above 20,000
  files), stamps it again after, and adds new or changed files to the
  run's list. A failed command counts too, because it may still have
  written files. `run_command` always needs approval, so only that path
  stamps.
- Tests: a scripted run (`cp seed.txt made.txt` then `ls`) whose
  `terminal.json` lists `seed.txt` and `made.txt` only. Hand mutation:
  3/3 killed. A 4th, the stamp on the no-approval path, survived because
  that path is unreachable for commands; I removed that code.
- Gap left: build output outside the skipped folders (for example
  `dist/`) is listed too, up to the 200-path cap; a change that keeps
  size and modified time is not seen.

## Round 18b: an edit sub-agent's clipped command output

- Competitors: opencode writes the full text of a truncated tool result
  to a file and tells the model where it is, for any agent including
  subagents (`packages/opencode/src/tool/truncate.ts`). Hermes spills
  large tool results to storage (`tools/tool_result_storage.py`).
- REX before: round 15's `read_output` kept the main agent's clipped
  command output, but an edit sub-agent's clipped output was lost.
- REX now: when an edit sub-agent's command output is clipped, the child
  is told the id it is kept under, and the batch result hands the full
  text to the parent: each entry has `full_outputs` (call id, line count,
  hint). The parent stores them in its own output store, so its
  `read_output` pages or searches them. Up to 4 per child; past that
  nothing is kept and no id is promised. Still in memory only, for one
  run; nothing is written to the workspace.
- Tests: a scripted run (an edit child `cat`s a 3,000-line log; the
  child sees the clipped text and the id, the batch result carries
  `full_outputs` with the line count, and the parent's `read_output`
  finds the hidden line 1501) and a cap unit test. Hand mutation: 7/7
  killed.
- Gap left: saved outputs are still lost on resume (closed in round
  18c); children cannot call `read_output` themselves.

## Round 18c: clipped outputs survive resume

- Competitors: opencode keeps truncated tool output in files under its
  data folder, so it outlives the process (`src/tool/truncate.ts`,
  `src/tool/truncation-dir.ts`). Hermes writes spilled results to
  `$HERMES_HOME/cache/spillover/{id}.txt` (`tools/tool_result_storage.py`).
- REX before: `read_output`'s store lived in memory, so a run resumed
  after the app closed could not read outputs it had already been told
  about.
- REX now: the store is attached to the run's state folder. Each kept
  output is written to `state/outputs/<call id>.txt` with an ordered
  `index.json`; outputs the store drops are deleted there too. Resume
  loads them back in order. Only ids of ASCII letters, digits and `-` (up
  to 64) become file names; any other id stays in memory only and is
  never read back. A broken index or missing file is skipped. Still
  nothing is written into the workspace.
- Tests: a store unit test (mirror, cap, re-save order, unsafe id, stale
  file removal, reload, broken index, missing file, unattached store) and
  a scripted resume: run 1 `cat`s a 3,000-line log then parks and the app
  "dies"; the resumed run's `read_output` finds the hidden line 1501.
  Hand mutation: 9/9 killed.
- Gap left: sub-agents still cannot call `read_output` themselves.


## Round 19: workspace notes (`remember`)

- Competitors: Hermes gives the agent a `memory` tool (add, replace,
  remove) over a curated `MEMORY.md` of about 2,200 characters, shows it
  at the start of every session and scans each entry for injection and
  exfiltration patterns first (`tools/memory_tool.py`,
  `tools/memory_tool_store.py`). opencode has no agent-written memory;
  it reads only the instruction files people write (`AGENTS.md` etc.).
- REX before: `past_runs` could recall what earlier runs did, but the
  agent had no way to keep a fact (build command, layout, a pitfall) for
  the next run of the same workspace.
- REX now: a `remember` tool (`action` add or remove, `text`). Notes
  live in REX's own runs folder under `.notes/<hash of workspace>.json`,
  never in the workspace, and are per workspace. Later runs see them in
  the state under `notes`, labelled as possibly stale and ranked below
  the constitution, approvals, tool limits and the task. Limits: 400
  characters per note, 20 notes, 2,200 characters in all. A note is
  refused when the output redactor would flag a secret in it, when it
  reads like an instruction to a future agent, or when it is empty or
  too long. The same note is not stored twice; remove needs a piece of
  text that matches exactly one note. Writes go through a temp file and
  rename. Sub-agents cannot use it. Unlike Hermes there is no replace
  action (remove then add) and no user-profile file.
- Tests: a store unit test set (per-workspace path outside the
  workspace, add/dedupe/remove, ambiguous and missing remove, secret and
  instruction refusals, count and size caps, corrupt and hand-edited
  files), an argument test, and a scripted three-run test: run 1 saves a
  fact, gets the secret and instruction refusals and a bad action, adds
  and removes a scratch note; run 2 of the same workspace sees only the
  kept fact with its label; run 3 of another workspace sees nothing.
  Hand mutation: 19/19 killed.
- Gap left: no replace action; the instruction check is a short phrase
  list, not a full injection scanner like Hermes's threat patterns.

## Round 20: build output left out of the changed-files list

- Competitors: opencode's snapshot drops gitignored files
  (`src/snapshot/index.ts`, `ls-files --exclude-standard` and "removing
  gitignored files from snapshot"). Hermes's checkpoints exclude
  `dist/`, `build/`, `out/`, caches, logs and similar through
  `DEFAULT_EXCLUDES` (`tools/checkpoint_manager.py`).
- REX before: the tree snapshot around `run_command` skipped only a short
  list (`.git`, `target`, `node_modules`, virtualenvs, Python caches), so
  a build that wrote `dist/` or `build/` listed every generated file as
  changed by the run (round 18's gap).
- REX now: the snapshot also skips the search walker's list
  (`dist`, `build`, `coverage`, `.next`, `.cache` and others) and
  whatever the workspace root `.gitignore` ignores, using REX's own
  ignore matcher: anchored and unanchored rules, file globs such as
  `*.log`, and directory-only rules that never hide a file of that name.
  Files the agent writes through its own write tools are still listed
  wherever they are.
- Tests: a snapshot test where a "build" writes eight generated files
  (dist, build, coverage, .next, a gitignored `/gen/`, two `*.log`, one
  listed file) and only the three real edits plus a file sharing a
  dir-only rule's name are reported. Hand mutation: 6/6 killed.
- Gap left: nested `.gitignore` files and `!` re-includes are not read
  (the matcher then keeps only directory rules, as in search).

## Round 21: `remember` can replace a note

- Competitors: Hermes's memory tool has `replace`: `old_text` finds one
  entry and the new content replaces the whole entry, after the same
  content scan as add (`tools/memory_tool_store.py`, `replace`,
  `_locate`). opencode has no agent-written memory.
- REX before: fixing an outdated note took a remove and an add, and the
  fixed note moved to the end (round 19's gap).
- REX now: `remember` takes `action: "replace"` with `old` (a piece of
  the note) and `text` (the new note). The note keeps its place. The new
  text goes through the same secret, instruction, empty and length
  checks; the total-size check counts the new text instead of the old;
  a replace that would duplicate another note is refused. Finding the
  note is shared with remove (case-insensitive, exactly one match).
- Tests: a replace unit test (in place, the checks, missing and
  ambiguous matches, duplicate, self-replace, size accounting), wider
  argument tests, and the three-run test now replaces a note in run 1 and
  run 2 sees only the new text. Hand mutation: 12/12 killed.
- Gap left: the instruction check is still a short phrase list.

## Round 22: edit sub-agents can read their own clipped output

- Competitors: opencode clips long tool output, saves the full text to a
  file and tells the model to read it with its own read tool
  (`src/tool/truncate.ts`), which task sub-agents have too. Hermes's
  spillover note likewise points the agent at `read_file`
  (`tools/tool_result_storage.py`).
- REX before: an edit sub-agent whose command output was clipped could
  only name the id in its report for the parent to read (round 18b); it
  could not look at the middle of its own test log to fix the failure.
- REX now: edit sub-agents have `read_output`, with the same page and
  search replies as the parent's. A child reads only outputs it kept
  itself, not the parent's or a sibling's; it counts toward the child's
  tool-call budget and follows the run role's allowed tools. The clipped
  result tells the child it can use it. Explore and research children
  (which cannot run commands) still do not have it.
- Tests: a scripted run where an edit child `cat`s a 3,000-line log,
  searches its own saved output and finds the hidden failing line, pages
  from line 2190, and is refused an id it never kept; an explorer child in
  the same run is not offered the tool and is refused. The edit child's
  tool list test now includes `read_output`. Hand mutation: 7/7 killed.
- Gap left: none from round 18b. A child still keeps at most 4 outputs.

## Round 23: a real injection check for workspace notes

- Competitors: Hermes scans every memory entry with its strict threat
  patterns before saving (`tools/memory_tool_store.py`
  `_scan_memory_content`, `tools/threat_patterns.py`): override phrases,
  hiding things from the user, role hijack, sending data to URLs, curl or
  wget with secret variables, reading secret files, SSH keys, editing
  agent config files, invisible unicode, after NFKC folding. opencode has
  no agent-written memory to scan.
- REX before: `remember` refused notes from a 14-phrase list checked by
  plain substring (round 19/21's gap), so "disregard your earlier rules"
  or a full-width "ｉｇｎｏｒｅ" got through.
- REX now: REX's own 15 patterns in the same groups (override,
  system_prompt, hide_from_user, approval_bypass, role_hijack,
  forced_rule, send_out, secret_in_command, secret_files, agent_config)
  run on NFKC-folded lowercase text, words in between allowed, plus a
  zero-width/bidi character check. The refusal names the group. Notes in
  a hand-edited file that fail the check or hold a secret are not loaded.
  New dependencies are `regex` and `unicode-normalization`, both already
  in Cargo.lock (no new downloads).
- Tests: 25 payloads, each caught under the right group, and 9 plain
  facts that must pass ("the previous maintainer kept rules in
  docs/rules.md", "config lives in .env.example", "AGENTS.md describes the
  crate layout", ...), plus a hand-edited file test. Hand mutation: 19/19
  killed (each pattern deleted in turn, the hidden-character check,
  folding, lowercasing, both load filters).
- Gap left: cross-script look-alikes (Cyrillic letters) are not folded,
  same as Hermes.

## Round 24: nested .gitignore files

- Competitors: opencode's glob and grep tools run ripgrep
  (`src/tool/glob.ts`, `src/tool/grep.ts`, `@opencode-ai/core/ripgrep`),
  and Hermes's file search prefers `rg` (`tools/file_operations.py`);
  ripgrep reads the `.gitignore` of every folder it walks.
- REX before: search_files, glob_files and the run_command change
  snapshot read only the workspace root `.gitignore`, so a monorepo's
  `web/.gitignore` (build output, generated files) was ignored by REX
  (round 20's gap).
- REX now: entering a folder with its own `.gitignore` adds its rules,
  which apply only below that folder and match against the path from
  there (anchored `/out/` means `web/out`, not `web/deep/out`). Rules stack
  through levels; a folder without a file shares its parent's rules
  (no copy); the root file is not read twice. The same walker feeds
  search_files, glob_files and the change snapshot. `!` re-includes
  still turn a file's rules into directory-only rules, as before.
- Tests: three walker tests (nested scope and anchoring, three-level
  stacking, folder-name boundaries and entry cases) and the snapshot test
  now has a nested rule that hides `pkg/*.gen` but not a root `a.gen`.
  Hand mutation: 7 of 8 distinct killed; the survivor drops the
  full-path match for rules without `/`, which only differs for `**`
  inside a name (kept as it was before this round). One dead guard the
  mutation run exposed was removed.
- Gap left: `!` re-includes; `.git/info/exclude` and the global excludes
  file are not read.
