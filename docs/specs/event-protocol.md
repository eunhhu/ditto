# Event Protocol

## Source of truth

Events are immutable and ordered by a daemon-local monotonically increasing
`seq`. `event_id` is globally unique. Projections may be deleted and rebuilt;
events may not be silently rewritten.

## Authority boundary

Public clients send narrow commands. They do not submit event actors or internal
event kinds.

```http
POST /v1/commands/input
```

```json
{
  "text": "inspect the service logs",
  "session_id": "local",
  "task_id": "task-7"
}
```

The kernel converts this to `actor=user`, `kind=input.received`. Model,
capability, policy, scheduler, and system events are issued only by their trusted
runtime components. The default daemon exposes no arbitrary event-append route.

## Envelope

```json
{
  "seq": 42,
  "event_id": "01J...",
  "recorded_at": "2026-08-30T10:00:00.000Z",
  "session_id": "local",
  "task_id": "task-7",
  "actor": "capability",
  "kind": "execution.output",
  "payload": {},
  "causation_id": "01J...",
  "correlation_id": "01J...",
  "span_id": "span-3"
}
```

- `seq`: durable resume cursor; never reused.
- `event_id`: global identity.
- `recorded_at`: daemon timestamp, never client supplied; append canonicalizes it
  to the millisecond precision preserved by durable storage and publication.
- `session_id`: conversational continuity boundary.
- `task_id`: durable work boundary.
- `actor`: user, model, capability, policy, scheduler, or system.
- `kind`: open namespaced string; unknown kinds remain storable.
- `payload`: kind-specific JSON.
- `causation_id`: event that directly caused this event.
- `correlation_id`: root operation or request.
- `span_id`: tracing span when available.

## Durable context nodes

`context.node.recorded` is an internal, kernel-authored event. Its fixed
version-1 envelope is `actor=system`, `kind=context.node.recorded`, and a
payload containing `event_version=1` plus the validated `ContextNode`. The
trusted draft is non-deserializable and carries only the node and its requested
session/task scope; it cannot choose an actor, kind, event identity, sequence,
timestamp, causation, or correlation. Only session and task scopes are
admitted. Node identity is `(session_id, node_id)` across both scopes, while
supersession is restricted to the exact same scope.

Every source event must already exist in the same session and provide valid
provenance with actor evidence matching the node origin. User-origin assertions
require user-authored evidence, and model-origin assertions are not admissible.
The kernel assigns causation to the cited source with the greatest durable
sequence, independent of caller ordering. The event spine remains the sole
authority; nodes and source events are immutable, and replacements use a new
node with `supersedes` rather than in-place mutation.

The derived `context-projection.db` is a separate, deletable cache. Its
schema-4 checkpoint binds `through_seq`, `through_event_id`, and the canonical
compact-index digest. Bounded startup/recovery replay derives global and
per-session digest chains plus immutable identity, provenance, causation,
scope, and supersession metadata; only a process-local proof for that exact
generation permits normal index use. Steady-state synchronization reads the
checkpoint delta and exact cited source IDs instead of rescanning an affected
session from sequence zero. The index and delta have fixed entry, byte, event,
payload, and verification-work limits and never repair or rewrite canonical
events. A durable append is the acceptance point. If post-append projection
synchronization fails, the kernel still makes one live publication attempt and
returns the committed event in a typed
`committed_but_projection_unavailable` outcome; later open or retrieval
replays the event spine to recover the cache. Recovery publishes no substitute
event, and a retry resolves the already committed identity without appending a
duplicate.

The read-only V2 working-set operation is all-or-nothing: it builds one bounded
`TaskQuery` and shares it between context and capability retrieval, then returns
the complete verified snapshot or a typed error, never a partial result.
Production behavior is explicitly lexical-only. An embedding provider may be
injected for tests or internal experiments; provider failure is surfaced rather
than silently falling back to lexical retrieval, and embeddings cannot bypass
scope or other hard filters.

## Explicit user memory

`POST /v1/commands/memory` accepts only:

```json
{
  "session_id": "personal",
  "input_event_id": "01J00000000000000000000000",
  "replaces": "memory-01h00000000000000000000000"
}
```

`replaces` is optional. IDs in this example are placeholders for real recorded
input and memory IDs. The input must be an existing same-session, task-free,
user-authored `input.received` event with nonblank text of at most 4,096 UTF-8
bytes. Unknown command fields are rejected. The kernel constructs a session
Claim with User/Asserted/Personal metadata, confidence 1.0, exact source text,
one source input ID, and the deterministic lowercase `memory-<input ULID>` ID.
The durable envelope, payload version, provenance checks, and commit boundary
remain those of `context.node.recorded` above.

Responses contain `memory_id`, the durable context `event_id` and `event_seq`,
and `outcome`. First acceptance is HTTP 201/`recorded`; a matching retry is
HTTP 200/`already_recorded` with the original event and no new publication.
Changing the replacement intent for an already saved input conflicts. A
replacement must be a currently active explicit memory in the same session;
concurrent corrections of one old memory are serialized by the admission gate
and at most one succeeds. HTTP 409 reports a conflict. Unknown/mismatched sources
and malformed commands are rejected before context append.

HTTP 202/`committed_but_projection_unavailable` means the context event is durable
but its searchable projection is unavailable. An identical promotion retry
recovers the projection or reports its existing durable identity. Operational
errors expose path-free messages. The CLI first records input through the
existing input command, then promotes it; failure between these steps leaves
input evidence, not a claimed saved memory. `memory from-input INPUT_ID` supports
explicit recovery with the same session/replacement options. There is no
automatic HTTP retry.

`GET /v1/memories?session_id=personal&limit=20` reads a source-verified active
snapshot. Each item contains `id`, exact `text`, `input_event_id`, and optional
`replaces`. The response includes `through_seq` and `next_after_id`; use that ID
as `after_id` on the next request. IDs are ordered lexically, page limits are
1 through 100 without clamping, and every returned item is compared to its exact
source input. Each page has its own high-water, so pagination across concurrent
changes is not a frozen historical snapshot. The existing 10,000 active-node
and cumulative snapshot byte limits still apply, including other active context
in that session. Listing adds no event and invokes no model/embedding provider.

`personal` is the CLI's default session name, not a new global scope. Other
sessions remain isolated. Deletion, automatic extraction, cross-session recall,
and recurring model scheduling remain deferred. [ADR 0015](../adr/0015-explicit-user-memory.md)
owns this command and retry contract.

## Explicit agent runs

`POST /v1/commands/run` accepts only `request_id`, `session_id`, and `text`.
The request ID is a canonical uppercase ULID; session IDs are canonical and
text is at most 16 KiB UTF-8 before existing whitespace normalization. The
kernel derives task ID `run_<request_id>` and creates a fresh turn correlation.
The durable input carries additive `agent_run: {version: 1, request_id: ...}`
metadata fixed by the kernel, never copied from client event fields.

Acceptance precedes dispatch and returns HTTP 202 with `running`. Identical
normalized text with the same session/request ID returns the existing run,
including terminal or interrupted runs; changed text is HTTP 409. One active
run is allowed per kernel, and additional identities get HTTP 429 without
acceptance or a queue. Disabled execution and shutdown return HTTP 503 for new
work. The daemon's default is disabled; input and memory routes remain free of
model calls. POST is not automatically retried by the CLI.

`GET /v1/runs?session_id=personal&request_id=...` and
`POST /v1/commands/run/cancel` (the same two identity fields as JSON) inspect or
cancel that exact run. Unknown identities return 404. Cancellation is a live
signal, not a durable promise of a terminal; `cancellation_requested` is true
while the matching live token is signalled. A durable cancellation becomes
`failed` with `failure_code: cancelled`. A finished run is `unverified` with a
response. If the original input exists without a terminal or live owner, state
is `interrupted`, not success or pending automatic restart. Status inspection
projects indexed trusted journal boundaries; complete trace verification is the
separate existing replay API.

Only explicit operator provider selection enables dispatch. The installed
OpenAI profile uses transport-only environment credentials and ephemeral remote
storage. It is not free inference. The public run command has no provider,
credential, timing, effect, lease, or trusted-context fields; unknown fields are
rejected. Live provider execution is loopback-only. A client disconnect does not
cancel accepted work. Graceful shutdown closes admission, cancels the one run,
drains it, and closes event followers. Crashes/storage failure can leave an
interrupted identity. The supported writer remains one kernel and its clones.

Current session/task context comes from a bounded source-verified projection
snapshot, with supersession and scope filtering before lexical compilation.
The recorded capsule is stable for the turn; subsequent corrections affect
later turns. Retrieval failure blocks provider I/O. No transcript injection,
memory inference, semantic embedding worker, or housekeeping call is added.

The CLI prints identity before POST. It waits on SSE terminal notifications by
default, retaining only an event-name line of at most 128 bytes, and reads
canonical status when notified. `--detach` returns after admission, Ctrl+C
signals cancellation, and connection failure reports recovery with the same ID.
A six-minute client follow ceiling does not restart or cancel server work;
the server's existing five-minute turn ceiling remains authoritative.
See [ADR 0016](../adr/0016-explicit-agent-runs.md).

## Kernel artifact-read turns

The kernel owns version 1 of the durable read-only turn state machine. Clients
cannot select its actors, kinds, correlations, or spans. The fixed mapping is:

```text
input.received          user
context.compiled        system
capabilities.selected   system
model.requested         system
model.output            model
capability.requested    model
execution.started       capability
execution.output        capability
turn.finished           system
turn.failed             system
```

All versioned payloads carry `event_version = 1` and a kernel-assigned `turn_id`.
Model request/output events use the request ID as their span; capability and
execution events use the call ID. Every transition is durably appended before it
is published or returned. Each `model.output` also records the
integer-millisecond instant at which the fully validated, bounded semantic event
was admitted.

Replay selects an explicit turn from one session snapshot and validates the
context provenance cutoff, exact selected manifest/epoch/schema, request and
stream order, output-admission time, call/result correlation, bounds,
cancellation/deadline failure stage and typed effective-deadline evidence,
terminal state, and absence of a `task.completed` claim. It reconstructs the complete transcript without calling
the provider or reading an artifact again. A pre-existing completion for the
target task rejects live turn admission without creating new events.

Version-1 agent-run input metadata enables Auto tool choice from request zero
and a direct final answer with zero tool calls. Legacy inputs retain Required
first-tool semantics. Replay validates metadata and the matching generation
controls; no capability authority, effect, or completion rule changes.

## Streaming

`GET /v1/stream?after_seq=N` subscribes to live events before capturing a durable
high-water sequence. It then replays all matching events through that high-water
mark in bounded pages, deduplicates buffered live events by `seq`, and follows
new events.

If a live sequence gap or broadcast lag is observed, the server captures a new
high-water mark and catches up from SQLite before resuming live delivery. A
query `limit` controls replay page size; it never truncates the logical stream.

SSE `id` equals global `seq`; SSE `event` equals the event kind. Session and task
filters may skip global sequence values, so continuity is evaluated against the
server cursor rather than requiring adjacent delivered SSE IDs.

## Evolution

Payload schemas are versioned by event kind when incompatible evolution is
unavoidable. Envelope fields are additive. Consumers ignore unknown payload
fields and preserve unknown event kinds.
