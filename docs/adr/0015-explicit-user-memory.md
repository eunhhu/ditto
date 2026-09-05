# ADR 0015: Explicit user memory through existing context admission

## Status and context

Accepted for Task 008. Tasks 006 and 007 are merged into main. The next product
slice connects durable context to an explicit user workflow before adding a
production embedding worker or another background subsystem. Existing context
admission already owns provenance, supersession, persistence, and recovery;
users currently cannot exercise it from the daemon or CLI.

## Decision

Add a narrow `POST /v1/commands/memory` command containing `session_id`,
`input_event_id`, and optionally `replaces`. It promotes the exact text of an
existing same-session, task-free, user-authored `input.received` event. The
kernel fixes the node kind to Claim, origin to User, epistemic status to
Asserted, scope to Session, lens to Personal, and confidence to 1.0. Identity
is `memory-<lowercase input ULID>` to preserve the context ID's canonical exact-
match form; provenance retains the original uppercase input event ID.
Clients cannot choose node fields, actors, internal kinds, or compiler authority.
Unknown command fields are rejected. This endpoint belongs to the daemon's
existing trusted local-user ingress, not to model tool invocation or a new
multi-user authentication boundary.

One input can be promoted once. An identical retry returns the original durable
event identity without appending or publishing again. A different replacement
for that input conflicts. A correction supersedes one currently active memory
in the exact same session. Validation and commit share the existing context
admission gate; concurrent corrections of one old memory cannot both succeed.
The original event and memory remain immutable historical evidence.

`GET /v1/memories` returns only source-verified active explicit memories for the
requested session. It uses the existing bounded verified context snapshot and
rechecks returned memories against their exact source input. Pages are sorted
by memory ID, accept a strict `after_id` cursor and a limit of 1 through 100,
and include the evaluated high-water and next cursor. Each page is a fresh
consistent snapshot; concurrent changes do not create a frozen multi-page view.
Existing V2 snapshot candidate/byte limits continue to apply. A memory text is
bounded to 4 KiB before admission, bounding a response page's user text to
400 KiB. Global/cross-session memory, deletion, automatic summarization and
automatic model injection remain outside this slice.

## User workflow and failure model

`ditto memory save TEXT` first uses the existing input command, then promotes
that returned input ID. It defaults to the named `personal` session; an explicit
session option selects another isolated memory set. `--replaces ID` corrects
one active memory. `ditto memory list` inspects the current set, and
`ditto memory from-input INPUT_ID` retries promotion of an already recorded
input. There is no automatic HTTP retry or model/provider call.

The two commands are deliberately separate durable steps. Failure after input
capture does not claim a saved memory: the CLI reports the captured input ID
and recovery command. Promotion itself appends one existing version-1 context
event or none. Durable append remains acceptance; projection catch-up failure
returns HTTP 202 and `committed_but_projection_unavailable` with the committed
identity. A matching retry recovers or reports the existing accepted identity.
Other failures expose bounded path-free API errors rather than SQLite details.

## Alternatives, compatibility, and evidence

A second memory database or a background extraction queue would duplicate
existing ownership and introduce recovery work. Reinterpreting every input as
a memory would remove user intent. Atomic input-plus-context admission would
require a new cross-layer transaction contract; explicit input promotion reuses
the existing reliable boundary and permits recovery by immutable input ID.

There is no durable schema or event-version change and no new runtime dependency or
always-running service. The trusted draft API retains its existing semantics;
the shared commit helper preserves its post-append publication behavior.
Rollback removes the new routes/CLI commands; already saved context remains
valid for existing readers and source replay.

Evidence must cover source/authority rejection, exact bounds, idempotent retry,
concurrent correction, scope isolation, paging, restart/cache rebuild,
post-append failure, HTTP translation, and a real CLI/daemon round trip.
