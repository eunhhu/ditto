# ADR 0019: One-shot scheduled requests with at-most-once dispatch

Status: accepted for Task 012.

## Contract

A local user can durably schedule one existing read-only agent request with an
explicit RFC 3339 due time and exclusive latest-start time. Both are canonical
UTC milliseconds. New requests must be future-due, within 365 days, with a
positive grace window of at most 24 hours. An identical ID/content/time retry
returns the original schedule, including after its due time; changed retries
conflict. At most 100 pending schedules are admitted per data directory.

This slice accepts text and session scope only. It grants no local process or
write permission. Dispatch reuses the current context verification, read-only
capability paging, bounded model turn, cancellation and unverified answer
semantics. No provider, actor, internal identity, lease or completion assertion
can be supplied by a client. Recurrence and scheduled effect grants are deferred.

The operator must explicitly enable the existing provider for model execution.
Disabled startup may retain and expire pending requests but makes no model call.
Scheduling itself never calls a model. Loopback-only schedule mutation prevents
the unauthenticated remote escape hatch from granting future execution.

## Persistence and delivery

The event journal remains authoritative. Schema 5 adds a compact SQLite schedule
index in the same database, updated in the event append transaction and rebuilt
from schedule events at startup. Pending selection and identity reads use indexed
queries, never rescan completed history during steady-state operation. Recovery
streams schedule events; it retains no transcript or pending prompt catalogue.

The start window is checked immediately before claim; it bounds the admission
attempt, not provider response or completion time. The existing turn deadline
continues to bound an admitted execution.

The kernel reserves a private run identity at admission. Manual admission cannot
use a reserved identity. Under the existing single-run gate, the scheduler first
commits `schedule.claimed`, then admits the existing run and dispatches it. Claims
are never automatically retried, even if a crash happens before run admission.
That gap is reported as interrupted, not pending or successful. This is honest
at-most-once dispatch, not exactly-once delivery or guaranteed external effects.
Claimed runs with durable terminals retain their original status after restart;
nonterminal runs without a live owner are interrupted. A new explicit schedule
identity is required to try interrupted or failed work again.

Pending cancellation and expiry are durable terminal transitions. Active
cancellation is journaled before signalling the existing run token. Cancellation
and dispatch share the run gate, so an accepted pending cancellation cannot
start later. The first due request waits while manual/scheduled work owns the
single slot; expiry continues to be enforced for all pending requests.

The supported deployment is one kernel owner and its clones per data directory.
As with existing WAL/NORMAL storage, process-restart durability is covered;
power-loss durability and concurrent independent daemon writers are not claimed.

## Wakeup and cost

One owned scheduler future waits on the nearest due/expiry timer, admission or
cancellation notification, active-slot release, or shutdown. Empty queues have
no timer. Disabled providers wait for expiry; busy execution waits for slot
release or expiry. There is no periodic model heartbeat, automatic provider
enablement, automatic run retry, new service or dependency.

UTC wall time is rechecked before claim. Time-zone offsets describe an instant,
so there is no local DST interpretation. Timers use monotonic delays computed
from wall time; a forward clock step can delay recognition until the existing
timer or another event wakes the scheduler. Backward steps cannot start a request
early. Clock-change monitoring and recurring calendar semantics are deferred.

## Alternatives, migration and rollback

An external cron service, periodic model heartbeat, and general recurring job
framework add infrastructure or ongoing work before this contract is useful.
Automatic replay of a claimed attempt risks duplicate provider work. A mutable
queue as a second authority risks disagreement with the existing journal. This
slice instead adds one derived index, one scheduler future and narrow commands.

Schema 5 preserves all existing events and model-turn wire formats. Older binaries
reject the newer schema rather than interpreting future work incorrectly. Stop
the daemon and back up its data directory before a version rollback; use a
compatible binary or restore the complete pre-upgrade backup. Do not decrement
the schema version or delete claim events. Startup replay reads only schedule
events in sequence using a dedicated index; startup work grows with schedule
history, while steady-state selection is bounded by 100 pending headers. Storage
or scheduler admission failures stop the daemon visibly instead of silently
abandoning its scheduler. Exact elapsed-time/RAM baselines remain a later task.

## Required evidence

Test restart before claim, after claim/before admission, during a run and after
its terminal; duplicate retries, cancel/claim ordering, busy-slot release,
expiry with a disabled provider, one scheduler owner, queue bounds, index rebuild,
source/scope rejection and real CLI/HTTP submission/status/cancellation. Run the
canonical repository gate without live paid model calls.
