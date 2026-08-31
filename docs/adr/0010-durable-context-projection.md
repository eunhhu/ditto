# ADR 0010: Durable context projection and shared retrieval query

## Status

Accepted.

## Context

Ditto has a typed context IR and deterministic context compiler, but context
nodes are not yet journaled as a durable semantic record and retrieval still
uses separate context and capability query paths. Adding persistence without a
fixed ownership boundary could make a cache look authoritative, permit a public
client to mint trusted memory, or let context and capability retrieval disagree
about the task being served. Adding optional embeddings without a closed
contract could also turn a missing or failed provider into an unreported
semantic downgrade or let similarity bypass scope, policy, or runtime filters.

This decision adds the first durable context-memory slice while preserving ADR
0002: the append-only event spine remains the sole durable source of truth. It
also preserves the Task 003 turn protocol in ADR 0009 unchanged.

## Decision

### Durable event authority

Add exactly one internal event kind, `context.node.recorded`. Its fixed mapping
is actor `system` and payload version 1:

```json
{
  "event_version": 1,
  "node": { "...": "ContextNode" }
}
```

The kernel owns a trusted, non-deserializable context-node draft and admission
API. The draft carries the node and requested session/task scope; it carries no
actor, kind, correlation, causation, span, event identity, sequence, or
timestamp. The kernel validates the draft and every named source, then assigns
all of those envelope fields. The span is absent. Task-scoped records correlate
to their task ID; session-scoped records correlate to their session ID. The
kernel chooses causation deterministically as the cited source event with the
greatest durable `seq`; caller order cannot choose it. Rebuild recomputes that
choice and rejects a recorded causation that is absent or not the greatest-seq
cited source.

There is no public mutation command, arbitrary event-append path, daemon route,
or serialized trusted draft for context nodes. In particular, this decision
does not add `POST /v1/commands/context`, public pinning or policy directives,
or a way for a client or model to choose the actor, kind, origin authority,
correlation, or causation.

### Scope, provenance, validity, and supersession

Version 1 admits only `ContextScope::Session` and `ContextScope::Task`:

- every record has an exact non-empty session ID;
- a session node has no task ID;
- a task node has the exact task ID in both its trusted draft and envelope;
- turn, project, device, and global nodes are rejected rather than stored as
  apparently retrievable context.

Every durable node is validated before append and again during projection
rebuild. A model-origin node cannot be an assertion. Confidence is finite and
in the closed range 0 through 1. When both validity bounds exist,
`valid_until` is strictly later than `valid_from`.

`source_event_ids` is non-empty, contains unique event IDs that remain non-empty
after trimming, and contains the event chosen as causation. Every source event
must already exist at a lower durable sequence and share the exact session. A
task node may cite a session-level event or an event for the same task, never a
different task. A session node may cite any earlier event in the same session.
Admission and rebuild reject a missing, later, cross-session, or disallowed
cross-task source; later events cannot retroactively authorize an existing node.

Evidence must also support the claimed origin. At least one resolved source
event must have the actor corresponding to the node origin:

```text
user       -> user
model      -> model
capability -> capability
policy     -> policy
system     -> system
```

Additional sources with other actors are allowed. A user-origin asserted claim
must therefore include user-authored evidence. A model-origin asserted node
remains forbidden even when a model-authored source exists. Admission and
rebuild apply the same actor/origin and asserted-claim checks.

A node ID and summary must remain non-empty after trimming. `ContextNode.id` is
the `node_id`, and identity is unique in the session namespace
`(session_id, node_id)` across the session node and every task node in that
session. The same `node_id` may be reused in another session. This wider
identity rule guarantees that a joint retrieval merging one session scope and
one task scope cannot produce duplicate node IDs.

History is immutable. Supersession remains narrower than identity:
A semantic replacement uses a new node ID whose `supersedes` list contains only
unique, non-empty, non-self IDs of nodes that already exist in that exact scope
key: `(session_id, no task)` for session scope or `(session_id, task_id)` for
task scope. Cross-session, cross-task, and cross-scope supersession are rejected.
Active retrieval excludes superseded, disputed, not-yet-valid, and expired
nodes without deleting or rewriting their source events.

Node-event version 1 rejects rather than truncates any bound violation:

- node ID: 256 UTF-8 bytes;
- summary: 65,000 UTF-8 bytes;
- each source-event or supersession ID: 256 UTF-8 bytes;
- `source_event_ids`: 1 through 64 entries;
- `supersedes`: at most 64 entries;
- serialized `ContextNode`: at most 131,072 bytes;
- serialized `ContextNodeRecordedPayloadV1`: at most 131,072 bytes.

These limits are part of the version-1 durable contract. Changing their meaning
for an existing event requires a new event payload version.

### Projection ownership and checkpointing

Derived context state lives only in `context-projection.db`, separate from the
canonical event store in `state.db`. The projection database uses its own WAL
and schema version 1. Its singleton checkpoint contains:

```text
schema_version = 1
through_seq
through_event_id
```

`through_event_id` is the exact event-spine identity at `through_seq`; the
zero checkpoint has no event-ID anchor. Before incremental synchronization, the
projection compares a nonzero checkpoint to an exact event-spine lookup. A
missing or mismatched anchor, supported cache-schema change, explicit rebuild,
or deleted cache discards only projection tables and replays from sequence zero.
Source events are never deleted, rewritten, or repaired from projection data.

Synchronization captures one event-spine high-water sequence and consumes
bounded global pages in order through that cutoff. Unknown non-context events
are preserved by the event store and advance the projection checkpoint. A
malformed `context.node.recorded` event, unsupported event version, invalid
authority, scope, provenance, or supersession stops synchronization before the
checkpoint passes it.

Each page's derived changes and its new `{ schema_version, through_seq,
through_event_id }` checkpoint commit in one transaction inside
`context-projection.db`. SQLite atomicity is claimed only for that one
projection-database transaction. Appending the source event to `state.db` and a
later projection catch-up are deliberately not a cross-database transaction.

`KernelInner` owns one in-process context-admission mutex shared by every
`DittoKernel` clone. Admission holds it continuously while it:

1. catches the projection up to a captured event-spine high-water;
2. validates namespace uniqueness, scope, sources, origin evidence, causation,
   validity, and supersession;
3. appends the canonical `context.node.recorded` event, which is the durable
   acceptance linearization point;
4. attempts projection synchronization and checkpointing through that exact
   event sequence and ID;
5. live-publishes the newly appended durable event exactly once regardless of
   synchronization success or failure, then returns the corresponding typed
   outcome.

The joint retrieval path acquires the same mutex while it synchronizes and
captures its projection snapshot. Acceptance always linearizes at the event
append. If exact-event projection synchronization commits, the API returns an
accepted/query-visible outcome containing the durable event; a joint retrieval
that acquires the mutex afterward observes the node when active for that query.
A retrieval that captured an earlier high-water may return the earlier snapshot.

If exact-event synchronization fails after append, the node is still durably
accepted. The API live-publishes that durable event before returning the typed
`committed_but_projection_unavailable` outcome. That outcome contains the full
durable `EventRecord` (and therefore its `seq` and `event_id`) plus a redacted
projection error of at most 4,096 UTF-8 bytes; oversized detail is replaced by a
fixed bound message. No later event is needed to flush publication. A later
retrieval or kernel open synchronizes the accepted event from the event spine.
The committed-beyond-projection outcome carries the event record so callers do
not retry blindly. Any retry or collision catches up first and finds the
session-wide node identity, then returns a typed duplicate-identity error that
references the already committed event. It does not compare payloads and
appends or publishes nothing.

One Ditto kernel process, with one shared `KernelInner` and any number of its
clones, is the supported writer for a data directory in this slice. Concurrent
cross-process writers, separately opened kernels for the same directory, and
out-of-band event-store writers are explicitly unsupported. The mutex is not a
cross-process lock and does not turn the source append plus cache commit into a
cross-database transaction. Crash recovery still replays the event spine, which
is authoritative over every projection row and checkpoint.

Context V2 search scope-selects every session-scoped projected row for the
requested session plus every task-scoped row for the exact requested task, in
durable sequence order. It
fetches at most 10,001 rows and increments the scan counter for every selected
row before disputed-status, validity, supersession, exact-match, or lexical
filters. Thus inactive and otherwise denied rows count. Row 10,001 returns a
typed scan-limit error without a partial result; at most 10,000 proceeds to
filtering and ranking. A requested context-result limit must be in 1 through
256. These are V2 rules, not retroactive legacy compiler limits.

### One shared bounded retrieval contract

Add a dependency-light internal `ditto-retrieval` crate. It owns the canonical
`TaskSignatureV2`, version 1 of the opaque `TaskQuery`, deterministic lexical
normalization and tokenization, exact terms, retrieval mode, retrieval-document
validation, embedding descriptor/vector validation, cosine similarity, and the
object-safe injected embedding-provider trait.

`TaskSignatureV2` contains, in this fixed canonical field order:

```text
request
active goal
entities
resources
constraints
expected effect
```

Text is normalized by trimming and collapsing whitespace and applying
deterministic Unicode lowercase. Lexical tokens are maximal contiguous runs of
Unicode alphanumeric characters, sorted and deduplicated. Entities, resources,
and constraints are normalized, sorted, and deduplicated. Invalid or excessive
input is rejected; no field, token list, document, or vector is silently
truncated.

For context V2 retrieval, an entity or resource is an exact match only when its
normalized whole value equals the normalized `ContextNode.id`. `ContextNode`
has no resource field, and this contract does not invent a hidden resource list
or treat kind/summary text as an exact resource. Kind and summary participate
only in positive lexical eligibility and optional reranking after hard filters.

The context retrieval document is exactly this ordered UTF-8 concatenation,
with literal ASCII separators, no escaping, and no trailing newline:

```text
id=<ContextNode.id>\nkind=<snake_case ContextNodeKind>\nsummary=<ContextNode.summary>
```

Byte counting is performed on that final UTF-8 byte sequence. Its length is
`18 + id_bytes + kind_bytes + summary_bytes`: the fixed labels, equals signs,
and two line feeds use 18 bytes, the longest version-1 kind name uses 13 bytes,
the ID uses at most 256 bytes, and the summary uses at most 65,000 bytes. The
maximum document is therefore 65,287 bytes, below the 65,536-byte retrieval
document ceiling. Newlines or equals signs inside stored fields are copied as
ordinary field bytes and do not change the formula or acquire authority.

The version-1 bounds are:

- request: at most 65,536 UTF-8 bytes;
- active goal, each entity, each resource, each constraint, and expected
  effect: at most 4,096 UTF-8 bytes each;
- entities, resources, and constraints: at most 64 entries each;
- canonical query text: at most 131,072 UTF-8 bytes;
- unique lexical tokens: at most 4,096;
- each retrieval document: at most 65,536 UTF-8 bytes;
- embedding descriptor: non-empty and at most 256 UTF-8 bytes;
- embedding vector: 1 through 4,096 dimensions, every value finite, and a
  finite non-zero norm;
- provider failure detail: at most 4,096 UTF-8 bytes; an oversized detail is
  replaced by a fixed typed bound error rather than copied or partially
  truncated.

The new V2/`TaskQuery` and joint-working-set surfaces also reject rather than
clamp these operational limits:

- context scope-selected rows inspected before all active/exact/lexical filters:
  at most 10,000; fetched row 10,001 is a typed scan-limit error;
- installed capability manifests inspected once in deterministic capability-ID
  order before placement, prerequisite, allowlist, effect, negative-example,
  exact, or lexical filters: at most 10,000; manifest 10,001 is a typed
  scan-limit error before a partial ranking is returned;
- requested context results: 1 through 256;
- requested ranked capability roots: 1 through 256;
- requested expanded execution-epoch capabilities, including complements:
  1 through 512.

These candidate and result-limit guarantees apply to the new V2 query and joint
APIs. Existing legacy context/compiler APIs and string capability-search
wrappers remain source compatible and retain their existing behavior and bounds;
this ADR does not retroactively claim that they scan or return according to the
new V2 limits.

Capability complement expansion uses bounded direct capability-ID lookup after
root ranking. It does not increment a second scan counter: every installed
manifest, including a complement-only or hard-denied one, was already counted
once by the pre-filter catalogue pass. A complement must still pass its runtime
filters, and each expanded card counts against the 512-card epoch ceiling.

The V2 capability path accepts the already validated `CapabilityRootLimit` and
`ExecutionEpochLimit` newtypes and returns the expanded `Vec<CapabilityCard>`
or one typed `CapabilitySearchError`. The returned order is each ranked root
followed by that root's direct complements in manifest order, with capability
IDs deduplicated across the whole result. Expansion stops at the requested
epoch capacity. Root membership is not a second public result.

The catalogue-length gate precedes sorting, filtering, document construction,
and provider work. A catalogue longer than 10,000 reports 10,001 as the
overflow sentinel; that sentinel manifest is counted only to establish the
error and is never filtered, documented, matched, or embedded. Catalogues at
or below the ceiling are inspected once in capability-ID order. Before any
embedding call, the V2 path verifies every direct complement reference and
preprocesses every hard-filter-eligible root in that same ID order.

Capability exactness uses only the `TaskQuery` exact terms derived from
normalized entities and resources. A capability ID or alias is exact when its
normalized whole value equals one of those terms. The request, active goal,
constraints, and expected effect contribute lexical terms but cannot create an
exact capability match.

The canonical V2 capability document is the following UTF-8 byte sequence:

```text
id=<raw id>\nnamespace=<raw namespace>\nsummary=<raw summary>
[\nalias=<raw alias>]...
[\nintent=<raw intent>]...
```

Aliases and intents are independently sorted by raw UTF-8 byte order. Manifest
validation already rejects exact duplicates; normalized-equivalent but
byte-distinct values remain separate raw lines. Fields are not escaped, and
there is no trailing newline. The final sequence must fit the shared 65,536
byte `RetrievalDocument` ceiling. This raw repetition can influence an injected
embedding provider, but the shared lexical tokenizer still deduplicates terms;
production remains lexical-only in this slice.

A negative example denies a root or complement only when its non-empty V2
normalized whole phrase occurs at whitespace boundaries in the canonical query
text. The retrieval crate owns this normalization. An example longer than the
4,096-byte component ceiling or containing a non-whitespace control character
returns the wrapped typed retrieval error for the whole operation; there is no
legacy token-overlap penalty or fallback in V2. Hard runtime filters run before
this denial predicate. Complements must pass both the same runtime filters and
the same negative-example denial before expansion, but are not required to be
lexically eligible and are never embedded.

Eligible roots have an exact match or positive lexical overlap. Their complete
deterministic ranking tuple is exact match descending, embedding cosine
descending when configured, lexical overlap descending, preferred-placement
match descending, then capability ID ascending. Every eligible root, including
an exact root, receives one document-embedding call in capability-ID order
when the query is embedded. A lexical-only query requires no provider, and an
embedded query requires one; the opposite pairings are typed errors before
catalogue work. Search itself never performs a query-embedding call. Query and
document descriptor/dimension continuity is the enforceable provider boundary;
provider object identity is neither exposed nor claimed.

Context and capability retrieval consume the same `TaskQuery`. Exact context
entity/resource-to-node-ID matches and exact capability IDs and aliases remain
ahead of embedding reranking. Domain hard filters run before optional embedding
work.
Context similarity cannot revive disputed, expired, not-yet-valid, superseded,
or wrong-scope nodes. Capability similarity cannot bypass placement,
prerequisite, allowlist, effect, negative-example, complement, or other runtime
filters. Embeddings may only narrow or rerank candidates that already have
positive lexical or exact eligibility; version 1 never introduces a
semantic-only candidate.

### Optional embeddings and production behavior

One joint working-set retrieval builds exactly one `TaskQuery`. With no injected
provider it reports `lexical_only`, performs no embedding work, and makes no
model call. This is absence, not failure, and is the production kernel behavior
for this slice: production is lexical-only. Ditto supplies no production
embedding implementation, worker, service, network API, credentials, mandatory
runtime, or persisted vector.

With an explicitly injected provider, the joint operation computes exactly one
query embedding and shares the same descriptor and validated vector with both
context and capability retrieval. Any document embeddings are ephemeral and
must match the query descriptor and dimension. A provider error, empty or
oversized descriptor, dimension change, non-finite vector, or zero vector fails
the whole configured joint retrieval with a typed bounded error. It does not
silently downgrade to lexical, report semantic retrieval, or return a partial
context/capability working set.

Neither lexical nor embedded retrieval appends an event, mutates context,
persists its query/vector/result, or invokes a model. A returned working set is
a read-only bounded projection, not durable evidence or authorization.

### Rust API compatibility and migration

The existing public five-field `ditto_context::TaskSignature` definition is
frozen as legacy Rust API V1. Its fields and full-struct-literal construction
remain unchanged. Adding `resources` to that struct would break downstream full
literals, and a re-export would not make those literals compatible.

`ditto_retrieval::TaskSignatureV2` is a distinct type with explicit resources.
New joint retrieval APIs accept V2. Existing compiler and Task 003 APIs continue
to use their separate legacy normalization, tokenization, scoring, and compiler
path unchanged; they never enter the V2 path. In particular, legacy
one-character-token behavior and previously accepted over-bound inputs remain
legacy behavior rather than being silently reinterpreted by V2 validation.

An explicit opt-in legacy-to-V2 adapter constructs `TaskSignatureV2` with
`resources = []`, then applies all V2 normalization, bounds, and semantics. It
may reject input accepted by the legacy path and may produce different lexical
terms; using it is an explicit migration choice, not a compatibility shim used
by existing calls. Context may re-export V2 and `TaskQuery` as conveniences, but
it does not rename or remove the legacy struct.

Removing the legacy type is deferred to a future explicitly breaking Rust API
release with its own migration notice. No public HTTP wire contract or existing
Task 003 durable payload changes in this slice.

## Rejected alternatives

- Storing canonical context in the projection was rejected because a cache
  corruption or deletion would then rewrite semantic history and contradict ADR
  0002.
- Using a projection table in `state.db` or an attached-database transaction was
  rejected because it would blur source/cache ownership and encourage a false
  cross-database atomicity claim.
- Exposing a public context-node/event command was rejected because untrusted
  clients must not choose trusted origin, actor, kind, scope, provenance, or
  compiler authority.
- Mutating or deleting nodes in place was rejected because supersession and
  dispute must remain auditable events and projections.
- Scope-local node identity was rejected because one session node and one task
  node could then share an ID and collide when joint retrieval merges them.
  Session-wide identity prevents that ambiguity while same-scope supersession
  preserves the narrower replacement rule.
- Caller-selected causation or list-order causation was rejected because source
  ordering is not evidence authority. The greatest durable source sequence is
  deterministic and replay-verifiable.
- Treating arbitrary summary/kind text as an exact context resource or adding a
  hidden node resource field was rejected because `ContextNode` has no such
  field. Exact V2 context terms compare only with `node_id`.
- Adding `resources` directly to legacy `TaskSignature` was rejected because it
  breaks existing Rust full literals.
- Separate context and capability query builders were rejected because their
  normalization, bounds, or embedding state could diverge for one working set.
- Silent truncation, semantic-only candidates, and lexical fallback after a
  configured provider failure were rejected because they hide retrieval loss or
  allow similarity to acquire authority.
- A production embedding worker, vector database, FTS migration, model call,
  network provider, or persisted vector was rejected because none is required
  to establish the bounded local retrieval contract.
- Project, device, global, or turn-scope projection was rejected because its
  widening and conflict rules are outside this vertical slice.
- Claiming multi-process writer safety was rejected because the admission mutex
  is shared only by clones of one in-process `KernelInner`. Cross-process
  coordination requires a later explicit locking or transactional design.

## Compatibility and migration impact

The new event kind and payload are additive; no version-1 context-node events
exist before this decision. Old event consumers already preserve unknown kinds,
and new consumers tolerate additive unknown payload fields while rejecting a
missing or unsupported `event_version`.

The projection database contains only derived state. Upgrading or rolling back
may delete `context-projection.db` and rebuild it from compatible source events.
No source event migration is needed for projection schema changes. The legacy
V1 compiler/search path protects current Rust context and Task 003 callers by
remaining separate and unchanged. The opt-in V1-to-V2 adapter sets empty
resources and deliberately adopts V2 validation; V2 is required only for new
joint APIs.

## Measurable consequences and rollback

Tests must prove trusted system-only admission and exact session/task envelopes.
The same node ID must be rejected across session/task scopes and across tasks in
one session before another event is appended, accepted in a different session,
and returned at most once by joint retrieval. Supersession must still reject a
target from another exact scope key.

Draft construction tests must prove there is no causation field. Admission with
sources in every permutation must record the greatest-sequence source as
causation and the fixed task/session correlation. Replay/rebuild must stop before
a forged non-maximum causation event. Evidence tests must cover every
origin/actor mapping, allow an additional mixed-actor source, reject an origin
with no matching source actor, require user-authored evidence for asserted user
claims, and keep model-origin assertions forbidden. Admission failures must not
change the source event count; forged durable records must not advance the
projection checkpoint.

Node N/N+1 tests must cover ID, 65,000-byte summary, source/supersession lists,
and serialized payload. Retrieval-document tests must compare exact bytes and
length for the fixed ID/kind/summary form, prove its maximum is no more than
65,536 bytes, and prove normalized entity/resource equality matches only
`node_id`, never kind or summary text.

Projection tests must prove multi-page checkpoint progress, exact
`through_seq`/`through_event_id` anchors, restart, incremental catch-up, cache
deletion, full rebuild equivalence, foreign-anchor recovery,
stop-before-malformed behavior, and unchanged source events. Concurrent cloned
admissions of the same ID must yield one canonical event, and a retrieval that
starts after successful admission must see its active node. An injected
post-append sync failure must return `committed_but_projection_unavailable` with
the durable record. A receiver subscribed before admission must observe that
record without any later event, on both sync-success and sync-failure paths.
Recovery must make it query-visible; a retry must return duplicate identity
referencing the committed event and append/publish nothing again. Cross-process
writing remains an explicit unsupported configuration, not a passed concurrency
claim.

Retrieval tests must prove legacy V1 source compatibility, deterministic V2
normalization and bounds, shared lexical ordering, exact-match priority, one
query embedding shared by both domains, hard-filter dominance, explicit
configured-provider failure, and production `lexical_only` behavior with zero
embedding/model calls. Context V2 scan tests must count inactive, disputed,
expired, superseded, exact-negative, and lexical-negative scope-selected rows;
capability V2 scan tests must count hard-denied, negative-example,
lexical-negative, and complement-only installed manifests. Both accept exactly
10,000 and error on fetched item 10,001. Complement direct lookup must not add a
second scan count, and expanded cards must count toward the 512-card ceiling.
Tests must also cover 0/1/N/N+1 for context-result, capability-root, and
expanded-epoch limits without clamping or a partial result.

Separate legacy regressions must prove unchanged V1 normalization, tokenization,
selection, and order for a one-character term and input exceeding V2 bounds.
The opt-in empty-resources adapter must then demonstrate the documented V2 term
divergence or bound rejection; existing compiler, capability wrapper, and Task
003 paths must never invoke it implicitly.

Public-ingress tests must continue to prove that clients cannot choose an actor,
internal event kind, correlation, or causation and that no public context
mutation route exists.

Rollback removes the additive event constant, shared retrieval and projection
crates, kernel admission/working-set APIs, cache database, and this Task 004
composition. It may delete `context-projection.db` and its WAL/SHM files. It
must not delete or rewrite the event spine. Once a deployment has persisted
`context.node.recorded` version-1 events, rollback must keep them readable as
unknown immutable events or explicitly migrate them before any later
incompatible reuse of the kind.
