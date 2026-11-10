# 0043: admission is the daemon's span, not the store's fsync tail

- Status: accepted
- Date: 2026-11-08
- Supersedes nothing; amends the `cost_latency` gate introduced by 0042.

## Problem

`budgets-v2` began gating `admission_ms` on the schedule family, and campaign 11
failed it at p95 6.20 ms against the frozen 5 ms ceiling. The number was traced
before any ceiling was touched.

`admission_ms` is the client's round trip for `POST /v1/commands/schedule`. The
only durable work behind that request is the append of one command event. Run the
same release binary against the same request 100 times:

| store | p50 | p95 | p99 | max |
| --- | --- | --- | --- | --- |
| tmpfs | 0.74 ms | 0.93 ms | 1.07 ms | 15.16 ms |
| SD card | 2.35 ms | 6.20 ms | - | 10.38 ms |

The value is the store's `fsync` tail. The harness's own admission of the turn -
the scheduler claiming an occurrence and the turn being admitted - is 0-1 ms in
the same journals. The 5 ms ceiling came from a 20-sample p95 on that same path
(campaign 9: median 2.37 ms, max 5.11 ms), so it sat at the noise floor of its own
sample: one I/O hiccup flips the gate. Recording an `fsync` latency as "warm turn
admission" measures the host, not the harness, and it makes the gate unstable
against the thing the bar is about.

## Decision

Admission is measured where the harness admits a turn: from the journal's
`schedule.occurrence.claimed` to `input.received`. That span is recorded as
`resources.daemon_admission_ms` and it is what `budgets-v3` gates on the schedule
family. The client's round trip stays recorded as `resources.admission_ms` and is
no longer gated.

No ceiling moves. `budgets-v3` repeats every number from `budgets-v2`; the new
field must equal the frozen `max_p95_admission_ms`, which a manifest check
enforces, so a manifest cannot quietly relax admission to pass. `budgets-v1` and
`budgets-v2` remain readable and keep their own semantics: v1 and v2 gate the
client round trip, v3 gates the daemon span.

## Consequences

- The gate now answers "does the harness admit a turn quickly" instead of "how
  slow is this SD card".
- The journal's millisecond resolution bounds the measurement: the span is
  quantized to 0, 1, 2 ms. That is enough for a 5 ms ceiling but it cannot show a
  sub-millisecond regression; a finer gate would need the daemon to journal the
  span with its own precision, which this ADR does not require yet.
- A slow store still shows up in the evidence - as a recorded number that a
  reader can see - but it can no longer fail the harness.
- `attempt.resources.daemon_admission_ms` is required in the record schema and
  takes a `not_applicable` null reason off the schedule family.
