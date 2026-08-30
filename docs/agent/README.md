# Agent context router

This directory is the operating system for long-running coding agents. Load the
smallest useful working set.

## Always read

- [`NEXT.md`](NEXT.md): ordered implementation frontier and exit criteria.
- [`HANDOFF.md`](HANDOFF.md): verified repository state and known risks.

## Read by task

| Task | Read next |
| --- | --- |
| Long autonomous run | [`OPERATING-MODE.md`](OPERATING-MODE.md) |
| Tests, CI, release confidence | [`QUALITY-GATES.md`](QUALITY-GATES.md) |
| Architecture or public contract | [`../architecture.md`](../architecture.md), relevant [`../adr`](../adr), and [`DECISIONS.md`](DECISIONS.md) |
| Event protocol | [`../specs/event-protocol.md`](../specs/event-protocol.md) |
| Context compiler | [`../specs/context-ir.md`](../specs/context-ir.md) |
| Capability retrieval/runtime | [`../specs/capability-manifest.md`](../specs/capability-manifest.md) |
| Current implementation task | the task file linked from [`NEXT.md`](NEXT.md) |

## Do not preload

Do not read all ADRs, all task files, and every crate before beginning. Search
for the owning contract, inspect its callers, and expand context only when a
concrete dependency or uncertainty requires it.
