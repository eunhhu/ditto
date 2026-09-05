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
