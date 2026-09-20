# REX Ultra benchmark reproducibility matrix

Status: REVIEWED / RE-SCOPED (2026-09-20). No task model calls. The scorer staging bug was fixed in e85d94b.
Scope rule (Agrim, 2026-09-20 10:06 IST): real testing only on benchmarks with (a) official Claude Opus 5 results and (b) openly, exactly reproducible benchmark + harness.

Opus 5 source: Claude Opus 5 System Card, section 8 (https://www-cdn.anthropic.com/b514064af1408018e64b1ad24e7d5e75850b4ffd/claude%20opus%205%20system%20card.pdf). Standard config per Table 8.1.A: adaptive thinking at max effort, default sampling, average of 5 trials, context up to 1M.

Verdicts: REPRODUCIBLE = open task set + open harness + pinnable versions. PARTIAL = benchmark open, comparator run config not fully public. NOT REPRODUCIBLE = vendor-run only.

---

## 1. SWE-bench Verified - REPRODUCIBLE (primary candidate)

- Opus 5 official: 96.0% (5 trials; scaffold not disclosed in system card section 8.2).
- Task set: 500 human-validated instances. HF dataset SWE-bench/SWE-bench_Verified. MIT (SWE-bench repo).
- Harness: evaluation code in github.com/SWE-bench/SWE-bench. Canonical open LM-comparison config: mini-SWE-agent "bash only" (single bash tool, ReAct loop), versioned by mini-SWE-agent release tag; temp 0.0 for 1.x, unset for 2.x (per swebench.com/verified "Click for more details on the bash-only setup").
- Scorer: per-instance Docker images, FAIL_TO_PASS + PASS_TO_PASS test execution. Fully open.
- Infra: prebuilt per-instance Docker images; heavy disk/network. Codespace disk fit for a shard must be verified during adapter build.
- Caveat: Anthropic's 96.0% scaffold is undisclosed, so we reproduce the benchmark and the open mini-SWE-agent config, not Anthropic's exact internal scaffold. This is the standard public-comparison setup.
- Source: https://www.swebench.com/verified

## 2. SWE-bench Pro - REPRODUCIBLE BENCHMARK; OPUS COMPARATOR NON-EXACT PENDING SPLIT PROOF

- Opus 5 official: 79.2% (5 trials). System card 8.2 does not name the split. Until first-party evidence binds that number to the 731-task public split, record the comparator as NON-EXACT; never assume the public split.
- Task set: 1,865 total = 731 public + 858 held-out + 276 commercial. Public split: HF dataset ScaleAI/SWE-bench_Pro (731 rows, 11 repos, 4 languages, GPL/copyleft repos chosen to reduce training leakage).
- Harness: official eval repo github.com/scaleapi/SWE-bench_Pro-os. DeepSWE paper footnote: Pro's leaderboard now runs recent frontier models under mini-SWE-agent.
- Scorer: per-instance Docker images via dockerhub_tag field; inherited PR test suites (F2P/P2P). Note: DeepSWE paper measured 32.4% verifier/judge disagreement on Pro inherited tests - scoring noise to be aware of, not a blocker.
- Infra: Docker per instance, multi-language (js, python, go, etc.).
- Sources: https://huggingface.co/datasets/ScaleAI/SWE-bench_Pro , https://scale.com/blog/swe-bench-pro

## 3. SWE-bench Multilingual - REPRODUCIBLE

- Opus 5 official: 89.5% (5 trials; 300 tasks, 9 languages).
- Task set: 300 curated tasks, 42 repos, 9 languages (C, C++, Go, Java, JS/TS, PHP, Ruby, Rust). HF dataset SWE-bench/SWE-bench_Multilingual.
- Harness: evaluation integrated into the main SWE-bench repo; same format and protocol as SWE-bench.
- Scorer: F2P/P2P per-instance Docker execution (base + instance images; no shared env images).
- Source: https://www.swebench.com/multilingual.html

## 4. SWE-bench Multimodal - PARTIAL (version and split mismatch)

- Opus 5 official: 59.4% (5 trials). System card 9.3: Anthropic ran an internal harness on the PUBLIC DEV SPLIT with modifications (removed 1 instance diegomura__react-pdf-1552, dropped specific flaky P2P tests on 4 react-pdf instances, rewrote Karma/Grunt/Jasmine reporters for machine-parseable output, inlined images as base64 data URIs).
- Task set: v1 = 517 tasks (HF SWE-bench/SWE-bench_Multimodal). v2 released 2026-09-01: 480 tasks retained for reproducible evaluation, flaky/ungradeable tests removed, Docker environments rebuilt, JS grading made robust; "complete test split and evaluation tooling are open source". Exact v2 dataset/tooling location to confirm.
- Implication: the 59.4% number is not exactly reproducible (dev split + internal mods). The open v2 test split is reproducible but is not the set that produced 59.4%. Recommendation: target v2 as the benchmark, treat 59.4% as directional only, and say so in every comparison.
- Extra requirement: multimodal input path (images inline in problem statements). Confirmed from Google's exact-model first-party page: `gemini-3.5-flash-lite` supports Text, Image, Video, Audio, and PDF inputs. Multimodal v2 remains a new benchmark; the 59.4 comparator stays directional only.
- Source: https://www.swebench.com/multimodal

## 5. DeepSWE v1.1 - REPRODUCIBLE (strong candidate)

- Preserve two separate dated comparator runs; never merge or select one: (1) Anthropic System Card section 8.3: 68.8% average over 5 trials; (2) Datacurve v1.1 leaderboard, updated 2026-09-03: claude-opus-5 [max effort] 74% +/- 4%. Compare a reproduced run only to the comparator whose exact version/config it matches.
- Task set: 113 original tasks, 91 repos, 5 languages (TS, Go, Python, JS, Rust), written from scratch, never merged upstream (contamination-free). Repo: github.com/datacurve-ai/deep-swe, Apache 2.0. Harbor task format (task.toml + environment/Dockerfile + tests/).
- Harness: Pier runner (uv tool install datacurve-pier; pier run -p deep-swe/tasks --agent mini-swe-agent --model <model>). Leaderboard protocol: mini-SWE-agent pinned at commit adfe2023, shared system prompt unmodified, single bash tool, 9000 s wall-clock cap, no step/cost cap. v1.1 grades committed code in a pristine verifier container (requires Pier > 0.3.0).
- Scorer: hand-written functional verifiers accepting any behaviorally correct implementation + regression checks; 1.4% judge disagreement (vs 32.4% on Pro). Verifiers released with the benchmark; full evaluation trajectories published.
- Metrics: pass@1 (macro-average of per-task pass fraction) and pass@4, 95% CIs from full-benchmark reruns.
- Sources: https://deepswe.datacurve.ai/ , https://deepswe.datacurve.ai/blog/deepswe-v1-1 , https://arxiv.org/html/2607.07946 , https://github.com/datacurve-ai/deep-swe

## 6. FrontierCode 1.1 - EXCLUDED (NOT REPRODUCIBLE)

- Opus 5 official: Main 53.4% (medium effort, its best), Extended 63.6% (medium), mean@5. Run and scored by Cognition (system card 8.4; cognition.com/frontiercode).
- Task set: 150 tasks (Main = 100 hardest, Extended = 150; Diamond deprecated in 1.1), maintainer-authored, individually reviewed. NOT PUBLIC: no downloadable dataset, no public harness, grading is an ensemble of held-out unit tests + weighted rubrics + model-graded checks, plus a fair-internet-use prompt and a classical scanner that zeroes runs consulting solution-bearing sources.
- Implication: cannot be run independently. Options: (a) drop from the program; (b) treat Cognition leaderboard as reference-only with no REX run. Recommendation: drop from the test matrix; note Opus 5 numbers as context only.
- Sources: https://cognition.com/frontiercode , https://cognition.com/blog/frontier-code-1.1

## 7. FrontierBench v0.1 - REGISTRY-ONLY UNTIL ₹0 ORACLE DRY-RUN

- Opus 5 official: 44.4% mean reward over 5 attempts x 74 tasks, xhigh effort, mini-SWE-agent harness on a GKE backend (Anthropic internal run; max scored 43%, within noise). 5% of API calls were safety-flagged with Opus 4.8 fallback on 4% of trials. Harbor also publishes its own evaluations (Table 8.1.A uses Harbor's).
- Task set: 74 tasks, successor to Terminal-Bench 2.1, terminal/command-line containerized work (comp-bio, physics sim, CAD, formal proofs, GPU performance). PUBLIC: github.com/harbor-framework/terminal-bench, frontier-bench/ directory; dataset and leaderboard on Harbor Hub. Tasks "evolve over time", so the v0.1 set must be pinned by commit.
- Harness: Harbor (uv tool install 'harbor[modal]'; harbor run -d frontier-bench/frontier-bench --agent <agent> --model <model>). Oracle-solution check procedure documented (run oracle 5x to validate the sandboxing environment).
- Infra risks: Harbor CI uses Modal; local Docker backend to confirm. Some tasks (GPU performance work, large sims) may not run on a free codespace. Pin and dry-run the oracle on a small subset before committing.
- Sources: https://github.com/harbor-framework/frontier-bench/blob/c622d7a2/README.md , system card 8.5

---

## Program recommendation

1. Phase 1 adapters: SWE-bench Verified + DeepSWE v1.1. Both fully open, pinnable, executable-scored, and they anchor the two most cited Opus 5 coding numbers.
2. Phase 2: SWE-bench Multilingual + SWE-bench Pro (after public-split confirmation).
3. Phase 3 gates: retain FrontierBench v0.1 only after a ₹0 oracle dry-run proves public tasks/scorer run on available infrastructure. Treat Multimodal v2 as a new benchmark only if first-party docs confirm exact-model image input; Anthropic 59.4% stays directional, never apples-to-apples.
4. Exclude FrontierCode 1.1 (not independently reproducible).
5. Dev/held-out discipline per benchmark: fixed stratified dev shard for Ultra iteration; disjoint final shard, frozen before any tuning, used for reported comparisons. Never tune on final shards.
6. Throughput reality: free tier is 500 requests/day, 15 RPM. One agentic SWE-bench attempt can take tens of model calls. Plan fixed small shards per daily window, identical shards across raw/Simple/Ultra, and record quota-stop reasons. Docker image pulls are the other bottleneck; verify codespace disk before fixing shard sizes.

## Resolved review decisions (2026-09-20)

- DeepSWE: preserve 68.8 and 74% +/- 4% as separate dated comparator runs.
- Pro: 79.2 comparator is non-exact unless its public split is independently proven.
- Multimodal: open v2 only as a new benchmark; 59.4 directional; skip if exact Gemini model lacks image input.
- FrontierCode: excluded.
- FrontierBench: registry-only pending a successful ₹0 oracle dry-run.
