# ADR 0017: One-shot local sort execution and verified artifacts

## Status

Accepted for Task 010. Extends ADR 0012's previously deferred process placement
and worker ingress, and adds the first contract-specific completion producer.

## Decision

The first local process profile is `artifact.sort`: byte-ordered UTF-8 line
sorting, optionally removing exact duplicate lines. A typed CLI/HTTP command
supplies at most 64 KiB and 4096 lines, a session and an idempotency ULID. The
kernel stores the input as an artifact, derives the canonical invocation, and
grants one exact-resource lease for at most 30 seconds. The client selects no
program, arguments, environment, path, effect, lease, actor, or evidence.

The capability owner consumes an affine ExecutionClaim, checks its exact
invocation/revision/resource/expiry, and verifies the supplied input hash before
dispatch. The only executable is the trusted OS `/usr/bin/sort`, with fixed
arguments (optional `-u`), stdin/stdout pipes, a cleared environment plus
`LC_ALL=C`, and a disposable private working directory. There is no shell,
PATH lookup, arbitrary executable registration, or network/credential operation.
The implementation's effects are content/reversible/local/user: reading the
exact artifact and producing a new immutable artifact, with bounded disposable
scratch. Neither a working directory nor a lease is an OS sandbox. This profile
trusts the installed OS sort implementation; arbitrary programs require a new
effect/containment contract and are not enabled by LocalProcess placement alone.

While owned by the runtime, the process has a five-second deadline and output is capped at input bound plus
one byte. CPU time, file growth and core dumps are bounded on supported Unix
targets. Cancellation, timeout, overflow and I/O failure kill its process group
and reap the child. Drop kills outstanding work. No process starts at discovery,
startup or replay. Linux and macOS are supported; other platforms fail closed.
Abrupt daemon SIGKILL/host failure cannot run managed cleanup; pipe closure and
OS CPU/file limits remain, but this slice makes no cross-platform wall-clock
containment promise after owner-process death. Recovery remains interrupted and
never dispatches that identity again.

The verifier independently checks nondecreasing byte order and exact input-line
multiplicities (or set equality for unique mode), including LF termination.
Exit code zero alone never completes a task. Only the sealed verified output
can be persisted as result evidence. A single `task.completed` event contains
the exact input/output references, claim/invocation identity and verifier
version; status checks the artifacts again. This certifies this sort request,
not a model's broader goal. Invalid output creates failure, never completion.

The existing kernel-owned single execution slot is shared with model runs.
Admission is durable before dispatch and survives client disconnect. Identical
retries return prior state; different content/options conflict; interrupted
work is never automatically rerun. No completed-task map or background loop is
added. Process events use their own versioned payloads and kernel-generated
`turn_*` correlation so existing indexed boundary lookup remains applicable.
The existing artifact-read/model trace format is unchanged.

Explicit sort submission is authorization for this narrow local operation and
requires no model/provider. Automatic model dispatch and general approval
fulfillment remain separate contracts. Process POST is enabled only on loopback
listeners, even if the legacy remote-read escape hatch is selected.

## Alternatives, compatibility, and rollback

An unrestricted shell plus a path lease cannot enforce its claimed resource
scope. A new sandbox framework or worker daemon adds infrastructure before a
useful bounded profile exists. Running sorting inside the daemon would not
exercise the real process lifetime/claim boundary. Rewriting the existing
read-only replay machine into a general workflow engine is unnecessary here.

The process-owning crate is a real execution/verification boundary and reuses
existing Tokio, libc, tempfile and SQLite dependencies. It has no new service
or paid call. New commands, capability and event payloads are additive; no
database migration is needed. Old binaries retain raw events but cannot inspect
the new sort result contract. Rollback disables this new ingress and retains
artifacts/events. Broad process execution remains deferred, and no performance
or zero-overhead claim follows from functional tests.

## Required evidence

Real process success/unique output, verifier rejection of missing/extra/unsorted
output, exact-claim/revision/input checks, expiry, cancellation, timeout,
overflow, child reaping, no inherited environment, duplicate admission races,
scope isolation, crash/reopen with no execution, corrupt-result rejection,
public authority-field rejection, and built CLI/daemon operation without a model.
