# ADR 0009: Durable read-only artifact turn loop

## Status

Accepted.

## Context

Task 003 needs the first kernel-owned model continuation loop. The model must
receive the complete `artifact.read` level-2 schema, issue a structured call,
receive a bounded result, and continue in the same execution epoch. The current
repository already owns context compilation, capability cards and epochs,
validated model streams, an append-only event spine, and verified
content-addressed artifact reads, but it does not compose them into one turn.

Several boundaries must be explicit before that composition exists:

- capability manifests currently own level-1 discovery metadata but do not
  contain level-2 JSON Schemas;
- ADR 0005 requires leases for canonical capability invocations, while this
  task deliberately calls for a policy-free read authority;
- provider stream completion is not task verification, and no durable turn
  terminal currently represents an assistant response whose task status is
  still unverified;
- model stream call IDs are validated within one request, but a continuation
  loop must also reject duplicate IDs across requests;
- replay needs enough durable, versioned data to reconstruct the complete turn
  without calling the provider or reading the artifact again.

## Decision

### Ownership and integration boundary

Add a narrow `ditto-artifact-read` crate for the builtin capability contract.
It owns the exact versioned input/output schemas, strict argument
normalization, canonical artifact resource, bounded binary-safe result
projection, stable redacted error projection, and the only storage operations
that the builtin may perform. Its capability ID and version must exactly match
the installed level-1 manifest before the kernel pages the schema.

The semantic kernel owns the turn state machine and durable event ordering. It
accepts the existing typed `SubmitInputCommand`, trusted non-deserializable
context candidates, an injected provider-neutral `ModelDriver`, and a
cancellation token. This is the production composition seam for Task 003; the
daemon does not select a provider or turn `POST /v1/commands/input` into an
automatic paid model request in this slice.

The kernel compiles one context capsule and creates one bounded execution epoch
containing only the installed `artifact.read` card. Every model request in the
turn carries the same epoch ID, the same ordered complete schema, and the
complete correlated conversation. Provider-managed response-ID continuation is
not used; tool continuation replays the provider-neutral conversation as
allowed by ADR 0008.

Selection also records the exact validated level-1 manifest. The card and
level-2 schema are projections of that manifest and builtin contract; replay
must reject a trace if any of the three disagree.

### Narrow policy-free read authority

This ADR amends ADR 0005 for exactly one builtin. `artifact.read` does not
require a policy lease because its authority is structurally bounded before
execution:

- the resource is a canonical `artifact:sha256:<lowercase-hex>` reference, not
  a path;
- an `artifact.created` event must root that exact reference in the current
  scope before it is readable by the turn: session IDs always match, and when
  the artifact event has a task ID it must also equal the turn task ID; a broad
  session match never overrides a different artifact task ID;
- offset and length are typed non-negative integers, length is at least one and
  at most 16 KiB, offset at end-of-object returns an empty successful range,
  offset beyond the object is rejected, and a range crossing EOF is truncated;
- the implementation can only read metadata and verified bytes from the local
  content-addressed store; it has no arbitrary filesystem, process, network,
  credential, mutation, approval, or secret-handle surface.

This bounded authority is not a precedent for general content reads. A future
reader with paths, devices, network access, credentials, or mutation must use
the canonical effect-and-lease path from ADR 0005.

### Tool and turn semantics

The input schema is Draft 2020-12 and accepts exactly `reference`, `offset`, and
`length`. Unknown fields, missing or incorrectly typed values, malformed
references, negative integers, zero length, and length above 16 KiB fail before
storage access. Results are deterministic structured JSON. Successful results
include the canonical reference, requested offset and length, returned and total
byte counts, EOF status, and base64 bytes. Capability validation, authorization,
range, availability, and integrity failures become bounded
`ToolResult { is_error: true }` values so the model can explain or correct them;
they are never represented as successful reads. Cancellation is a turn failure
and never becomes a tool result after the cancellation checkpoint.

The loop forbids parallel calls and accepts at most one ready call per model
request. `Completed(ToolCalls)` requires exactly one ready call in that request;
a ready call followed by any other finish reason, or `ToolCalls` without a
ready call, is a protocol failure before execution or final presentation. It
permits at most eight model requests, so at most seven tool results can precede
a required final model terminal. A request may emit at most 4,096 semantic
events, and the turn may accumulate at most 256 KiB of assistant text. The
kernel-only, non-deserializable `ReadOnlyTurnControl` may supply an earlier
absolute deadline alongside the existing `SubmitInputCommand`; the public
command and record-only daemon endpoint do not gain a deadline field. Otherwise
the kernel sets a hard deadline five minutes after accepting the input. A
caller deadline can shorten but never extend that ceiling. The one absolute
deadline is propagated to every model request and enforced while awaiting model
output and at every tool checkpoint. Call IDs are unique across the whole execution
epoch. Unknown capabilities, unknown call/result correlations, duplicate call
IDs, changed epoch/schema data, malformed provider lifecycles, or exhausting a
bound fail the turn without executing an uncorrelated call.

Every durability-admitted model-output payload is at most 320 KiB after JSON
serialization, and all model-output payloads for one request total at most
4 MiB. These journal bounds are intentionally tighter than the model IR's
1 MiB accumulated tool-argument bound. A candidate that would exceed an event,
request, or assistant-text bound is not appended as `model.output`; the kernel
instead appends an adjacent, bounded `turn.failed` record. Turn failure messages
are at most 4 KiB and are truncated on a UTF-8 boundary with an explicit marker.
Each admitted output payload also carries an integer-millisecond `admitted_at`
timestamp captured only after semantic validation and bound checks. Event-store
append timestamps are canonicalized to the same durable millisecond precision,
so returned, published, reopened, and replayed event envelopes agree exactly.
Reasoning stream events are outside this read-loop contract: an otherwise valid,
bounded reasoning event is recorded and followed by an exact protocol failure,
rather than being silently ignored or treated as assistant text.

Cancellation and the one absolute deadline are checked before and after each
external-await or capability boundary: before the first model request, after a
durable `model.requested` and before driver I/O, while awaiting the model, before
admitting each validated model output, after final output validation and before
its durable append, before a final `turn.finished`, before
`capability.requested`, after that durable request and before
`execution.started`, after the durable execution start and before the read, and
after the read and before its result is recorded. A checkpoint failure is
durable and stage-specific; replay accepts only the corresponding code, message,
request/call correlation, and (for deadlines) a timestamp at or after the
recorded deadline.

Every harness-enforced deadline failure carries the effective absolute deadline
as typed integer-millisecond evidence, including failures before the first model
request. Replay requires the terminal event time to be at or after that deadline
and rejects a missing or extended value. A provider-reported deadline is a model
failure, not a claim that the kernel's own absolute deadline elapsed.

The validated model stream owns semantic validation, lifecycle checks, and
sequence assignment. Raw provider EOF is converted there into a valid terminal
`ModelEvent::Failed`, which is admitted as `model.output` before `turn.failed`.
The kernel does not invent an output-less protocol terminal; impossible
post-terminal stream exhaustion is an internal error whose durable prefix
replays as truncated.

A provider `Completed` event with a non-tool finish reason ends the model turn.
The kernel returns and durably records the final assistant response with task
status `unverified`. It never emits `task.completed`; a later task-specific
verifier is required to change task completion state. Before creating a turn,
the kernel rejects a target task that already has `task.completed`, without
appending a new input or turn event.

### Durable transitions and replay

Add versioned internal event payloads for model requests, validated model
outputs, turn completion, and turn failure while reusing the existing
`context.compiled`, `capabilities.selected`, `capability.requested`,
`execution.started`, and `execution.output` kinds. The kernel assigns actors,
kinds, request IDs, epoch IDs, turn correlation, and causation. It appends each
transition before publishing or returning it.

The durable sequence records the compiled context and receipt, selected
manifest, epoch, and exact schema, every complete `ModelRequest`, every
durability-admitted `ModelStreamEvent`, normalized call and canonical resource,
deterministic tool result, and the unverified final outcome. A replay projector
validates versions, ordering, request and stream sequences, same-manifest,
epoch, and schema continuity, call-ID correlation, tool-result insertion,
terminal state, all bounds, output-admission temporal evidence, and the absence
of `task.completed`. Replay
reconstructs the turn without provider I/O or another artifact read and fails
closed on truncation or inconsistent payloads.

Replay consumes an ordered snapshot for exactly one session plus an explicit
turn ID. The snapshot may include taskless provenance roots and unrelated turns,
but it may not mix sessions. The projector selects the correlated turn while
also rejecting any `task.completed` event for the target task anywhere in the
snapshot. It returns the compiled context, selected capability evidence, exact
requests and outputs, requested/started/output tool-call transcript, terminal,
and sequence span rather than only the terminal outcome.

Because these records are visible through the event and stream APIs, their
version-1 kind/actor mapping is fixed:

- `input.received` / `user`;
- `context.compiled` / `system`;
- `capabilities.selected` / `system`;
- `model.requested` / `system`;
- `model.output` / `model`;
- `capability.requested` / `model`;
- `execution.started` and `execution.output` / `capability`;
- `turn.finished` and `turn.failed` / `system`.

The kernel crate owns the typed payload structs because it enforces and replays
their state machine; the protocol crate owns the stable event-kind constants
and generic envelope. The existing `input.received` payload remains the
unversioned `{ "text": ... }` public-ingress record and may also exist outside a
turn. A loop-created input event is linked by the kernel-assigned turn
correlation ID and is the causation root of the first versioned turn event; it
does not change the public command wire shape. Every subsequent turn payload
carries `event_version = 1` and `turn_id`. Context payloads carry the compiled
nodes, receipt, exact capsule, and captured provenance high-water sequence;
capability-selection payloads carry the exact manifest, serialized epoch, and
ordered full schemas; model-request payloads carry round number and complete request;
model-output payloads carry round, request ID, complete stream event, and the
integer-millisecond semantic-admission timestamp;
capability/execution payloads carry round, call ID, capability ID, normalized
arguments or canonical resource, and the complete structured result; terminal
payloads carry bounded response or stable failure, request/tool counts, and
task status `unverified`. Deadline terminals additionally carry the typed
effective-deadline evidence needed for temporal replay. Additive unknown payload
fields remain ignorable as
required by the event protocol, but missing or contradictory required fields
fail replay.

Persisting compiled context makes even a turn-scoped node durable. Therefore
every included node must have at least one source event ID, and every named
source must resolve to an existing trusted event in the same session/task
scope at or before the captured provenance high-water sequence before
`context.compiled` is appended. The loop rejects rather than durably recording
a provenance-free capsule. The kernel performs the same check again at the
durable `model.requested` timestamp so an expiring node cannot cross the model
I/O boundary unnoticed.

Likewise, `execution.started` captures an authorization high-water sequence.
An artifact root is canonical only when an `artifact.created` event with actor
`system`, the exact session, compatible task scope, and sequence at or before
that cutoff exists. Later roots cannot retroactively authorize a recorded read,
and an unauthorized result cannot contradict a valid root at the same cutoff.

## Rejected alternatives

- Wiring the daemon directly to the OpenAI adapter was rejected because provider
  selection, credential configuration, paid execution, and command scheduling
  are separate public and external boundaries.
- Treating the artifact reference as an arbitrary store-wide bearer token was
  rejected because a model could guess or recover a hash from another scope.
- Adding paths or a generic file reader was rejected because it would create
  ambient filesystem authority and require the general lease path.
- Reusing only capability cards was rejected because invocation requires the
  complete level-2 contract.
- Storing only the final response was rejected because it cannot prove the
  request/schema, call/result correlation, continuation epoch, or durable-before-
  presentation ordering.
- Re-executing tools during replay was rejected because replay must not repeat
  effects or depend on mutable runtime state.
- Emitting `task.completed` with `verified: false` was rejected because the event
  kind itself denotes a completion claim and would weaken the trusted event
  boundary.

## Compatibility and migration

The durable turn protocol is additive. No durable model turn events exist on
`main`, so there is no event-data migration. New payloads carry an explicit
version and old readers already preserve unknown event kinds and fields. The
existing typed input endpoint remains record-only.

The preceding artifact-read foundation did expose a `ditto-artifact-read` 0.1.0
Rust API and level-2 schema source. Task 003 retains its safe constructors,
aliases, normalization helpers, and execution wrappers, but direct public-field
construction cannot coexist with strict invariant-preserving result and resource
types. The Rust package therefore advances to 0.2.0 and exposes getters for those
fields. This package API decision does not change the serialized builtin
capability contract: `artifact.read` remains capability version 0.1.0 with the
same valid Draft 2020-12 wire schema; the hardening rejects states that were
already outside that schema. Existing Rust callers must replace direct struct
literals with the retained constructors, while valid durable capability records
are not silently reinterpreted.

Changing the artifact schema, result encoding, event payload contract, scope
authorization rule, or interpretation of the unverified terminal requires a
new version rather than silently reinterpreting recorded turns.

## Measurable consequences and rollback

Tests must prove full-schema page-in, same-epoch continuation, exact bounded
range projection, malformed/negative/excessive argument rejection before read,
scope authorization, tamper detection, error-result continuation, duplicate and
unknown call rejection, cancellation between durable request and result, hard
round/event/text bounds, durable-before-publication ordering, final unverified
status with no `task.completed`, and complete replay after reopening storage.
Replay must reject missing, reordered, duplicated, malformed, or inconsistent
events.

Rollback removes the additive turn module, builtin crate, new event constants,
and this ADR. Already-recorded version-1 events must remain readable or be
explicitly migrated once any deployment persists them.
