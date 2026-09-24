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
  `AGENTS.md`, `CLAUDE.md`, `.cursorrules`. REX (round 2) loads the first of these at the workspace root into per-turn state, capped at 8k chars, symlinks refused. Gap left: no parent-dir walk or global file (opencode does both).
- Sub-agent delegation: opencode `src/tool/task.ts`; Hermes
  `tools/delegate_tool.py`. REX (round 2): `explore` tool starts a read-only
  explorer sub-agent (read/search/glob only, no recursion, inside the parent's
  remaining budgets, honours cancel) and returns one report. Gap left: no
  writing/parallel sub-agents: opencode picks agent types (`task.ts:46,131`);
  Hermes gives children the parent toolsets and a batch/parallel mode
  (`delegate_tool.py:6-8`).
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
  no semantic (type/LSP) diagnostics, and no YAML/TOML check.
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
  Gap left: no markdown/html output format.
- Found while wiring this: `web_search` in the autonomous loop sends
  `seeds: []`, so with the built-in keyless REX provider it has no seeds to
  crawl. Not changed yet.
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
  `agentUndo()` in `src/data/agentRun.ts`. Gap left: no UI button yet
  (Tauri shell not compiled on the build box), and no undo to an arbitrary
  step in one call.

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
- Gap left: no UI panel renders the question yet (the phase `question` and
  types exist); one question per call (opencode/Hermes batch several).

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
- Gap left: children are still read-only; Hermes gives them the parent's
  toolsets and opencode lets the model pick agent types.

## Round 5: YAML/TOML checks and app panels

- Syntax check now covers TOML (full parse via the `toml` crate already in
  Cargo.lock; messages carry no position text so shifted problems still
  count as pre-existing) and YAML (REX's own conservative check: tab
  indentation, unclosed or mismatched flow collections; block scalars and
  brackets inside plain scalars such as `${{ x }}` are ignored). False-alarm
  sweep: 0 YAML/TOML files flagged across this repo, opencode, Hermes and
  the local cargo registry (630 YAML + 248 TOML files).
- Gap left: YAML check is not a full parser (no duplicate-key or
  indentation-structure errors); opencode's LSP route covers more.
- App: AgentRunView shows the ask_user question (choices, free text, "Let
  REX decide") and an "Undo last file change" button on finished runs that
  wrote files. Covered by 2 vitest tests; rendered with the compiled theme
  and checked visually.
