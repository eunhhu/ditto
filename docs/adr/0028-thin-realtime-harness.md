# ADR 0028: Thin real-time harness

Status: proposed on 2026-09-30. It is decided per phase as each lands; the
full design is in [docs/design/realtime-harness.md](../design/realtime-harness.md).

## Context

Measurements with `scripts/measure-harness.py` (Raspberry Pi 5, release build,
instant loopback model) show where the harness spends its effort:

- Only about half of each prompt (median 50.6 %) is byte-identical to the
  previous turn's, so a provider or local KV cache recomputes the rest every
  turn. The complete-set capsule orders memories by relevance to each
  question, the history window slides once past eight exchanges, and the
  local time sits in the instructions ahead of both.
- Every streamed delta costs 74–80 µs as its own SQLite transaction, and
  journaling is 81–107 bytes per answer byte.
- Each request re-journals the whole prompt.
- One global run slot rejects concurrent runs from other sessions (1 of 3
  accepted).
- Turns re-read and re-verify context on the critical path (5–10 ms before
  dispatch).

## Decision (proposed)

1. **Context planes.** The prompt is layered from least to most volatile:
   constitution, tool surface, memory base, thread, turn tail, working set.
   Changes enter at the tail as deltas. The memory base (epochs) and the
   thread (stepped, extractive compaction) are rewritten only at explicit,
   journaled boundaries. Budgets follow the model's context window. This
   becomes turn payload version 6.
2. **Two streaming planes.** Advisory live deltas are fanned out immediately
   and are outside the durable sequence. Text is journaled in coalesced chunks
   (48 ms / 2 KB / structural boundaries). Tool calls stay durable before
   execution and terminals durable before reporting.
3. **Journal writer.** One writer thread group-commits batches; reads go to a
   WAL reader pool; no SQLite call runs on async workers; large immutable
   payload parts become content-addressed blobs.
4. **Session actors.** Hot state lives in memory per session, derived from
   committed events. Runs are FIFO within a session and parallel across
   sessions, bounded by per-provider lanes, with a `queued` status instead of
   429.
5. **One tool lifecycle.** All builtin tools share a lifecycle; independent
   read-only calls run in parallel; tool progress feeds the live plane.

Kept: provenance, extractive compaction only, no housekeeping model calls,
leases and claims, replay without provider, artifact or network I/O.

## Consequences

Phase A (layout) should make a local model prefill only the new tail of each
turn. Phases B–D remove per-token transactions, most journal bytes and the
single-run ceiling. Each phase amends the contracts it touches (payload
version 6, the event protocol's publish rule, ADR 0016's run slot, payload
blob references, the per-turn verification of ADR 0010/0013) when it lands,
with its measured before and after.
