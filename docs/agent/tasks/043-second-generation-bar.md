# 043 - The second-generation bar

Task 017 measures Ditto against itself. This file states what "a generation
ahead of the 24/7 agent harnesses that exist now" means as numbers, so the
work after the evaluation is aimed at a bar and not at a feeling.

The comparison targets are the harnesses in use today: a single agent loop with
a fixed tool schema, the whole memory in the prompt, tools called one at a time,
and every turn paying a full round trip per tool. Hermes, OpenClaw, Claude Code
and Codex all sit in that shape. Beating them is not a feature list; it is the
same work in fewer round trips and fewer tokens per turn.

## The bar

Measured on the same model, the same machine and the same suite as Task 017.

| property | today (campaign 9) | the bar |
| --- | --- | --- |
| model requests per turn | 2.34 | **1.3 or fewer** |
| turn p50 / p95 | 13.4 s / 29.9 s | p50 **under 6 s** on the same provider |
| harness share of turn time | median 0.0 %, p95 4 % | **under 15 %**; already met |
| time to the first visible token | not measured | **under 2 s** |
| warm turn admission | 2.4 ms median, 5.1 ms max | **under 5 ms**; already met |
| recall at 5 without a search call | 1.000 with 0-3 searches | **0.95 with no search** |
| cost per turn | $0.0014 | **$0.0012 or less** |
| model calls for housekeeping | 0 | **0** |
| cold start | not measured | **under 300 ms** |

Two rows are already met, and that is the useful finding: measured from the
journals, the harness's own time is 0.0 % of a turn at the median and 4 % at
p95. A turn is the provider's time and nothing else, so **round trips are the
only lever left in the harness** - and the count, not the CPU.

The provider's own latency is 2.4x what it was at calibration (per request:
2.08 s median then, 4.89 s now), so a bar written in seconds alone would only
measure the provider's day. Two of the rows above are the harness's own work
and do not move with the provider: requests per turn, and the harness share of
turn time. Both are the point of the generation gap.

## Where the round trips go

Campaign 9, by family, model requests per turn: memory 1.96, artifact 2.69.
The artifact turns pay for a second request because the model asks for a tool,
reads the result, and then answers. That second request is the whole gap: the
harness already knows which read-only work the request implies.

## The four moves, in the order that pays

1. **Prefetch the read-only work during compilation.** The request arrives, the
   context compiles, the capability page names the tools it can offer - and the
   read-only ones the request implies (memory search, artifact read, artifact
   sort under a lease) run *before* the first model request, with their results
   in the same prompt. Nothing is written, nothing is leased, no model call is
   added. This removes one round trip from every artifact turn and every memory
   turn that searches once.
2. **Run independent read-only calls together.** ADR 0038 made ready calls run
   in order because a parallel batch could not be verified line by line. A
   read-only batch can be: same lease, same verifier, results in request order.
   One round trip instead of N.
3. **Answer while the tool works.** The first visible token must not wait for a
   tool. Where the model already has enough to state the outcome ("sorting the
   attached list"), the answer starts before the verification finishes and the
   result follows in the same turn.
4. **Route by task class.** Intent and plan for a compiler do not need the
   answer model. Measure time to first token per route and per task class, and
   send the plan to the cheapest one that keeps the eval suite unchanged. This
   is the only move that touches the provider, so it is last.

Moves 1 and 2 are the generation gap and both are harness-only: no model
weights, no new capability, no prompt prefix change. Move 3 is a stream
contract change. Move 4 is a configuration policy with a measurement.

## Exit criteria

- Requests per turn 1.3 or fewer over the full suite, with recall at 5 still
  1.000 on both profiles and every gate that does not depend on the provider
  passing.
- Harness share of turn time under 15 %, measured from the journal split
  (turn completion minus the model request spans).
- Time to the first visible token under 2 s on p95, measured at the stream.
- No new model call added per turn; the request body stays at or below 4.7 KB.

## The first slice

Prefetch for the memory and artifact read paths, behind the capability page
hint that already exists, gated by a counter that the journal records
(`prefetch.executed` with the tool name and its byte size). Measure it on P2,
P3 and X4 first: those are the families that pay two requests today.
