# Task 038: Memory search that matches Korean and ranks rare words

Why: the comparison in
[comparison-2026-11.md](../../design/comparison-2026-11.md) found Ditto's memory
search to be the weakest of the four systems on two counts that are visible to
the user, and both are fixable without a new service.

1. **Korean.** `tokenize` splits only on non-alphanumeric characters
   (`crates/context/src/lib.rs:2357`), so "냉장고에" and "냉장고" are different
   tokens and a particle changes the answer. The user writes Korean, and every
   Korean memory question in production is exposed to this. Hermes builds a
   trigram index and a CJK tokenizer (`hermes_state_fts.py:100-126`);
   OpenMausBot drops punctuation and ORs terms. Ditto has neither.
2. **Ranking.** Every match scores the same regardless of how common the word
   is, so a memory that shares "my" and "is" can rank above the memory that
   shares the one rare noun in the question.

## Contract

- **Particle folding, deterministic and language-scoped.** A Hangul token is
  matched against another when they are equal after removing one suffix from a
  fixed, closed list of Korean particles and endings (은/는/이/가/을/를/에/에서/
  에게/한테/의/도/만/와/과/로/으로/부터/까지/처럼/보다/라고/이라는/입니다/해요)
  and the remaining stem is at least two characters. The list is a constant with
  a unit test per entry. No dictionary, no network, no model, no new dependency.
- **Rarity ranking.** Within the candidate set, a matched token contributes
  `1 / (number of candidates containing it)`; ties keep the existing stable
  order. This is a local, recomputable weight — no index, no stored statistics.
- **The context compiler is unchanged.** Complete-set selection keeps exact
  tokens in ID order, because that selection is part of the turn payload
  contract; only the agent's memory search changes. `TURN_PAYLOAD_VERSION` 15 → 16
  and ADR 0038 gains the amendment.
- **Replay recomputes the same result** from journaled memories and the recorded
  query; no new event kind.
- **A measured comparison, not a claim.** `eval/task017/suite-v2.json` adds four
  Korean memory cases (P6 Korean single fact, P7 Korean two-hop, X5 Korean
  mixed with sort, P8 Korean correction after a change) at 20 repetitions, with
  the same oracle shape as P1–P5. Suite v1 stays frozen and its numbers stay
  comparable; v2 is a separate file with its own frozen budget.

### The correction record

OpenMausBot can show a memory's history and undo a change
(`~/.openmausbot/memory-journal/<botId>.ndjson`, one-click undo, expiry dates).
Ditto supersedes a memory and can forget it, but the replaced text is not
listed and cannot be restored, so a wrong correction is permanent for the user.

- `ditto memory history <id>` and the memory list in the web app show the
  superseded text with its event sequence and time, read from the journal that
  already holds it. No new store.
- `ditto memory restore <id> --seq <n>` supersedes the current memory with the
  earlier text as a new, ordinary supersession — same admission gate, same
  provenance, nothing rewritten in place.
- A memory may carry an expiry (`until`): once it passes, compilation excludes
  it as expired rather than as superseded, and status says which.

## Exit criteria

- Unit tests: every suffix in the list matches its stem in both directions; a
  stem shorter than two characters does not; "냉장고등" does not match "냉장고";
  rarity ranking puts the rare-noun memory first on a fixture where the common
  words are shared; identical input replays to an identical order.
- `cargo test --locked -p ditto-context -p ditto-kernel` passes, including the
  existing plural tests unchanged.
- A recorded suite-v2 run over the real daemon and the real model reports
  Korean Recall@5 with a denominator of 80 and at least 0.90, plus no regression
  on the v1 gates (safety, tool, schedule, resource) and no increase in
  `max_input_tokens` for the memory family beyond the frozen headroom.
- `budgets-v2.json` frozen from a calibration run of suite v2, not from v1.
- History and restore: a superseded memory's text is listed with its sequence
  and time; restoring it creates one new supersession event and the listing then
  shows the restored text as current; an expired memory is excluded from
  compilation with `expired` in status, and a test proves it never reaches the
  model.
- ADR 0038 amended and `docs/architecture.md` says version 16.

## Non-goals

- No embeddings, no vector service, no stemming of English beyond the existing
  plural rule, no Japanese or Chinese segmentation.
- No change to complete-set selection, to the thread window, or to how memories
  are admitted.
- No new model call anywhere in the search path.

## Risks

- A particle list is a closed approximation: an unseen ending still misses.
  That is acceptable if the measured Korean recall clears the bar and the list
  has a test per entry; a broader morphological analyzer is a separate decision
  with its own evidence.
- Rarity weighting changes the order of existing search results, so an
  evaluation that pinned exact orders must be re-frozen with the version bump.
