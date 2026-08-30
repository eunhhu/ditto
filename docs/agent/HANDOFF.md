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
  semantic frames. `ditto-model-openai` adds a closed `gpt-5.6` Responses
  profile with deterministic request projection, a fixed-origin
  redirect-disabled reqwest/rustls transport, redacted transport-only
  credentials, bounded SSE decoding and correlation, exact model/storage/
  continuation checks, optional-versus-required usage handling, and explicit
  ephemeral or provider-managed remote response state. No kernel turn loop
  exists yet, and provider completion is not task completion.

## Latest verified slice

- Task 002 is complete under ADR 0008. Public adapter types include
  `OpenAiResponsesDriver`, `OpenAiRetryPolicy`, `OpenAiStoragePolicy`,
  `OpenAiApiKey`, `OpenAiConfigError`, `OpenAiTransportConfig`,
  `OpenAiReqwestTransport`, `OpenAiTransport`, `OpenAiHttpRequest`,
  `OpenAiHttpResponse`, `OpenAiTransportError`,
  `OpenAiTransportErrorKind`, and `OpenAiTransportFuture`.
- The production constructor sends only to
  `https://api.openai.com/v1/responses`; CI uses the credential-free injected
  transport. Ephemeral mode sends `store: false`. Provider-managed mode sends
  `store: true` and alone advertises and emits exact
  `openai/responses-previous-response-id-v1` continuation state. No live or
  billable provider request was run.
- Exact request fixtures cover stable system and context epistemic projection,
  complete ordered tool schemas, deterministic provider-name mapping, tool
  choice, structured output, prompt-cache controls, and continuation suffix
  rejection. Stream fixtures cover split-at-every-byte text, interleaved tool
  arguments, final item correlation, usage, finish reasons, provider errors,
  unknown and post-terminal events, bounded active and historical state,
  cancellation/deadline during handshake, body, and backoff, and the
  pre-response-only retry cutoff.
- Security and boundary regressions include
  `production_http_errors_scrub_exact_and_masked_credentials_everywhere`,
  `in_band_provider_failures_scrub_exact_and_masked_credential_tokens`,
  `terminal_usage_object_null_and_omission_follow_request_requirement`,
  `terminal_status_is_optional_but_present_contradictions_fail_closed`,
  `response_profile_storage_and_previous_id_are_correlated_before_completion`,
  `total_sequential_output_history_accepts_n_and_rejects_n_plus_one`, and
  `post_terminal_failure_preserves_chunk_independent_prefix_and_allows_done`.
- `cargo test -p ditto-model-openai --all-targets` passed 43 tests; the package
  suite including its credential non-serialization doctest passed 44.
  `./scripts/agent-check.sh` passed 140 workspace tests, and
  `cargo +1.88.0 check --locked --workspace --all-targets` passed against the
  repository MSRV. Independent code review and manual QA reported no blockers.

## Intentionally deferred

- kernel provider selection and the model/tool continuation loop;
- additional providers, OpenAI model profiles, reasoning replay, remote cancel,
  and explicit prompt-cache breakpoints;
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
