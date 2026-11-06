# ADR 0038: The ready calls of one response run in order

Status: accepted on 2026-11-06 for Task 037. Turn contract 15 (replacing 14,
as [ADR 0034](0034-one-turn-contract.md) requires). Amends
[ADR 0028](0028-thin-realtime-harness.md), which failed any response with more
than one ready tool call.

## Context

Ditto asks every provider for one tool call per response
(`parallel_tool_calls: false`) and, since ADR 0028, failed the whole turn
with a protocol error when a response carried a second ready call anyway.
ADR 0028 left this to be revisited once such turns were measured.

The Task 017 calibration measured them. DeepSeek V4.1 Flash through
OpenCode Go ignores the request: 16 of 130 attempts (12%) failed on it, all
with calls that a person would expect to run together - two memory searches
with different words, or reading an attachment and then sorting it. Four of
the five clean attempts of "say you do not know" (P5) failed this way, and no
failed attempt produced an answer.

Two ways out were weighed. Keeping the first call and dropping the rest wastes
a request whenever the model planned both and hides calls the model made.
Running the calls concurrently needs per-call journal phases and a merge order
for replay, the cost ADR 0028 declined.

## Decision

- **One after another, in call order.** A response may yield at most
  `MAX_READY_CALLS_PER_REQUEST` (4) ready calls. They run one at a time in the
  order the model produced them; each keeps its own lifecycle
  (`tool.requested` to `tool.output`) under the same request index, and its
  result joins the conversation before the next call runs. Every result
  reaches the next model request.
- **Same rules per call.** Each call is checked against the turn's tools,
  read grant, memory-write bounds and deadline exactly as a lone call is; a
  web read earlier in the response forbids a memory write later in it, as it
  would across requests. A failure stops the turn at that call: later calls
  of the response do not run.
- **A fifth ready call fails the response** before any of its calls run, with
  a protocol error, so the bound is checked from the model output alone.
- Ditto still asks for one call per response; providers that honor it are
  unaffected.
- **Memory search guidance.** The same calibration found turns answering
  without searching memory, or searching only with the request's words: the
  compiled context favours memories that share words with the message, and
  memory search is lexical. Contract 15's instructions say so and ask the
  model to search before saying it does not know, to try the words a memory
  may use, and to follow a memory that points to another place or object.
- **Plural folding in memory search.** Calibration 2 still missed a memory
  because "flight" did not find "My flights originate in Lakeside". Memory
  search now folds regular English plural endings on both the query and the
  memory before matching. The context compiler keeps exact words, so its
  complete-set selection is unchanged. Memory search is recomputed on replay,
  so this belongs to contract 15.
- Replay rebuilds the same sequence from the recorded calls. Turns recorded
  under contract 14 stay readable but replay rejects them (ADR 0034).

## Consequences

- Turns no longer fail when a provider batches calls, and a batched pair costs
  one model request instead of two.
- A response can now take up to four tool executions before the model sees a
  result. The per-turn bounds (requests, memory writes, sort runs, deadline)
  are unchanged and still cap the turn.
- Concurrent execution of read-only calls stays out of scope: the measured
  calls finish in milliseconds against seconds of model time.
