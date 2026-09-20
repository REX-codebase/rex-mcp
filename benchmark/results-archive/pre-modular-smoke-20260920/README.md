# Invalid pre-modular smoke records (2026-09-20, diagnostic history only)

These three records are PRESERVED AS INVALID: pre-modular-prompt binary (prompt_version/hash null),
repo 9b3cfd7, codespace zany-space-potato, model gemini-3.5-flash-lite (free tier).

- raw/seed0: provider call succeeded (408 tokens) but no solution.py written. Genuine raw-mode fail.
- simple/seed0: provider call succeeded (15471 tokens), solution.py PROVEN present, then hidden-test
  scorer check-2 failed "No such file or directory" = HARNESS STAGING BUG. Model artifact unscored.
  Recorded as fail but is diagnostically an infrastructure error.
- ultra/seed0: genuine provider HTTP 429 at step 3 (5163 tokens consumed first). Clean quota stop.

No percentages, no publishable numbers, no held-out guidance may be drawn from these records.
Suites humanevalplus/cruxeval are OUT OF SCOPE since 2026-09-20 ~10:06 IST by owner decision
(shared-Opus-5-comparators-only rule).
