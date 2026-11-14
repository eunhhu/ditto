# ADR 0050: Retrieval keys learned from the model's own searches

Status: accepted on 2026-11-14 for [Task 045](../agent/tasks/045-learned-retrieval-keys.md).
It is the first learning loop of the
[self-improvement plan](../design/self-improvement-plan-2026-11.md), under
[ADR 0049](0049-learned-behavior-decisions.md). Turn contract 21 replaces 20
(ADR 0034).

## Context

Past the context budget the compiler selects memories by word overlap with the
request (ADR 0021). When the request and the memory use different words, the
capsule misses the memory, and the model pays a search to find it. In
campaign 16 the memory family stood at 1.40 requests per turn. P1 and P2 each
paid one search for a fact the capsule left out
([Task 043](../agent/tasks/043-requests-per-turn.md)).

The journal already holds what a learning loop needs to close that gap:

- the request's words;
- the memories the compilation excluded;
- the memory the model's search then found;
- the answer that followed.

Nothing has read them.

## Decision

### The learned item

A **retrieval key** names one memory and up to six of the user's words that
should bring it into the capsule. It lives in the journal as a chain of
session-scoped, task-free events, using the kinds the improvement compiler
already reserved. The chain is linked by `supersedes`.

| Event | Actor | Meaning |
| --- | --- | --- |
| `improvement.candidate_created` | system | One episode of evidence; the key is not used yet |
| `improvement.promoted` | system | A second distinct episode; the key is active |
| `improvement.retired` | user | The user undid the key |

The payload is version 1 and has these fields:

- `kind: "retrieval_key"`;
- `target`, the memory's node id;
- `words`;
- `evidence`, a list of `{turn_id, input_event_id}`;
- `supersedes`, the event this one replaces, or `null`;
- `expires_at`, on a promotion only;
- `blocked`, on a candidate whose promotion was refused, with the reason.

### The detector

The detector runs after a turn finishes, with no model call. It reads only the
turn's journaled events and the session's active memories. A turn is evidence
when all of these hold:

- **It is an ordinary agent run** that finished.
- **It read nothing external.** It called no tool other than `memory.manage`
  and `skill.read`, and carried no prefetched attachment. A page or a file
  could have steered the model's search wordings (the same reason ADR 0031
  refuses memory writes after such reads).
- **One memory is the target.** Exactly one memory meets all three conditions:
  - its compilation excluded it, as irrelevant or over budget;
  - a `memory.manage` search in the turn returned it;
  - it shares a search word with the answer.

  If no memory qualifies, or more than one does, the turn teaches nothing.
- **The target is the user's own assertion.** An inferred memory can be wrong,
  and a key would bring it back more often.
- **There are words to learn.** These are the request's search words (ADR 0029
  rules) that the target lacks and that at most two active memories contain.
  They are ranked longest first, then alphabetically, and at most six are kept.

### From candidate to promoted

- **First episode.** It records a candidate.
- **A later episode is distinct** when its set of request words differs from
  every earlier episode in the chain. A retry or an immediate re-ask with the
  same words is the same episode. In this version, the same question asked
  again on another day is also the same episode.
- **A distinct second episode within 30 days** promotes the key. The words
  become the union of the episodes' words, and the key expires 90 days later.
- **Breadth check.** A promotion is refused when the key would match more than
  2 of the session's last 50 finished agent-run requests, evidence excluded.
  The detector then records a candidate with a `blocked` reason, so the ledger
  shows why.
- **Renewal by use.** A turn that uses an active key renews it with a new
  promotion when its last renewal is more than 30 days old. Renewal costs at
  most one event per key per month.

ADR 0049 permits this without approval: the key widens no effect, and the user
can undo it. The invariant "no permanent improvement from one successful
trajectory" holds in two ways. One episode only creates a candidate, and no
key is permanent.

### How the compiler uses a key

1. The turn compiles as before.
2. Active keys are loaded from the journal, at or before the high-water and
   valid at the turn's acceptance.
3. A key **matches** when the request shares at least `min(2, words)` of its
   words, with the memory-search folding.
4. If an active key matches and its target is a current candidate the first
   compilation left out, the turn compiles again with that memory
   `policy-required`.
   - The receipt reason is `learned key <promotion event id>`, so the "Why?"
     view and replay can name the key.
   - At most two keys apply per turn, the most shared words first, then the
     oldest.
5. A turn whose complete set fits the budget never changes, because nothing is
   left out.

### Replay

Replay checks every receipt that cites a learned key:

- the cited event is an `improvement.promoted` of the same session;
- it was recorded before the turn's provenance cutoff;
- nothing at or before the cutoff supersedes it;
- it had not expired when the turn was accepted;
- it targets the included node;
- the request matches its words.

A receipt that fails any check, or a turn that cites more than two keys, does
not replay. Replay does not re-run the detector. The events record its
decisions, with their evidence turns, for the ledger and for review.

### Lifecycle

- **Inert when the target goes.** A key whose target is forgotten or
  superseded does nothing, because its target is no longer a candidate. The
  ledger shows it as retired for that reason. This is derived from journaled
  facts, and no event is written.
- **Expiry.** A key that is not renewed by use expires 90 days after its last
  promotion. A candidate older than 30 days no longer promotes.

### Inspection and control

- `GET /v1/learning?session_id=…` lists each chain: its state, words, target
  text, evidence turns, expiry and how many turns it brought its memory into.
- `POST /v1/commands/learning/retire` undoes one. The kernel assigns the user
  actor.
- The CLI has `ditto learning list` and `ditto learning retire`.
- `ditto-daemon --disable-learning` (`DITTO_DISABLE_LEARNING`) turns the
  detector and key use off. The Task 017 runner passes it, so suite v1 keeps
  measuring the system its budgets were frozen on.

## Consequences

- **The first harness-side learning in Ditto, at no model cost.** The model
  already wrote the search wordings during normal use.
- **Effect is limited to over-budget sessions.** Complete-set turns are
  byte-identical to contract 20, so the gain shows only in sessions whose
  memories exceed the budget (the `long_use` profile).
- **Stacked on an unmeasured contract.** Contract 21 sits on contract 20,
  which no campaign has measured, so one campaign cannot separate the two.
  Measuring this ADR's gain needs a learning-curve profile that is not built
  yet. Until then the claim is limited to the scripted turn tests.
- **A narrow injection channel remains.** A key's words are the user's own.
  Which memory it brings in is chosen by the model's search, in a turn that
  read nothing external. Injected text that reached Ditto's earlier answers is
  not caught, as ADR 0031 already records for memory writes.

## Alternatives

- **Adding the words to the memory's own text.** That would rewrite a user
  assertion with model-chosen words. It would also change the relevance score
  that replay checks.
- **Embeddings.** A new mandatory service, which comparison-2026-11 rejected.
  The misses so far were wording gaps, which learned words close.
- **A retrospection call that writes keys.** ADR 0049 allows one. It is not
  needed here, because the turn already holds the words.
- **Promoting on one episode.** That would break the single-trajectory
  invariant and learn from a single coincidence.
