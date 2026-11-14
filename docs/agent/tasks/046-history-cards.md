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

#### Phase 0 rules, registered before the selector existed (2026-11-14)

- **Probes.** The probes are `eval/history-cards/probes-v1.json`: 16 targets,
  8 Korean and 8 English. Each target has:
  - two hard distractors written in the target's own vocabulary;
  - three needs-history probes that share at least one search form with the
    target (*lexical*);
  - one needs-history probe that shares none (*paraphrase*, an honest miss
    for any lexical selector);
  - one *related* probe that touches the topic but does not need the old
    exchange.

  On top of these come 24 *unrelated* probes. The benchmark checks the
  lexical and paraphrase labels mechanically.
- **Split.** Probes alternate between the tuning half and the held-out half,
  in file order. Gates are read once, on the held-out half, after the
  parameters are frozen.
- **Background.**
  - 2,000 generated distractor exchanges over 90 days, for recall and firing;
  - 10,000 exchanges, for latency;
  - the 16 most recent exchanges stand in for the thread window and are
    never carded.
- **Gates (held-out half).**
  - Lexical Recall@5 of at least 0.90.
  - A false-fire rate on unrelated probes of at most 10%.
  - Selection p95 of at most 5 ms at 10,000 exchanges, in a release build on
    the Raspberry Pi.
- **Reported, not gated.**
  - Recall@5 over *all* needs-history probes, paraphrases included.
  - Recall@3 and MRR.
  - The fire rate on related probes, which is the cost term `p`.
  - Tokens per firing.
  - Index build time and resident memory at 10,000 exchanges.
  - The SQLite FTS5 comparison.
- **Why paraphrases are reported, not gated.** They are unreachable by
  construction for a lexical selector, and they are what the `history.recall`
  search tier exists for. The spec's bar of "Recall@5 at least 0.90" did not
  say whether they count. This registration settles that before any result
  exists, and still reports the combined figure.
- **Tuning rule.** On the tuning half, choose the parameters with the highest
  lexical Recall@5 whose false-fire rate is at most 10%. Ties go to the lower
  related fire rate, then the fewer tokens.
- **Failure.** If a held-out gate fails, the result is reported, the corpus
  is not changed, and no integration is built on it.

#### Phase 0 v1 result (2026-11-14): failed on false fire

The benchmark ran once, as registered, in a release build on a quiet
Raspberry Pi 5 (load average 0.8):

- command: `cargo test --release -p ditto-context --test
  history_cards_phase0 -- --ignored --nocapture`;
- code: `874b8f4`, plus diagnostics that list the matched words of each
  false fire;
- report: `target/task046/phase0-report.json`, SHA-256
  `b275f83b85d828482a598c384ca3438891a4485ec93906bb2c23973c57152573`;
- log: `target/task046/phase0.log`, SHA-256
  `d6f3e0cce209f230005dfdf2d84c91160cdcf2067ffe0be34c8506196048a081`.

The diagnostic rerun reproduced every number.

**Frozen rules** (chosen on the tuning half): idf weighting, age buckets on,
the user's words only, at least 2 shared words or one word held by at most 3
exchanges, no minimum score, 5 cards.

**Gates on the held-out half:**

| Gate | Result | Pass |
| --- | --- | --- |
| Lexical Recall@5 at least 0.90 | 0.958 (23/24); Recall@3 0.958, MRR 0.917 | yes |
| False fire on unrelated probes at most 10% | **0.167 (2/12)**; the tuning half was 0/12 | **no** |
| Selection p95 at most 5 ms at 10,000 exchanges | 0.239 ms (p50 0.006 ms, max 0.78 ms) | yes |

**Reported, not gated:**

- Recall@5 over all needs-history probes was 0.719. Paraphrases scored 0/8,
  as expected for a lexical selector.
- The related probes fired 0.875 of the time (7/8). That is the cost term
  `p`.
- A firing added about 65 tokens.
- At 10,000 exchanges the index built in 68.5 ms and added 3.1 MB of
  resident memory.
- The one lexical miss was "Which level do I park on at work?". Its only
  match is the common word "level".

**SQLite FTS5 comparison** (held-out half, 2,000 exchanges, firing on any
match):

| Tokenizer | Lexical recall | False-fire rate | p95 |
| --- | --- | --- | --- |
| unicode61 | 0.958 | 0.83 | 1.03 ms |
| trigram | 0.958 | 0.42 | 0.87 ms |

FTS5 recalls the same, fires far more often, is slower, and would need a new
schema. The in-memory index is the better option.

**Why the gate failed.** Both false fires came through the single-rare-word
path, each on a word that is weak evidence:

- "이메일 초안 좀 써줘…" fired on the one-syllable filler 좀, which matched
  "엔진 소리가 좀 이상한데".
- "What's 18 times 24?" fired on the bare number 18, which matched "December
  18" in the flight target.

**Consequence, as registered.** No integration is built on this corpus, and
the corpus and rules stay as they are. A v2 needs its own registration and a
fresh held-out set, because these held-out probes have now been seen. The
evident v2 change is that a single matched word may fire only when it is
neither a bare number nor a single syllable. With 12 probes a half, one
false fire is 8.3 points, so v2 also needs a larger unrelated set.

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
