# Roadmap

Roadmap items are vertical slices with executable completion criteria. Dates are intentionally omitted until benchmark data exists.

## A. Runtime spine — scaffolded

- Rust daemon and CLI
- SQLite WAL event log
- database-enforced append-only event integrity
- SHA-256 content-addressed artifact storage
- HTTP append/query API
- resumable SSE replay/follow stream
- typed event protocol
- graceful shutdown

Exit criterion: a client can disconnect, reconnect from a sequence number, and reconstruct the same durable session.

## B. Semantic working set — started

- typed Context IR and Context Receipt
- capability manifests and cards
- exact/alias/lexical retrieval with placement and policy hard filters
- namespace map and full-schema page-in
- pluggable local embedding worker
- temporal + graph reranking
- bounded, append-only execution-epoch tool ordering

Exit criterion: with 1,000 synthetic capabilities, the model sees at most the relevant working set and the UI explains every selected context node and capability.

## C. Effectful execution

- device registry
- structured local process runner
- SSH transport with host-key pinning
- secret handles
- effect claims
- approval UI and bounded leases
- process-group cancellation
- deterministic output projection
- evidence verifiers

Exit criterion: a remote service can be inspected and restarted without exposing credentials or granting authority beyond one lease.

## D. Gateway UX

- WebSocket event protocol
- web timeline + inspector
- context receipt editor
- lease approval surface
- task pause, redirect, branch, and replay
- one messaging gateway
- ACP adapter

Exit criterion: web, CLI, messaging, and IDE clients observe and control the same task state.

## E. Model drivers

- provider-neutral request IR
- provider feature flags
- streaming and structured tool calls
- deferred tool search when available
- prompt-cache-stable epochs
- local Program Cell fallback

Exit criterion: at least two frontier providers pass the same replay suite without collapsing to a lowest-common-denominator interface.

## F. Improvement compiler

- deterministic signal detectors
- typed patch schema
- semantic and trigger deduplication
- replay corpus
- shadow/canary promotion
- expiration and rollback
- task-local ephemeral runbooks

Exit criterion: repeated retrieval failure can improve measured Recall@k without creating a new permanent skill or regressing unrelated scenarios.

## G. Ecosystem

- TypeScript capability SDK
- MCP consumer
- Agent Skills importer
- signed capability packages
- benchmark dashboard
- migration and backup tooling
