# 0041 - A read-only tool may run before the model asks for it

Status: proposed
Date: 2026-11-07

## Context

Campaign 9 measured 2.34 model requests per turn: 1.96 for memory turns and
2.69 for artifact turns. The journals show where the extra request comes from.
The model asks for a read-only tool, the harness runs it and returns the result,
and the model answers in a second request. The same journals show the harness's
own share of a turn at 0.0 % median and 4 % p95 - a turn is the provider's time
and nothing else - so the only lever left is the number of round trips.

The harness already knows, before the first request, which read-only work the
turn implies: the compiled context names the memories it selected, the
capability page names the tools the turn may use, and an attachment or a sort
permission is fixed at admission. Asking the model which of those to call is a
round trip spent on a decision the harness has already made.

## Decision

After tool selection and before the first model request, the harness may run
**read-only** tools whose input the turn already fixes:

- `memory.search` when the compiled context's recall is below its threshold, or
  when the request names a fact the selected memories do not carry;
- `artifact.read` when the turn has an attachment and the request asks about
  its content;
- `artifact.sort` when a sort permission was granted with the turn.

The prefetched text is **appended after selection**, with its own canonical
receipt reason (`prefetch-attachment`), so it rides with the compilation instead
of competing for its budget: a turn that carries an attachment can never push a
memory it needs out of the selection. It is also journaled with the compilation
rather than as a tool call, so replay derives it and the request digest is
unchanged. At most one prefetch runs per turn, under the same lease and verifier
the model call would use. Its result is journaled as `tool.requested` with
`origin: prefetch` (a new field, `serde(default)`) followed by the ordinary
`tool.output`, and the compiled prompt carries the result before any model
request. The tool stays offered in the same request, so the model can still ask
for a different call.

## What this is not

It is not a model call: no inference is added, and the "no model call solely for
housekeeping" invariant is untouched. It is not a side effect: every prefetch
candidate is read-only, idempotent and reverses nothing. It is not a hidden
answer: the model still decides what to say, and a prefetched result the model
ignores costs one small tool call, not a turn.

## Alternatives

**Let the model ask, as today.** Keeps the flow simple and costs the round trip
that this ADR exists to remove - 2.34 requests per turn against a bar of 1.3.

**Run the tool in parallel with the model's first request.** The result would
arrive after the model has already committed to an answer or a call, so it saves
latency but not a round trip, and it needs a cancellation path when the model
asks for something else.

**Answer the tool's question in the harness.** This would put the answer in the
harness's voice, which the model owns. Rejected.

## Risks

A wrong prefetch spends one tool call and one prompt line. The bounded count
(one per turn) and the read-only set keep the worst case at the cost the turn
already pays. Measured by requests per turn over the full suite; the slice is
accepted only if that falls below 1.3 while recall at 5 stays 1.000.
