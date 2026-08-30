# ADR 0008: First provider adapter through OpenAI Responses

## Status

Accepted.

## Context

Task 002 needs one real frontier-provider adapter behind the provider-neutral
model IR. A test-only source-shape mapper would not establish a production
transport, while placing provider HTTP, credentials, and raw wire types inside
`ditto-model` would weaken the boundary established by ADR 0007.

The current OpenAI Responses API supplies the required semantic surface:
streaming text, function-call item and argument lifecycles, terminal usage,
structured outputs, prompt-cache controls, and response-ID continuation. Its
stream is extensible and its transport can fail before or after a response has
been created, so the adapter needs its own bounded parser and retry cutoff.

The decision was checked against the official current API references:

- <https://developers.openai.com/api/reference/cli/resources/responses/methods/create>
- <https://developers.openai.com/api/docs/guides/streaming-responses>
- <https://developers.openai.com/api/docs/guides/prompt-caching>
- <https://developers.openai.com/api/docs/guides/conversation-state>
- <https://platform.openai.com/docs/api-reference/authentication>

## Decision

Create a `ditto-model-openai` crate. It owns the OpenAI Responses request
projection, raw SSE state machine, retry controller, and production HTTPS
transport. It depends on `ditto-model`; the provider-neutral crate remains free
of provider names, HTTP, authorization headers, and provider wire objects. The
kernel and daemon do not select or invoke this driver in Task 002.

The first configured profile is the documented `gpt-5.6` Responses profile.
The profile is a closed constructor rather than arbitrary caller-supplied
capabilities. Its descriptor advertises only behavior implemented and tested by
this adapter. Adding another model profile requires an explicit capability map
and fixtures; an unknown model string cannot inherit the `gpt-5.6` descriptor.

### Credential and transport boundary

The real transport sends only to `https://api.openai.com/v1/responses`, uses
TLS through the workspace `reqwest`/rustls stack, and disables HTTP redirects so
a bearer credential cannot be redirected to another origin. The API key is held
by a non-serializable type with redacted `Debug` output and enters only the
`Authorization: Bearer` header inside the real transport. Optional organization
and project identifiers are bounded transport configuration, never model IR.
The driver, compiled request, semantic events, failures, mock fixtures, and
snapshot diagnostics contain no authorization material. Before production
transport errors cross that boundary, the exact configured key and bounded
credential-shaped or masked fragments are replaced with a redaction marker.
The same credential-shape scrub is applied to in-band provider failure codes
and messages before they become model events. An arbitrary injected mock is
not claimed to know or scrub a non-OpenAI secret that was never given to it.

The production transport implements the same injected transport trait used by
deterministic CI mocks. A successful transport handshake returns a bounded byte
stream. Dropping the pending request or byte stream is the local cancellation
mechanism. Task 002 does not call the provider's remote cancel endpoint and does
not claim that a locally cancelled request stopped remote computation.

### Retention and continuation

Provider storage is an explicit construction-time policy:

- ephemeral mode sends `store: false`, does not advertise continuation, and
  never emits a response-ID continuation;
- provider-managed mode sends `store: true`, advertises exactly
  `openai/responses-previous-response-id-v1`, and emits a bounded continuation
  whose state is exactly `{ "response_id": "..." }`.

Provider-managed response state is remote provider state, not durable Ditto
state or task evidence. Its retention and availability follow the provider's
contract. The API key and storage policy are harness configuration; a
`ModelRequest` cannot enable provider storage.

When that continuation format is supplied, the request conversation is an
incremental suffix after the provider checkpoint. The stable instructions are
still sent because the API does not carry previous instructions forward. To
avoid duplicating or orphaning provider call history, version 1 rejects
`ToolCall`, `ToolResult`, and opaque reasoning items in continuation suffixes.
Tool-result continuation remains possible without response-ID state by replaying
the complete correlated conversation, which is the path Task 003 may compose.

### Request projection

The adapter serializes a typed request to deterministic JSON bytes and enforces
a compiled-request limit before transport. Ordered arrays are never rebuilt
through unordered maps.

- stable system segments become `instructions` in their original order;
- a nonempty context capsule becomes the first volatile `developer` message,
  prefixed with `DITTO_CONTEXT_V1` and containing the complete compact capsule
  JSON, including origin, epistemic status, scope, confidence, provenance, and
  validity, so inferred context is not represented as a user assertion;
- conversation messages preserve their order and user/assistant role;
- structured content is tagged and rendered as deterministic JSON text;
- correlated tool calls and results become Responses `function_call` and
  `function_call_output` items when no response-ID continuation is active;
- full capability schemas remain in execution-epoch order and compile to
  function tools with input and output schemas;
- structured output uses `text.format.type = "json_schema"` and is parsed into
  one final `StructuredOutput` event;
- `PromptCachePolicy::Disabled` uses explicit mode with no breakpoint, and
  `Automatic` uses implicit mode plus an optional stable `prompt_cache_key`;
  the first profile does not advertise explicit stable-prefix breakpoints;
- tool choice and parallel-call controls map exactly or fail before transport;
- explicit reasoning controls and replay state are not advertised in this
  first slice and therefore fail before transport rather than being dropped.

OpenAI function names use a narrower alphabet and length than Ditto capability
IDs. Already-valid names are preserved. Other IDs receive a deterministic
bounded SHA-256-derived provider name; the adapter retains a per-request reverse
map and rejects a provider call whose name is absent or whose item metadata
changes. Provider call IDs remain the canonical public call identity.

### Stream, terminal, and retry semantics

The SSE decoder accepts arbitrary byte chunking, CRLF, comments, and multiline
`data:` fields. It bounds compiled request bytes, HTTP error bodies, individual
SSE events, unterminated buffers, provider codes/messages, and active item
state. Invalid UTF-8, malformed SSE/JSON, a mismatched `event:`/JSON `type`,
non-monotonic sequence numbers, changed item indexes or identities, final text
or arguments that disagree with accumulated deltas, and EOF before a semantic
terminal produce one typed failure. `[DONE]` is not success without a Responses
terminal event.

Known non-semantic lifecycle events are consumed. Unknown future event types
are ignored after their envelope and sequence are validated; they cannot create
completion or tool calls. Raw frames are not retained for diagnostics.

Every nested Response object must retain the configured `gpt-5.6` model and
`response` object identity. Optional echoed storage and previous-response fields
must agree with the configured policy and request when present. Final message
content, item status, function identity, and arguments are correlated with the
streamed lifecycle. Both currently active state and the total set of seen output
and call identifiers are bounded.

The mapping is:

- `response.output_text.delta` and refusal deltas become `TextDelta`; refusal
  is retained in the terminal `FinishReason`;
- a function-call output item, its argument deltas, and its argument-done event
  become `ToolCallStarted`, `ToolCallArgumentDelta`, and `ToolCallReady`. If the
  provider sends no raw argument delta for an empty/small call, the documented
  final `arguments` string supplies one semantic delta before ready; otherwise
  the final string must exactly match the accumulated raw deltas;
- terminal response usage, when present, becomes one cumulative `UsageUpdate`
  before the terminal event. If `Usage` was required by the request, absent or
  null usage is a protocol failure; otherwise the provider's legal omission is
  accepted without inventing a usage event;
- `response.completed` becomes `Completed(EndTurn)` or
  `Completed(ToolCalls)`, with optional configured continuation;
- `response.incomplete` accepts the documented `max_tokens` and current-schema
  `max_output_tokens` spellings, maps them and content filtering to typed
  completed finish reasons, and preserves other bounded reasons as `Other`;
- `response.failed`, standalone `error`, and a delivered terminal response
  envelope whose nested status is `cancelled` become typed terminal failures.
  No undocumented `response.cancelled` SSE shape is invented; an unknown event
  with that name is nonterminal and eventual closure still fails closed.

The terminal event type supplies the terminal status when the nested optional
`status` field is absent; when that field is present it must agree with the
event, except for the explicitly handled nested `cancelled` status. Mapping a
decoded transport batch preserves every valid nonterminal semantic prefix even
when a later frame fails. A success terminal is withheld until the remainder of
that decoded batch is validated, so a trailing invalid frame cannot produce
both success and failure and transport chunk coalescing cannot erase earlier
deltas. The documented `[DONE]` sentinel may follow a semantic terminal but is
never sufficient on its own.

Every semantic event still passes through `ModelEventStream`, which remains the
sole owner of Ditto sequence numbers and the provider-neutral terminal, tool,
and reasoning lifecycle contract. A provider terminal is never
`task.completed`.

Automatic retry is allowed only when the transport fails before returning a
successful streaming response. Once response headers have been accepted, no
body error, `response.created` event, or later failure is retried. Eligible
connection, timeout, HTTP 408/409/429, and 5xx failures use a bounded attempt
count and bounded exponential delay, honor bounded delta-seconds or HTTP-date
`Retry-After`, stop at the remaining request deadline, and are interruptible by
cancellation. Quota and billing failures are not retried. This deliberately
stricter cutoff avoids duplicating a response when creation is not documented
as idempotent.

## Rejected alternatives

- Provider code inside `ditto-model` was rejected because it would make the
  reusable semantic boundary own HTTP and credentials.
- An OpenAI SDK dependency was rejected because the current workspace HTTP and
  serialization stack is sufficient and direct wire fixtures are required
  anyway.
- A mock-only adapter was rejected because Task 002 requires a production HTTPS
  path.
- Arbitrary base URLs and redirect following were rejected because they make
  bearer-token destination control a caller-controlled footgun.
- Implicit provider storage was rejected because remote retention is an
  external-state policy, not a model decision.
- Retrying after the first accepted response was rejected because it can create
  duplicate output, calls, cost, and remote state.
- Treating stream closure or `[DONE]` as completion was rejected because neither
  is semantic completion evidence.
- Replaying full conversation history together with `previous_response_id` was
  rejected because it silently duplicates the checkpointed prefix.

## Compatibility and migration

This is additive. No provider adapter, configured model, credential record, or
persisted provider stream exists on `main`. `ditto-model` version 1 does not
change. The new exact continuation namespace prevents another adapter from
mistaking an OpenAI response ID for its own state.

Future support for stateless encrypted reasoning replay, explicit cache
breakpoints, another OpenAI model profile, custom endpoints, or remote cancel is
not implied by this version and requires new capability evidence. If response-ID
suffix semantics need to carry provider tool results later, the continuation
format must be versioned rather than reinterpreted.

## Measurable consequences and rollback

CI must use only the injected mock transport and prove exact request bytes,
stable ordering, context epistemic labeling, provider-name round trips,
credential redaction, wrong continuation rejection before I/O, split-at-every-
byte SSE decoding, text and interleaved partial tool calls, structured output,
required-versus-optional usage, finish reasons, continuation, response identity
and storage correlation, final item metadata, bounded active and historical
state, malformed SSE/JSON, provider errors and credential canaries, unknown and
post-terminal events, cancellation/deadline during handshake/body/backoff,
pre-response retry, and the no-retry-after-response cutoff. The real transport
is compiled and covered through its bounded configuration and response
handling; live tests remain absent unless explicitly added and opt-in.

Rollback is removal of `ditto-model-openai`, its workspace entry, and this ADR.
No neutral IR or durable data migration is required.
