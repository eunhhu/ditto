# Implementation frontier

Work in order unless the user explicitly changes priorities. Complete one task
file's exit criteria before moving the marker.

## Active

- No implementation slice is active. The next listed priority is a compact,
  source-verified session index that removes session-history rescans from
  context delta verification and admission; its contract has not been defined.

## Completed

- [`005 Canonical capability invocation and effect/resource authority`](tasks/005-canonical-capability-invocation.md):
  authority-free model calls, exact epoch/revision binding, bounded raw and
  normalized schema validation, typed effect/resource derivation, sealed
  canonical invocations, atomic idempotent authorization, sealed invocation-
  bound permits, and the existing `artifact.read` migration through a static
  no-approval permit without Task 003 execution or replay drift.
- [`004.2 Repair budget and validity precision`](tasks/004-2-repair-budget-validity-precision.md):
  monotonic work charging across cache repair, millisecond-canonical new
  validity admission, and schema-3 exact replay of legacy fine-precision
  inclusive-start/exclusive-end windows.
- [`004.1 Retrieval resource envelope and lifecycle`](tasks/004-1-retrieval-resource-envelope.md):
  one fixed request-local budget across query, context, and capability work;
  bounded context candidate materialization, streaming document processing, and
  top-K ranked retention; active lifecycle capacity; typed verified snapshots;
  startup/recovery full replay with steady-state delta verification; canonical
  scope and search validation before provider work; private SQLite paths; and a
  tracked CI canary plus replayable evidence manifest.
- [`004 Durable context projection and shared retrieval query`](tasks/004-durable-context-projection.md):
  canonical system-authored session/task context events, a separately stored
  rebuildable projection, kernel-only trusted admission, one bounded V2 query
  shared by context and capability retrieval, and an all-or-nothing read-only
  working set. Production remains lexical-only; injected embedding failures are
  typed and never fall back or return a partial result.
- [`003 Read-only tool continuation loop`](tasks/003-read-only-tool-loop.md):
  exact `artifact.read` manifest and full schema page-in, bounded same-scope
  verified reads, same-epoch provider-neutral continuation, durable versioned
  transitions, complete no-I/O replay, typed deadline evidence, and an
  explicitly unverified final state with no `task.completed` claim.
- [`002 First frontier provider`](tasks/002-first-provider.md): closed
  `gpt-5.6` OpenAI Responses adapter with fixed-origin HTTPS, external redacted
  credentials, deterministic prompt and schema projection, bounded SSE
  correlation, text/tool/structured-output streaming, exact optional usage and
  terminal semantics, cancellation/deadline propagation, pre-response-only
  retry, explicit remote-storage policy, and response-ID continuation.
- [`001 Provider-neutral model IR`](tasks/001-model-ir.md): versioned request and
  validated stream contract, compact context and Draft 2020-12 schema
  projections, typed generation/replay capabilities, deterministic fixture
  driver, cancellation/deadline handling, correlated tool and reasoning
  lifecycles, usage/continuation, and distinct OpenAI/Anthropic source-shape
  coverage.

## Later

- compact source-verified session indexes that remove session-history rescans
  from context delta verification and admission;
- a bounded capability package-header index with symlink-safe discovery and
  cold full-manifest paging before a large capability ecosystem;
- batched, compact rerank pools and descriptor/hash caching before a production
  embedding worker;
- device registry and local process worker;
- SSH as placement transport;
- gateway inspector and approval UX;
- evidence-gated improvement compiler.
