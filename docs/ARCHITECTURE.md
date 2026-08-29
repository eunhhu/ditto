# Architecture map

Ditto follows the responsibility split in [`PLAN.md`](PLAN.md): the model owns
intent and judgment; the harness owns context, capabilities, effects,
persistence, and execution lifetime.

## Current vertical slice

| Boundary | Package | Scaffold status |
| --- | --- | --- |
| Event spine | `ditto-event-store` | SQLite WAL, append-only triggers, filtered replay |
| Artifact store | `ditto-artifact-store` | SHA-256 content addressing and integrity checks |
| Task ledger | `ditto-task-state` | Event-reduced tracked-path states |
| Context IR | `ditto-context-graph` | Typed nodes, provenance, scope, epistemic validation |
| Context compiler | `ditto-context-compiler` | Local token-budget compilation and receipt |
| Capability pager | `ditto-capability-index` | Hard filter, lexical retrieval, append-only epoch working set |
| Effect firewall | `ditto-effect-policy` | Opaque, expiring, call-bounded leases |
| Capability runtime | `ditto-capability-runtime` | Invocation and worker JSON contracts |
| Executor | `ditto-executor` | Shell-free local `device.process.run` with evidence |
| Model boundary | `ditto-model-driver` | Streaming trait, feature flags, deterministic dev driver |
| Kernel | `ditto-kernel` | Context → page-in → stream → evidence event loop |
| Client protocol | `ditto-protocol` | One JSON Lines event protocol for all clients |
| Daemon and CLI | `apps/daemon`, `apps/cli` | Unix socket, reconnect/replay, cancellation handle |

The development model driver proves streaming and persistence without claiming
provider integration. A production frontier provider driver is the next Runtime
Spine increment.

## Durable layout

```text
.ditto/
├── state.db
├── state.db-shm
├── state.db-wal
├── objects/
│   └── sha256/
└── ditto.sock
```

Important state must enter `state.db` before it is presented as durable. Large
content enters the object store, and events retain only the artifact reference.

## Package direction

```text
apps → kernel → domain crates → protocol
                 │
                 └→ isolated worker contracts
```

Domain crates do not depend on apps. External capability implementations use
JSON Lines, Unix sockets, WASI, MCP, or remote transport; they are never loaded
into the daemon through dynamic imports.

## Explicitly deferred

- frontier provider HTTP/SSE adapter
- persistent context graph projections and hybrid dense retrieval
- SSH transport and device registry
- browser and Program Cell workers
- WebSocket/web inspector and messaging gateways
- Improvement Compiler promotion pipeline
- MCP, ACP, and Agent Skills importers

These boundaries exist in the tree, but deferred features contain no fake
success paths.
