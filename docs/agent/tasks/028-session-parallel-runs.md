# Task 028: Session-parallel runs (ADR 0028 Phase D)

Contract and evidence in one file. Decision:
[ADR 0028](../../adr/0028-thin-realtime-harness.md), Phase D; design in
[realtime-harness](../../design/realtime-harness.md).

## Problem

One kernel-wide slot admitted one run or sort at a time. A chat in the web
app, a Telegram conversation and a scheduled reminder in different sessions
excluded each other: of three sessions starting a run at once, one was
accepted and two got HTTP 429, and a due reminder waited for any other
session's run to end.

## Contract

- Each session has at most one active run or sort; up to four sessions run at
  once (`MAX_ACTIVE_RUNS`). A second run in a busy session, or work beyond the
  limit, is `Busy` (HTTP 429) without acceptance or a queue, as before.
- Status, cancellation and completion address the session's own active run.
  Shutdown closes admission, cancels every active run and drains them all.
- The scheduler starts due one-shot and repeat work in due order, a one-shot
  first at equal times, as soon as the item's own session can start. Work in
  a busy session waits for that session (`runtime_busy`) or its expiry.
- No wire change: `queued`, per-session queues and provider lanes were
  replaced (see the ADR).

## Exit criteria

1. Three sessions starting a run at once are all accepted and stream at once.
2. A second run in one session is still refused, and one session's run never
   holds back another session's scheduled work.
3. The canonical gate passes.

## Evidence (2026-09-30)

- `measure-harness.py`, release build, same machine, three sessions starting
  a run at once against a provider that answers after 0.5 s: before, one
  accepted and two HTTP 429; after, all three accepted, reaching the provider
  within 2 ms of each other, all finished after 513 ms. Turn start, streaming
  and journal figures are unchanged from Task 027.
- New kernel tests, checked red against a single kernel-wide slot and against
  a missing limit:
  - four sessions stream at once; a fifth session and a second run in a busy
    session are refused; cancelling one run ends it alone and frees a slot
    for the fifth; every run replays;
  - shutdown cancels and drains the runs of two sessions and then refuses new
    work;
  - a due reminder in an idle session starts while another session runs, and
    one in the busy session waits with `runtime_busy` until that session's
    run ends or it expires.
- Existing run, sort, schedule and repeat tests pass unchanged: each used one
  session for its busy case.
- `./scripts/agent-check.sh` passed on the unchanged final tree in 8 min 5 s
  (pinned Rust 1.88.0, load average near 6 on the shared machine): 564 Rust
  tests (557 workspace including doctests, seven built-CLI scenarios), 8 + 21
  Python tests, 12 web renderer cases and both smokes. Local log
  `target/task028/gate.log`, SHA-256
  `0445f36ae43967f3fc2ec5044a8ba544273c4199d94d26c54efd50c56f9ffe9c`.
