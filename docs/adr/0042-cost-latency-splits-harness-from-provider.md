# ADR 0042: The cost_latency gate measures the harness, and records the provider

Status: accepted on 2026-11-08, on the owner's approval of the split. Amends the
`cost_latency` gate of [Task 017](../agent/tasks/017-evaluation-outcomes.md);
the frozen ceilings of `eval/task017/budgets-v1.json` do not change.

## Context

`cost_latency` compared the p95 of the whole turn with a ceiling frozen from
calibration 3. A turn is two things: the time the provider spends answering the
model requests, and the time Ditto spends on everything else. Only the second
is the harness's to change, and the journals show it is small - the harness's
own share of a turn is 0.0 % at the median and 4 % at p95
([Task 043](../agent/tasks/043-second-generation-bar.md)).

The gate has therefore been measuring the provider's day. Per model request the
same binary took 2.08 s at the median in one campaign and 4.89 s in another
([Task 017](../agent/tasks/017-evaluation-outcomes.md), "What the latency is
made of"). Campaign 10 failed the gate at a turn p95 of 58.9 s while its median
turn took 5.5 s and its requests per turn fell from 2.26 to 2.00: the harness
improved and the gate reported the provider. Widening the ceiling to match the
day would make the gate a record of whichever day was last measured; keeping it
makes every slow provider day a "regression" that no code change can fix.

The report already reserves the field for this: `resources.ditto_overhead_ms`
and `max_p95_ditto_overhead_ms`, null with the reason `not_instrumented`.

## Decision

Budgets get a version. `budgets-v1.json` (`schema_version` 1) keeps its meaning
and stays readable; `eval/task017/budgets-v2.json` (`schema_version` 2) selects
the rule below. **No frozen number changes**: every ceiling in v2 equals its v1
value.

For each attempt the runner records

- `harness_ms` - the turn's completion less the sum of that turn's model request
  spans (`model.requested` to the request's terminal event, on the arrival
  clock), stored in the existing `resources.ditto_overhead_ms`. It is `null`
  (`not_observed`) when either is unobserved, or when the spans exceed the turn,
  which is a clock fault and not evidence about the harness. A scheduled attempt
  measures it per run, as the largest of each run's completion less that run's
  spans.
- `provider_ms` - the sum of those spans, already recorded as `model_ms`.

Under budgets version 2 the gate **fails or blocks on**:

- `harness_ms` p95 against the frozen turn ceiling (`max_p95_ditto_overhead_ms`
  is that same number, and the validator rejects any other value);
- model round trips (`model_calls`) and input tokens, as in version 1;
- scheduled admission and due-to-claim latency, which are the harness's own;
- tool calls, output tokens and provider cost, as in version 1, because this
  decision is about which time is the harness's, not about what else is allowed.

It **records, without gating**, per family and profile: the absolute turn p50
and p95, first-progress p50 and p95, `provider_ms` p50 and p95, and the mean
time per model request in each attempt (p50 and p95 over attempts). The last two
are the same-day provider baseline: they say what the provider's day was, in
the same report that says what the harness did with it. They carry
`"gating": false` in the gate evidence.

The run manifest records `budgets_version`; validation refuses a report whose
budgets file is another version than its run used, and recomputes the gate with
the version it recorded. Older run manifests omit the field and mean version 1.

## Consequences

- A slow provider day can no longer fail the gate, and a regression of the
  harness's own time or round trips still can. Campaign 10's numbers would have
  decided on 2.00 requests and a harness share in milliseconds, not on a 58.9 s
  p95.
- The harness ceiling is the old turn ceiling (3.8 s for schedules, 19-48 s for
  the rest), so it will pass a harness orders of magnitude slower than the
  journals show today. That is deliberate in this
  change - no number moved - and it makes the gate a guard against a runaway,
  not a statement of the bar. The bars that bind the harness are in Task 043
  (share of turn time under 15 %, warm admission under 5 ms, requests per turn
  under 1.3) and tightening this ceiling is a separate decision on measured
  harness numbers.
- First-progress time is no longer gated; the 2 s time-to-first-token bar of
  Task 043 remains unmeasured and has no gate. Absolute turn time is recorded
  every campaign, so a provider-adjusted reading stays possible by hand.
- The measurement is as honest as the arrival clock: `harness_ms` includes
  tool execution, storage and streaming overhead, which is the point.

## Alternatives

**Widen the ceilings to the latest day.** Rejected: it hides a harness regression
behind whatever the provider did when the number was last frozen.

**Normalize by a provider baseline** (a ratio to calibration's per-request
time). Rejected: it needs a baseline that is itself a day's measurement, and the
journal already supports the direct subtraction.

**Drop latency from the gate.** Rejected: the harness's share would then be
ungated, and a slow harness is a real failure.

## Rollback

Run with `budgets-v1.json`; the runner and validator treat it exactly as
before, and `resources.ditto_overhead_ms` is recorded but not gated.
