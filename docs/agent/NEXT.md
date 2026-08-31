# Implementation frontier

Work in order unless the user explicitly changes priorities. Complete one task
file's exit criteria before moving the marker.

## Next priority — Canonical capability invocation and effect derivation

This remains an unimplemented priority, not an active contract. Define its ADR
and task file before changing the frontier or connecting an effectful executor.
The slice must derive a canonical effect profile from validated capability
arguments and authorize it with a bounded lease; it must not add a fake success
path for the still-deferred worker lifecycle.

## Completed

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

- device registry and local process worker;
- SSH as placement transport;
- gateway inspector and approval UX;
- evidence-gated improvement compiler.
