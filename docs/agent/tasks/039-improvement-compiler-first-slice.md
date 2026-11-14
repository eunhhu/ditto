# Task 039: The improvement compiler's first evidence-gated slice

> **Amended 2026-11-14 by [ADR 0049](../../adr/0049-learned-behavior-decisions.md).**
>
> - **Contract item 4.** A patch that widens no effect and can be undone now
>   applies on its own as an expiring candidate shown in the learning ledger.
>   Only a patch that widens a capability, effect or resource still needs an
>   approval event.
> - **Drafting** (contract item 3 and the first risk) may also run as a
>   retrospection that a detector wakes, within ADR 0049's ceiling. It is no
>   longer only an operator command.
>
> The rest of this spec stands.

Why: [comparison-2026-11.md](../../design/comparison-2026-11.md) ranks this first.
The user's stated fear is self-improvement that gets less efficient over time,
and the improvement compiler is the designed answer
(`docs/architecture.md` § Improvement compiler) that has never been built.

Harness² supplies the one measured argument in favour and the one sharp warning:

- one recursive step with a frozen model: GLM-5 +11.5 on WorkBuddy, +11.8 on
  LAB; editing *without* execution evidence drops the score (Code 73.4 → 63.2).
- `KEEP_BASE` must be a first-class outcome, an invalid edit inherits the base
  tree, and the budget is an identity checked up front, not a hope.

Hermes and OpenMausBot both run a review after every turn (background review,
skill review). Those are model calls for housekeeping, which Ditto forbids, so
the Ditto form runs offline, on journaled evidence, under an explicit budget.

## Contract

1. **Deterministic detectors, no model.** From journaled events and evaluation
   reports, detect four classes: a repeated user correction of the same memory,
   a memory search that missed a fact that was present, an argument the model got
   wrong twice, and an approval the user granted repeatedly for the same
   capability. Each detection is an event with the exact evidence identities and
   is idempotent per `(class, evidence set)`.
2. **Admission test — name the moment.** A proposal may be drafted only when the
   same failure appears in at least two distinct recorded attempts. Harness²'s
   label-free loop cannot be copied here because Ditto has graded evidence, so
   the stricter rule applies.
3. **Bounded drafting.** At most three model calls and a fixed token ceiling per
   proposal, refused before the first call if the ceiling would be exceeded. The
   draft is one typed patch to one file: a `capability.toml` retrieval hint, a
   memory-search guidance sentence, or an evaluation case — never kernel code,
   policy, credentials, or evaluator logic.
4. **Journaled proposal, never auto-apply.** `improvement.proposed` carries the
   patch, the evidence identities, the budget actually spent, and the reason
   `KEEP_BASE` was rejected or chosen. Applying requires an explicit user
   approval event, like any other effect.
5. **Validation before promotion.** The patch must parse, the affected unit tests
   must pass, and a replay of the recorded trajectories must reproduce the
   original failure without the patch and stop reproducing it with the patch.
   A failure of any stage inherits the base state (the rollback primitive).
6. **Held-out check.** Promotion requires a re-run of the evaluation suite with
   the motivating case excluded, and no gate may regress. A proposal that only
   improves the case that produced it is recorded as overfit and retired.
7. **Bounded library.** At most eight live proposals; the ninth evicts the
   oldest retired one. No proposal is ever edited in place — a later revision is
   a new proposal citing the old one.

## Exit criteria

- A recorded run over the existing calibration evidence produces at least one
  deterministic detection and zero model calls when no proposal is admitted
  (proved by the journal: no `model.requested` between start and end).
- A proposal draft costs at most three model calls and is refused without a call
  when its ceiling is lower than the estimated cost.
- Replay proof: for a chosen historical miss, the unpatched state reproduces the
  miss and the patched state does not, both from journaled events.
- `KEEP_BASE` is reachable and observed at least once in the recorded run.
- An invalid patch (unparsable `capability.toml`) leaves the base state
  untouched and records the fallback.
- The ninth proposal evicts by the stated rule; a test pins the boundary.
- `./scripts/agent-check.sh` passes, ADR added for the promotion boundary, and
  `docs/agent/HANDOFF.md` records what was promoted or kept.

## Non-goals

- No per-turn reflection, no review after every turn, no heartbeat.
- No self-editing of kernel code, root policy, credentials, evaluator logic, or
  provider settings — the architecture already excludes these.
- No automatic application, no rollout search inside a user request, no
  population or bandit loop.

## Risks

- Three model calls per proposal is still spend; it must be an operator command
  with a printed ceiling, not something a chat turn can trigger.
- Detectors can be gamed by their own thresholds. The held-out check and the
  overfit retirement are the guard, and both must be real runs, not assertions.
