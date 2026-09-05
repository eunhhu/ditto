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
                         │                    ├─ context-projection.db
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

Durable context uses the same authority boundary: the kernel admits only a
trusted, non-deserializable session/task draft and emits the fixed
system-authored `context.node.recorded` event. The event spine remains the sole
durable source of truth. The separately stored `context-projection.db` is a
schema-4, checkpointed, WAL-backed cache that can be deleted and rebuilt from
canonical events; it never replaces event history. Startup and recovery replay
the source in bounded pages and issue a process-local verification proof for
the exact event anchor, global digest, and compact per-session identity index.
Normal retrieval and admission then use only a bounded event delta, exact
source-event lookups, and proof-gated index lookups. Global and per-session
digest chains cover canonical node, provenance, causation, scope, and
supersession metadata, while fixed entry, byte, event, payload, and work limits
reject rather than truncate an over-limit operation.

The SSE adapter subscribes before capturing a high-water mark, replays the
bounded snapshot in pages, deduplicates buffered live events, and recovers gaps
or lag from durable storage.

## Context compiler

Conversation transcripts are evidence, not the prompt. Ditto builds a task
signature from the current request, active goal, entities, unresolved
constraints, and expected effects. Retrieval spans personal, task, environment,
and conversation lenses.

Durable nodes carry origin, epistemic status, scope, confidence, provenance,
validity, and supersession. Version 1 admits only session and task scope through
the kernel admission boundary; public clients and models cannot mint these
records. Identity is session-wide `(session_id, node_id)`, while supersession is
restricted to the exact scope. Canonical admission requires matching actor
evidence for the claimed origin and derives causation from the cited source with
the greatest durable sequence. Compiler authority is separate:

```text
ranked | user-pinned | policy-required(reason)
```

A model cannot create a pinned node by serializing a field. Token cost is derived
locally. Required invalid context blocks the request rather than disappearing
silently.

Context and capability retrieval can be composed through one bounded V2
`TaskQuery`. The read-only joint working-set operation captures one projection
high-water, compiles the context snapshot, and pages capabilities while
preserving lexical eligibility and capability hard filters. Production is
honestly lexical-only. An explicitly injected embedding provider is a
test/explicit-local-composition seam; provider failure is typed and never
silently falls back to lexical-only or returns a partial working set.

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

The joint context/capability working set shares the bounded V2 query but does
not expose embedding vectors or capability credentials to the model. Capability
retrieval remains read-only in this slice; execution and effectful invocation
are separate deferred boundaries.

Capability package headers are bounded discovery input. Active unknown
complements and malformed headers fail catalogue load. Full manifests remain
runtime input: selected paging verifies their digest and exact header projection
before schema binding or invocation. Header-backed startup/search read no full
manifest bodies; headerless packages use a metered bounded compatibility read.
No full-manifest cache accumulates as packages are selected. See ADR 0014.

## Effect firewall

Effects are not one danger number. They form an orthogonal profile:

```text
access       none < metadata < content < credentials
mutation     none < reversible < irreversible
externality  local < network < human-communication
privilege    user < elevated
```

Every selected authorization source must permit every dimension. Elevated
access alone never grants deletion, credential access, or messaging. If a lease
scopes devices, programs, or resources, omission of that derived field is a
denial.

The canonical authority path is:

```text
untrusted model tool call
→ exact execution-epoch/capability-revision resolution
→ JSON Schema instance validation
→ bounded capability-specific normalization and effect/resource derivation
→ sealed canonical invocation
→ policy authorization or approval-required outcome
→ sealed invocation-bound permit
→ isolated executor (deferred except existing bounded artifact.read)
```

The model call has no effect, resource, device, program, placement, lease,
approval, verification, or idempotency authority. Policy selects static policy
or a harness-side lease only after canonical derivation. Permits are sealed,
expiring, and bound to one invocation digest.

SSH is placement transport, not a model-facing raw shell.

## Artifacts and evidence

Large outputs live in a SHA-256 content-addressed store. Immutable object metadata
contains only content facts. Task-specific meaning—producer, MIME interpretation,
purpose, session, and task—is rooted in an `artifact.created` event.

Artifact reads reject malformed references and symlink traversal, enforce object
size limits, and verify content through the same descriptor used to return data.
The builtin `artifact.read` surface accepts only canonical
`artifact:sha256:<hex>` references and bounded ranges. A system-authored
`artifact.created` event must root the exact object in compatible session/task
scope before the kernel can return its deterministic projection.

Task completion remains a claim until a task-specific verifier supplies evidence
such as a diff, commit, provider message ID, health response, or artifact hash. A
model stream ending is never completion evidence.

## Model boundary

`ditto-model` owns the versioned provider-neutral request and validated semantic
stream. It retains structured tool calls, partial arguments, typed reasoning
and replay state, usage, cancellation, continuation, and finish reasons rather
than flattening providers to text. Driver descriptors keep request controls
separate from emitted features, and unsupported features fail before output.

`ditto-model-openai` owns the first production adapter: a closed `gpt-5.6`
Responses profile with a fixed HTTPS origin, redacted transport-only
credentials, deterministic request projection, bounded raw-stream correlation,
and explicit ephemeral or provider-managed response storage. Its terminal is a
model terminal only; the adapter neither selects itself for the daemon nor makes
a task-completion claim.

The semantic kernel owns the first injected-driver continuation loop. It
compiles context, pages the complete `artifact.read` schema into one bounded
epoch, persists validated model/tool transitions before publication, and can
replay the complete turn without provider or artifact I/O. A final assistant
response remains explicitly `unverified`, and the loop never emits
`task.completed`.

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
