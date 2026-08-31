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
  dimensions. No effectful executor is connected yet; the sole executable
  builtin is the structurally bounded, lease-free `artifact.read` exception from
  ADR 0009.
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
  ephemeral or provider-managed remote response state. The kernel now owns an
  injected-driver `artifact.read` continuation loop and pure replay projector;
  provider completion still is not task completion.

## Latest verified slice

- Task 003 is complete under ADR 0009. `DittoKernel::run_artifact_read_turn`
  compiles trusted context, validates its provenance cutoff, pages the exact
  installed manifest/card/full schema into one bounded execution epoch, accepts
  at most one structured call per request, executes a same-scope bounded read,
  and continues the complete provider-neutral conversation to an explicitly
  `unverified` final response. The daemon remains record-only and does not select
  a provider or trigger an automatic paid request.
- `ditto-artifact-read` owns strict arguments, the canonical
  `artifact:sha256:<hex>` resource, a 16 KiB range ceiling, binary-safe
  deterministic projections, stable structured failures, and exact manifest/
  schema validation. Artifact bytes are captured through the same sequential
  descriptor pass that verifies their SHA-256 content. The invariant-safe Rust
  package is version 0.2.0 with checked deprecated 0.1 API wrappers; the valid
  serialized capability contract remains version 0.1.0.
- Version-1 turn events durably record compiled context, selected capability
  evidence, complete model requests and admitted outputs, calls/results, and the
  terminal. Append timestamps and output admission evidence use canonical
  millisecond precision. Kernel deadline failures carry the effective deadline;
  provider deadline reports remain model failures. Cancellation/deadline stages,
  journal/request/text bounds, manifest/epoch/schema continuity, task-completion
  absence, and call correlation are all replay-validated.
- `replay_artifact_read_turn` reconstructs an explicit turn from one session
  snapshot without provider or artifact I/O. It rejects missing, reordered,
  duplicated, out-of-scope, forged, temporally impossible, oversized, or
  contradictory records. A pre-existing completion rejects live admission
  without adding events; the loop itself never emits `task.completed`.
- Focused Task 003 gates passed 95 tests across context, event store, artifact
  store/read, protocol, and kernel. `./scripts/agent-check.sh` passed 213 workspace
  tests, and `cargo +1.88.0 check --locked --workspace --all-targets` passed the
  repository MSRV. Strict Clippy, formatting, diff hygiene, and secret/path
  canaries were clean. Independent code and contract reviews approved with no
  blockers, and repaired-edge manual QA passed. No live or billable provider
  request was run.

## Intentionally deferred

- daemon provider selection, paid-request scheduling, and general/effectful tool
  continuation;
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
- Version-1 replay recognizes several closed validator failures through stable
  display-message grammar; introduce typed subcodes before changing those
  messages.
- Excluded context receipts are trusted but do not yet have an independent
  encoded payload ceiling; the durable context-projection slice must bound them.
- Task-completion admission is a high-water check followed by append rather than
  an atomic verifier/admission transaction; no verifier producer exists yet.

Update this file only after code and checks establish a new fact.
