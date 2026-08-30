# ADR 0007: Provider-neutral model request and stream IR

## Status

Accepted.

## Context

Ditto needs a reusable boundary between the semantic kernel and frontier-model
providers. Flattening provider streams into assistant text would discard tool
call identity and partial arguments, usage, structured output, finish semantics,
and continuation state. Passing provider SDK objects through the kernel would
instead make providers, credentials, and transport lifecycle part of the core
architecture.

The contract was compared with two representative frontier-provider shapes:
OpenAI Responses streams use typed, sequenced events for output items, text, and
function-call argument deltas, while Anthropic Messages streams use ordered
content-block lifecycles with partial JSON tool input and cumulative usage. Both
require tool arguments to be accumulated before parsing, but their raw event
taxonomies and usage timing differ.

- <https://platform.openai.com/docs/api-reference/responses-streaming>
- <https://platform.claude.com/docs/en/build-with-claude/streaming>

## Decision

Create a provider-SDK-free `ditto-model` crate that owns version 1 of the model
request, driver, and stream-event contract. The serialized IR is an internal,
replayable boundary, not a public daemon mutation API and not a durable task
completion protocol.

Requests separate an ordered stable system prefix from volatile turn data. Turn
data contains ordered conversation items, a model-facing context capsule, an
execution-epoch ID, an append-only ordered set of full capability input/output
schemas, required and preferred output features, typed generation controls, an
optional output constraint, and cancellation/deadline metadata. Generation
controls preserve reasoning mode, effort, summary disclosure and replay state;
prompt-cache policy, namespace, and exact TTL; and tool choice plus parallel-call
policy. Explicit controls are requirements and are never silently downgraded.
Tool results and assistant tool calls reference the provider call ID that
originated in the same execution epoch. Request validation rejects duplicate or
orphaned call IDs, results before calls, unresolved calls, and calls whose
capability schema is absent from that epoch. Credentials are not representable
in request control metadata.

The context crate owns a compact projection from compiled context to a
model-facing capsule; compiler receipts, lenses, and supersession metadata remain
outside the model request. Every serialized capsule field is included in the
compiler's locally derived token estimate, and a deserialized request revalidates
provenance, epistemic status, confidence, validity, and the absolute budget
before provider use. The capability crate owns provider-neutral level-2
input/output schema records and recursively checks the shape of recognized JSON
Schema keywords. Level-2 schemas use JSON Schema Draft 2020-12: an omitted
`$schema` is interpreted as that dialect, an explicit `$schema` must name the
canonical Draft 2020-12 URI, and other declared dialects are rejected rather
than partially interpreted. Unknown Draft 2020-12 extension keywords are
retained but not assigned legacy-dialect validation rules. The model crate
orders those schemas for one request but does not become a second capability
catalogue.

The stream is pull-based and backpressured. Drivers provide raw semantic events
to a concrete validated stream wrapper; only that wrapper assigns versioned
envelopes and sequence numbers. It enforces lifecycle ordering and exactly one
semantic terminal event:

- `completed` means the provider reached a semantic finish reason and may carry
  continuation state;
- `failed` represents provider, transport, protocol, malformed-argument,
  unsupported-feature, cancellation, or deadline termination;
- transport closure without either terminal state is a protocol failure;
- no event is valid after a terminal event.

Tool call IDs are non-empty, bounded, and unique within a stream. A call must be
started before argument deltas, and it becomes ready exactly once only after the
accumulated JSON parses successfully. The ready capability and arguments must
exactly match the started capability and parsed accumulated deltas; a driver
cannot substitute an unchecked ready payload. Interleaved calls remain
distinguishable by ID. Prose is never parsed to manufacture a tool call.

Reasoning is also a typed item lifecycle rather than provider prose. Summary
segments and provider reasoning content remain distinct, and a ready item must
exactly match the deltas accumulated for its segment keys. Signed or encrypted
reasoning state needed for replay is provider- and format-namespaced, bounded to
64 KiB, debug-redacted, and carried in ordered conversation history. A reasoning
item must start before deltas, become ready at most once, and cannot remain open
at successful completion.

Usage updates state whether they are deltas or cumulative snapshots and retain
canonical input, output, cached-input, and reasoning counters plus bounded named
numeric details. Common finish reasons are typed; an unknown provider reason is
preserved as bounded text. Provider raw transport frames are not retained merely
for debugging. Semantics required to continue a response may be retained only
inside a provider- and format-namespaced opaque continuation value limited to 64
KiB of serialized JSON and 32 levels of nesting. Its debug representation is
redacted.

Driver descriptors separate request capabilities from observable stream
features. Request capabilities state the exact reasoning, cache, tool-choice,
parallel-call, replay-item, replay-state, and continuation formats a configured
adapter can honor. Incoming opaque reasoning or continuation state is matched by
provider and format; support is not inferred from the adapter's ability to emit
later state. Required unsupported stream features and unsupported explicit
controls fail before provider output; preferred unsupported stream features may
be ignored. The deterministic fixture driver has no request capabilities
because it replays semantic frames rather than compiling provider requests. It
derives observable features only from reachable, semantically complete fixture
lifecycles, so a fixture cannot claim unexercised support.

Current OpenAI Responses and Anthropic Messages shapes are represented by
distinct test-only source fixtures before normalization. Their different item
and content-block lifecycles exercise call-ID correlation, partial JSON,
cumulative usage, reasoning summaries, signed or encrypted replay state, and
terminal mapping without introducing either provider SDK into this crate.

Provider completion is never translated into `task.completed`. Completion
verification remains owned by a later kernel verifier boundary.

## Rejected alternatives

- A text-only stream was rejected because it destroys structured call identity,
  partial-argument failure semantics, usage, and continuation.
- A universal `serde_json::Value` event envelope was rejected because missing or
  misspelled fields could silently erase control and lifecycle checks.
- Provider SDK types in the kernel were rejected because they couple core state,
  cancellation, and replay to one transport and risk credential leakage.
- Raw provider events as the canonical IR were rejected because provider event
  taxonomies are not stable across vendors and cannot express one enforceable
  terminal/tool-call state machine.
- Treating stream closure as completion was rejected because it cannot
  distinguish success from cancellation, truncation, or transport failure and
  is never task evidence.

## Compatibility and migration

Version 1 is additive: no model driver or persisted model stream exists on
`main`, so there is no data migration. Serialized requests and event envelopes
carry an explicit IR version and reject unsupported versions at validation.
Future incompatible changes require a new version and replay conversion rather
than changing version 1 in place.

The capability schema and context capsule additions do not replace existing
cards, execution epochs, compiled context, or receipts. They are projections for
the model boundary.

## Measurable consequences and rollback

The crate must prove ordered text delivery, interleaved stable tool-call
lifecycle, typed malformed-JSON failure, every event variant, usage semantics,
continuation and reasoning-state round trips, unsupported-feature and
unsupported-control rejection, deadline and cancellation termination,
opaque-state limits/redaction, compact fully accounted context projection,
structural tool-schema validation, distinct OpenAI/Anthropic shape mappings,
and absence of a task completion variant. The repository gate remains the
release criterion.

Until a production adapter or persisted model stream depends on version 1,
rollback consists of removing the new crate and its additive projections. Once
persisted streams exist, rollback must retain a version-1 reader or migrate
those records explicitly.
