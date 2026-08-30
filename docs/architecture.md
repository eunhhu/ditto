# Architecture

Ditto is a **local-first semantic agent microkernel**. It does not own a frontier
model's strategy. It owns the model's environment: visible context, discoverable
capabilities, permitted effects, durable execution state, and promotion of
learned behavior.

> Context is compiled. Capabilities are paged. Effects are leased. Improvements
> are promoted.

## Ownership boundary

The model owns intent interpretation, decomposition, judgment, and deciding when
it needs another capability or context fragment.

The harness owns event authority, persistence, context provenance, capability
discovery and lifecycle, credentials, policy, approval, cancellation,
verification, and improvement promotion.

## Runtime spine

```text
clients and gateways
       │ typed commands
       ▼
trusted ingress ──► semantic kernel ──► append-only event spine
                         │                    │
                         │                    ├─ projections
                         │                    └─ replay/follow stream
                         ├─ context compiler
                         ├─ capability pager
                         ├─ effect firewall
                         ├─ model drivers
                         └─ artifact store / isolated workers
```

Public clients never append arbitrary internal events. The kernel assigns actor
and kind, persists the event, then publishes it. SQLite sequence is the durable
resume cursor; ULID is the global event identity.

The SSE adapter subscribes before capturing a high-water mark, replays the
bounded snapshot in pages, deduplicates buffered live events, and recovers gaps
or lag from durable storage.

## Context compiler

Conversation transcripts are evidence, not the prompt. Ditto builds a task
signature from the current request, active goal, entities, unresolved
constraints, and expected effects. Retrieval spans personal, task, environment,
and conversation lenses.

Durable nodes carry origin, epistemic status, scope, confidence, provenance,
validity, and supersession. Compiler authority is separate:

```text
ranked | user-pinned | policy-required(reason)
```

A model cannot create a pinned node by serializing a field. Token cost is derived
locally. Required invalid context blocks the request rather than disappearing
silently.

## Capability pager

The complete capability universe is virtual address space; model context is RAM.

```text
L0 namespace map
L1 capability card
L2 full provider-neutral schema
L3 lazy isolated runtime
```

Runtime search fails closed against installed placements, prerequisites, allowed
IDs, and minimum effect. Available placements are a set so a remote primary
operation can compose with a local artifact reader. An execution epoch has a
bounded, append-only working set to preserve prompt-cache ordering.

Capability manifests are runtime input. Unknown complements and malformed
runtime metadata fail catalogue load.

## Effect firewall

Effects are not one danger number. They form an orthogonal profile:

```text
access       none < metadata < content < credentials
mutation     none < reversible < irreversible
externality  local < network < human-communication
privilege    user < elevated
```

A lease must permit every dimension. Elevated access alone never grants deletion,
credential access, or messaging. If a lease scopes devices, programs, or
resources, omission of that field is a denial.

The eventual execution path is:

```text
model tool call
→ schema validation
→ argument normalization
→ resource canonicalization
→ capability-specific effect derivation
→ lease authorization
→ canonical invocation
→ isolated executor
```

SSH is placement transport, not a model-facing raw shell.

## Artifacts and evidence

Large outputs live in a SHA-256 content-addressed store. Immutable object metadata
contains only content facts. Task-specific meaning—producer, MIME interpretation,
purpose, session, and task—is rooted in an `artifact.created` event.

Artifact reads reject malformed references and symlink traversal, enforce object
size limits, and verify content through the same descriptor used to return data.

Task completion remains a claim until a task-specific verifier supplies evidence
such as a diff, commit, provider message ID, health response, or artifact hash. A
model stream ending is never completion evidence.

## Model boundary

The next active slice is a provider-neutral model IR. It must retain structured
tool calls, partial arguments, usage, cancellation, continuation, and finish
reasons rather than flattening every provider to text. Provider-specific
advantages are compiled through feature flags; unsupported features are not
advertised.

## Improvement compiler

Most experience remains trace data. Deterministic detectors identify repeated
corrections, retrieval misses, argument errors, approval repetition, latency
regressions, or verifier mismatches. Typed patches pass deduplication, validation,
replay, shadow, and canary stages before promotion.

Kernel code, root policy, credentials, evaluator logic, audit history, pinned
context, and active provider settings are not self-editable.

## Runtime composition

- Rust: daemon, storage, context, capability index, policy, model IR, executor,
  scheduler, and protocol.
- TypeScript/Bun: integration SDKs, browser/app connectors, and gateways.
- Python: optional out-of-process worker only for workloads that require it.
- SQLite and local object storage: mandatory persistence.
- MCP: external capability/resource boundary.
- ACP: IDE/editor client boundary.
- A2A: only for independent external agents.

No Redis, Postgres, vector service, graph database, Docker daemon, or cloud
service is mandatory.

## Repository-native agent operation

`AGENTS.md` contains stable invariants. Scoped instruction files and
`docs/agent` provide progressive disclosure for long-running coding agents.
`NEXT.md` owns the implementation frontier and `HANDOFF.md` owns verified
recovery state. This keeps the development agent's context working set small for
the same reason Ditto keeps its runtime working set small.
