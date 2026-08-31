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
- Context state: typed provenance graph, deterministic compiler, shared bounded
  V2 task query, and a standalone rebuildable SQLite projection of canonical
  `context.node.recorded` events. Pinning and policy-required inclusion remain
  trusted ephemeral directives; token cost is derived locally. A compact
  model-facing capsule projects ordered selected nodes without exposing the
  compiler receipt, lens, or supersession metadata; its exact serialized fields
  are charged locally and revalidated for trust, time, and the absolute budget
  at the model boundary. V2 embedded ordering is carried only by an opaque,
  non-serializable context-owned ranking and is revalidated after compilation.
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

- Task 004 remains in progress. Its shared retrieval, protocol/event-store,
  context, capability, standalone projection, and trusted kernel admission
  slices are complete; joint working-set composition is the next unverified
  slice.
  `ditto-retrieval` owns bounded canonical task queries, optional injected
  embeddings, exact operational limits, descriptor continuity, and typed
  fail-closed provider errors. Context and capability V2 paths consume the same
  query contract without changing their legacy APIs; embeddings cannot bypass
  lexical eligibility or runtime hard filters.
- `ditto-context-projection` owns a separate WAL SQLite cache with atomic
  page/checkpoint commits, stable high-water replay, anchor/schema recovery,
  source-immutable rebuilds, session-wide node identity, exact-scope
  supersession, and detached scope snapshots. Live draft identity and
  supersession authority comes only from bounded pages of canonical event
  history; relevant cache corruption permits one rebuild and one recheck but
  never authorizes admission. The fixed `state.db` source filename is rejected
  before filesystem mutation.
- The context projection enforces actor, envelope, scope, provenance, origin,
  causation, trust, and exact durable N/N+1 bounds. Its 21 integration tests,
  including the five Task 004 acceptance scenarios and real SQLite corruption/
  rollback cases, passed with strict Clippy and Rust 1.88. The full
  `./scripts/agent-check.sh` repository gate passed after both the authenticated
  ranking and projection commits; independent code review and manual QA cleared
  with no blockers.
- `DittoKernel::admit_context_node` accepts only a non-deserializable trusted
  draft with no actor, kind, causation, correlation, span, event identity,
  sequence, or timestamp authority. One mutex shared by every clone of a
  `KernelInner` orders pre-sync, canonical validation, durable append, exact
  post-append projection catch-up, and one live publication attempt. Source
  causation is the greatest durable cited sequence, identity is session-wide,
  and unsupported scopes, unattested origins, invalid provenance, duplicate
  identities, and exact bound failures are rejected before append or publish.
- A durable append is acceptance. If post-append projection catch-up fails, the
  exact committed record is still published once and returned inside the typed
  `committed_but_projection_unavailable` outcome with a path-free diagnostic
  bounded to 4,096 UTF-8 bytes. Recovery synchronizes canonical history without
  another append or live publication; a retry returns the committed identity as
  a duplicate without comparing payloads. Kernel open eagerly replays the
  projection and publishes nothing. This slice supports one `KernelInner` and
  its clones as writers for a data directory; separately opened kernels,
  cross-process writers, and out-of-band event-store writers remain explicitly
  unsupported.
- The admission slice passed 48 kernel tests (five kernel unit tests, five
  durable-admission integration tests, and the 38 Task 003 turn regressions),
  two compile-fail doctests, strict Clippy, Rust 1.88 package checking,
  formatting, and diff hygiene. The full `./scripts/agent-check.sh` repository
  gate passed. Real SQLite regressions cover an intervening event sequence,
  publish-after-checkpoint observation, short-path redaction, multibyte detail
  overflow, and post-append recovery. Independent code review and manual QA
  cleared with no blockers.
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
- a production embedding worker/provider and persisted embedding cache;
- completion verifiers and improvement compiler;
- authenticated remote gateway and web inspector.

## Known engineering debt

- SQLite calls are synchronous and will need a measured async boundary before
  high-concurrency gateways.
- Artifact range reads verify the whole object for integrity; optimize only with
  a design that preserves immutable-object trust.
- Context graph edges are validated but not yet used in ranking.
- V2 retrieval supports one injected provider, but production remains lexical
  until the embedding worker slice.
- Version-1 replay recognizes several closed validator failures through stable
  display-message grammar; introduce typed subcodes before changing those
  messages.
- Legacy excluded context receipts are trusted but do not yet have an
  independent encoded payload ceiling; address that before making them a new
  durable wire input.
- Task-completion admission is a high-water check followed by append rather than
  an atomic verifier/admission transaction; no verifier producer exists yet.

Update this file only after code and checks establish a new fact.
