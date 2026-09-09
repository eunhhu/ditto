# ADR 0020: Bounded recurring read-only schedules

Status: accepted for Task 013. Extends ADR 0019 without changing one-shot inputs.

## Contract and limits

An explicit local repeat command specifies the original text/session, first due
and exclusive latest-start times, an interval in whole seconds, and a finite
occurrence count. Supported intervals are 60 seconds through 31 days, counts are
2 through 1000, and the final due time must be within 365 days of admission. The
first window retains ADR 0019's UTC millisecond rules; its width must not exceed
the interval or 24 hours. Occurrence numbers start at one. Due and expiry instants
are the original instants plus `(occurrence - 1) * interval`, without drift from
completion time. This is elapsed-time recurrence, not a named-zone calendar or
cron expression. Existing wall-clock observation limits still apply.

The fixed policy is to skip expired occurrences and execute at most the one
currently eligible occurrence. On wake/restart, arithmetic identifies an expired
range and one durable event records the entire range. There is no replay of old
model work, no overlapping start windows and no background housekeeping model.
Pending one-shot schedules and active repeat definitions share a global capacity
of 100. Prompts and future child runs are not preloaded or precreated.

Each due occurrence uses the existing shared execution slot, current verified
session context, read-only capability scope and bounded model turn. The provider
remains explicitly configured and disabled by default. Repeat commands grant no
sort/process permission, arbitrary action, credential or provider selection.

## Journal, identity and recovery

Schema 6 adds a derived compact repeat index in the same database. A
`schedule.repeat.requested` user event owns the original command. An occurrence
claim is a single scheduler event that atomically creates a claimed child in the
existing schedule index, reserves its private run ID, and advances the parent
cursor. The claim references its parent source and previous parent transition;
the child derives text/time/scope from that original source instead of copying a
prompt into every occurrence record. Execution starts only after this commit.

A crash before claim leaves the occurrence available within its window. A crash
after claim never retries that occurrence; its child status is interrupted until
durable run evidence proves another terminal. Later distinct occurrences remain
authorized by the original finite repeat command. A failed or interrupted run
does not implicitly cancel the entire series. Skipped ranges advance the same
cursor and retain explicit missed counts without creating child rows or calls.

Repeat cancellation is journaled under the shared run gate, stops all future
claims and signals the latest child if it owns the active slot. Cancellation
remains durable across a crash between persistence and signalling. Cancelling a
child alone only cancels that occurrence. Parent status distinguishes active,
exhausted and cancelled; exhausted means the finite timetable was consumed, not
verified model-goal completion. The latest child retains normal run evidence;
older claimed children remain inspectable through their original schedule IDs.

One scheduler future selects the earliest due work across both queue types and
services expiry while disabled or busy. Empty future-work sets have no timer.
Typed loopback HTTP/CLI expose repeat creation, inspection, compact active-list
and cancellation. An identical normalized retry returns the original series;
changed text, times, interval or count conflicts. No automatic retry is added.

## Migration, alternatives and evidence

The journal remains the sole authority. Both indexes rebuild in one streaming
schedule-only sequence pass; normal work uses bounded indexed headers and exact
source lookups. Schema 5 one-shot events retain their meaning. Schema 6 binaries
reject malformed repeat histories; older binaries reject the newer schema. Stop
the daemon and restore a complete compatible backup for downgrade. Never delete
claims or decrement the schema version to make a rollback appear safe.

Precreating every occurrence would scale retained work with the full timetable;
replaying every missed interval would create a restart workload and surprise
provider charges. A separate cron service or calendar library adds maintenance
before named-zone calendar behavior has been requested. The chosen arithmetic
policy uses the existing journal, timer, admission and model paths with no new
dependency. Startup replay cost still grows with recorded schedule history and
is not claimed to be zero; this task does not establish end-to-end benchmarks.

Required evidence includes old one-shot migration, atomic parent/child claims,
source/index validation, long missed ranges, exact-boundary eligibility, shared
queue capacity and slot ownership, parent/child cancellation, owner-loss recovery,
no duplicate calls and actual CLI/HTTP operation with an injected model. Run the
canonical gate, production disabled-provider smoke and CI before merging.
