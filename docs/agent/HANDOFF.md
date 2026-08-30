# Verified handoff

## Canonical state

- Branch: `main`.
- Runtime: Rust daemon and CLI.
- Durable stores: SQLite event spine plus local SHA-256 artifact objects.
- Public mutation ingress: typed user-input command only; arbitrary event append
  is not a public route.
- Streaming design: subscribe-first, high-water-bounded, paginated durable
  replay with sequence-gap and lag recovery.
- Capability state: file-backed manifests, validated complements, strict runtime
  hard filters, append-only bounded execution epochs, and validated
  provider-neutral level-2 input/output schema records. Recognized schema
  keywords are checked recursively against JSON Schema Draft 2020-12 while
  unknown extension keywords remain opaque.
- Context state: typed provenance graph and deterministic compiler. Pinning and
  policy-required inclusion are trusted ephemeral directives; token cost is
  derived locally. A compact model-facing capsule projects ordered selected
  nodes without exposing the compiler receipt, lens, or supersession metadata;
  its exact serialized fields are charged locally and revalidated for trust,
  time, and the absolute budget at the model boundary.
- Policy state: leases authorize canonical invocations against orthogonal effect
  dimensions. No executor is connected yet.
- Model state: `ditto-model` owns version 1 of the provider-neutral request,
  driver, and backpressured stream-event contract. It preserves ordered stable
  prefix/volatile turn data, structured tool-call lifecycles, final structured
  output, delta-or-cumulative usage, warnings, finish reasons, bounded redacted
  continuation and reasoning replay state, typed generation controls, deadline,
  and cancellation. A concrete validated stream owns sequence, terminal,
  tool-argument, and reasoning-item lifecycle checks. Driver descriptors keep
  exact request capabilities separate from emitted features, and the
  deterministic fixture driver derives emitted features only from reachable
  semantic frames. No production provider adapter or turn loop exists yet, and
  provider completion is not task completion.

## Latest verified slice

- Task 001 is complete under ADR 0007. Public types include `ModelRequest`,
  `ModelTurn`, `StableSystemPrefix`, `ConversationItem`, `ModelEvent`,
  `ModelStreamEvent`, `ModelEventStream`, `ModelDriver`, `DriverDescriptor`,
  `FeatureRequest`, `GenerationControls`, `RequestCapabilities`,
  `ProviderStateFormat`, `ReasoningItem`, `OpaqueReasoningState`,
  `FixtureDriver`, `CancellationToken`, `ToolCallBuffer`, `ContinuationState`,
  `TokenUsage`, `CapabilitySchema`, `ContextCapsule`, and
  `ContextCapsuleItem`.
- Distinct current OpenAI Responses and Anthropic Messages source-shape
  fixtures were normalized through the IR before any production adapter work.
  They exercise different item/block lifecycles, partial tool JSON, reasoning
  state, usage, warnings, continuation, and terminal mapping.
- `./scripts/agent-check.sh` passed with 56 `ditto-model` tests and 96 workspace
  tests. Exact contract tests include
  `text_only_fixture_emits_ordered_deltas_and_completion`,
  `tool_fixture_emits_stable_started_deltas_and_ready`,
  `malformed_tool_arguments_emit_typed_failure`,
  `cancellation_terminates_without_provider_completion`,
  `usage_and_continuation_survive_serialization_round_trips`,
  `fixture_features_are_derived_from_emitted_frames`,
  `incoming_continuation_requires_an_exact_provider_format_capability`,
  `conversation_tool_history_accepts_interleaved_resolved_calls`,
  `wrapper_assigns_sequences_to_a_valid_tool_lifecycle`,
  `raw_ready_arguments_must_equal_the_accumulated_json`,
  `openai_responses_source_shape_preserves_item_call_reasoning_usage_and_continuation`,
  and
  `anthropic_messages_source_shape_preserves_indexed_blocks_thinking_signature_partial_json_usage_warning_and_stop`.
- `cargo +1.88.0 check --locked --workspace --all-targets` passed against the
  repository MSRV.

## Intentionally deferred

- production provider adapters and model turn loop;
- capability worker protocol and lifecycle;
- device registry, local process runner, SSH transport, and secrets;
- persistent context projections and embeddings;
- completion verifiers and improvement compiler;
- authenticated remote gateway and web inspector.

## Known engineering debt

- SQLite calls are synchronous and will need a measured async boundary before
  high-concurrency gateways.
- Artifact range reads verify the whole object for integrity; optimize only with
  a design that preserves immutable-object trust.
- Context graph edges are validated but not yet used in ranking.
- Search remains lexical until the embedding worker slice.

Update this file only after code and checks establish a new fact.
