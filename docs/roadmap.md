# Roadmap

Roadmap items are vertical slices with executable completion criteria. Dates are
omitted until benchmark data exists. The active implementation task is always
named in `docs/agent/NEXT.md`.

## Product delivery order

Ditto is a personal general-purpose agent. Zero cost and zero overhead include
development and future maintenance burden; they are targets, not achieved
results. The user's 2026-09-06 direction prioritizes the following first usable
path ahead of remote placement and ecosystem expansion:

1. Explicit request → configured model → bounded tool → inspectable answer
   (Task 009), with scoped current memory and honest interruption/retry state.
2. Useful local work: structured process execution, bounded authority, cancel,
   and task-specific evidence. Remote SSH placement is a later expansion.
3. Reliable future work: durable schedules, event-driven wakeups, and a defined
   missed/duplicate-run and restart policy. No periodic LLM heartbeat.
4. Minimum status/approval experience and measured cost, RAM, latency, and
   repeated-use quality. Use these baselines before promoting improvements.

These are delivery milestones, not equal effort units. Completed infrastructure
tasks are not a percentage of finished personal-agent functionality.

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

Tasks 006–008 additionally provide bounded source-index verification, selected-
only full capability loading, and explicit session memory save/list/correction.
Automatic memory extraction, cross-session personal scope, and long-use semantic
recall are not established by those slices.

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

Verified boundary: injected model/transport fixtures call `artifact.read`,
continue, and replay without process or SSH authority. No live paid model turn
has been run. Task 009 connects this bounded loop to explicit CLI/HTTP requests;
it does not complete effectful execution or establish real-model task quality.

## E. Effectful execution

- device registry
- canonical invocation envelope — completed for the bounded `artifact.read`
  reference in Task 005
- capability-specific argument normalization and effect derivation — completed
  for `artifact.read`; additional effectful capabilities remain deferred
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

Promotion must also account for creation/evaluation cost, added context and
runtime work, rule accumulation, expiration, and rollback. A first successful
trajectory cannot create a permanent improvement.

## H. Ecosystem

- TypeScript capability SDK
- MCP consumer
- Agent Skills importer
- signed capability packages
- benchmark dashboard
- migration and backup tooling

## I. Reliable scheduled work — one-shot implemented, finite repeats in verification

Task 012 implements one-shot read-only requests with explicit UTC start windows,
an indexed journal-backed queue, no housekeeping model calls, visible
cancellation/expiry, and at-most-once restart inspection. Its local corpus covers
restart before/during/after dispatch and the claim/admission gap. See
[the contract](adr/0019-one-shot-scheduled-runs.md) and
[verification evidence](agent/tasks/012-evidence.md). Task 013 extends that path
with [finite fixed-interval repeats](adr/0020-bounded-recurring-schedules.md),
atomic occurrence claims, aggregate missed ranges and parent/child cancellation.
Its full local gate and production smoke checks passed; Linux CI is pending.
Calendar cron, indefinite
repetition, notification delivery and scheduled effect grants remain deferred.

- durable one-time and recurring intent, next due time, and run identity
- event/timer wakeup without housekeeping inference
- defined time zone, missed-run, overlap, and duplicate-delivery behavior
- restart recovery that distinguishes pending, running, interrupted, and terminal
- visible cancellation and failure; explicit policy before any automatic retry

Exit criterion: an agreed schedule corpus survives restart before/during/after
dispatch with no silent missed or repeated work under its declared delivery
policy. Effects retain the same lease and evidence requirements as manual runs.

## J. Cost and repeated-use performance — baseline pending

- separate necessary model/tool work from Ditto-added work
- idle/peak RSS, startup cost, and catalogue/history growth
- first useful progress and end-to-end p50/p95 latency on a fixed workload
- memory correction/recall, verified task success, user intervention, and cost
- repeated-use scenarios with interruptions, schedules, and accumulated learning

Exit criterion: a replayable baseline records workload, hardware, model/settings,
versions, measurements, and quality outcomes; later changes compare the same
conditions. Deterministic read counters and passing tests alone do not establish
zero overhead, free inference, or a performance advantage over another agent.
