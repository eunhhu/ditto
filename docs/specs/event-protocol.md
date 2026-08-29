# Event Protocol

## Source of truth

Events are immutable and ordered by a daemon-local monotonically increasing `seq`. `event_id` is globally unique. Projections may be deleted and rebuilt; events may not be silently rewritten.

## Envelope

```json
{
  "seq": 42,
  "event_id": "01J...",
  "recorded_at": "2026-08-29T10:00:00.000Z",
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
- `recorded_at`: daemon timestamp, not a client-supplied timestamp.
- `session_id`: conversational continuity boundary.
- `task_id`: durable work boundary.
- `actor`: user, model, capability, policy, scheduler, or system.
- `kind`: open namespaced string; unknown kinds must remain storable.
- `payload`: kind-specific JSON.
- `causation_id`: event that directly caused this event.
- `correlation_id`: root operation or request.
- `span_id`: tracing span when available.

## Streaming

`GET /v1/stream?after_seq=N` first replays matching durable events and then follows newly appended events. SSE `id` equals `seq`; SSE `event` equals the event kind. A lagged subscriber replays from durable storage rather than silently dropping events.

## Evolution

Payload schemas are versioned by event kind when incompatible evolution is unavoidable. Envelope fields are additive. Consumers must ignore unknown payload fields and preserve unknown event kinds.
