# Task 045: Retrieval keys learned from the model's own searches

**Status: complete on 2026-11-14.** Every exit criterion below passed. The
full gate exited 0 on `42e1d5d` and again on `97ab79a` after a review fix,
and HANDOFF has the counts. Measuring the
gain on a live provider was never part of this task.

The first learning loop of the
[self-improvement plan](../../design/self-improvement-plan-2026-11.md) (steps
1–3 as one vertical slice), under [ADR 0049](../../adr/0049-learned-behavior-decisions.md).
The contract is [ADR 0050](../../adr/0050-learned-retrieval-keys.md).

## Why

When a session's memories exceed the context budget, a request that names a
fact in other words misses it, and the turn pays a search. The journal already
records the miss, the search and the memory found. Learning the user's words
for that memory lets the next compilation include it without the search.

## Contract

See ADR 0050. In short:

- A deterministic detector runs after each ordinary agent turn.
  - It records a candidate key on the first episode.
  - It promotes the key on a second distinct episode within 30 days, unless
    the key is too broad.
  - It refuses turns that read anything external, and memories that Ditto
    inferred.
- A promoted key brings its memory into a compilation that left the memory
  out.
  - The receipt names the key, and replay checks it against the journal.
  - At most two keys apply per turn.
- The user lists and undoes keys from the CLI or HTTP. A key whose memory goes
  away does nothing.
- `--disable-learning` turns the loop off. The Task 017 runner passes it.
- Turn contract 21.

## Exit criteria

- **Unit tests for the detector rules:**
  - target selection (none, one, ambiguous);
  - word selection with rarity and the cap;
  - refusal after an external read, and for an inferred target;
  - distinct episodes;
  - the breadth block.
- **A kernel turn test** over the over-budget fixture:
  - two differently worded turns miss and then search, and record a candidate
    then a promotion;
  - a third turn includes the memory in its first request with a
    `learned key` receipt and makes no search;
  - all three replay;
  - after the user retires the key, the same request leaves the memory out
    again.
- **Adversarial tests:**
  - a turn that called `web.browse` teaches nothing;
  - a replayed turn whose receipt cites a retired, missing or other-session key
    is rejected.
- **No regression in existing tests.** The context, kernel and `personal-quality`
  tests pass with no expected value edited.
- **The full gate.** `./scripts/agent-check.sh` passes.
- **Documents.** HANDOFF, NEXT, the event protocol and the README describe
  what is implemented.

## Not in this task

- Measuring the gain on a live provider. That needs a learning-curve profile
  and a paid run on the owner's go.
- Other learned kinds (lessons, procedures, preferences), retrospection, and
  the web console.
