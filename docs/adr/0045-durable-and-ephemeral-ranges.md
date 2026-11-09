# ADR 0045: durable and ephemeral ranges are different kinds

Date: 2026-11-09
Status: accepted

## Context

The design so far separated documents by *scope* (turn, session, task, project,
device, global) and by *mutability inside a turn* (pinned, recallable). The user
pointed out the missing axis: persistence. A turn note, an in-flight run, a tool
result and a remembered preference all have a scope, but only one of them should
still exist tomorrow - and if the two are confused, the agent either forgets
things it promised to keep or keeps things it should have dropped.

## Decision

Every piece of state is one of three kinds, and the kind is part of the contract,
not a habit:

1. **Durable document** - in the journal, survives restart, replayable, has
   provenance, a scope and a supersession rule. Memories, instructions, skills,
   installed plugins, schedules, conversation turns.
2. **Session-scoped state** - survives a restart only as history, is never
   recalled as a fact, and expires with its window: the conversation index, the
   runs in flight, the last N turns used for history. It is reconstructed from the
   journal on start, never authored directly.
3. **Turn-scoped state** - lives for one turn and is never read again except by
   replay: the compiled capsule, the turn notes (`[Ditto: local time ...]`), tool
   results, the pending prefetches. Its whole life is inside the journal record of
   that turn.

Rules that follow, and that a test can check:

- An ephemeral fact never becomes a durable document by accident. Promotion
  requires an explicit write with provenance - `memory.write` labelled `user` or
  `model`, or `/remember`.
- A durable document is never mutated in place inside a turn. A change is a new
  document superseding the old one, so a request prefix stays byte-identical and
  replay reproduces the old turn.
- Nothing session- or turn-scoped may be cited as a fact. "We talked about this
  yesterday" is not a memory; the memory is what was written.
- Expiry is a property of the kind, not a cleanup job: turn state is gone when the
  turn ends, session state when the window closes. No sweeper may be the only
  thing standing between a user and a stale fact.

## Consequences

- The context compiler selects only durable documents for the capsule, so the
  capsule cannot smuggle a tool result into tomorrow's context.
- `DITTO_CONTEXT_V1` keeps one meaning: the durable facts this turn may use.
- Memory growth is bounded by what users (or the model, labelled) chose to keep,
  not by conversation volume.
- A future retention policy (forget after N days) is a change to a document's
  scope, not to this ADR.
- A turn records the standing instructions it was given inside its own
  `context.compiled` payload, so replay reproduces the request without reading
  the current profile - and an edit to the profile cannot rewrite history.
