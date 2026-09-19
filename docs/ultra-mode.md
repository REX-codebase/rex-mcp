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

## Phase 3: counterexamples, mutation, and behavioral differential

Ultra now generates a deterministic, replayable counterexample manifest from each validated acceptance obligation. Each mutation runs in a private copied candidate world with byte, count, wall-time, cancellation, and cleanup limits. Results are `killed`, `survived`, or `inconclusive`; only real verifier evidence can kill a mutation, and inconclusive results block promotion.

The semantic catalog covers stale cache, wrong account or target, partial writes, duplicate and reordered events, expired authorization, corrupt checkpoints, cancellation races, and unsupported provider responses. File obligations have concrete mutation seams now. Command and behavior obligations without an explicit fixture seam stay inconclusive rather than claiming a fake kill.

Ultra captures old and new observables for workspace files, fresh command obligations, and process outcomes before the adversary can add its own inspection artifacts. Both traces carry the same provider, model, and deterministic seed. Every difference must map to a real acceptance obligation and an explicit allowed-difference record. Missing, incomplete, unreplayed, non-reproducible, survived, inconclusive, or unexplained evidence fails closed.

`ultra/phase3.json` is sealed into the evidence store and epistemic ledger and referenced by the proof bundle. Phase 2 qualification requires it before a candidate can be selected. Simple mode does not import or call this subsystem.

## Phase 4: causal replay and recovery

Ultra completion now carries a fail-closed Phase 4 bundle. Its canonical trace uses stable, content-derived event IDs and validated parent links from the contract and model decision through the worker result, fresh verifier evidence and the completion decision. Payloads are hash-bound, sequence-checked and bounded. Missing, future, duplicated or forged parents invalidate the run.

Replay bundles pin provider, model, seed, trace hash, redacted inputs and expected observables. Inputs containing secrets are redacted and truthfully mark the run non-reproducible rather than pretending that hidden data can be replayed. Checkpoints are versioned, size-bounded, hash-verified and atomically replaced. Every effect has a request-bound idempotency key, so recovery suppresses a second application and rejects key reuse for a different request.

The fault harness has deterministic seams for process death, timeout, network interruption, truncated or reordered streams, disk failure, corrupt state or checkpoint, and cancellation races. Campaign count, time and trace sizes are bounded and cancellation is explicit. Workspace snapshots reject symlinks and unsafe paths, capture a deterministic tree, and restore every changed or added file to the exact pre-run tree. Non-promoted runs automatically trigger that rollback guard. A separate rollback rehearsal must pass before promotion.

Regression isolation performs bounded prefix bisection followed by a single-change proof. If the baseline fails, the head cannot reproduce, the probe budget expires, or the failure depends on an interaction, it refuses a culprit instead of making a false attribution. Phase 4 results are sealed into `ultra/phase4.json`, entered in the epistemic ledger, and linked from the proof bundle. Simple mode does not call this module.

## Phase 5: repository proof and independent reconstruction

Ultra now rebuilds a content-addressed repository digital twin immediately before promotion. It covers source files, dependency manifests, runtime process seams, UI and IPC routes, data flows, Tauri permissions, tests and deployment surfaces. The twin is regenerated after edits and byte-compared before use; stale or forged twins fail. A deterministic semantic symbol/import index is bound to the twin hash and is independently regenerated before promotion.

Static security, dependency, license and secret scans run over the same bounded tree. Reproducible profile records bind repository size, file count and semantic-index size to explicit budgets and commands; the profile hash is recalculated before use. The visual evidence model binds screenshots and interaction steps to the twin and fails on baseline drift. Product UI changes must supply this evidence; non-UI changes leave the optional visual field absent rather than inventing screenshots.

The capability synthesizer accepts only a closed capability enum, refuses network and subprocess authority, canonicalizes every input under a disposable workspace, compiles a narrow temporary tool, tests and uses it, then requires verified deletion. It cannot widen production authority. A fresh reconstruction pass receives only the original request, final repository twin and evidence manifest. It never receives the builder narrative or acceptance-contract prose. It must recover every required acceptance id and cite known content-addressed evidence or promotion fails.

Cross-model critics are used only when an explicit alternate provider/model route is configured and marked safe. Otherwise the report labels the critic as the same model and says why. The final gate links every acceptance id to executable evidence and links builder, adversary, shadow, recovery, clean-room, Phase 3 and Phase 4 worlds. Missing links fail closed. `ultra/phase5.json` is sealed into the evidence store and epistemic ledger; the top-level proof bundle links it before promotion.

The benchmark queue is versioned, request-bound, bounded, atomically checkpointed and resumable without silently reapplying running items. It is instrumentation only: no benchmark API run is started by Phase 5.
