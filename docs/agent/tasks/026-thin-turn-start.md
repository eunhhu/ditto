# Task 026: Thin turn start and storage off async threads (ADR 0028 Phase B)

Contract and evidence in one file. Decision:
[ADR 0028](../../adr/0028-thin-realtime-harness.md), Phase B; design in
[realtime-harness](../../design/realtime-harness.md).

## Problem

A run spent 5 ms (200-delta answers) to 10 ms (1,000-delta answers) between
its durable input and its provider request, and the cost grew with the
previous answer. Per-stage timers on a release build showed where:

| Stage | 200-delta turn | 1,000-delta turn |
| --- | --- | --- |
| Context projection sync: every event since the last turn | 2.0 ms | 6.7 ms |
| Snapshot capture and source rechecks | 0.4 ms | 0.4 ms |
| Thread history: one read per past exchange | 0.35 ms | 0.4 ms |
| Tool manifests read, hashed, parsed and revalidated | 0.9 ms | 1.0 ms |
| Three appends, each its own commit plus a broadcast copy | 1.1 ms | 1.1 ms |

All of it ran on async runtime threads: HTTP handlers, the turn loop, the
scheduler and event replay called SQLite directly.

## Contract

- **Context reuse.** A run reuses its session's verified context while no
  `context.node.recorded` event has been committed since it was taken, checked
  with one kind-index probe that also sees other writers. Sessions with a
  task-scoped node or a validity window are compiled from the projection every
  turn. Sources are checked once for every task of the session, so reused
  context skips the per-turn provenance lookups.
- **Kept threads.** Each session's thread is kept between turns and advanced
  by the conversation events committed since (usually one `turn.finished`),
  read from the conversation index. It is reloaded when absent, more than 64
  events behind, or ahead of the input. It holds exactly what the indexed reads
  return, so replay's recomputation is unchanged.
- **Context-only deltas.** The projection reads only `context.node.recorded`
  events of a delta through the kind index. The normal-delta ceiling counts
  those nodes; ordinary events are never read (amends ADR 0013).
- **Tool contracts once per process.** `InvocableContract` holds a manifest,
  schema and revision validated once; each turn's epoch binds it without
  revalidation. The kernel keeps at most its three builtin contracts; failures
  are not kept (amends ADR 0014).
- **One commit for the prelude.** `context.compiled` and
  `capabilities.selected` are staged and committed with the first
  `model.requested`, or with the `turn.failed` that ends the turn first. Events,
  order and payloads are unchanged, so replay is unchanged and no payload
  version is added. Events are published only after that commit.
- **Storage off async threads.** Daemon handlers run kernel calls on the
  blocking pool. Runs, sorts and the scheduler run on blocking threads that
  drive their I/O through the runtime; on a current-thread runtime they stay
  tasks. Debug builds reject journal access on a thread that drives async
  tasks: runtime workers are marked when they first park, and the thread
  serving the router once startup is done.
- A copy of each event is broadcast only while someone subscribes, and appends
  reuse a prepared statement.

## Exit criteria

1. Before-dispatch time ≤ 2 ms median in `measure-harness.py`, independent of
   the previous answer's length.
2. No SQLite call on async runtime threads in the daemon, enforced where the
   gate runs the daemon's runtime.
3. Reused context and kept threads equal a fresh read, including after other
   writers, and replay is unchanged.
4. The canonical gate passes.

## Evidence (2026-09-30)

- `measure-harness.py`, release build, same machine as the baseline, eight
  runs across both answer lengths:

  | Measure | Before (200 / 1,000 deltas) | After (both) |
  | --- | --- | --- |
  | Durable input to `model.requested`, median | 5 / 10 ms | 1 ms |
  | Same, mean of the whole-millisecond timestamps | — | 1.0–1.4 ms |
  | Client POST to the provider receiving the request | 6.5 / 11.8 ms | 2.7–3.0 ms |
  | Client POST to the 202 response | 1.0–1.1 ms | 1.1–1.2 ms |
  | Harness cost per streamed delta | 74–80 µs | 55–60 µs |

  One 1,000-delta run under a load average near 4.5 gave a 2 ms median (mean
  1.6 ms); the other seven gave 1 ms. The 0.1 ms added to acceptance is the
  handler's hop to the blocking pool. Events per turn, journal bytes and
  concurrency are unchanged; phases C and D target them.
- New kernel tests, each checked red against the defect it guards:
  - a new memory, a correction and a node appended by another writer straight
    to the journal all reach the next turn (fails if reuse skips the probe);
  - a node scoped to one run never reaches another run's context, and nodes
    that expire or become valid between turns are evaluated at each turn (both
    fail without the invariant check);
  - a reset written by another writer empties the kept thread, and replay
    recomputes the same window (fails if the kept thread ignores resets);
  - three turns page `artifact.read` and `web.fetch` once each (fails without
    the contract cache);
  - on a multi-thread runtime whose workers are marked, runs complete (fails
    when runs are spawned on workers: the guard panics).
- Projection tests: 65,537 ordinary events synchronize with no event work;
  with the node ceiling lowered to three in a unit test, three nodes are
  accepted and the fourth is rejected unread with the checkpoint at node three.
  The million-event scale test now visits 10,000 context nodes at startup
  instead of 1,010,000 events.
- Event store tests: a batch commits every event or none; a marked thread
  cannot reach SQLite and leaves the store usable.
- The guard in the gate: with `/health` changed to query SQLite inline, the
  offline baseline, whose fixture server now uses the daemon's runtime, failed
  at startup; restored, it passes. The browser E2E passed 26/26 against the
  debug daemon, including SSE streaming.
- `./scripts/agent-check.sh` passed on the unchanged final tree in 5 min 55 s
  (pinned Rust 1.88.0): 557 Rust tests (550 workspace including doctests,
  seven built-CLI scenarios), 8 + 21 Python tests, 12 web renderer cases and
  both smokes. Local log `target/task026/gate.log`, SHA-256
  `62a205574ebde486a7e7b66f5dd67e6328fbfda48903a57c24d7e8c87ebc2343`.
