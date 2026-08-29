# Architecture

Ditto is a **semantic agent microkernel**. It does not own the model's strategy. It owns the model's environment: what context is visible, what capabilities can be discovered, which effects are allowed, how execution survives process boundaries, and what becomes durable knowledge.

## Core sentence

> Context is compiled. Capabilities are paged. Effects are leased. Improvements are promoted.

## Ownership boundary

The model owns:

- intent interpretation;
- strategy and decomposition;
- judgment under uncertainty;
- deciding when another capability or context fragment is needed.

The harness owns:

- durable events and resource lifetime;
- context selection and provenance;
- capability discovery and loading;
- credentials, policy, approval, and effect execution;
- resumability, cancellation, verification, and self-improvement promotion.

This avoids both extremes: a loose chat wrapper that gives the model raw machine access, and a rigid planner/executor pipeline that suppresses a frontier model's native competence.

## 1. Event spine

Every meaningful state transition is an event. SQLite in WAL mode is the initial durable implementation; the append-only sequence is the source of truth. UI state, task projections, context graphs, replay tests, and improvement signals are rebuildable projections.

No important state may exist only in process memory. Ephemeral handles may point to a browser, process, SSH channel, workspace, or child context, but their lifecycle is represented durably.

Representative event families:

```text
input.received
context.compiled
capabilities.selected
model.started
model.delta
capability.requested
policy.approval_required
policy.lease_granted
execution.started
execution.output
artifact.created
task.blocked
task.completed
improvement.candidate_created
improvement.promoted
```

The daemon fans new durable events out to clients over one event protocol. Clients are projections, not separate agent loops.

## 2. Context compiler

Conversation transcripts are evidence, not the prompt. Before a model turn, Ditto builds a Task Signature from the current request, active goal, explicit entities, unresolved constraints, and expected effect class. Retrieval runs against four lenses:

- personal: preferences, recurring constraints, long-term goals;
- task: goals, decisions, blockers, evidence, completion conditions;
- environment: devices, repositories, apps, accounts, processes;
- conversation: references, corrections, negations, and supersession.

The intermediate representation is a typed temporal graph. Every node carries origin, epistemic status, scope, confidence, source events, validity, supersession, and token cost. The graph is not dumped into the model. It is compiled into a bounded Context Capsule and an inspectable Context Receipt.

The initial crate implements deterministic lexical ranking and hard inclusion for pinned/compiler-required nodes. Hybrid lexical, embedding, temporal, and graph scoring belongs behind the same compiler boundary.

## 3. Capability pager

The complete capability universe is virtual address space; model context is RAM. The pager exposes four levels:

```text
L0 namespace map
L1 short capability card
L2 full provider-native schema
L3 lazy runtime worker
```

Hard filters run before semantic retrieval: device availability, OS/runtime support, policy compatibility, permissions, and worker health. Exact aliases and lexical retrieval precede embeddings. Embeddings narrow candidates; they never directly authorize execution.

A working set is stable for one execution epoch. New schemas append to the epoch rather than reordering the prefix, preserving provider prompt caches.

Capability implementations never dynamically import into the core daemon. A capability is an isolated stdio/socket/WASI/MCP/remote worker described by a manifest.

## 4. Effect firewall

SSH is a transport, not a model-facing tool. The model requests a structured capability invocation against a registered device. The kernel chooses local, SSH, container, or remote placement.

Every invocation declares an effect class:

```text
pure
read
write-reversible
write-irreversible
external-communication
credential-access
privileged
```

Approval grants an expiring, bounded Capability Lease scoped to capability IDs, devices, programs, resources, call count, and effect ceiling. Credentials remain opaque handles and never enter model context or event payloads.

The current policy crate evaluates already-normalized resource identifiers. Transport workers must independently revalidate canonical paths, binaries, service names, and secret audiences at execution time.

## 5. Adaptive execution

Simple read-only work takes the fast path: compile context, call the model, optionally call one capability, return. Multi-device, long-running, destructive, approval-gated, externally blocked, or evidence-sensitive work gains a durable Task Graph.

The Task Graph is not a forced plan. It is a ledger of external commitments and remaining state:

```text
accepted → assembling → running
                      ↘ waiting_event
                      ↘ waiting_approval
                      ↘ blocked
running → verifying → completed | failed | cancelled
```

Tasks wake on events—process exit, webhook, timer, device online, approval, or user input—not periodic LLM heartbeats.

Subagents are short-lived context forks with a bounded objective, context filter, capability set, effect ceiling, output schema, and budget. They are not permanent personas.

## 6. Artifacts and evidence

Large outputs are content-addressed artifacts. Model context receives deterministic summaries, selected structured fields, and artifact references. Completion is a claim until a verifier produces evidence such as a file hash, diff, commit SHA, provider message ID, health response, or build artifact.

The UI distinguishes `completed` from `unverified`.

## 7. Improvement compiler

Self-improvement never means writing another free-form skill after every turn. Deterministic detectors first identify repeated corrections, retrieval misses, argument errors, retry loops, approval repetition, latency regressions, or verifier mismatches.

A model may then propose a typed patch against a bounded surface:

```text
retrieval alias or example
capability relationship
context ranking rule
argument normalizer
validator or verifier
temporary runbook fragment
user preference claim
capability implementation
```

Candidates pass deduplication, validation, replay, shadow, and canary stages before promotion. Kernel code, root policy, credentials, the evaluator, audit logs, pinned context, and active provider settings are not self-editable.

Most trajectories remain traces. One success never becomes a permanent capability.

## Runtime composition

- Rust: daemon, event store, context compiler, capability index, policy, executor, scheduler, protocol.
- TypeScript/Bun: integration SDK, browser and app connectors, gateway adapters.
- Python: optional out-of-process worker for workloads that genuinely require it.
- SQLite + local object store: mandatory persistence.
- MCP: external capability/resource boundary.
- ACP: IDE/editor client boundary.
- A2A: only for truly independent external agents.

## Zero-cost definition

Ditto cannot make frontier inference, CPU, RAM, or security checks literally free. “Zero” means:

- zero mandatory infrastructure beyond one daemon and local storage;
- zero housekeeping inference by default;
- near-zero hot-path work before the model request;
- zero eager loading of capability implementations.
