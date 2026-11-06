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

## Outcome (calibration 3 and the first probes)

Measured facts, with the runs that produced them:

- **Suite v1, calibration 3** (130 attempts, release build, contract 15):
  128/130 verified, Recall@5 45/45 clean and 43/45 long_use (0.956), $0.233.
  The two failures were a model that sorted an artifact by hand instead of
  calling `artifact.sort`. `eval/task017/budgets-v1.json` is frozen from it.
- **Case-level gates are noise at five repetitions.** Campaign 1 failed the tool
  gate on `X1`/`long_use` at 3/5; an immediate re-run of the same case with the
  same binary scored 5/5 (10/10 across both profiles, $0.0067). The gate now
  blocks a case below 0.40 until a case reaches ten repetitions, and the
  aggregate gate over the family or profile carries the standard meanwhile
  (`case_threshold` in `scripts/eval_task017.py`).
- **Harness candidates A and B on `P3` did not help.** Base: 10/10 answers,
  Recall@5 18/20. Candidate A (pointer rule as a stopping rule): 10/10, 17/20.
  Candidate B (object-to-location retrieval hint): 10/10, 15/20, and the safety
  gate flagged unused capability body loads. Both are `KEEP_BASE`, which is the
  outcome expected when the failure is the model's search choice rather than the
  harness's wording.
- **What the P3 misses are.** Answers were right in every attempt; only the
  recall metric missed a second memory. The user-visible defect there is smaller
  than the metric suggests, and the real retrieval gap remains Korean matching
  (Task 038), not this case's wording.
- Two candidates out of two in this family came back `KEEP_BASE`. Under this
  task's own saturation rule, `P3` wording is not worth a third edit; the next
  probe should target a case whose *answer* fails, not only its recall.
- **Candidate D did target a real answer failure.** Campaign 4 failed
  `P2`/`long_use`#2 with `bound_exceeded`: the model spent all eight requests on
  seven reworded memory searches (`reading group`, `book club`, `Birch Room`,
  `get-together venue`, `party venue pub restaurant`, `reading`, `gathering place
  meeting`) and never answered. The guidance told it to search and never said
  when to stop. D adds a plain stop rule ("two searches are usually enough;
  once a search returns the memory that answers the request, answer instead of
  rephrasing"). Two rounds of `P2`, ten attempts per profile: answers 10/10 and
  Recall@5 10/10 on both sides; `long_use` mean requests **5.10 -> 3.90**,
  median completion **11.4 s -> 6.7 s**, spend **$0.0311 -> $0.0211**; `clean`
  drifted the other way (2.30 -> 2.50 requests). Pointed at `P3`, the same rule
  cost recall (17/20 -> 15/20) because it also stopped before the second hop.
- **Candidate E tried to keep the pointer hop and lost on both counts.** Wording
  it as "stop unless the memory points at something else" raised `P2`/`long_use`
  to 6.2 requests and 21.2 s (base 5.2 / 11.4 s) and left `P3`/`long_use` recall
  at 7/10, so the round failed `semantic` outright. Both D and E are `KEEP_BASE`.
- **The harness, not the prose, now bounds the loop.** A turn makes at most
  three memory searches and a fourth returns `limit_reached` (ADR 0029). The
  cap is replayable, keeps the tool on the surface so the prompt prefix is
  unchanged, and fires only after the model has had the fact, a pointer and one
  rewording. Measured on `P2`/`P3`, five attempts per profile: a cap of two lost
  a fact a third query would have found (`P2`/`long_use` recall 5/5 -> 4/5, the
  model searching `reading group get-together`, `reading group meeting place`,
  then refused on `book club` and answering honestly that it had nothing); a cap
  of three restored recall to 5/5 with 4.8 requests and a 10.7 s median against
  the pre-cap 5.2 and 11.4 s. That is inside the noise of five attempts, so this
  is recorded as a **bound, not a measured gain**: the loop it prevents
  (`P2`/`long_use`#2, seven searches, `bound_exceeded`) did not reproduce in any
  later round, and the honest claim is that the pathology can no longer consume
  the request budget.
- **What the four rounds actually teach.** Prose that adds a *condition* makes
  the model search more; prose that adds a *stop* makes it stop too early on the
  chained case. Wording is saturated. The next lever is deterministic: a
  per-turn cap on memory searches that the kernel enforces and states in the
  tool result ("two searches used; answer with what you have"), which needs no
  model call and cannot be argued away by the model.


Every probe was one command:
`scripts/probe_variant.py round <variant> --out <dir> --compare-to <base>`.

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
