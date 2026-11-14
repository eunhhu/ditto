# ADR 0049: Learned behavior is handed off, retrospection is bounded, and the contract freezes at 1.0

Status: accepted on 2026-11-14, on the owner's answers to the five decisions in
[self-improvement-plan-2026-11](../design/self-improvement-plan-2026-11.md).
Amends [ADR 0034](0034-one-turn-contract.md), the promotion rule in
[improvement-patches](../specs/improvement-patches.md), and
[Task 039](../agent/tasks/039-improvement-compiler-first-slice.md). Amends the
"No model call solely for housekeeping" invariant in the root `AGENTS.md`.

## Context

On 2026-11-13 the owner named self-improvement as Ditto's headline result,
with everything inspectable and controllable (product intent, "North star").
The plan written from that left five decisions to the owner. The owner
answered them on 2026-11-14:

> 1. 알아서
> 2. 그렇게 하자.
> 3. 회고 방식에 많지 않은 비용이 든다면 허가할게.
> 4. 추천대로
> 5. 턴계약은 1.0때 고정. 그 전까진 better way가 있다면 무조건 그 방식으로
>    연구하고 적용도 해보고 하는 식

## Decision

### 1. A learned change is handed off, not approved

"알아서" (use your judgment) leaves the choice to the plan's proposal. The
owner's standing preference is hand-off rather than an approval layer.

- **Applies on its own.** A learned change that widens no effect and can be
  undone applies without asking. Examples are a retrieval key and a lesson. It
  enters as a candidate with an expiry and appears in the learning ledger with
  its evidence.
- **Still needs approval.** A change that widens what Ditto may do: a new
  capability, a new effect, or a new resource.
- **Rollback is manual for now.** No per-item quality measure exists outside a
  paid campaign, so this decision does not promise an automatic rollback on a
  falling measure. Two things retire an item automatically, and both are
  journaled facts:
  - its target is forgotten or superseded;
  - it reaches its expiry without being used.

  The user can undo any item at any time. Undo is a superseding event, never
  an edit.

### 2. What "post-training level" may claim

A gain is claimed only per case family, against
[Task 042](../agent/tasks/042-harness-only-self-improvement.md)'s bar:

- held-out cases;
- at least 8 points, or Recall@5 of 0.95;
- no gate regressing;
- at most 5% more prompt tokens.

No general claim of parity with weight training is made.

### 3. Retrospection is allowed, within a stated ceiling

The owner allowed retrospection on condition that it costs little. This
amends the invariant "No model call solely for housekeeping" with one bounded
exception:

- **Woken by a detector.** A retrospection run starts only when a detector has
  admitted journaled evidence (Task 039's admission rule: the same failure in
  at least two distinct recorded episodes). The invariant "No periodic LLM
  heartbeat" is unchanged, so no timer may start one.
- **Ceiling.** At most 3 model requests per run, at most 8,000 input tokens per
  run, and at most 2 runs per profile per day. A run whose estimate exceeds
  the ceiling is refused before its first request.
  - For scale: campaign 16 recorded 2,220 tokens and $0.00070 per turn, so a
    full day at the ceiling is about 48,000 input tokens, the size of about 20
    ordinary turns.
  - These numbers are this ADR's, not the owner's. They can be changed by
    amending this section.
- **Visible.** Every run journals its requests, its token usage and what it
  proposed, like a turn does. Its output is a learned-change candidate under
  decision 1, never a direct edit.
- **No reviewer by default.** The invariant "No permanent improvement from one
  successful trajectory" is unchanged. The first learning loop in the plan
  (retrieval keys learned from the model's own searches) needs no model call,
  and this exception does not license adding one where a deterministic
  detector suffices.

### 4. Step 7 moves forward, starting with a source that needs no credential

The plan's decision 4 stated a trade-off but no recommendation, so "추천대로"
(as recommended) is applied here as this ADR's recommendation, labeled as
such:

- **First, right after step 3.** A read-only connector that needs no
  credential: the files of a directory the user grants. It comes before steps
  4–6, because daily use (step 1's fuel) needs Ditto to be useful past memory
  and the web, and because it gives the learning loop procedure to learn.
- **Then.** The credential boundary, by its own ADR, and read-only calendar and
  mail.
- **After that.** Effects with approval delivered as a hand-off, events that
  wake a task, and durable long tasks (Task 040).

### 5. The turn contract freezes at 1.0, and until then the better way wins

- ADR 0034's "until the first release" now reads "until 1.0". Until 1.0
  there is one contract, and a change replaces it. At 1.0 the contract
  freezes, and from then on a change adds a version that replay keeps.
- Until 1.0, when a better design is found it is researched, tried and
  applied, even when that replaces the contract.
  - This does not waive ADR-first for the boundaries the root `AGENTS.md`
    lists.
  - It does not waive Task 042's evidence bar for any claimed gain.

## Consequences

- **Replay-based features cover only current-contract turns until 1.0.**
  Detectors read events and are unaffected. The plan's "what if" re-runs and
  Task 042's counterfactual replay are not: each contract change strands the
  turns recorded before it, for replay purposes only.
- **Retrospection is the one model call not started by a user request or a
  schedule.** Its ceiling is part of the contract, its spend is in the
  journal, and changing either takes an ADR amendment.
- **Learned items cannot widen authority on their own.** The approval boundary
  for capabilities, effects and resources is unchanged.
- **Task 039 changes.**
  - Drafting is no longer operator-only: a retrospection under decision 3 may
    draft.
  - Applying no longer needs an approval event for a change that widens
    nothing.

## Alternatives

- **Approval for every learned change**, as Task 039 specified. It contradicts
  the owner's standing preference for hand-off. It also makes the learning
  loop wait on the user, which is the overhead the north star rules out.
- **Unbounded retrospection after every turn**, as Hermes does. That is the
  "self-improvement that becomes less efficient over time" the owner named on
  2026-09-06, and it pays a model call per turn.
- **Freezing the contract now.** That would keep every replay-based feature
  working across changes. It would also lock in a design the owner expects to
  keep improving before 1.0.
