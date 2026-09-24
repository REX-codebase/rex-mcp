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
  `AGENTS.md`, `CLAUDE.md`, `.cursorrules`. REX loads none.
- Sub-agent delegation: opencode `src/tool/task.ts`; Hermes
  `tools/delegate_tool.py`.
- Context compaction of old turns: opencode `session/compaction.ts`; Hermes
  `agent/context_compressor.py`. REX keeps old evidence on disk and sends a
  digest, which is a different design; to be compared on behaviour.
