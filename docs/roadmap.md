# Roadmap

Roadmap items are vertical slices with executable completion criteria. Dates are
omitted until benchmark data exists. The active implementation task is always
named in `docs/agent/NEXT.md`.

## A. Trusted runtime spine — scaffolded

- Rust daemon and CLI
- SQLite WAL event log with schema versioning
- database-enforced append-only event integrity
- typed command ingress; trusted actor/kind assignment
- subscribe-first, high-water, paginated SSE replay/follow
- sequence-gap and broadcast-lag recovery
- SHA-256 content-addressed artifact storage
- graceful shutdown and non-loopback safety guard

Exit criterion: a client can disconnect, reconnect from a sequence number, and
reconstruct the same durable session without manufacturing trusted events.

## B. Semantic working set — in progress

Existing foundations:

- typed Context IR, graph edges, provenance validation, and Context Receipt
- trusted ephemeral pin/policy directives and derived token cost
- capability manifests and cards
- validated complements and strict runtime hard filters
- bounded, append-only execution-epoch ordering
- namespace map and full-schema page-in

Task 004 delivered:

- kernel-only version-1 durable `context.node.recorded` events for session/task
  context, with provenance, supersession, and session-wide node identity
- source-authoritative, rebuildable `context-projection.db` with checkpointed
  recovery and committed-but-projection-unavailable handling
- one bounded V2 query shared by context and capability retrieval, with an
  all-or-nothing lexical production working set and an injected provider seam

Still deferred:

- production local embedding worker and persisted embeddings
- temporal and graph reranking
- UI that explains every selected context node and capability

Exit criterion remains open: with 1,000 synthetic capabilities, the model must
see only the relevant working set and the UI must explain every selected
context node and capability.

## C. Provider-neutral model IR — completed

- stable/volatile request separation
- structured text and tool-call streaming
- usage, finish reason, warnings, and continuation
- feature flags without lowest-common-denominator collapse
- cancellation and deadline propagation
- serialization and replay fixtures

Exit criterion: two representative provider response shapes map losslessly into
the same IR, and provider completion never creates task verification.

## D. First provider and read-only agent loop — completed

- one frontier provider adapter with mock transport tests
- full `artifact.read` schema page-in
- structured tool invocation and bounded artifact result
- model continuation after tool result
- durable turn replay
- explicit unverified final state

Exit criterion: one real model turn can call `artifact.read`, continue, and be
fully replayed without process or SSH authority.

## E. Effectful execution

- device registry
- canonical invocation envelope
- capability-specific argument normalization and effect derivation
- structured local process worker
- SSH transport with host-key pinning
- secret handles
- approval UI and bounded leases
- process-group cancellation and resource limits
- deterministic output projection and evidence verifiers

Exit criterion: a remote service can be inspected and restarted without exposing
credentials or granting authority beyond one lease.

## F. Gateway UX

- WebSocket event protocol
- web timeline and inspector
- context receipt editor
- lease approval surface
- task pause, redirect, branch, and replay
- one messaging gateway
- ACP adapter

Exit criterion: web, CLI, messaging, and IDE clients observe and control the same
task state.

## G. Improvement compiler

- deterministic signal detectors
- typed patch schema
- semantic and trigger deduplication
- replay corpus
- shadow/canary promotion
- expiration and rollback
- task-local ephemeral runbooks

Exit criterion: repeated retrieval failure improves measured Recall@k without
creating a new permanent skill or regressing unrelated scenarios.

## H. Ecosystem

- TypeScript capability SDK
- MCP consumer
- Agent Skills importer
- signed capability packages
- benchmark dashboard
- migration and backup tooling
