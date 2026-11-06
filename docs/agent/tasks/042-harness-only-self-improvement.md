# Task 042: Harness-only self-improvement that reaches post-training-scale gains

The user's bar, in their words: no weight updates, harness only, gains comparable
to post-training, and the cheapest way to get there.

This task owns that claim. [Task 039](039-improvement-compiler-first-slice.md)
owns the plumbing (detectors, journaled proposals, approval, rollback). This one
owns the part that decides whether the result is worth anything: **which edits
are worth searching, what a probe costs, and what counts as a real gain.**

## What the evidence says the gains are made of

Harness² (arXiv 2610.14224) holds the model fixed and edits eight harness files.
Numbers worth building on:

| Lever | Measured | Ablation (removing it) |
| --- | --- | --- |
| Task-scoped harness revision, one step | +11.5 (WorkBuddy, GLM-5), +11.8 (LAB) | — |
| Execution evidence before adopting an edit | — | −10.2 and −4.9 points when edits are made without evidence |
| Domain-scoped editors (facts, procedures, constraints) | — | −2.9 / −3.7 |
| Library of adopted harnesses + lessons | — | −3.1 / −1.8 |
| Composition of two candidates | — | −1.6 / −1.9 |
| More attempts without editing (best-of-k base) | beaten at k=2 by both recursion forms | — |

Three conclusions decide the design:

1. **The gain is procedural, not informational.** Edits that win are ordering
   rules, tool-use discipline, context selection, stopping and delegation
   behaviour, and domain facts. That is exactly the surface Ditto already owns:
   instruction preludes, capability retrieval hints, memory guidance.
2. **Evidence is load-bearing.** An edit adopted without an execution result
   *lowers* the score. So a proposal that cannot be probed must not be adopted —
   `KEEP_BASE` is a first-class outcome, not a failure.
3. **Width beats depth.** Harness² improves by sampling a few candidates per
   task (k) and keeping one, not by a long chain of self-reflection.

## Why the naive loop is unaffordable, and what replaces it

Harness² spends `R = 4k` rollouts and `P = 3k` editor sessions per task. At
Ditto's measured price — $0.233 for 130 attempts over five case families,
i.e. $0.0018 per attempt — 4,000 rollouts would be ≈ $7.20 **per task**, before
the editor calls. Copying that would break both the cost premise and the
"zero overhead" premise.

Five substitutions make the same search affordable:

1. **Probe the failing family, not the suite.** A candidate is measured only on
   the cases whose oracle it is meant to move, plus the two neighbouring cases
   that share the capability. Cost is proportional to the failures, not to the
   corpus.
2. **Deterministic verdicts where the oracle is checkable.** Tool artifacts,
   schedules, memory atoms and effect counts are already verified by code, so a
   probe pays model calls only; grading adds nothing. Model grading is reserved
   for the answer axis, on the held-out run.
3. **Counterfactual replay instead of a fresh rollout.** The journal holds the
   exact request, the tool results and the events of each recorded attempt. A
   candidate changes the prompt, so the affected model call must be repeated —
   but the *tool outputs of unchanged steps are reused* from the record. One
   changed step costs one call, not a whole turn. A full 8-step rollout becomes
   1–3 calls.
4. **Budget identity as a refusal check.** A probe round is two candidates plus
   one compose, each with a fixed call ceiling and a fixed dollar ceiling; the
   round refuses to start if the estimate exceeds it. No adaptive spend.
5. **Distillation after the win.** A winning diff is compressed into the
   smallest surface that carries it — usually one sentence in the instruction
   prelude or one retrieval hint in a `capability.toml` — and the compressed form
   is re-probed. The gain survives only if it does; then the verbose harness
   text is discarded, so steady-state context cost is ~0 new tokens.

Cost of one full round under these rules, for a family with five failures:
2 candidates × (5 failures × 1–2 calls) + 1 compose × 5 = 25–35 calls ≈ $0.06,
against ≈ $7.20 for the copied recipe. That is the efficiency claim of this
task, and it is falsifiable: the journal records the calls and the dollars.

## What counts as "post-training comparable"

A gain is only claimed when all of these hold, on a run anyone can repeat:

- **Held-out by construction.** The candidate is probed on the motivating cases
  and measured on *different* tasks of the same family, at least two, so the win
  cannot be the case that produced it.
- **Size.** The family's verified-answer rate rises by ≥ 8 points, or a
  retrieval family's Recall@5 reaches ≥ 0.95, with the same model, price and
  machine, and the delta exceeds twice the observed run-to-run spread.
- **No regression.** No gate (safety, tool, schedule, resource, cost, latency)
  may fall; the neighbouring family that shares the edited capability must not
  lose verified answers.
- **No context inflation.** Prompt tokens for the affected family grow by ≤ 5%
  after distillation, measured from the run's own accounting.
- **Honest ceiling.** If more than 60% of candidates in a family come back
  `KEEP_BASE` across two rounds, the remaining failures are capability gaps, not
  harness gaps; the task reports saturation and stops editing that family.

## The first measured milestone

Not a library, not a scheduler: one family, one round, one number.

Three candidates are written down in `eval/variants/`, all aimed at failures
that campaign 1 actually produced (its latency numbers are void because a build
ran during it, but its tool-use and answer evidence is valid):

- **P3 second hop** — the tin is found, its location is not: 2 of 5 long_use
  attempts failed in calibration 3 with the guidance already in place.
  - Candidate A (`p3-second-hop-a.json`, invariant editor): the pointer rule
    becomes a stopping rule — search the named object before answering.
  - Candidate B (`p3-second-hop-b.json`, retrieval hint): the capability's
    intents name the object-to-location pattern, so the second search is
    *chosen*, not instructed.
- **X1 alphabetize** — campaign 1 failed the tool gate on this case
  (`X1`/`long_use` 3/5): both failures made exactly one model call and produced
  no artifact, while their answers were otherwise right. The request says
  "alphabetize"; `artifact.sort` advertised only "sort a list", so the capability
  was never paged in. Candidate C (`x1-alphabetize-c.json`) adds the vocabulary.
  This is the cheapest possible test of the whole thesis: if a retrieval-hint
  edit moves a tool gate from 3/5 to 5/5 at zero prompt cost, harness-only
  improvement is real and measurable here.

Probe order: C first (smallest, clearest), then A and B, then compose. The
held-out measurement for C is the same case with a different wording once
[Task 038](038-korean-and-ranked-memory-search.md) adds the Korean mixed cases.

`scripts/probe_variant.py` is the tool for this loop: it applies a variant to a
named file, runs only the chosen cases, compares against a base run directory
with the same gates, and reverts — so a round is one command, not a manual edit
and a rebuild.

## Exit criteria

- `scripts/probe_variant.py` applies, runs, compares and reverts a variant
  without leaving the tree dirty; a test proves the revert restores the exact
  bytes and that a failed apply changes nothing.
- One recorded round over the P3 family with the two candidates above, its calls
  and dollars in the journal, and a verdict that is either a measured ≥8-point
  family gain or a recorded `KEEP_BASE` with the reason.
- The distillation step is measured: the compressed form's tokens and its delta,
  or an explicit statement that the win did not survive compression.
- ADR for the promotion boundary amended with the probe economics actually
  observed, not the estimate in this file.

## Non-goals

- No population search, no bandit, no chained self-reflection, no per-turn review.
- No edits to kernel code, policy, credentials, evaluator logic or provider
  settings — the same exclusion as Task 039.
- No new benchmark framework: the existing suite, its oracle and its gates are
  the only measurement.
- No claim of post-training parity beyond the families actually measured.

## Risks

- Counterfactual replay reuses recorded tool output; if a candidate changes an
  *earlier* step the later tool calls become invalid, so the probe must detect
  that and re-run the tail. Under-detecting this would overstate gains.
- A family with few failures cannot show an 8-point move; the task must say so
  rather than shrinking the denominator.
- The cheapest probe is also the most tempting to overfit. The held-out tasks
  and the saturation rule are the guard.
