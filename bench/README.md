# REX Bench

Fixed, in-repo suites scored blindly by the deterministic verifier. The same
worker model runs raw, Simple and Ultra; the checker only re-executes proofs
against the produced workspace, so uplift belongs to the harness, not to
grading prose.

    cargo run -p rex-ultra --bin rex-bench -- run --suite bench/suites/core.jsonl \
        --mode raw --provider gemini --model gemini-3.5-flash-lite --out /tmp/raw.jsonl
    # repeat with --mode simple and --mode ultra (same model)
    cargo run -p rex-ultra --bin rex-bench -- report /tmp/raw.jsonl /tmp/simple.jsonl /tmp/ultra.jsonl

The provider key comes from the normal REX credential store. Every result row
records tokens, wall time, steps, approvals, per-check outcomes and the
terminal state, so a claim like "Ultra doubles a Flash-Lite worker" is
reproducible from artifacts, not asserted.
