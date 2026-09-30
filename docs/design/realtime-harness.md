# Thin real-time harness

Status: design, proposed on 2026-09-30 ([ADR 0028](../adr/0028-thin-realtime-harness.md)).
Phase A is implemented (Task 025, turn payload version 6: prefix reuse 50.6 %
to 96.4 %) and Phase B (Task 026: input to provider request 5–10 ms to 1 ms);
the ADR lists what each deferred. Phases C–E are not implemented.

The goal is a harness that does as little as possible per turn, streams without
delay, serves several conversations at once, and gives the model exactly the
context it needs, placed so that no work is repeated. Every design choice below
answers a measured cost.

## 1. Baseline

Measured on 2026-09-30 on a Raspberry Pi 5 (release build, loopback
OpenAI-compatible mock that answers instantly, so these are the harness's own
costs) with `python3 scripts/measure-harness.py`:

| Measure | 200-delta answer | 1,000-delta answer |
| --- | --- | --- |
| Harness work before the provider request (input to `model.requested`) | 5 ms | 10 ms |
| Harness cost per streamed delta | 80 µs | 74 µs |
| Durable events per turn | 206 | 1,006 |
| Journal payload bytes per answer byte | 107 | 81 |
| Prompt prefix identical to the previous turn (median of 11) | 50.6 % | — |
| Three sessions starting a run at once | 1 accepted, 2 × HTTP 429 | — |

After about ten 1,000-delta turns the event database held 6.6 MB plus a 4.1 MB
WAL: about 1 KB on disk per streamed delta.

## 2. Where the time and bytes go

- **Half of every prompt is recomputed.** The model sees
  `[instructions][tools][memory capsule][history][question]`. The
  complete-set compiler orders memories with lexical matches first, so the
  capsule changes whenever the question does. Past eight exchanges, the
  history window slides by one exchange per turn. The minute-resolution local
  time sits in the instructions, before both. Providers cache and llama.cpp
  or Ollama reuse their KV cache only for a byte-identical prefix, so
  everything from the capsule on (about half today, most of it as memories
  and history grow) is prefilled again every turn. On a CPU-bound local model,
  prefill of thousands of tokens is the largest share of time to first token.
- **Every streamed token is a transaction.** Each text delta is serialized
  three times (twice for bound checks, once for storage), inserted as its own
  autocommit SQLite transaction that updates the table and six index B-trees,
  and published. This
  runs synchronously on an async worker while holding the store's single
  mutex.
- **Each request re-journals the whole prompt.** `context.compiled` stores the
  compiled context and the capsule. `model.requested` stores the full request:
  instructions, tools, capsule and history, 10–25 KB, once per request of a
  turn.
- **One run per kernel.** A global run slot rejects every other run, from any
  session, channel or schedule, with 429.
- **Reads on the critical path.** A turn re-synchronizes and re-verifies the
  context projection under a global gate, then reads each history turn's
  input individually (1 + N queries).

## 3. Principles

1. Work is proportional to what changed, not to how much state exists.
2. Hot state lives in memory, derived from the journal by events; the critical
   path never reads the journal.
3. One durable commit per turn phase; no per-token transactions.
4. A real-time plane (advisory, immediate) is separate from the durable plane
   (authoritative, batched).
5. The prompt of a thread is append-only. Changes are injected as deltas at the
   tail and consolidated only at explicit, journaled boundaries.
6. Existing invariants stay: provenance on every injected item, no model call
   for housekeeping, leases for effects, replay without I/O.

## 4. Context: planes, timing and scope

### 4.1 Planes

The prompt is built from six planes, ordered from least to most volatile:

| Plane | Content | Scope | Changes when | Cache |
| --- | --- | --- | --- | --- |
| P0 Constitution | versioned instructions | payload version | a new version | always reused |
| P1 Surface | every tool the session may be offered, fixed order | session profile | a capability is installed or disabled | reused |
| P2 Memory base | consolidated memories in admission order, epoch *n* | session | consolidation only | reused within an epoch |
| P3 Thread | frozen compaction blocks, then exchanges | thread | an exchange is appended; a compaction step | reused; one miss per step |
| P4 Turn tail | local time, memory changes since epoch *n*, relevance hints, grants, channel | turn | every turn | not reused (small) |
| P5 Working set | tool calls and results | request chain | each tool call | reused across a turn's requests |

The request is `P0 P1 P2 P3 [P4 + user message] P5…`. The user message and its
tail are the only new bytes of a turn. Tool results only extend the end.

### 4.2 Injection timing

- **T0, accept.** The input becomes durable and the session's hot state is
  read from memory: memory base and epoch, pending memory changes, thread.
- **T1, assemble.** P0–P3 are byte-identical to the previous turn's prefix
  unless a boundary was crossed. P4 is computed. The request is journaled as
  references to P0–P3 plus the inline tail.
- **T2, continue.** Each tool result appends to P5. Nothing before it changes,
  so every later request of the turn reuses the whole earlier prompt.
- **T3, finish.** The exchange (question and final answer, not tool results)
  is appended to the thread and becomes part of P3 for the next turn.

Mutations can arrive at any time from any client:

- A memory saved or corrected becomes a **delta** in the next turn's tail,
  such as "Remembered: …" or "Memory X is corrected by Y". The base is untouched.
- `conversation.reset` starts a new thread (empty P3) and consolidates the
  memory base.
- A capability change produces a new P1. This is rare and accepted as a miss.

### 4.3 Consolidation and compaction

Rewriting an early plane invalidates everything after it. Early planes
therefore change only at explicit boundaries that are journaled, so replay
reproduces the exact layout:

- **Memory epochs.** Pending changes fold into a new base when a thread
  starts, when the thread compacts, or when the changes exceed 8 items or 400
  tokens. `context.epoch` records the epoch number, the ordered memory IDs
  and the capsule's hash. Until then, a correction appears as base plus tail
  note, so the model sees both, with provenance and the supersession spelled
  out.
- **Stepped thread compaction.** P3 has a budget. When it is exceeded, the
  oldest exchanges covering at least half the budget fold into one frozen
  block, then appending resumes. The block is extractive and deterministic:
  user messages verbatim, assistant messages cut to their first sentence
  (at most 200 characters), each marked as an excerpt with its turn ID.
  `conversation.compacted` records the covered turns and the block's hash.
  Compared with today's sliding window, the prefix misses once per step
  instead of once per turn. No model call is made and nothing is paraphrased,
  so no inference is presented as something the user said.
- **Relevance without reordering.** The base keeps admission order. If the
  memory set exceeds its budget share, the base holds a stable top-K (pinned,
  then most recently admitted). The tail lists the non-base memories that
  match the question lexically, within a small budget. Everything else is
  reachable through a read-only `memory.search` tool (Phase E). The right
  memory reaches the model without moving any byte of the base.

### 4.4 Scope

| Scope | Holds | Visible to |
| --- | --- | --- |
| Session | memories, epochs | every thread and channel of the session |
| Thread | exchanges, compaction blocks | turns of that thread (one active thread per session) |
| Turn | tail facts, grants (links, attachments) | that turn only |
| Request chain | tool calls and results | later requests of the same turn |

Tool results do not carry into later turns; only the final answer does. There
is no cross-session context. A future global profile would require explicit
promotion.

### 4.5 Budgets by model

Today every model gets 900 estimated tokens of memories and at most eight
exchanges or 24 KB of history, whether it has 8 K or 1 M tokens of context. A
provider profile declares its context window *W* and output reserve *R*
(`--context-window`, with provider defaults), and the planes share it:

- P0 and P1 are measured.
- P2 gets at most 20 % of *W*, P3 at most 45 %, P4 at most 5 %.
- P5 and *R* get the rest.

## 5. Execution and concurrency

The kernel becomes a small set of tasks with no global mutex on the hot path:

- **Journal writer.** One dedicated OS thread owns the single SQLite write
  connection. Batches arrive over a channel and are group-committed (flushed
  at 2 ms or 128 events), sequences assigned, callers completed, then published
  on the durable hub. Durable-before-publish holds per batch.
- **Reader pool.** WAL read connections on blocking threads serve status,
  events, replay and projection rebuilds. No SQLite call runs on an async
  worker.
- **Session actor.** Each active session has one actor (started lazily,
  evicted when idle). It holds hot state: memory base and pending changes,
  epoch, thread and blocks, run queue and active turn. It builds this from the
  journal once, then keeps it current from its own session's committed
  events. It serializes that session's runs (FIFO, bounded, with visible queue
  position), memory writes and resets. The global context-admission gate
  becomes this per-session order.
- **Turn task.** One per run, spawned by the session actor, owning the
  cancellation token and deadline. Before dispatch it takes a permit from its
  **provider lane**, a semaphore per provider profile: one for a loopback
  local server by default (one model, one prefill at a time), four for hosted
  providers.
- **Scheduler.** It enqueues due runs into session actors instead of a global
  slot.
- **Hubs.** The durable hub fans out committed events; a lagging subscriber
  recovers from the reader pool, as today. The live hub fans out advisory
  deltas per session. It is bounded and lossy by design: a slow subscriber
  skips ahead.

Run status gains `queued`. Runs in different sessions proceed in parallel up to
their lane. Runs in one session stay strictly sequential, so each turn sees the
previous exchange and the thread stays linear.

## 6. Streaming: live and durable planes

- Each validated provider event is published at once on the live hub as
  `live.delta {turn_id, request_index, offset, text}`. The web app and
  Telegram drafts render from it.
- Text deltas are buffered per request and journaled as one coalesced
  `model.output` chunk when 48 ms have passed since the first buffered delta,
  2 KB are buffered, or a non-text event arrives. Non-text events (tool calls,
  usage, completion) are journaled individually, in order, right after the
  flush.
- A ready tool call is durable before it executes, and `turn.finished` is
  durable before a run is reported finished, as today.
- Chunks record the raw sequence range they cover. Runtime bounds (event
  count, bytes) are enforced on raw events and recorded. Replay rebuilds the
  same assistant text and conversation.
- Live deltas are advisory. After a reconnect a client renders durable state
  and resumes live deltas by turn, request and offset. Nothing authoritative
  depends on the live plane.

For a 1,000-delta answer this means about 20 to 40 durable events instead of
1,000. Live latency is bounded by in-process fan-out plus the socket write,
and durable latency by the 48 ms flush.

## 7. Journal

- **Content-addressed blobs.** A `blobs(sha256 primary key, bytes)` table is
  append-only (by trigger) and written in the same transaction as the
  referencing event. Instructions, the tool surface, memory bases, compaction
  blocks and thread snapshots are stored once. `model.requested` becomes
  references plus the inline tail and new working-set items: under 1 KB
  instead of 10–25 KB.
- **Turn prelude in one commit.** A turn's input, context and selection events
  and its first request commit as one batch: one commit before dispatch.
- **Verification where it matters.** Context sources are verified when a
  memory is admitted and when the projection is rebuilt at startup. Hot state
  derives from that same append-only journal, so per-turn re-verification is
  dropped. Replay still verifies everything.
- **Indexes.** They stay as they are until coalescing has cut the event count
  20–50×; then they are revisited with measurements.

## 8. Tools

- **One lifecycle.** A `BuiltinTool` interface covers the schema, deriver,
  grant from the turn scope, execution with a consumed claim, outcome codec
  and replay checks. A generic journaled lifecycle
  (`tool.requested` / `tool.started` / `tool.output`, versioned) carries
  tool-specific payloads. `artifact.read`, `artifact.sort` and `web.fetch`
  migrate to it, and a new tool becomes one module instead of about 700 lines
  across run and replay.
- **Parallel reads.** When every ready call of a response is read-only and
  within its lease budget, the calls run concurrently (three links are
  fetched at once). Results append in call order. `parallel_tool_calls` is
  allowed only then.
- **Progress without tokens.** Tool lifecycle events feed the live plane
  ("reading example.com…"), so the interface responds while the model waits.

## 9. Contracts that change

Each needs its ADR text before implementation (drafted as ADR 0028):

- Turn payload version 6:
  - the prompt layout: stable capsule order, a tail carrying time, memory
    deltas and relevance hints, and a stable tool surface;
  - `context.epoch` and `conversation.compacted`;
  - budgets derived from the context window.
- Event protocol: live deltas are advisory and outside the durable sequence,
  and text is journaled in coalesced chunks. "Every transition is durably
  appended before it is published" now applies to authoritative
  transitions.
- ADR 0016: session-parallel execution with a `queued` status replaces the
  single run slot.
- Blob references in payloads (versioned).
- Context verification moves from every turn to admission and startup.

Kept unchanged:

- provenance, and no model inference as a user assertion (compaction is
  extractive);
- no housekeeping model calls, including no pre-warming by default;
- leases and claims for effects;
- replay without provider, artifact or network I/O.

## 10. Plan

Phases in order of user-visible effect; each lands with tests, an ADR,
`measure-harness.py` numbers and the canonical gate.

| Phase | Scope | Exit criteria |
| --- | --- | --- |
| A | Cache-stable layout, payload v6: stable capsule order with tail relevance hints; time and memory deltas in the tail; epochs and stepped compaction; stable tool surface; context-window budgets | median prefix reuse between boundaries ≥ 90 % (today 50.6 %); versions 1–6 replay; corpus harness adapted |
| B | Journal writer, reader pool, hot session state, turn prelude in one commit | no SQLite call on async workers; before-dispatch ≤ 2 ms median (today 5–10 ms) |
| C | Live plane, coalesced durable text, blobs; web and Telegram on live deltas | ≤ 50 durable events per 1,000-delta answer (today 1,006); ≤ 3 journal bytes per answer byte (today 81–107); live delta to socket ≤ 5 ms p99 |
| D | Session-parallel runs: session queues, provider lanes, `queued` status, scheduler without a global slot | three sessions at once all stream (today 1 of 3); same-session runs queue in order with positions |
| E | Generic tool lifecycle, parallel read-only calls, progress events, read-only `memory.search` | existing tools migrated with unchanged evidence; three links fetched concurrently |

A comes first because a stable prefix saves the most time with local models
and touches only prompt assembly and replay. B and C cut harness CPU and
storage. D depends on B. E thins the code and speeds multi-tool turns.

## 11. Rejected alternatives

- **Dropping durable streaming.** Replay and inspection would lose the
  model's actual output.
- **Model-written summaries of history.** They need a housekeeping model call
  and present inference as conversation fact.
- **Per-turn relevance ordering or a sliding window.** Each defeats prefix
  caching on every turn.
- **Unordered parallelism within a session.** The thread would stop being
  linear.
- **A database per session.** Recovery and cross-session queries would get
  harder, with no measured benefit.
- **An actor framework or message broker.** Tokio tasks, channels and SQLite
  suffice.
- **Pre-warming the model cache with prefill-only requests.** It is a model
  call that answers nothing. It may become an opt-in for local providers
  only.
