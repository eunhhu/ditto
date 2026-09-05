# Verified handoff

## Confirmed product intent

- On 2026-09-06, the user confirmed a personal general-purpose agent, with
  Hermes as a positioning reference. The target problems are process memory
  overhead, context inefficiency, memory and scheduled-job reliability,
  long-use self-improvement degradation, task performance, and latency.
  [Product intent](../product.md) records that clarification and separates
  proposed measurements from implemented guarantees. This is user-provided
  positioning, not a benchmark of Hermes or evidence of achieved superiority.
- The user additionally made zero cost and zero overhead primary premises,
  including implementation effort and future technical debt converging toward
  zero. Product guidance now evaluates lifetime development and maintenance
  burden alongside runtime efficiency. Whether eliminating external model/API
  charges is included remains an open clarification; no free-inference policy
  or achieved zero-cost result is asserted.
- Product framing is now explicit in the root README and agent guidance.
  Runtime contracts, deferred subsystems, and the Task 006 frontier are
  unchanged by this documentation clarification.
- Documentation verification: `rtk ./scripts/agent-check.sh` passed the canary,
  formatting, strict Clippy, 351 unit/integration tests, and 24 compile-fail
  doctests. The new product document passed the staged canary and diff checks;
  all 27 local Markdown link targets across the five changed documents resolve.

## Canonical state

- Branch: `dev/task-007-capability-package-headers`, stacked on Task 006.
- Runtime: Rust daemon and CLI.
- Durable stores: SQLite event spine plus local SHA-256 artifact objects.
- Public mutation ingress: typed user-input command only; arbitrary event append
  is not a public route.
- Streaming design: subscribe-first, high-water-bounded, paginated durable
  replay with sequence-gap and lag recovery.
- Capability state: generated file-backed package headers with selected-only
  full-manifest paging and explicit bounded headerless compatibility. The
  catalogue retains headers and source paths; selected bodies must pass exact
  byte-digest, full validation, and header-projection checks before existing
  invocation validation. The Rust capability crate is version 0.2.0; event and
  serialized capability versions are unchanged. It retains default-active
  active/retired/quarantined lifecycle, active-only bounded retrieval,
  validated complements, strict runtime hard filters, append-only bounded
  execution-epoch evidence, and validated provider-neutral level-2 input/output
  schema records. Provider-neutral disclosure remains structurally checked
  Draft 2020-12 data, while live invocation accepts only the explicitly closed,
  byte/depth/work-preflighted Ditto Invocation Schema Profile V1.
  `ExecutionEpochEvidence` is replayable and has no invocation authority.
  A sealed process-local `LiveExecutionEpoch` alone issues an
  `InvocableCapabilityBinding` that owns the exact epoch ID, model-visible card,
  manifest, schema, capability ID/version and their digests, and deriver
  revision. Legacy card-only entries remain discoverable and replayable but
  cannot authorize a new live invocation.
- Context state: typed provenance graph, deterministic compiler, one cumulative
  V2 retrieval-work budget, and a standalone rebuildable SQLite projection of
  canonical `context.node.recorded` events. A typed source-verified snapshot is
  distinct from a derived cache snapshot. Schema 4 binds the exact event anchor
  and a canonical global digest to compact per-session identity, provenance,
  causation, scope, and supersession state. Open/recovery performs bounded-page
  full replay; normal retrieval and admission use only the checkpoint delta,
  bounded exact source lookups, and a process-verified compact index. New
  durable validity is millisecond-canonical while legacy version-1 source
  events retain exact fine-precision semantics. Pinning and policy-required
  inclusion remain trusted ephemeral directives; token cost is derived locally.
  A compact model-facing capsule projects ordered selected nodes without
  exposing the compiler receipt, lens, or supersession metadata; its exact
  serialized fields are charged locally and revalidated for trust, time, and
  the absolute budget at the model boundary. V2 embedded ordering is carried
  only by an opaque, non-serializable context-owned ranking and is revalidated
  after compilation.
- Policy state: sealed canonical invocations carry only harness-derived effect,
  typed resource, and local-builtin placement authority. A live epoch moves
  monotonically from paging to authorization-sealed and issues exactly one
  non-wire, non-cloneable authorization ticket. Policy consumes that ticket
  into one expiring ledger whose cloned handles share one mutex; dropping any
  ticket or handle never rearms paging or creates another ledger. The daemon
  owns no authorizer. The mutex atomically binds invocation IDs to digests,
  evaluates a trusted static policy or harness-selected lease, consumes a
  successful lease at most once, and issues a sealed epoch- and invocation-
  bound permit or approval-required outcome. Any future effectful worker must
  consume a sealed non-cloneable one-shot `ExecutionClaim`; no such worker is
  connected. The existing bounded `artifact.read` executor still requires a
  matching no-approval static-policy permit.
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

- Task 007 is complete under
  [ADR 0014](../adr/0014-capability-package-headers.md). The tracked
  [verification evidence](tasks/007-evidence.md) maps its exit criteria to
  implementation and regression tests. Implementation commit:
  `a2efcd3d073ea6eaa3411712e46eaff5e9d8a796`; tested tree:
  `47c94e7086b50dc714352300d5534316d9586f8d`.
- Compact `CapabilityHeader` values are distinct from executable manifests.
  Startup/search of generated packages reads zero full bodies. Headerless
  packages read and discard one bounded body at startup, and selected paging
  reads again. No history-dependent full-manifest cache, daemon header writer,
  worker, model call, database, or background process was added.
- Linux/macOS descriptor-relative discovery and paging reject symlinks and
  non-regular metadata. The configured root's parent is resolved once to allow
  platform aliases; the root and descendants are opened with no-follow flags.
  Fixed limits cover depth, entries, packages, file bytes, aggregate startup
  bytes, and retained header data. Tests cover exact N/N+1 limits and later
  symlink replacements, missing bodies, digest changes, and contradictory headers.
- The 1,000-package fixture searches without reading any full body and pages
  exactly one selected body. A separate fixture keeps a 400,000-byte
  verification field out of the catalogue, with startup bytes and accounted
  retained header data each below 4 KiB. Successful artifact turns and all
  historical replay tests pass; selected-package failures occur before model
  invocation and replay after package removal using stable path-free evidence.
- The final local canonical gate passed canaries, formatting, strict Clippy,
  366 unit/integration tests, and 24 compile-fail doctests across 37 suites.
  Rust 1.88 workspace/all-target checking and staged/unstaged diff checks passed
  on aarch64 macOS. Linux execution remains for CI; local counters are not RSS
  or latency benchmarks and no percentage improvement is asserted.

- Task 006 is complete under
  [ADR 0013](../adr/0013-compact-source-verified-session-index.md). The tracked
  [verification evidence](tasks/006-evidence.md) maps every exit criterion to
  schema-4 implementation, adversarial fixtures, deterministic work counters,
  local gates, and independent PR checks. The contract and implementation are
  commits `5a7402f46a4044022238f06cc32c7d1a2cee05f2` and
  `71a41791dfbf3e5c7affca38d8a4fa1de90c045f`; the tested code tree is
  `4f4a2fce9187439d3f7c81fa604fd6659ba21be3`.
- Projection schema 4 stores a compact immutable identity/provenance index,
  per-session ordered digest/count/byte state, and a global digest in the exact
  sequence/event-ID checkpoint. Retrieval rows, supersession edges, index rows,
  session state, and checkpoint advance atomically per 500-event page. Schema
  1-3 caches reset and rebuild without rewriting the event spine.
- A non-serialized process-local proof binds the checkpoint, event anchor,
  digest, SQLite data version, and compact identities. Normal synchronization
  visits only the ordered delta; admission uses proof-gated identity lookups and
  at most 64 exact source-event lookups. External cache drift is internally
  revalidated or gets one source replay/recheck and cannot return an identity or
  snapshot on persistent mismatch.
- Fixed checked limits are 65,536 identities and 256 MiB accounted bytes per
  session, plus 65,536 events, 64 MiB context payload, and 2,000,000 work units
  per normal delta. Exact N succeeds and N+1 fails before its operation or page
  commit. The scale fixture replays 1,000,000 ordinary events plus 10,000
  context identities once, then records exactly one delta event and one
  admission index lookup for steady-state retrieval/admission.
- Focused runs passed 45 context-projection tests and all 15 durable-kernel
  projection/working-set tests. The canonical `rtk ./scripts/agent-check.sh`
  gate passed its canary, formatting, strict workspace Clippy, 351
  unit/integration tests, and 24 compile-fail doctests across 36 suites.
  Rust 1.88 workspace/all-target checking and diff hygiene passed. PR #7 Actions
  run `33868899639` independently passed both `rust` and `msrv` on the exact
  implementation commit.

- Task 005.1 has a verified final review-closure implementation under
  [ADR 0012](../adr/0012-canonical-capability-invocation.md). The tracked
  [verification evidence](tasks/005-1-evidence.md) maps every amended exit
  criterion to implementation and adversarial, concurrency, integration, and
  compile-fail tests. The original contract, implementation, and adversarial
  closure are commits `ad18b33`,
  `85fde0ad862220435f28a1effdc52bb7f2136183`,
  and `cb4c71ecfb64bc69445fc91ed49263d896131676`. The final review contract and
  implementation are `ddf9a6cccaefd016b1f0775b6292fc9d4cb0ea28` and
  `98e8f676fc325bf4400aae324c2447e56098bcdb`; normalized-output work
  preflight is pinned by `7b5e4b9528c17d08953d2de1d1bb8ca6bf824f90`. The tested code tree is
  `d3ba3e14e136921106a5d5431f1abc3530ecf64a`.
- Replayable `ExecutionEpochEvidence` is now distinct from sealed,
  non-wire `LiveExecutionEpoch`. Only the latter issues a sealed
  `InvocableCapabilityBinding`; it owns one epoch's exact model card, manifest,
  schema, revision, and digests. The compiler cannot accept deserialized
  evidence and rederives the whole relationship before normalization.
- Live invocation uses closed Ditto Invocation Schema Profile V1 rather than a
  Draft 2020-12 evaluator claim. Iterative byte/depth/work preflight precedes
  recursion. Exact `i64`/`u64` integer semantics cover values beyond 2^53 and
  integer `multipleOf`; equality keywords distinguish `1` from `1.0`, and
  `artifact.read` `length = 1.0` preserves the Task 003 `invalid_arguments`
  result and continuation behavior.
- A live epoch now seals paging exactly once and moves its sole affine
  `EpochAuthorizationTicket` into policy. Every cloned authorizer handle shares
  that ticket's one `Arc`-owned ledger, while second ticket issuance, post-seal
  paging, and reissue after ticket or authorizer drop fail closed. The ledger is
  constructed inside the turn rather than `KernelInner`; permit and approval
  expiry remains capped at the epoch boundary. Its mutex transaction preserves
  failed-without-consumption, consume-once, idempotent retry, digest-conflict,
  and one-call concurrency guarantees.
- `ExecutionClaim` is a sealed, non-cloneable, non-deserializable one-shot token
  bound to the epoch, permit, and invocation digest. Atomic claim issuance
  succeeds at most once. It defines the mandatory ingress for a future
  effectful worker but no worker or dispatch path was added. The existing
  bounded `artifact.read` path still requires its sealed static-policy permit,
  and Task 003 durable execution and no-I/O replay semantics are unchanged.
- Recursive profile equality now charges each compared JSON node, including
  nested `uniqueItems`, and direct number comparison allocates no representation
  strings. Raw and normalized values pass iterative byte/depth/work preflight
  before recursive canonical projection. The compiler is the only raw
  `artifact.read` normalizer; the kernel decodes its sealed normalized value,
  while Task 003 invalid-reference/invalid-arguments codes and no-read
  continuation behavior remain unchanged.
- Focused capability, policy, and kernel runs passed 57, 14, and 60 tests
  respectively, including compile-fail doctests. The canonical
  `rtk ./scripts/agent-check.sh` gate passed its tracked canary, formatting,
  strict workspace/all-target/all-feature Clippy, workspace tests, and
  doctests. `rtk cargo +1.88.0 check --locked
  --workspace --all-targets` and `rtk git diff --check` passed. PR #6 Actions
  run `33522132273` independently passed `msrv` and `rust` on final code commit
  `7b5e4b9528c17d08953d2de1d1bb8ca6bf824f90`. No compact session-index
  work, capability worker, subprocess, network, model, credential, provider,
  SSH, approval fulfillment, file mutation, or billable operation ran.
- The original Task 005 evidence remains at
  [tasks/005-evidence.md](tasks/005-evidence.md); its initial dual-purpose epoch,
  evaluator, and daemon-ledger descriptions are superseded by Task 005.1 above.
- Task 004.2 is complete under the ADR 0011 amendment. The tracked
  [verification evidence](tasks/004-2-evidence.md) maps both post-review
  correctness findings to implementation and adversarial tests.
- A verified-snapshot cache repair no longer resets candidate work to the
  caller's pre-attempt budget. The repaired capture continues from the first
  capture's charged budget, so combined N+1 work returns a typed dimension error
  without a partial `VerifiedContextSnapshot`.
- New trusted durable `valid_from` and `valid_until` values must be exact
  milliseconds and fail with a field-specific typed error before append or
  publication. The schema-3 implementation introduced both the millisecond
  value and exact sub-millisecond nanosecond remainder; schema 4 retains that
  representation. Legacy version-1 events with finer precision therefore keep
  Rust's inclusive-start/exclusive-end behavior in SQL, and older caches rebuild
  automatically without changing events.
- Focused Task 004.2 runs passed 38 projection tests across two suites and all 15
  durable-kernel projection/working-set tests. The canonical
  `rtk ./scripts/agent-check.sh` gate passed the tracked canary, formatting,
  strict all-target/all-feature Clippy, and 328 tests across 35 suites.
  `rtk cargo +1.88.0 check --locked --workspace --all-targets` and
  `rtk git diff --check` passed. No network, model, credential, provider, or
  billable embedding operation ran.
- Task 004.1 is complete under ADR 0011. The tracked
  [verification evidence](tasks/004-1-evidence.md) maps every exit criterion to
  implementation and regression tests. The reviewed implementation is commit
  `bf58d7a3a001c0505460919a61fa6cc722dc5269`, tree
  `3c38444eaad0e5065888edcea4e5dc4f6430174a`.
- One request-local `RetrievalWorkBudget` is shared by query construction,
  verified context retrieval/ranking, and capability ranking. Version 1 fixes
  cumulative candidate, document, and lexical work at 64 MiB each, provider
  input at 32 MiB, and provider calls at 513 including the query. Checked
  N/N+1 failures occur before the over-budget allocation, tokenization, or
  provider call. Context uses bounded candidate materialization followed by
  one-at-a-time document processing and top-K ranked retention plus bounded
  exclusion metadata. Capability streams documents while retaining top-K roots.
- V2 capacity now counts lifecycle-active values. Context scope, supersession,
  disputed status, validity start, and expiry are applied in SQLite before the
  10,000-row guard. Capability manifests default to active and retired or
  quarantined manifests neither count nor page into roots, complements, or
  execution cards. Hard runtime and positive lexical filters still precede
  embedding work.
- Projection schema 4 retains the active-filter fields and exact timestamp
  remainder introduced by schema 3. Kernel open/recovery performs one canonical
  rebuild and records a non-durable process-local verification generation.
  Unchanged reads validate the checkpoint anchor, digest, compact index, and
  SQLite data version without full replay; later events are canonical
  delta-validated.
  External cache drift causes at most one source rebuild/recheck and never
  authorizes a result. Only `VerifiedContextSnapshot`, not
  `DerivedContextSnapshot`, can enter the kernel ranking path. Verification
  metrics expose full-replay events, delta events/bytes/work, admission index
  lookups, fast snapshots, and cache repairs for regression evidence.
- Working-set scope uses bounded canonical `SessionId`, `TaskId`, and
  `RetrievalScope` values. New durable context admission additionally requires a
  canonical exact `ContextNodeId`. `SearchContext` bounds and canonicalizes its
  collections, requires complete runtime fields, and rejects an unavailable
  preferred placement before the shared query embedding can call a provider.
- Event and projection SQLite families reject symlink database targets and
  symlink parents, require current-user ownership on Unix, set data directories
  to `0700`, and set present database/WAL/SHM members to `0600`.
  `scripts/agent-canary.sh` is tracked and runs first in the canonical gate; it
  rejects tracked local-state/build/database artifacts, developer absolute
  paths, and credential-shaped content.
- The canonical `rtk ./scripts/agent-check.sh` gate passed the canary,
  formatting, strict all-target/all-feature Clippy, and 326 tests across 35
  suites. The all-feature workspace test inside that gate also passed 326 tests;
  focused runs passed 14 retrieval, 27 capability, 51 context, 36 projection, 8
  event-store, and 15 durable-kernel tests. `rtk cargo +1.88.0 check --locked
  --workspace --all-targets` and `rtk git diff --check` passed. No model,
  network, credential, or billable embedding operation ran.
- Task 004 is complete under ADR 0010. The immutable event spine is the sole
  durable authority for version-1, system-authored `context.node.recorded`
  events. `ditto-context-projection` owns a separate WAL SQLite cache with
  atomic page/checkpoint commits, stable high-water replay, anchor/schema
  recovery, source-immutable rebuilds, and detached scope snapshots. The fixed
  source filename `state.db` is rejected before filesystem mutation; the cache
  can be deleted and rebuilt without changing source events.
- Durable admission is limited to session- and task-scoped nodes. Identity is
  session-wide `(session_id, node_id)` while supersession is exact-scope.
  Provenance must resolve to prior same-session events, task provenance remains
  task-compatible, and each origin requires matching actor evidence; user-origin
  assertions require user-authored evidence and model-origin assertions are
  rejected. The kernel derives causation from the greatest durable source
  sequence, independent of source-list order.
- `ditto-retrieval` owns `TaskSignatureV2`, version-1 `TaskQuery`, canonical
  retrieval scope/identity types, and the fixed cumulative work budget. It
  provides bounded canonical normalization, optional injected embeddings,
  descriptor continuity, and typed fail-closed provider errors. Context
  summaries accept at most 65,000 bytes and the fixed
  `id=...\nkind=...\nsummary=...` document is bounded at 65,287 bytes. Active
  context and capability candidate 10,001 fails. Context results and capability
  roots accept 1 through 256, and expanded epoch cards accept 1 through 512.
  Zero and N+1 are rejected without clamping or a partial value.
- Historical five-field context signatures, compilers, and raw-string
  capability searches remain separate V1 paths with their existing behavior.
  The explicit fallible V1-to-V2 adapter supplies `resources = []` and applies
  V2 bounds; legacy APIs do not silently delegate to V2.
- `DittoKernel::retrieve_working_set` validates raw limits in fixed
  context/root/epoch precedence, builds one V2 query, captures one canonical
  high-water and one evaluation instant under the clone-shared admission gate,
  then returns one detached projection checkpoint, compiled context and capsule,
  and bounded execution epoch or one typed error. It appends no event, invokes
  no model, persists no query/vector state, and never returns a partial working
  set. Production `DittoKernel::open` is lexical-only. The explicit injected
  provider constructor stores the caller-owned provider; each subsequent joint
  retrieval performs one shared query embedding. Configured provider,
  descriptor, dimension, or vector failures do not fall back, and embeddings
  cannot revive candidates excluded by lexical eligibility or capability hard
  filters.
- Projection synchronization validates canonical delta semantics before cache
  application, compares exact canonical rows, identities, and supersession edges
  for the requested snapshot, and permits one rebuild and one recheck for
  logical cache drift. Bounded affected-session history uses file-backed
  temporary state. Cache-only rows, edges, event IDs, sequences, or a fake
  10,001st row cannot authorize admission or forge a retrieval denial; malformed
  canonical history and SQLite operational failures propagate without a repair
  retry. Persistent drift is the typed
  `ProjectionSnapshotIntegrityMismatch` failure.
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
  projection and publishes nothing. The single-writer support boundary is one
  `KernelInner` and its clones for a data directory; separately opened kernels,
  cross-process writers, and out-of-band event-store writers remain explicitly
  unsupported.
- The final focused command
  `rtk cargo test -p ditto-retrieval -p ditto-context -p ditto-capability -p ditto-event-store -p ditto-context-projection -p ditto-protocol -p ditto-kernel --locked --all-targets`
  passed 178 tests across 11 suites; the projection package passed 32 tests
  across two suites and the durable kernel projection/working-set integration
  target passed 13. The matching strict
  `rtk cargo clippy -p ditto-retrieval -p ditto-context -p ditto-capability -p ditto-event-store -p ditto-context-projection -p ditto-protocol -p ditto-kernel --locked --all-targets -- -D warnings`
  command was clean. `rtk cargo fmt --all -- --check`,
  `rtk git diff --check`, and scoped `git grep -qE` secret,
  absolute-path, database, and build-artifact canaries passed.
- The canonical `rtk ./scripts/agent-check.sh` gate passed 310 workspace tests,
  and
  `rtk cargo +1.88.0 check --locked --workspace --all-targets`
  passed the repository MSRV. Independent final code review approved with no
  blockers after the source-authority repair, and post-fix manual QA cleared the
  cache-collision, supersession, edge, malformed canonical-history, SQLite
  no-retry, and one-rebuild-budget scenarios. No model, network, credential, or
  billable embedding operation ran.
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
- additional capability derivers, durable/cross-process authorization,
  approval fulfillment, and the capability worker protocol;
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
- The context-projection authority workflow is large; split it into internal
  modules later without weakening its single-gate, single-rebuild, or atomic
  checkpoint semantics.
- V2 retrieval supports one injected provider, but production remains lexical
  until the embedding worker slice.
- Headerless capability packages still incur one bounded startup body read
  until explicitly packaged with generated headers. Actual RSS and latency
  measurements remain separate from Task 007's deterministic read/data counters.
- The injected embedding interface is synchronous and may make up to 513 serial
  calls within its fixed envelope. A production worker needs compact rerank
  pools, batching, and descriptor/hash caching.
- Version-1 replay recognizes several closed validator failures through stable
  display-message grammar; introduce typed subcodes before changing those
  messages.
- Legacy excluded context receipts are trusted but do not yet have an
  independent encoded payload ceiling; address that before making them a new
  durable wire input.
- Task-completion admission is a high-water check followed by append rather than
  an atomic verifier/admission transaction; no verifier producer exists yet.

Update this file only after code and checks establish a new fact.
