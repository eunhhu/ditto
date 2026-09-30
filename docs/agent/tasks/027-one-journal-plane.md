# Task 027: One journal plane, each fact once (ADR 0028 Phase C)

Contract and evidence in one file. Decision:
[ADR 0028](../../adr/0028-thin-realtime-harness.md), Phase C; design in
[realtime-harness](../../design/realtime-harness.md).

## Problem

Every streamed delta was its own durable event and SQLite transaction: 206
events for a 200-delta answer, 1,006 for a 1,000-delta one, and 80–111 journal
bytes per answer byte. A client saw a burst of 1,000 deltas 116 ms after the
provider sent it. Each request re-journaled the instructions, tools, capsule
and history (10–25 KB, growing with the conversation), although replay already
rebuilt every part of it from earlier durable events. `context.compiled` and
`capabilities.selected` also repeated the capsule and the builtin schemas.

## Contract (turn payload version 7)

- **Coalesced text.** Consecutive text deltas commit as one `model.output`
  chunk: `stream_event.sequence` is the first provider sequence it covers and
  `through_sequence` the last, absent for one delta; `admitted_at` is its last
  delta's. The first text after 48 ms without a text commit commits at once.
  Later text commits 48 ms after its chunk opened, before the chunk would pass
  2 KiB, or with the next non-text output, failure or terminal in one
  transaction. Bounds count every provider event and bound every durable
  event, as before.
- **Requests as digests.** `model.requested` records `request_id`, `deadline`
  and `request_sha256`, the SHA-256 of the request's JSON encoding as sent.
  One function builds requests for the runtime and replay; replay rebuilds
  each request from durable state and must reproduce the digest.
- **Derived parts not recorded.** `context.compiled` has no `capsule`;
  `capabilities.selected` has no `schemas`.
- Replay rejects earlier forms in version-7 turns and version-7 forms in
  earlier turns. Versions 1–6 replay under their own rules.
- Clients change only where they read a dropped field: the web inspector reads
  the compiled nodes. The corpus harness binds each driver observation to the
  journaled digest (report schema 5).

## Exit criteria

1. At most 50 durable events per 1,000-delta answer in `measure-harness.py`.
2. At most 3 journal bytes per answer byte.
3. Text reaches a client without waiting for a flush when the stream is quiet,
   and within the flush interval otherwise.
4. Replay rebuilds requests and text exactly and rejects forged digests,
   chunk ranges and mixed forms.
5. The canonical gate passes.

Criterion 2 is not met (see Evidence and the ADR); the rest pass.

## Evidence (2026-09-30)

- `measure-harness.py`, release build, same machine, 200- and 1,000-delta
  answers, before (Task 026 build) and after:

  | Measure | Before | After |
  | --- | --- | --- |
  | Durable events per turn | 206 / 1,006 | 8 / 9 |
  | Journal bytes per answer byte | 111 / 80 | 16.3 / 4.9 |
  | Provider to the last text on a client's stream | 56 / 116 ms | 2.6 / 10–16 ms |
  | Provider to the first text on the stream | 1.7 / 5.2 ms | 1.6 / 5.6–7.6 ms |
  | Client POST to the provider receiving the request | 2.8 / 2.9 ms | 2.0 / 2.1 ms |

  The first-text times include the Python mock building its whole stream.
  A version-7 turn with a 4 KB answer journals 19.7 KB: compiled context
  6.5 KB, capability selection 3.3 KB, request 0.27 KB, text chunks and
  terminal 5.2 KB, `turn.finished` 4.3 KB (the answer again). Closing the gap
  to 3 bytes per byte would need node references in the compiled context and
  an answer derived from its chunks; both change several readers and are
  deferred.
- New kernel tests, checked red against the defect each guards:
  - a 1,000-delta burst commits its first delta alone, then chunks of at most
    2 KiB covering the sequence exactly; forged gaps, overlaps, a range on one
    delta or on the terminal, and changed chunk text fail replay (a range on
    the terminal passes when replay stops checking the event kind);
  - a paced provider shows "a" durable before it sends more, "b" and "c"
    durable during its 250 ms silence and "d" durable at once after it (fails
    without the timer flush and without the quiet-interval rule);
  - 4,096 text deltas without a terminal fail on the event bound with chunks
    covering exactly 4,096 events; a chunk claiming one more fails replay;
  - each `model.requested` holds the digest of the request the driver
    received and no request body (under 512 bytes), and replay rebuilds both
    requests of a tool turn exactly.
- Existing forgery tests now forge the digest of a changed request (tool
  choice, prompt-cache controls, the time note, an added time instruction, a
  conversation without its history, a context whose validity changed) and
  replay rejects each; they fail when replay skips the digest check. The
  relabel test turns a version-7 turn into the version-6 forms, which replay
  as version 6, and rejects each version's forms under the other.
- Corpus harness (21 tests): observations carry the exact request bytes;
  their SHA-256 must equal the journaled digest. New adversaries: a request
  other than the journaled one, a zeroed digest, and changed cancellation or
  request IDs.
- The browser E2E passed 26/26 against the debug daemon: the answer streams
  incrementally and the inspector lists the memory and why it was sent.
- `./scripts/agent-check.sh` passed on the unchanged final tree in 1 min 39 s
  (warm caches, pinned Rust 1.88.0): 561 Rust tests (554 workspace including
  doctests, seven built-CLI scenarios), 8 + 21 Python tests, 12 web renderer
  cases and both smokes. Local log `target/task027/gate.log`, SHA-256
  `615d8e0dd3d34ecff46702ce4b909582de21930adad672d09b9c1289fde7c1ca`.
