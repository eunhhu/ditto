# Task 006: Compact source-verified session index

## Status

Active under
[ADR 0013](../../adr/0013-compact-source-verified-session-index.md).

## Objective

Keep the event spine as Ditto's sole durable context authority while removing
affected-session sequence-zero rescans from normal context admission and delta
verification. Startup/recovery may replay full source history; the verified
steady-state path must use a compact schema-4 session index plus only new
events and bounded exact source lookups.

## Required vertical slice

1. Add schema-4 checkpoint and per-session canonical digests plus compact
   immutable identity, provenance, causation, scope, and supersession metadata.
2. Build the index only through validated startup/recovery replay, then bind it
   to a non-serialized process-local proof containing the exact checkpoint,
   canonical digest, anchor, and SQLite data version.
3. Replace normal delta dependency seeding and live-admission history scans with
   proof-gated index lookups and ordered delta application. Keep explicit audit
   replay separate.
4. Apply retrieval rows, index rows, edges, session state, and the digest-bearing
   checkpoint atomically per projection page. Preserve one rebuild/recheck and
   typed persistent-integrity failure behavior.
5. Enforce the ADR's exact entry, byte, delta-event, delta-byte, and work limits
   before the over-limit work or commit. Expose deterministic full/delta/index
   counters for regression evidence.
6. Preserve context provenance, identity, supersession, publication, recovery,
   Task 003 execution/replay, and no-completion semantics.

## Non-goals

- No event compaction, source rewrite, new context event version, public context
  command, cross-process writer claim, or authoritative serialized cache proof.
- No context ranking, query, embedding, model, capability catalogue/header
  loader, live invocation, lease, permit, or epoch change.
- No capability worker, process spawn, network access, credential resolution,
  SSH, approval fulfillment, file-mutation capability, verifier, or
  `task.completed` emission.
- No host-dependent latency or RSS threshold as a correctness gate. Optional
  measurements supplement but do not replace deterministic work counters.

## Exit criteria

- Full replay is confined to startup, explicit audit/recovery, or the one
  integrity-repair attempt. Repeated verified retrieval performs no full replay
  and no affected-session prefix scan.
- Normal admission and delta verification use the source-verified compact index
  and checkpoint-only delta. Exact source-event lookup remains bounded by the
  durable node's 64-source limit.
- A fixture with one million ordinary events and 10,000 context identities
  proves through counters that a one-event steady-state retrieval/admission
  visits only the delta and bounded index/source lookups.
- Identity, provenance/actor, source scope, greatest causation, and exact-scope
  supersession adversarial tests retain their pre-append failure semantics.
- Global/session digests and counts are deterministic across rebuild,
  incremental sync, cache deletion, reopen, and schema-3 replacement.
- Row, edge, index, state, anchor, and digest tampering triggers exactly one
  rebuild. Persistent post-rebuild drift returns a typed integrity error and no
  identity, snapshot, or partial result.
- Exact N/N+1 tests cover 65,536 session identities, 256 MiB accounted session
  bytes, 65,536 delta events, 64 MiB delta context bytes, and 2,000,000 delta
  work units without performing or committing the N+1 operation.
- `committed_but_projection_unavailable`, live publication, retry, recovery,
  working-set, legacy validity, and all Task 003 turn/replay tests remain
  unchanged.
- Focused projection/kernel tests, strict Clippy, `./scripts/agent-check.sh`,
  `cargo +1.88.0 check --locked --workspace --all-targets`, diff hygiene, and
  pull-request `rust`/`msrv` checks pass with tracked reproducible evidence.
