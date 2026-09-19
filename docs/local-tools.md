# Local agent tools

REX models use one provider-independent request protocol owned by `rex-tools`:
`read_file`, `search_files`, `create_file`, `edit_file`, and `run_command`.
The model never receives a raw filesystem or process handle.

## Trust boundary

1. The desktop runtime opens one explicit workspace root and canonicalizes it.
2. `tool_prepare` validates and classifies the request. Reads are bounded and
   may continue; writes and commands stop in `pending_approval`.
3. Only the trusted desktop UI may call `tool_resolve_approval`. Approval is
   bound to the server-side call ID and exact stored request, not a boolean a
   model can place in its payload.
4. `tool_execute` can run the stored request once. It returns a truthful typed
   error or an audit receipt with target, command argv, byte counts, duration,
   exit code, redaction count, and exact bounded before/after diff for writes.

## Current policy

- Relative paths only. `..`, absolute paths, canonical escapes, and symlink
  components are refused. Reads and edits are capped at 2 MiB.
- Search skips symlinks and files over 2 MiB, and caps traversal/results.
- File writes require a visible approval. Exact-match edits fail on stale or
  ambiguous content rather than guessing.
- Commands use argv, never a shell string. Shells, interpreters, network
  clients, privilege tools, destructive file/process commands, and command
  chaining are hard-denied even after approval.
- Commands require approval, use a workspace cwd, receive a cleared environment
  with only PATH/HOME/REX_WORKSPACE, have a 120-second ceiling, 512 KiB output
  ceiling, Unix CPU/address-space/file-descriptor limits, and process-group
  termination on timeout.
- Common API keys, bearer tokens, GitHub tokens, and secret assignments are
  redacted before output or errors return to the model.

This is the strongest first milestone, not unrestricted computer access. A
future capability may widen the command allowlist, but it must preserve the
same approval, receipt, cancellation, and policy boundary.
