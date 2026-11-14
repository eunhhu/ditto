# Task 046: History cards — related past exchanges before the model asks

**Status: spec only (2026-11-14).**
- Phase 0 is offline and can start at any time.
- Phase 1 needs a live provider and waits on the OpenCode Go quota.
- The design, the efficiency model and the post-training discussion are in
  [design/history-cards](../../design/history-cards.md).

## Why

The owner named sub-context injection as a secretary agent's biggest
usability problem.

- **What is lost.** Anything that left the current thread's window, or came
  before a reset, is invisible to the model.
- **What it costs today.** Ditto has no search over past conversations, so
  the user must restate the old context. Even with a search, each lookup
  costs a full extra round trip.
- **The proposal.** The harness lays out related history as cards before the
  first model request. The model pulls detail only for the card it needs.

## Contract (to become an ADR before code)

- **Cards.**
  - At most 5 cards and about 400 tokens per turn.
  - Each card has a turn id, a date, a speaker label and an extract of at
    most 160 characters.
  - Cards are selected deterministically from the session's finished
    agent-run exchanges outside the current window, with ADR 0029 folding,
    rarity weighting, recency decay and a firing threshold.
  - No model call is made.
- **Placement and replay.** Cards go in the latest message's note region,
  never the stable prefix. Their turn ids are recorded in `context.compiled`
  and recomputed by replay.
- **Tools.**
  - `history.read { card }` returns one whole exchange, bounded.
  - `history.search { queries }` covers what the cards missed.
- **What is carded.** Only user messages and Ditto's final answers. Tool
  results and fetched pages never are. Ditto's answers are never labelled as
  the user's words.
- **A new turn contract,** under ADR 0049: until 1.0 a contract change
  replaces the version.

## Exit criteria

### Phase 0 (offline)

- A synthetic Korean and English history corpus with labelled probes:
  - about 300 exchanges with resets, plus 2,000 distractors;
  - history probes and non-history probes.
- Card Recall@5 at least 0.90.
- A false-fire rate on non-history probes of at most 10%.
- Selection p95 of at most 5 ms at 10,000 exchanges on the Raspberry Pi.
- A comparison of index options: a journal scan, an in-memory inverted index,
  and SQLite FTS5 with trigram.
- The thresholds frozen from the corpus.

### Phase 1 (live, OpenCode Go)

- A suite-v3 history family at 10 repetitions per case, run in four arms:
  - A: no cards, with `history.search`;
  - B: cards, read and search;
  - C: cards only;
  - D: whole exchanges injected.
- **The claim, B against A:**
  - At least 0.3 fewer requests per turn on the history family.
  - Correctness not lower, and at least 8 points higher where A did not
    search.
  - Suite v1 families unchanged within the observed spread.
  - Input tokens per turn up by at most 10%.
  - Prompt-prefix reuse not lower.
- Runs stored and digested under QUALITY-GATES "Campaign evidence".

### Phase 2

- Outcome labels on cards: acknowledged, corrected, re-asked, cancelled.
- Measured on a learning-curve profile.
- Recurring patterns promoted into keys, lessons or procedures under
  ADR 0049.

## Not decided yet (owner)

- Cross-session cards.
- How far back cards reach, and how forgetting interacts with carding.
- The default once Phase 1 passes.
