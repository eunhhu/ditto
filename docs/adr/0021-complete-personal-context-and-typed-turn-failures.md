# ADR 0021: Complete personal context and typed turn failures

Status: accepted for Task 016.1. Amends ADR 0009 (durable turn payloads and
replay) and ADR 0016 (run context selection) by introducing turn payload
version 2.

## Context

Runs compiled context with the version-1 positive-overlap rule: a ranked memory
entered the model context only when it shared a token of two or more characters
with the query, and every run query also carried the fixed text
`local content read` inherited from the Task 003 read loop. On 2026-09-30 a probe
of the real compiler showed the effect. For `What is my meeting preference?`
the memory `I prefer afternoon meetings` was excluded, while unrelated memories
containing `my`, `is` or `read` were included. Paraphrases never match, and
inflection (`meeting`/`meetings`) or attached particles (Korean `회의를`) break
exact tokens. Memory therefore failed exactly where a personal agent needs it,
while unrelated personal facts reached the provider. The Task 015/016 corpus
used literal queries without function words, so it could not detect this.
Semantic retrieval (a production embedding worker) remains deferred.

Separately, replay validated failures derived from other crates' validators
(context compiler, manifest, driver contract, tool-call buffer) by matching or
recomputing their display text. Any wording change in those crates would make
newly recorded turns unreplayable.

## Decision

### Complete-set context selection

`ditto-context` owns a second selection contract,
`ContextSelection::CompleteSet`, beside the unchanged `PositiveOverlap`:

- Tokens exclude a frozen list of 97 English function words (articles,
  copulas and auxiliaries, pronouns, wh-words, common prepositions and
  conjunctions, `no`/`not`). Spatial, temporal and quantity words still count.
- When required context plus every eligible ranked candidate fits the selection
  budget, all of them are included. Positive-overlap nodes rank first as before;
  zero-overlap nodes follow in node-ID order with the receipt reason
  `complete-set` and score zero.
- Otherwise selection falls back to positive overlap within the budget and
  zero-overlap candidates are excluded as irrelevant.
- Validation accepts `complete-set` only under this contract, only with a zero
  score and never beside an irrelevant or token-budget exclusion. Ordering,
  budget and token accounting checks are unchanged.

Run turns use this contract with the request text alone; `local content read`
is no longer part of the query. Eligibility is unchanged: the verified
session/task snapshot, supersession, provenance, validity, required-context
rules, budgets and capsule format stay as they were.

### Turn payload version 2

Every turn payload now carries `event_version = 2`, and one turn never mixes
versions. Replay accepts versions 1 and 2 and applies the rules of the recorded
version: version 1 keeps positive overlap, the legacy query text and the frozen
failure-message grammar; version 2 uses the complete-set contract and typed
failure reasons. Run status reads both. Sort payload versions are unchanged.

`TurnFailure` gains an optional closed `reason` for validator-derived
failures. Each of the 19 reasons implies exactly one failure code and belongs to
one stage: context compilation (7), capability selection (7), driver contract
(4) and tool-call lifecycle (1). Version-2 replay checks the reason against the
stage's closed set and code, and treats the bounded message as diagnostic text.
Kernel-owned messages (checkpoints, protocol and bound failures) remain exact
and are shared between runtime and replay. A version-1 failure with a reason is
rejected, as is a version-2 validator failure without one.

## Consequences

- At the default budget of 900 estimated tokens, about twelve short memories
  (about 70 tokens each) fit, so paraphrased questions receive the answering
  memory in small sessions. Larger sessions fall back to lexical overlap and
  miss pure paraphrases until semantic retrieval exists. The budget is a cost
  decision and is not changed here.
- The context ceiling per request is unchanged. In small sessions the model now
  receives all current same-session memories, including unrelated ones; other
  sessions and superseded memories are never sent. Returned-context precision
  falls by design and is measured as a raw metric (corpus schema 3).
- Validator wording can change without invalidating version-2 turns. Version-1
  traces still rely on the frozen grammar.

## Rejected alternatives

- Function-word filtering and stemming alone fix inflection but not paraphrase.
- Raising the budget to hundreds of memories changes per-request cost; that is
  the user's decision.
- A production embedding worker adds a dependency, model download and resident
  memory; it stays deferred behind the existing injected seam.
- A separate model call to select memories adds latency and cost to every run.
- A model-facing memory tool needs a new capability, effect profile and replay
  surface; it is larger than this fix.
- Reinterpreting version-1 events in place is forbidden by ADR 0009.

## Compatibility, evidence and rollback

No event is rewritten. Version-1 turns replay as before; older binaries reject
version-2 turns as an unsupported payload version. Evidence: `ditto-context`
unit tests for paraphrase inclusion, fallback, required context and forged
receipts; kernel regressions for paraphrased runs, over-budget fallback,
version-1 relabeling, mixed versions and forged reasons; and the corpus harness
(schema 3) exercising both modes in the canonical gate. Rollback restores
positive-overlap selection for new runs and sets the written version back to 1
while keeping version-2 decoding for already recorded turns.
