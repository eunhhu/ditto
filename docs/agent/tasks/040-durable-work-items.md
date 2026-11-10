# Task 040: A durable work item for a long delegated task

Status: **after the first release.** Needs an ADR before any code, because it
changes what a run is — the one-turn contract (ADR 0034) exists precisely to
postpone this.

Why: OpenMausBot's assignment tree is the strongest engineering found in
[comparison-2026-11.md](../../design/comparison-2026-11.md). Its rules are worth
copying almost verbatim:

- durable `assignments` / `assignment_events` rows, a `rootId`/`parentId`/`depth`
  tree, states `queued → running → waiting/resume/needs_input`;
- one wake per result, so a completion cannot start a second run;
- on restart, refuse to rerun a turn that already acted outward, and decide that
  from recorded activity rows, never from model text.

Ditto today: a scheduled request admits one bounded turn; a task that needs
three days and two interruptions has no representation, so the user re-asks.

## Contract (draft, to be settled by the ADR)

- An admitted **work item** is a journaled user intent plus a step budget:
  maximum turns, maximum wall clock, maximum spend. Exceeding any bound ends the
  item as `exhausted`, not as a failure of the step that hit it.
- Each step is an ordinary turn under the existing contract; the work item adds
  no authority, no new capability, and no wider effect profile. A step that
  needs approval still needs it.
- The item resumes from the journal after a restart: the recorded steps are
  replayed, the next step is admitted only if no step is in an unknown effect
  state, and a step that acted outward is never re-executed.
- `wake` is one-shot per recorded result: a result event can admit at most one
  following step, and the claim is committed before the step is admitted.
- The user sees, per item: the intent, the steps taken, the evidence for each
  claimed effect, the remaining budget, and why it stopped.

## Exit criteria

- A three-step item interrupted between steps 2 and 3 resumes and finishes with
  the same recorded evidence as an uninterrupted run, and its second step is
  never executed twice.
- An item whose step acted outward and then crashed reports `interrupted` with
  that step's effect state, and refuses to auto-resume until the user decides.
- Budget exhaustion is typed and visible; the item does not continue with a
  fresh budget.
- No new model call is introduced by the item machinery itself (proved by
  journal: admissions produce no `model.requested` of their own).
- Replay of a whole item from the journal reproduces every step boundary.

## Non-goals

- No parallel sub-agents, no fork/join, no cross-item scheduling priority.
- No change to the scheduler's at-most-once guarantees; the work item composes
  with them, it does not replace them.

## Risks

- The tempting version of this task is a general multi-agent platform. That is
  explicitly out of scope: the item is a resumable sequence of ordinary turns,
  nothing more, and the ADR must say so in those words.
