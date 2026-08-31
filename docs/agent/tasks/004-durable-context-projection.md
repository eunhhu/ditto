# Task 004: Durable context projection and shared retrieval query

## Status

Active under [ADR 0010](../../adr/0010-durable-context-projection.md).

## Objective

Deliver the first durable context-memory vertical slice without widening public
authority: journal trusted task/session context nodes in the canonical event
spine, rebuild a separately checkpointed projection, and use one bounded shared
query for context and capability retrieval. Production remains lexical-only;
optional embeddings exist only through an injected seam whose configured
failures are explicit.

## Flow

1. Add shared `TaskSignatureV2` and `TaskQuery` version-1 retrieval primitives,
   exact bounds, deterministic lexical normalization, the fixed bounded context
   document, and optional injected embedding validation. Keep legacy
   `ditto_context::TaskSignature` V1 source compatible through an explicit
   opt-in empty-resources adapter without routing existing calls through V2.
2. Add the internal `context.node.recorded` kind and exact event lookup needed
   for provenance and projection checkpoint anchors.
3. Make both context and capability retrieval consume the same prebuilt query
   while retaining normalized node-ID exact matches, capability hard filters,
   legacy wrappers, typed scan ceilings, and stable bounded output.
4. Build `context-projection.db` as a deletable cache of the event spine. Apply
   each bounded replay page with its exact sequence/event-ID checkpoint in one
   projection-database transaction, and rebuild from zero when its anchor or
   schema is invalid.
5. Add kernel-only trusted admission for version-1 session/task nodes. Under one
   mutex shared by kernel clones, catch the projection up, validate node bounds,
   session-wide identity, envelope scope, prior origin evidence, and same-scope
   supersession, derive causation from the greatest source sequence, append, and
   attempt exact-event checkpointing. Publish the committed event once on either
   sync outcome; return query-visible success or the typed committed-but-cache-
   unavailable outcome.
6. Add a read-only joint working-set API that synchronizes through one captured
   high-water, constructs one V2 query, compiles context, pages capabilities,
   and either returns the whole bounded result or one typed error.
7. Run focused N/N+1 and adversarial tests, full Task 003 regressions, strict
   Clippy, the canonical repository gate, and the Rust 1.88 workspace check.
   Update recovery/frontier documents only from captured passing evidence.

## Constraints

- The event spine is the sole durable source of truth. Projection state and its
  checkpoint are derived, separately stored, deletable, and rebuildable.
- Admit only kernel/system-authored `context.node.recorded` records for
  `ContextScope::Session` and `ContextScope::Task` nodes, with prior
  same-session provenance, origin-matching actor evidence, kernel-derived
  greatest-sequence causation, and exact same-scope supersession. Reject
  unsupported scopes, cross-session sources, cross-task task sources,
  cross-scope supersession, model assertions, malformed validity, duplicate IDs,
  and every bound violation. The trusted draft has no causation field.
- Treat `(session_id, node_id)` as the node identity across session and task
  scopes, while allowing the same ID in another session. Joint retrieval must
  never contain duplicate node IDs.
- Preserve immutable events and nodes. A semantic replacement gets a new node
  ID and `supersedes`; active views filter rather than rewrite history.
- Use exactly the node, query, document, vector, candidate-scan, and result
  bounds accepted in ADR 0010. Reject instead of truncating.
- Context entity/resource exact matching is normalized equality with `node.id`
  only. Do not infer an unmodeled resource field from node kind or summary.
- Keep the five-field legacy context signature unchanged. New joint APIs use
  V2; old compiler and Task 003 APIs retain their separate legacy normalization,
  tokenization, scoring, and compiler path. The opt-in `resources = []` adapter
  applies V2 bounds and may reject data the legacy path accepts.
- One joint retrieval builds one query. Provider absence means
  `lexical_only`; configured provider failure or descriptor/vector mismatch
  fails the whole operation without a lexical fallback or partial result.
- Exact matches and context/capability hard filters remain decisive before any
  embedding rerank. Embeddings never authorize, revive, or introduce a
  semantic-only candidate.
- Every page and checkpoint is atomic only inside `context-projection.db`.
  Event append and later cache catch-up are intentionally not cross-database
  atomic.
- Serialize admissions and joint projection snapshots through the shared
  in-process mutex. Durable append is acceptance. Attempt exact-event projection
  sync, then publish exactly once before returning either query-visible success
  or `committed_but_projection_unavailable` with the durable record and bounded
  error. A duplicate retry references the committed event and appends/publishes
  nothing. Support one kernel process/writer per data directory; do not claim
  cross-process writer safety.

## Non-goals

- No public context mutation, arbitrary event append, client-selected actor or
  internal kind, serialized trusted draft, pin/policy directive ingress, or
  daemon/CLI route.
- No turn, project, device, or global context projection; no in-place update,
  event rewrite, cross-scope supersession, or recovery that changes source
  events.
- No production embedder, model call, network embedding API, credential,
  mandatory worker/service, vector persistence, vector database, FTS migration,
  or housekeeping turn.
- No hidden context resource field, caller-selected causation, multi-process
  writer coordination, silent limit clamp, or partial V2 result at a scan or
  result bound.
- No Task 003 event/state-machine change, provider scheduling, completion
  verifier, Task 005 invocation/executor, device registry, lease path, SSH, or
  unrelated cleanup.

## Acceptance tests

- Retrieval primitives: exact N/N+1 signature, canonical-query, token,
  document, descriptor, and vector tests; byte-exact
  `id=...\nkind=...\nsummary=...` context documents; separately deterministic
  legacy and V2 behavior; one shared query embedding; typed provider failure
  with no fallback.
- Context and capability retrieval: exact-match priority, positive lexical
  eligibility, normalized entity/resource equality against `node.id` only,
  scope/time/supersession exclusion, capability placement, prerequisite,
  allowlist, effect, negative-example, and complement filters. Context V2 tests
  count inactive/superseded/lexically negative scope-selected rows before
  filters; capability V2 tests count hard-denied, lexical-negative, and
  complement-only installed manifests before filters. Each accepts 10,000 and
  errors at fetched item 10,001. Complement direct lookup adds no second scan
  count, while expanded cards count toward 512. Separate regressions preserve
  legacy-wrapper behavior.
- Durable event admission: actor `system`, kind `context.node.recorded`, payload
  `event_version = 1`, exact session/task/correlation, no draft causation, and
  greatest-source-sequence envelope causation independent of source-list order.
  Tests reject every invalid provenance/scope/supersession and N+1 case before
  the source event count changes.
- Evidence authority: each origin has a positive matching-actor source case and
  a negative no-matching-actor case; mixed extra actors remain valid; asserted
  user claims require user-authored evidence; model-origin assertions fail.
  Forged durable variants stop rebuild before the projection checkpoint passes.
- Session identity: a session/task duplicate ID and two-task duplicate ID in one
  session fail before append, the same ID in another session succeeds,
  same-scope supersession succeeds, cross-scope supersession fails, and merged
  joint results have unique IDs.
- Projection: 2,005 mixed events across multiple pages, exact checkpoint anchor,
  captured high-water isolation, reopen/incremental equivalence, deleted-cache
  and explicit full rebuild, foreign-anchor recovery, and no checkpoint advance
  past malformed or unsupported context events.
- Admission concurrency: cloned-kernel races for one node ID append exactly one
  event. Pre-subscribed receivers observe the durable event on both sync success
  and injected sync failure without a later event. Failure returns
  `committed_but_projection_unavailable` with that record; later retrieval makes
  it visible. Retry/recovery appends and publishes nothing twice. Cross-process
  concurrency remains unsupported.
- Joint kernel working set: lexical-only production behavior with zero
  embedding/model events, one injected query embedding reused by context and
  capability ranking, no hard-filter bypass, restart/incremental/cache-rebuild
  equivalence, and no partial result after configured embedding failure. Limit
  tests reject 0 and N+1 while accepting N for context results (256), capability
  roots (256), and expanded epoch capabilities including complements (512).
- Compatibility and authority: unchanged legacy full struct literals and Task
  003 turn tests; one-character token and over-V2-bound fixtures retain exact V1
  selection/order, while the opt-in adapter takes `resources = []` and exhibits
  explicit V2 divergence or rejection. Public ingress still cannot choose an
  actor or kind; no `commands/context` route exists.
- Release gates: `cargo fmt --all -- --check`, focused all-target tests, strict
  package Clippy, `./scripts/agent-check.sh`,
  `cargo +1.88.0 check --locked --workspace --all-targets`, diff hygiene, and
  secret/path canaries all pass with non-empty evidence before Task 004 moves to
  Completed.
