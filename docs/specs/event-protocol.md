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
- `recorded_at`: daemon timestamp, never client supplied.
- `session_id`: conversational continuity boundary.
- `task_id`: durable work boundary.
- `actor`: user, model, capability, policy, scheduler, or system.
- `kind`: open namespaced string; unknown kinds remain storable.
- `payload`: kind-specific JSON.
- `causation_id`: event that directly caused this event.
- `correlation_id`: root operation or request.
- `span_id`: tracing span when available.

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
