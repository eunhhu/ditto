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
and model-driven memory housekeeping remain deferred. [ADR 0015](../adr/0015-explicit-user-memory.md)
owns this command and retry contract.

## Explicit agent runs

`POST /v1/commands/run` accepts `request_id`, `session_id`, `text`, and the
optional bounded `sort` permission described below.
The request ID is a canonical uppercase ULID; session IDs are canonical and
text is at most 16 KiB UTF-8 before existing whitespace normalization. The
kernel derives task ID `run_<request_id>` and creates a fresh turn correlation.
Without a sort attachment the durable input carries additive
`agent_run: {version: 1, request_id: ...}` metadata fixed by the kernel, never
copied from client event fields.

Acceptance precedes dispatch and returns HTTP 202 with `running`. Identical
normalized text with the same session/request ID returns the existing run,
including terminal or interrupted runs; changed text, attachment or permission
is HTTP 409. One active
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

## Explicit local sort requests

Loopback listeners expose `POST /v1/commands/sort` with only `request_id`
(uppercase canonical ULID), `session_id`, `text` and required boolean `unique`.
Text is at most 64 KiB / 4096 UTF-8 LF-delimited lines and contains no NUL.
Content and whitespace are preserved before hashing; changing the final LF or
unique option with the same ID conflicts. Unknown fields are rejected.
`GET /v1/sorts` and `POST /v1/commands/sort/cancel` take only the two identity
fields. The provider may remain disabled. No program, environment, path,
effect, lease, actor or evidence is client-selectable.

The kernel derives `sort_<request_id>` task scope and fresh `turn_*` correlation,
shares the existing model-run slot, stores the input artifact, then durably
accepts `sort.requested` (user, version 1) before dispatch. This event contains
the request ID, input reference and unique option, not the source text or local
filename. Exact retries return prior state, changed retries conflict, busy work
is not queued, and owner loss projects interrupted state without reexecution.

`sort.started` (system, version 1) records the dispatch attempt after a canonical
invocation, one-call expiring lease and one-shot claim exist. It records exact
revision, epoch, effect/resources, expiry and invocation/claim identity before
spawning; its presence alone is not proof of a successful process start.
Verified output gets an `artifact.created` root. A single `task.completed`
(system, version 1) certifies only this sort contract: `capability_id`, `verifier`,
input/output references, unique mode, input/output line counts, zero exit code,
start-event ID and matching invocation/claim identity. Its cause is the result
artifact event; that artifact's cause is the dispatch event. Only the sealed
verifier output reaches this producer. Failure appends `sort.failed` (system,
version 1) with a closed, path-free error code. Storage failure may leave an
interrupted request rather than inventing a terminal.

Responses report `running`, `verified`, `failed` or `interrupted`, identities,
cancellation intent, and optional output/reference or failure code. Inspection
uses indexed boundaries and exact evidence/root lookups, hashes both bounded
artifacts and reruns the line verifier before returning verified output. It
never starts a process. Corrupted or contradictory evidence returns storage
failure. Cancellation and completion use the shared slot gate to resolve their
race; cancellation after completion returns that completion.

HTTP statuses are 202 for running admission, 200 for prior/terminal results,
400 invalid input, 404 absent/wrong scope, 409 changed retry, 422 unknown fields,
429 occupied slot, 503 shutdown and 500 unavailable storage. Non-loopback
listeners do not mount these routes. See [ADR 0017](../adr/0017-bounded-local-sort.md).

## One-shot scheduled requests

Loopback-only `POST /v1/commands/schedule` accepts exactly `request_id` (canonical
uppercase ULID), `session_id`, `text` (16 KiB), `due_at`, and `expires_at`. The
RFC 3339 times require explicit offsets and millisecond precision; the kernel
normalizes them to UTC. New due times must be future instants within 365 days,
with an exclusive latest-start time at most 24 hours later. Exact normalized
retries return the original schedule even after expiry. Changed retries return
409; the shared global limit of 100 pending one-shots plus active repeats returns
429. Unknown authority, provider,
process-grant, recurrence or internal identity fields are rejected.

`GET /v1/schedules` and `POST /v1/commands/schedule/cancel` use the existing
session/request query shape. `GET /v1/schedules/pending?session_id=personal`
returns every pending request in the session, bounded by the global cap. Status
contains original times, `pending|running|unverified|failed|interrupted|cancelled|missed`,
durable `cancellation_requested`, optional `waiting_for` (scheduler stopped,
provider disabled, due time, busy runtime or dispatch), and the original `run`
result after admission. The CLI prints failed/interrupted/missed JSON and exits
unsuccessfully. It does not wait for a future schedule or poll in the background.

These one-shot events have payload version 1, task `schedule_<request_id>`, the
user's session, and no correlation/span. The kernel constructs actor and kind:

| Event | Actor | Payload and cause |
| --- | --- | --- |
| `schedule.requested` | user | Original normalized `command` and reserved `run_request_id`; no cause |
| `schedule.claimed` | scheduler | Version only; caused by the request |
| `schedule.missed` | scheduler | Version only; caused by the request |
| `schedule.cancelled` | user | Version only; caused by the pending request |
| `schedule.cancel_requested` | user | Version only; caused by the claim |

Each transition and its derived schedule index update commit atomically. Under
the shared run gate, claim precedes durable input admission and model dispatch.
The original request authorizes the later copied user text; its reserved run ID
links the ordinary version-1 run evidence back to the schedule. Public manual
admission cannot use a reserved ID. Scheduled runs receive no sort grant and
retain current source-verified session context and read-only turn semantics.

Pending cancellation prevents future dispatch. Active cancellation is persisted
before signalling the run token. A crash after claim but before input admission
is `interrupted` with no run result; no claim is retried automatically. A disabled
provider can accept future intent but only expires it unless the operator later
enables the provider. The enabled daemon can dispatch pending due work on startup.
See [ADR 0019](../adr/0019-one-shot-scheduled-runs.md) for clock-change, power-loss,
startup replay and single-owner limitations.

## Finite recurring requests

Loopback-only `POST /v1/commands/repeat` accepts the same five one-shot command
fields plus required `every_seconds` and `occurrences`. Unknown fields are
rejected, including authority, provider, internal progress and sort grants.
Intervals are whole seconds from 60 through 2678400 (31 days); counts are 2
through 1000. The first due instant must be future and the final due instant
within 365 days of admission. Canonical millisecond start windows are positive,
no longer than the interval or 24 hours, and never overlap. Occurrence `n` is due
at `due_at + (n - 1) * every_seconds`, with the same anchored expiry. The fixed
miss policy skips expired ordinals in one range and runs at most the one current
eligible ordinal. Calendar time zones, indefinite repeats and automatic retry
policies are not accepted inputs.

`GET /v1/repeats` and `POST /v1/commands/repeat/cancel` use the existing
session/request query. `GET /v1/repeats/active?session_id=personal` returns compact
active headers without expanding run output. Responses contain original times,
interval/count, `active|exhausted|cancelled`, `claimed_occurrences`,
`missed_occurrences`, optional `next_occurrence`, `next_due_at`, `waiting_for`,
`last_occurrence_id` and (on exact inspection) `last_occurrence`. The latter uses
the existing `ScheduleResponse`; historical children retain their original
schedule IDs. Exhausted means the timetable was consumed, including claims and
misses, and can coexist with a still-running final child. It is not a work
completion result. Repeat CLI exit success means the command/inspection succeeded;
the nested child has the actual work status. Child status CLI retains its existing
failed/interrupted/missed exit behavior.

All repeat event payloads have version 1 and no correlation/span. Roots have
task `repeat_<request_id>`, user actor and no cause. Transitions reference the
original `source_event_id`, are caused by the previous parent transition, and
carry a checkpoint `progress: {next_occurrence, claimed, missed, last_child_id}`.
The event-store transaction verifies the derived checkpoint before commit.
Before using cached parent progress, an exact indexed lookup verifies that no
newer parent transition follows that checkpoint. A unique journal successor
constraint independently rejects duplicate branches after a cache rewind.
The original session/request identity is also unique in the journal, even if
its derived parent row is deleted.

| Event | Actor / task | Additional payload |
| --- | --- | --- |
| `schedule.repeat.requested` | user / parent | Original normalized `command` |
| `schedule.repeat.skipped` | scheduler / parent | `observed_at_ms`, inclusive `from_occurrence` and `through_occurrence` |
| `schedule.occurrence.claimed` | scheduler / `schedule_<child_id>` | `parent_request_id`, `occurrence`, private `run_request_id`, `observed_at_ms` |
| `schedule.repeat.cancelled` | user / parent | No extra fields beyond version, source and checkpoint |

An occurrence claim is also its child schedule's immutable source and claimed
state. It references the original repeat command instead of copying its prompt.
Parent advancement, child creation and permanent run reservation share one event
transaction. Dispatch follows that commit. A crash in the gap leaves that child
interrupted and never rearms it; later distinct ordinals remain authorized.
Parent cancellation commits before signalling its active child and prevents
future claims. Child-only cancellation uses `schedule.cancel_requested` caused by
its occurrence claim and leaves the parent active. Identical normalized retries
return the original series, including cancelled/exhausted state. Changed retries
return 409; new work beyond shared capacity returns 429. Schema-6 migration and
rebuild preserve schema-5 one-shot events. See
[ADR 0020](../adr/0020-bounded-recurring-schedules.md).

## Model-directed sort permission

`POST /v1/commands/run` additionally accepts optional
`sort: {"text": "b\na", "allow_deduplicate": false}`. Both nested fields are
required and unknown fields are rejected. The same 64 KiB / 4096 UTF-8 LF-line,
no-NUL bound applies. The kernel stores this exact input as a task artifact and
records version-2 `agent_run` metadata with its reference, source event ID and
permission bit. Clients cannot choose those internal fields. Without an
attachment, metadata remains version 1 and the sort schema is absent.

CLI `--sort-file FILE` grants one sort of the attachment until the end of this
bounded turn; `--allow-deduplicate` additionally permits duplicate removal.
Normalized request text, exact attachment bytes and permission are retry identity.
The initial user message projects the reference and permission, with no local
path or file body. The existing loop pages the exact read/sort schemas, uses one
canonical-resource lease/affine claim, and continues with a structured result.
Each process retains the existing five-second owned lifetime; the lease expires
with the turn, at most five minutes after admission.

Version-1 `agent.sort.requested` (model), `agent.sort.started` (capability), and
`agent.sort.output` (capability) extend the same strict turn cause chain. They
carry `turn_id`, request index and call ID; spans use the call ID. Requested
evidence includes raw and normalized arguments. Started evidence records the
normalized operation, epoch/digest/permit/claim identities and claim/expiry times.
`claimed` on output means a claim was consumed, not that process spawning was
observed. Output is either a verified reference/verifier/line-count result or a
closed error code. A successful artifact root is system-authored, task-scoped,
caused by that start and outside the turn correlation. Output names its exact
artifact event while retaining the start as its cause.

No model `run_*` task.completed is emitted. The response optionally contains
`sort` with input reference, deduplication permission, state (`not_run`,
`running`, `verified`, `failed`, `interrupted`) and output/reference or failure
code. A rejected attempt may report a failure code while remaining `not_run`.
Later rejected attempts cannot overwrite an already executed result. Status
uses a schema-4 partial index for start/output records with a nine-row corruption
sentinel (one start plus seven outputs are valid). Exact evidence/root lookups,
bounded hashes and an independent line verifier establish verified output,
including after model failure or owner loss. Corruption returns storage failure.

Pure replay adds `sort_calls`, validates permission, selected contracts,
normalization, one-shot dispatch, output roots and exact continuation without
I/O or reconstructed live authority. It checks structural evidence; status also
checks artifact content. Existing turns remain readable, and an old reader must
reject version-2 metadata. See [ADR 0018](../adr/0018-user-scoped-model-sort.md).

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
