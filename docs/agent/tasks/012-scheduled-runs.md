# Task 012: One-shot scheduled requests and restart recovery

Contract: [ADR 0019](../../adr/0019-one-shot-scheduled-runs.md).

Status: complete. Local and Linux CI checks passed; see
[source-bound evidence](012-evidence.md) and [PR #17](https://github.com/eunhhu/ditto/pull/17)
for final-head checks and merge state.

## Exit criteria

- Local CLI/HTTP schedule, inspect, pending-list and cancel work end to end.
- Existing read-only agent requests dispatch once, inside their explicit UTC
  start window, using the existing single execution slot and current context.
- Journal-backed indexed scheduling survives restart; every claimed attempt is
  inspectable as running, terminal or interrupted without automatic retry.
- Pending cancellation/expiry prevent later execution; disabled/busy/idle
  scheduler paths do no model housekeeping or periodic polling.
- Queue/resource bounds and public trust boundaries have regression evidence.
- The canonical check, required actual CLI scenario and CI pass; documentation
  reports measured evidence and remaining limits honestly.

## Scope

One-shot, text-only, read-only agent requests. Recurrence, scheduled process
permissions, arbitrary commands, notification delivery and cross-process daemon
ownership are not implemented by this task.
