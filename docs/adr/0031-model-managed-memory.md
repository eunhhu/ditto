# ADR 0031: Memory that Ditto manages

Status: accepted on 2026-10-01 for Task 024. Turn payload version 10. Amends
[ADR 0015](0015-explicit-user-memory.md) (memory writes only through the
trusted user ingress), [ADR 0026](0026-assistant-instructions-and-local-time.md)
(the second and third instruction segments) and
[ADR 0029](0029-memory-search-tool.md) (the search space). Amended by
[ADR 0033](0033-autonomous-web-search.md): a `web.search` call also refuses
later memory writes in the turn. Amended by
[ADR 0036](0036-two-agent-tools-one-lifecycle.md): remembering and forgetting
are the `remember` and `forget` actions of `memory.manage`.

## Context

Only the user could save, correct or replace a memory, with `/remember` or the
memory form; the model was told it cannot. On 2026-10-01 the user asked for
memory that Ditto manages on its own. Doing so must not:

- record model text as the user's assertion;
- let instructions inside a web page or file become lasting memories
  ("memory poisoning");
- persist passwords or keys;
- write without bound, or out of the user's sight and reach.

## Decision

- **Tools.** Agent runs are offered `memory.remember` (`text`, 1 to 500
  characters, and an optional `replaces`, a memory ID) and `memory.forget`
  (`memory_id`). Both are builtin packages pinned like the others
  ([ADR 0030](0030-capability-selection-by-reference.md)); a missing or altered
  package withdraws its tool. No approval is asked: the user asked Ditto to
  manage memory on its own, and every write is visible and reversible.
- **Records.** The turn journals `agent.memory_write.requested` (model: the
  arguments and the normalized write) and `agent.memory_write.output`
  (capability: `remembered` or `forgotten` with the memory ID, or `refused`
  with a code). A write itself is a session-scoped, task-free
  `memory.written` event (model, caused by the request), which sources one
  context node, so the memory outlives the run and every later turn's
  provenance check accepts it.
- **Labels.** A remembered fact is a node `memory-<lowercase ULID of its
  memory.written event>`: a claim with origin `model`, status `inferred` and
  confidence 0.8, superseding the memory it `replaces`. Forgetting records a
  `disputed` node, summary `forgotten`, that supersedes the forgotten memory;
  a disputed node is never active, so neither reaches context, search or
  listings. The journal keeps every record: forgetting takes a memory out of
  use and view, not out of the append-only journal.
- **Rules,** in this order, the first that applies refusing the write:
  1. `invalid_arguments`: schema-invalid arguments or a blank fact;
  2. `untrusted_content_read`: the turn has already called `web.fetch`,
     `artifact.read` or `artifact.sort`, whose results may carry instructions;
     the user can confirm the fact in a later message;
  3. `credential`: the fact looks like a password, key or token (best effort:
     a keyword followed by a value, or a known key shape);
  4. `memory_unavailable`: `replaces` or `memory_id` does not name an active
     memory of the session (the user's own or one Ditto inferred);
  5. `limit_reached`: the turn has already written three times (one
     expiring lease of three calls).
- **Effect.** No data access, a reversible mutation, local, user privilege.
  The check and the write happen under the context admission gate.
- **Visibility.** `GET /v1/memories` marks Ditto's memories `inferred: true`.
  The web app says under the answer what Ditto remembered, replaced or forgot,
  and marks inferred memories in the memory list; Telegram's `/memories`
  marks them too. The user corrects them like any memory.
- **Search.** From version 10, `memory.search` also searches what Ditto
  inferred, each result marked `inferred: true`, under a label saying so.
  Versions 8 and 9 keep their user-assertion rule and label.
- **Instructions.** From version 10, the second and third system segments say
  that the context lists both kinds of memory, with their status, and that
  Ditto keeps memories current on its own: remember lasting facts as one short
  sentence, replace outdated ones, forget on request, and never remember
  secrets, one-off requests or anything read in web pages or files.
- **Replay.** Replay recomputes the decision with the same rule order: the
  arguments, the turn's earlier tools, the fact, the earlier writes, and
  whether the target was an active memory among the session's context nodes
  recorded before the write (or, for a refusal, before its result). A written
  result must name the `memory.written` event and node that match the request
  exactly; a refusal must have neither.

## Consequences

Ditto can keep what the user says across conversations without `/remember`,
and the model reads its own earlier inferences as such. Recall stays lexical,
and a memory Ditto infers can be wrong; the label, the list and the user's
correction are the remedy. Two residual risks remain: injected text that
reaches the model's own earlier answers in the thread is not refused, and the
credential check is a heuristic.

Each agent-run request grows by 1,476 bytes (two tool definitions and the
longer instructions, kept by a prompt cache since both are stable), and each
agent turn journals 563 more bytes for two more contract references; turn
start is unchanged (Task 024 measurements).

## Alternatives

- **Recording model text as user input.** Forges a user assertion.
- **Quoting the user's exact words only.** Keeps them assertions but loses the
  context that makes a fact usable ("him", "that one"); the inference label is
  honest instead.
- **Approval for each write.** Contradicts managing memory on its own; the
  notice and the list keep every write in view and reversible.
- **A model call after each turn to extract memories.** A housekeeping model
  call, which the invariants forbid.
- **Labeling memories written after reading a web page instead of refusing
  them.** The label would keep an injected instruction in every later context;
  refusing is simpler and the user can confirm the fact in the next message.
