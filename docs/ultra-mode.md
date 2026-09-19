# REX Ultra Mode

Ultra is a higher tier of the REX Harness: the same worker model, wrapped in a
verification institution. Simple asks "does it work?"; Ultra asks "how can this
be wrong while every green check still passes?" and then goes after those gaps.

Design rule: **the harness never trusts the builder's own claim.** Completion
requires independent, executable evidence for every obligation in an explicit
acceptance contract.

## Roles

- **Worker model** - the model selected in the prompt box. It drafts the
  contract and builds the change. Benchmarks hold this model fixed across
  raw / Simple / Ultra so measured uplift belongs to the harness.
- **Adversary** - a separate run whose only job is to find concrete ways the
  builder's result is wrong. Same safe provider routes; may be a different
  model when one is configured.
- **Clean-room judge** - a single-shot pass that sees the original task, the
  contract, and the evidence manifest - never the builder's narrative. It
  renders per-obligation verdicts in strict JSON.
- **Verifier** - not a model at all. Deterministic Rust that re-executes every
  executable proof in the contract against the real workspace and hashes every
  artifact. Forged or stale claims fail here.

## Phase status

### Phase 1 - contract, epistemic ledger, proof-carrying completion (landed)

- Acceptance contract compiler: the worker model turns the task into
  machine-checkable obligations; deterministic validation rejects empty,
  duplicated or unexecutable proofs.
- Epistemic ledger: every fact the run relies on is Observed (tied to an
  evidence id), Inferred (tied to observed facts) or Guess. Only Observed
  facts discharge obligations. Guesses may guide exploration; they never
  unlock completion.
- Proof-carrying completion: a run completes only when (a) the deterministic
  verifier re-executes every executable obligation against the workspace and
  it passes fresh, (b) the adversary pass returns no standing defect, and
  (c) the clean-room judge passes every obligation. All three are recorded in
  a proof bundle on disk: `contract.json`, `verification.json`,
  `adversary.json`, `verdicts.json`, `ledger.jsonl`, `evidence/`.
- Repair loop: failed verification, standing adversary defects or judge
  rejections feed a bounded repair pass (max 2) with the failure digest.
- Ultra budgets are materially larger than Simple but still bounded.
- Benchmark instrumentation: `rex-bench` runs a fixed suite against the same
  worker model in raw, simple and ultra modes and scores every task blindly
  through the deterministic verifier. No cherry-picking: the suite is data in
  the repo, scoring is command execution, results land as JSONL.

### Phase 2 - competing candidates and promotion (landed)

- Up to four candidates start from one immutable pre-run state. Git targets
  use disposable worktrees and branches; plain directories use full isolated
  copies plus a checksum-locked backup. Candidates run two at a time under
  the same provider, pinned worker model and Ultra options.
- Every candidate runs the complete Phase 1 pipeline and owns a separate
  proof bundle. Selection reloads the bundle, rejects missing or inconsistent
  contract / verifier / adversary / judge evidence, and independently
  re-executes every executable obligation against that candidate workspace.
- Selection is deterministic: qualified candidates only, then fewest repairs,
  fewest tokens and original candidate order. Scheduler-dependent wall time
  is recorded but never breaks ties.
- `candidates.json` is atomically checkpointed after each candidate. A matching
  interrupted competition can resume its independently verified candidates;
  mismatched targets, models, providers, candidate counts or corrupted backup
  hashes fail closed.
- Promotion occurs only after selection: a no-fast-forward winner merge for a
  git target or an exact manifest sync for a plain directory. Rollback restores
  the exact pre-run commit/tree, and disposable workspaces and branches are
  removed after their manifests have been sealed.

### Phase 3 - adversary world, counterexamples, differential oracle

- Counterexample engine: generates inputs and environment states designed to
  falsify each completion claim, then executes them.
- Behavioral differential oracle: old vs new builds run side by side; every
  observable difference must be justified by the contract or the run fails.
- Semantic mutation testing: stale cache, wrong account, partial write,
  duplicate event, reordered stream, expired auth, corrupt checkpoint,
  unsupported provider responses.

### Phase 4 - shadow and recovery worlds

- Shadow world replays recorded real interactions against old and new builds.
- Recovery world: fault injection - process death mid-action, network loss,
  corrupt state, cancellation - with proof that state survives.
- Deterministic replay of runs and failures from the ledger.

### Phase 5 - independent reconstruction and capability synthesis

- A fresh agent receives only the original request, the final workspace and
  the proof bundle. If it cannot reconstruct why the result is correct, the
  task stays open.
- Capability synthesizer: temporary narrow tools built inside the sandbox,
  tested, used, discarded - production authority never expands.
- Causal failure tracer across UI event, IPC call, process, file mutation and
  model decision.

## Honesty rules

- The UI shows the real phase and the real obligation states. Nothing claims
  a capability that is not implemented in this repository.
- The judge is labelled with the exact provider/model that rendered verdicts;
  when it is the same model as the worker, the bundle says so.
- A phase that has not landed does not appear in the product.
