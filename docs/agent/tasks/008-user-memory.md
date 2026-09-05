# Task 008: Explicit user memory

## Status

Complete on `dev/task-008-user-memory`, based on main merge
`32bb4d16893403db6dd4b108d51ec7832980a934`.
[Verification evidence](008-evidence.md) records all exit criteria, exact code
identities, local gates, and a repeatable CLI/daemon smoke test.

## Contract and exit criteria

Implement [ADR 0015](../../adr/0015-explicit-user-memory.md) as one CLI-to-daemon-
to-kernel path using existing durable context and event ownership.

- Users can save, inspect, and correct exact user-authored text within a session.
- Public commands cannot mint actors, internal kinds, node provenance, or
  model-inferred user assertions. Reject source/scope mismatches and oversized
  or noncanonical input before context append/publication.
- Matching retries are idempotent; conflicting retries and concurrent stale
  corrections do not create multiple active replacement memories.
- Reads are bounded, source-verified, paginated, and stable across restart or
  projection deletion. Memory operations invoke no model/embedding provider.
- Partial input capture and accepted-but-unprojected memory have explicit
  inspectable outcomes; existing context-admission behavior remains unchanged.
- Domain, HTTP adapter, and real CLI scenarios pass; canonical gate, MSRV,
  diff checks, and factual handoff/evidence are complete before closing.
