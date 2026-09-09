# Task 013: Bounded recurring schedules

Contract: [ADR 0020](../../adr/0020-bounded-recurring-schedules.md).

## Exit criteria

- Typed loopback CLI/HTTP creates, inspects, lists and cancels finite repeats.
- Occurrences use stable anchor arithmetic and private independent run identities
  through the existing read-only run and single execution slot.
- Parent advancement and child claim are atomic and journal-backed. Reopen never
  repeats a claimed occurrence, including loss before admission or during a run.
- Missed ranges are skipped in bounded work without model calls or child creation.
- Parent cancellation stops future claims and signals its active child; terminal
  and interrupted children remain inspectable without false verified completion.
- One-shot compatibility, shared bounds, public trust rejection, source validation,
  real CLI integration, the canonical gate and Linux CI pass.
- Documentation, inspectable evidence and the completed implementation agree.

## Scope

Elapsed-time intervals, explicit finite counts and read-only model work. Named
time zones/calendar cron, indefinite repetition, scheduled effects and notification
delivery remain deferred. Existing single-owner and process-restart limits apply.

## Verification state

Complete. The final implementation passed the local canonical gate, MSRV check,
both production smoke scripts and Linux CI. [Evidence](013-evidence.md) records
the immutable implementation, source identities and scenarios for this finite
read-only repeat slice.
