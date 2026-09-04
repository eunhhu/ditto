# ADR 0013: Compact source-verified session index

## Status

Accepted.

## Context

ADR 0010 made the event spine the sole durable authority for context admission,
and ADR 0011 separated source-verified snapshots from ordinary projection
inspection. Kernel open now performs one full replay and unchanged retrievals
reuse a process-local verification proof. Two steady-state paths nevertheless
remain proportional to the affected session's complete age:

- delta preflight discovers prior identities and supersession dependencies by
  rescanning each affected session from sequence zero; and
- live admission rescans the complete session to resolve a proposed identity
  and its supersession targets.

The scans are bounded in retained memory but not in work or admission-gate hold
time. A session with one million ordinary events and a small context namespace
therefore makes a one-event context delta or admission unnecessarily revisit
the million-event prefix. Trusting arbitrary projection rows instead would
remove the scan by turning a mutable cache into authority.

This decision removes those normal-path rescans without changing the authority
or Task 003 contracts.

## Decision

### Source authority and cache ownership

The append-only event spine remains the only durable semantic authority.
`context-projection.db` remains a separately stored, deletable, rebuildable
cache. Its compact session index never authorizes a value unless the current
process holds a source-verification proof for the exact cache generation.

Kernel startup and explicit recovery discard the derived schema and replay the
event spine from sequence zero in fixed-size pages. That replay validates every
`context.node.recorded` event, its exact prior sources, actor attestation,
causation, session/task scope, identity, and supersession before issuing the
process-local proof. The proof is neither serialized nor deserializable and is
lost on process exit.

Normal synchronization may then consume only the ordered global event delta
after the verified checkpoint. It must not scan an affected session before the
checkpoint. Exact source-event lookup by the event store's unique event-ID
index remains canonical source access; it is not a session-history scan.

An out-of-band SQLite data-version change invalidates the fast proof. When the
stored checkpoint is still byte-for-byte equal to the remembered source
checkpoint, recovery may perform one complete compact-index consistency check
bounded by the session-index envelope. That check must rederive both digest
chains, compare every indexed row with its retrieval row and complete edge
shape, verify all active-filter columns from canonical node JSON, and compare
the resulting identities with the remembered source identities. Only a
non-content change that leaves all of those values identical may refresh the
process-local data-version binding without source replay. Any content mismatch
must reset and replay from source. Canonical delta validation runs first so a
malformed source event retains its established typed failure and checkpoint.

### Projection schema 4 and canonical digests

Projection schema 4 keeps the existing retrieval rows and supersession edges
and adds a compact immutable identity/provenance index. One index row contains
only the fields needed to validate later context state:

```text
session_id, node_id, task_id
recording seq and event ID
canonical node digest
canonical provenance digest and source count
greatest-source causation seq and event ID
canonical supersession digest and target count
accounted index bytes
```

A per-session state row binds:

```text
session_id
last context-event seq and event ID
canonical session-state digest
identity entry count
accounted index bytes
```

The singleton projection checkpoint binds schema version 4, global
`through_seq`, the exact event anchor at that sequence, and a canonical index
state digest. The zero checkpoint and an empty session use fixed domain-
separated SHA-256 digests. Each valid context event advances the global and
affected-session digests with length-framed canonical fields; non-context
events advance the sequence/event anchor but do not change the context-state
digest. Digest construction never uses SQLite row order, platform formatting,
or non-canonical JSON text.

The index records all immutable context identities, including inactive or
superseded nodes. Active retrieval remains a separate lifecycle projection.
Discarding inactive identities would permit forbidden ID reuse.

Schema-1, schema-2, and schema-3 databases are derived caches. Open under this
contract replaces them with schema 4 and replays canonical events; no event
migration or rewrite occurs.

### Verified normal admission and delta application

One process-local verification record binds the complete schema-4 checkpoint,
including its canonical digest, to SQLite's observed data version. While the
projection synchronization gate is held, a normal operation must establish all
of the following before reading indexed authority:

1. the remembered checkpoint exactly equals the stored checkpoint;
2. the checkpoint's sequence/event-ID anchor is the exact event-spine record;
3. the remembered and stored canonical state digests are equal; and
4. SQLite's data version has not changed outside the supported writer path.

Live admission resolves session-wide identity and exact-scope supersession only
from this verified compact index. It still resolves each newly cited source by
exact event ID and rechecks prior sequence, session/task compatibility, origin
actor attestation, and greatest-sequence causation. A successful append remains
the acceptance point. Exact-event projection catch-up and one live publication
attempt retain the existing accepted or
`committed_but_projection_unavailable` outcomes.

Normal delta synchronization validates new context events against the verified
index state, applies the retrieval row, index row, edges, session state, and
checkpoint atomically in the same projection-database page transaction, and
only then advances the process-local proof. A failed page does not advance its
checkpoint or proof.

Lookup and snapshot APIs that return source-authoritative values must require
the same proof. Low-level derived-cache inspection remains non-authoritative.
The explicit full-scope audit may still replay a session from zero because it
is a recovery/audit operation, not steady-state admission or retrieval.

### One rebuild and fail-closed recovery

A missing proof, legacy schema, checkpoint-anchor mismatch, digest mismatch,
failed external-data-version consistency check, or indexed row/state
inconsistency causes one schema reset, full source replay, and recheck at the
same captured high-water. If the recheck still disagrees, the operation returns
a dedicated typed integrity failure and exposes no identity, snapshot, or
partial result.
Canonical event corruption and operational event-store failures remain direct
typed failures; they are not relabeled as repairable cache drift.

No cache value repairs or rewrites a source event. Concurrent out-of-band cache
writers and separately opened kernel writers for one data directory remain
unsupported.

### Fixed bounds and inspectable work

Version 1 of the compact index and normal delta path rejects rather than
truncates these maxima:

```text
session index identities             65,536
session index accounted bytes       256 MiB
normal delta events                   65,536
normal delta context payload bytes     64 MiB
normal delta verification work     2,000,000 units
```

Accounted index bytes include every variable-width indexed field plus fixed
integer/digest storage. Delta work charges each visited event, decoded context
node, source lookup, identity lookup, and supersession lookup before the work
occurs. Checked arithmetic reports a typed dimension and exact attempted value.
Startup/recovery full replay is not disguised as a normal delta and is not
limited by the normal-delta event count; it remains page-memory bounded and is
subject to the per-session index entry/byte limits.

Operational metrics distinguish full-replay events, delta events, delta context
bytes/work, and admission index lookups. Regression evidence uses these
deterministic counters rather than host-dependent latency or RSS thresholds.
A non-gating benchmark may record wall-clock, gate-hold, or RSS observations,
but those observations cannot replace the fixed correctness envelope.

### Compatibility boundary

This decision changes no context event payload version, public ingress, Task
003 durable turn payload, replay semantics, context ranking, model request,
embedding provider, capability catalogue, invocation/permit authority, or
artifact-read execution. It adds no worker, process, network, credential,
approval fulfillment, SSH, file-mutation capability, or completion event.

## Rejected alternatives

- Trusting `projected_nodes` after checking only `through_seq` was rejected
  because a cache mutation could acquire identity or supersession authority.
- Persisting the source-verification proof was rejected because restart must
  not turn a derived database into durable authority.
- Keeping targeted sequence-zero rescans was rejected because retained-memory
  bounds do not bound mutex hold time or source work.
- Indexing every ordinary session event in memory was rejected because it would
  reproduce the million-event history instead of compacting context authority.
  Newly cited sources use the event store's exact indexed lookup.
- Evicting inactive identities was rejected because immutable session-wide
  identity and exact-scope supersession depend on historical membership.
- Automatically falling back to an unbounded delta or returning a partial
  snapshot on a limit was rejected because it hides request cost or retrieval
  loss.
- Host-dependent latency and RSS limits were rejected as correctness gates;
  deterministic visit/work counters establish the O(delta) property.

## Compatibility and migration impact

The database change affects only the rebuildable projection. Deleting
`context-projection.db` and reopening recreates schema 4 from the event spine.
Existing version-1 context events, including legacy fine-precision validity,
retain their ADR 0010/0011 interpretation. New typed index/delta limit failures
are possible when previously unbounded steady-state work exceeds this fixed
envelope. There is still no public context-mutation route.

Rollback deletes the schema-4 projection and restores schema 3 code, then
rebuilds from source. It must not delete, migrate, or rewrite any canonical
event.

## Measurable consequences

Tests and tracked evidence must prove:

- startup/recovery performs full replay while a second unchanged retrieval
  visits no source-history prefix;
- after a verified million-event session prefix, one new ordinary or context
  event causes steady-state retrieval and admission work proportional only to
  the delta plus bounded exact lookups;
- normal admission detects duplicates and exact-scope supersession through the
  verified index without a sequence-zero session scan;
- provenance actor, source scope, greatest-sequence causation, identity, and
  supersession failures remain fail-closed before append;
- checkpoint sequence, event anchor, canonical global digest, per-session
  digest, entry count, and accounted bytes are deterministic across rebuild,
  reopen, and incremental application;
- tampered rows, index rows, state rows, edges, checkpoint anchors, or digests
  cause one rebuild; persistent post-rebuild drift returns the typed integrity
  error without a value;
- exact N/N+1 entry, byte, delta-event, delta-byte, and verification-work cases
  reject before the over-limit work or page commit;
- cache deletion and schema-3 migration preserve source events and rebuild the
  same verified state;
- post-append projection failure still publishes and returns the exact durable
  event once, then recovers without duplicate append or publication; and
- the canonical gate, Rust 1.88 workspace/all-target check, diff hygiene, and
  independent pull-request `rust` and `msrv` checks pass.
