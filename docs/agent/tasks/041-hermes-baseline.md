# Task 041: A measured Hermes baseline on the same memory workload

Why: the goal is to replace Hermes, and every Ditto number so far is absolute.
[comparison-2026-11.md](../../design/comparison-2026-11.md) compares mechanisms,
not results, because Hermes was read and never run. Task 017 already forbids
comparing runs whose workload, model, hardware or tools differ, so the honest
comparison is the same questions, the same model, the same machine, blind
grading.

What is and is not comparable, settled before measuring:

- **Comparable:** the five memory cases P1–P5 and the mixed case X1, clean
  profile, the same DeepSeek V4.1 Flash endpoint and list price, the same
  grading rubric, the same machine.
- **Not comparable:** the `long_use` profile. Hermes injects a curated
  `MEMORY.md` with a 2,200-character budget, so 1,000 distractor memories cannot
  be represented; its session history is searched through SQLite FTS5 instead.
  Recording that gap is the result, not a reason to fake a profile.
- **Not comparable:** sort, schedule, artifact and effect-gate cases. Ditto's
  sort tool is lease-bound and Hermes' equivalent is a general shell; measuring
  them would compare toolboxes, not the memory path this task is about.

## Contract

- Run P1–P5 and X1, five repetitions each (30 turns), through Hermes in an
  isolated `HERMES_HOME` with a fresh session per attempt, the same model, and
  the user's own words as the request. No prompt tuning for Hermes beyond what
  its configuration already does.
- Record per attempt: prompt tokens, completion tokens, wall-clock to the first
  useful word and to the final answer, tool calls, and the provider's reported
  usage. Take cost from usage × the same list price, never from zero-cost
  subscription accounting.
- Grade blind with the Task 017 rubric, by the same first assessor, with the
  provenance hidden.
- Report both systems side by side on: verified answers, Recall@5 where the
  fact was present, p50 and p95 completion, tokens, cost per verified answer.
- Record the harness differences that make a number incomparable, rather than
  hiding them in a footnote.

## Exit criteria

- A report exists with 30 Hermes attempts, none dropped, each with usage and
  timing, plus the matching Ditto clean-profile numbers from calibration 3
  recomputed by the same script.
- The report states one number for each system that a reader can compare
  directly, and names every caveat that changes its meaning.
- No Hermes configuration, skill, memory file, or profile in the user's real
  `~/.hermes` is modified; everything runs in a temporary home and the path is
  recorded.
- The cost of running this task is reported in the same table.

## Non-goals

- No attempt to make Hermes look bad or good: no extra instructions, no
  retrieval hints added to its prompt, no disabled features that would normally
  be on.
- No general benchmark suite; this is one workload where both systems can answer
  the same question.

## Risks

- A single model at a single price is a narrow result. It is still the only
  honest one available, and repeating it later with a second model is cheap
  because the harness is a script.
