# ADR 0028: Thin real-time harness

Status: proposed on 2026-09-30 and decided per phase as each lands. Phase A
was accepted with Task 025 (turn payload version 6), Phase B with Task 026,
Phase C with Task 027 (turn payload version 7), Phase D with Task 028 and
Phase E, in part, with Task 029 (turn payload version 8).
The full design is in
[docs/design/realtime-harness.md](../design/realtime-harness.md).

## Context

Measurements with `scripts/measure-harness.py` (Raspberry Pi 5, release build,
instant loopback model) show where the harness spends its effort:

- Only about half of each prompt (median 50.6 %) is byte-identical to the
  previous turn's, so a provider or local KV cache recomputes the rest every
  turn. The complete-set capsule orders memories by relevance to each
  question, the history window slides once past eight exchanges, and the
  local time sits in the instructions ahead of both.
- Every streamed delta costs 74–80 µs as its own SQLite transaction, and
  journaling is 81–107 bytes per answer byte.
- Each request re-journals the whole prompt.
- One global run slot rejects concurrent runs from other sessions (1 of 3
  accepted).
- Turns re-read and re-verify context on the critical path (5–10 ms before
  dispatch).

## Decision (proposed)

1. **Context planes.** The prompt is layered from least to most volatile:
   constitution, tool surface, memory base, thread, turn tail, working set.
   Changes enter at the tail as deltas. The memory base (epochs) and the
   thread (stepped, extractive compaction) are rewritten only at explicit,
   journaled boundaries. Budgets follow the model's context window. This
   becomes turn payload version 6.
2. **Two streaming planes.** Advisory live deltas are fanned out immediately
   and are outside the durable sequence. Text is journaled in coalesced chunks
   (48 ms / 2 KB / structural boundaries). Tool calls stay durable before
   execution and terminals durable before reporting.
3. **Journal writer.** One writer thread group-commits batches; reads go to a
   WAL reader pool; no SQLite call runs on async workers; large immutable
   payload parts become content-addressed blobs.
4. **Session actors.** Hot state lives in memory per session, derived from
   committed events. Runs are FIFO within a session and parallel across
   sessions, bounded by per-provider lanes, with a `queued` status instead of
   429.
5. **One tool lifecycle.** All builtin tools share a lifecycle; independent
   read-only calls run in parallel; tool progress feeds the live plane.

Kept: provenance, extractive compaction only, no housekeeping model calls,
leases and claims, replay without provider, artifact or network I/O.

## Consequences

Phase A (layout) should make a local model prefill only the new tail of each
turn. Phases B–D remove per-token transactions, most journal bytes and the
single-run ceiling. Each phase amends the contracts it touches (payload
version 6, the event protocol's publish rule, ADR 0016's run slot, payload
blob references, the per-turn verification of ADR 0010/0013) when it lands,
with its measured before and after.

## Phase A as accepted (Task 025)

Turn payload version 6 implements the cache-stable layout with the fewest
moving parts:

- **Instructions without the time.** The local time moves to a note at the
  start of the latest message; an added segment says Ditto wrote it.
- **Capsule in ID order.** Selection and receipts are unchanged; only the
  order the model sees changes.
- **Stable tool surface.** `web.fetch` is offered to every agent run while
  enabled, and its grant still comes only from the message.
- **Stepped history window.** Steps of 8 exchanges, at most 16 exchanges,
  24 KiB. The anchor is the count of finished `run_*` turns since the reset,
  so no new event is needed.

Measured with `scripts/measure-harness.py` on the same machine, the median
prompt prefix identical to the previous turn rose from 50.6 % to 96.4 %
(minimum 96.0 %).

Four parts of the design were deferred:

- **Memory epochs and tail deltas.** A memory change is a one-time miss for
  the turn after it; add epochs only if measurements show it matters.
- **Context-window budgets.** The model IR caps a capsule at 1,800 tokens;
  raising that is an IR change.
- **Relevance hints for over-budget sessions.** Their lexical fallback set
  still varies with the question, now presented in ID order.
- **Excerpt blocks for exchanges that left the window.** They are dropped, as
  before.

## Phase B as accepted (Task 026)

Phase B removes the journal from the turn's critical path and SQLite from
async runtime threads, with no wire or payload change:

- **Reused session context.** A run reuses its session's verified context
  while no `context.node.recorded` event has been committed since it was
  taken; one kind-index probe checks that, including for other writers.
  Sessions with a task-scoped node or a validity window are compiled from the
  projection every turn. Sources are checked once for every task of the
  session, so reused context skips per-turn provenance lookups.
- **Kept threads.** Each session's thread is kept and advanced by the
  conversation events committed since the last turn, and reloaded when absent,
  far behind or ahead.
- **Context-only deltas.** The projection reads only the delta's context nodes
  through the kind index, and its normal-delta ceiling counts them. This
  amends ADR 0013, whose deltas read every event.
- **Tool contracts validated once.** `InvocableContract` binds a manifest,
  schema and revision validated at first use into every later epoch. The
  kernel keeps at most its three builtin contracts, which amends ADR 0014's
  "no manifest cache" for that fixed set; package changes already need a
  restart.
- **One commit before dispatch.** Context compilation and capability
  selection are staged and committed with the first model request, or with
  the failure that ends the turn first. Events are published after that
  commit, so "durable before published" holds per batch.
- **Storage off async threads.** Handlers use the blocking pool; runs, sorts
  and the scheduler run on blocking threads that drive their I/O through the
  runtime (tasks on a current-thread runtime). Debug builds reject journal
  access on any thread that drives async tasks.

Measured with `scripts/measure-harness.py` on the same machine, the durable
input to `model.requested` fell from a 5 ms (200 deltas) and 10 ms (1,000
deltas) median to 1 ms for both (mean 1.0–1.4 ms), and the client's POST to
the provider receiving the request from 6.5 ms and 11.8 ms to 2.7–3.0 ms.
The cost per streamed delta fell from 74–80 µs to 55–60 µs. Acceptance gained
0.1 ms for the hop to the blocking pool.

Three parts of the design were deferred:

- **A journal writer thread with group commit.** Appends from blocking
  threads under the one connection lock showed no contention at personal
  scale, and Phase C cuts the event rate 20–50×. Measure again with Phase D's
  parallel sessions.
- **A WAL reader pool.** Same reason: no read contention was measured.
- **Session actors.** Hot state is kept as reused context and kept threads;
  per-session run queues belong to Phase D.

## Phase C as accepted (Task 027)

Turn payload version 7 records each fact once and keeps one durable plane:

- **Coalesced text.** Consecutive text deltas commit as one `model.output`
  chunk that names the provider sequences it covers. The first text after a
  quiet interval of 48 ms commits at once; later text commits 48 ms after its
  chunk opened, before the chunk would pass 2 KiB, or with the next non-text
  output, failure or terminal in the same transaction. Every provider event
  still counts against the request's event bound.
- **Requests as digests.** `model.requested` records the request ID, the
  deadline and the SHA-256 of the request as sent. Instructions, tools,
  capsule and conversation already derive from earlier durable events, and
  the runtime and replay build requests with one shared function, so replay
  rebuilds each request and must reproduce the digest.
- **Derived parts not recorded.** `context.compiled` omits the capsule and
  `capabilities.selected` the builtin schemas.

Measured with `scripts/measure-harness.py` on the same machine (200- and
1,000-delta answers):

| Measure | Before | After |
| --- | --- | --- |
| Durable events per turn | 206 / 1,006 | 8 / 9 |
| Journal bytes per answer byte | 111 / 80 | 16.3 / 4.9 |
| Provider sending the answer to its last text on a client's stream | 56 / 116 ms | 2.6 / 10–16 ms |
| Provider to the first text on the stream | 1.7 / 5.2 ms | 1.6 / 5.6–7.6 ms |

The first-text times are dominated by the loopback mock building its stream.
Journal bytes also no longer grow with the conversation: requests used to
repeat up to 24 KiB of history each.

Two parts of the design were replaced:

- **The live plane.** Committing the first text after a quiet interval at
  once gives a slow stream no added latency and bounds a fast one to 48 ms at
  about twenty chunks a second, with no second plane, resume protocol or
  client change. Web and Telegram render chunks as they rendered deltas.
- **Blobs.** Digests of derivable requests remove the largest repeated part
  without a blob table or references that readers would have to resolve.

The target of 3 journal bytes per answer byte is not met: 4.9 for a 4 KB
answer, 16.3 for an 800-byte one. The rest is the compiled context of each
turn (6.5 KB with twelve memories, relevance-ordered per question), the
capability selection (3.3 KB) and the answer, recorded in its chunks and again
in `turn.finished`, which status, history and clients read. Node references in
the compiled context and an answer derived from its chunks would close it;
they change several readers and are deferred until journal size matters.

## Phase D as accepted (Task 028)

The run slot is now per session: each session has at most one active run or
sort, and up to four sessions run at once. A second run in a busy session, or
a fifth session, gets HTTP 429 as before. Due scheduled work starts as soon as
its own session can, so a run in one session never holds back another
session's reminder. Cancellation, status and shutdown address each session's
run, and shutdown drains all of them.

Measured with `scripts/measure-harness.py` on the same machine, three sessions
starting a run at once against a provider that answers after 0.5 s: before,
one was accepted and two got HTTP 429; now all three are accepted, reach the
provider within 2 ms of each other and all finish after 513 ms.

Three parts of the design were replaced:

- **Session actors and a kernel queue with a `queued` status.** Clients
  already keep one session's messages in order: the Telegram gateway queues
  each chat's messages for one worker and retries a busy answer, and the web
  app sends one request at a time. A kernel queue would add a durable queued
  state, dequeueing, cancellation and restart rules for no user-visible gain,
  so a busy session still answers HTTP 429 and nothing new reaches the wire.
- **Provider lanes.** Local servers such as Ollama and llama.cpp queue or
  parallelize requests themselves; the four-run limit bounds the load the
  kernel creates.
- **A scheduler without a slot.** The scheduler keeps its one loop and index
  and now checks each due item's own session.

## Phase E as accepted (Task 029)

- **`memory.search`** ([ADR 0029](0029-memory-search-tool.md)): agent runs
  can search the memories their compilation saw, including those the budget
  left out, and replay recomputes each result.
- **Progress.** While a tool runs, the web app says what it is doing
  ("Searching memories…", "Reading the linked page…") from the tool call
  already on the event stream, until the model writes again.

Two parts are deferred:

- **One builtin tool lifecycle.** Each tool keeps its own journal events and
  replay checks. A shared lifecycle would restructure the request, claim and
  replay path of every tool, including the effectful sort, with no change a
  user sees; revisit when another effectful tool is added.
- **Parallel read-only calls.** A request still yields at most one tool call.
  Running several at once needs every tool's journal in phases (all requests,
  then all starts, then all results) and matching replay, for a gain only in
  turns that call several tools; revisit when such turns are measured to
  matter.

