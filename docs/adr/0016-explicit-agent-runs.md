# ADR 0016: Explicit, bounded personal-agent runs

## Status

Accepted for Task 009.

## Context

The CLI can record input and memory, but cannot ask the existing model/tool loop
to answer. Connecting it must not turn ordinary input, daemon startup, retries,
or recovery into implicit model calls. Client disconnects must not erase the
identity of potentially billable work. Personal context must retain its source
and scope, and a model answer must remain unverified.

## Decision

Add typed run submission, lookup, and cancellation commands. A client supplies
an uppercase canonical ULID request ID, a canonical session, and bounded text.
The kernel derives task identity `run_<request_id>`, generates turn identity,
and durably appends the existing user-input event before dispatch. Its additive
`agent_run` metadata fixes version 1 and the request ID; clients cannot supply
internal event metadata. The old input command remains record-only.

One clone-shared kernel slot admits at most one live run, without a queue or
periodic work. Exact retries return the existing run; changed text conflicts.
The event spine owns identity and terminal state. Two indexed boundary reads
inspect the original input and latest correlated event, without loading the
transcript. Completed runs leave no growing in-memory registry. Unfinished
durable work without a live owner is `interrupted`, never silently rerun.
This is at-most-once local admission, not exactly-once provider execution.

An accepted task owns its execution independently of an HTTP connection.
Cancellation signals the existing bounded loop; graceful daemon shutdown closes
admission, cancels the active run, and waits for its terminal. Process crashes or
storage failures can leave an interrupted run. No synthetic success or automatic
retry is created. The supported writer remains one KernelInner and its clones.

The loop selects source-verified current session/task context through the
existing bounded projection and lexical compiler. It never includes an entire
conversation transcript or calls a model to extract memory. Snapshot/retrieval
failure fails the run before provider I/O. Corrections affect subsequently
compiled runs; a running turn retains its recorded capsule.

This composition retains the existing turn compiler's V1 lexical semantics;
it does not silently substitute the separate V2 joint retrieval contract.
Snapshot candidate/byte bounds still apply. Selected provenance uses exact
event-ID lookups, and task-completion checks use a scoped indexed predicate,
rather than replaying unrelated session history on each request. Artifact-root
authorization still uses the existing paginated scope search; measuring and
improving that path remains separate from the present admission work.

Agent runs allow a direct answer by using Auto tool choice from the first model
request. The original injected artifact-read API retains its Required first
request. Replay derives that distinction from the versioned input metadata and
continues to validate every request. New readers preserve old traces; old
readers fail closed on the new Auto-first traces. Artifact authority, leases,
turn limits, and the unverified terminal do not change.

The daemon defaults to no provider. Explicit operator selection of the existing
closed OpenAI profile enables run submission and reads its key from the
transport-only environment boundary. No key is a CLI argument or protocol field.
Enabled execution requires loopback binding, even with the legacy remote escape
hatch. Storage is ephemeral at the provider. This does not claim free inference;
development validation uses injected model/transport fixtures and makes no paid
requests. No extra runtime, database, worker, or background model call is added.

## Alternatives

- Reusing input POST for execution would unexpectedly change its cost contract.
- A durable job queue and automatic restart would add scheduler semantics before
  delivery, approval, and effect recovery are specified.
- An in-memory result map would grow with use and lose retry identity on restart.
- Forcing an artifact read for every question would waste work and prevent direct
  answers. Broadening to arbitrary tools belongs to the effectful execution slice.

## Compatibility, measurement, and rollback

The run routes and CLI are additive. Event-store schema 3 adds lookup indexes,
without rewriting events; rollback needs a schema-3-aware binary. Existing
memory and turn payloads keep their meaning. Disable provider selection to stop
new model work while retaining run inspection. Tests cover retry races, scope,
interruption, cancellation, corrected memory, direct answers, read continuation,
and no-I/O replay. Resource counters and test timing are not RSS/latency or
zero-cost claims; the product roadmap requires measured baselines separately.
